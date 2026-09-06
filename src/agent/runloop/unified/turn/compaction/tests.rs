use super::{
    CompactionContext, CompactionState, GroundedFactRecord, SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION,
    SessionMemoryEnvelope, build_server_compaction_context_management, build_summarized_fork_history,
    compact_history_for_recovery_in_place, compact_history_from_index_in_place, compact_history_in_place,
    compact_history_in_place_with_events, compact_history_on_model_switch_in_place, effective_compaction_threshold,
    inject_latest_memory_envelope, latest_memory_envelope_path_for_session, manual_compact_history_in_place,
    maybe_auto_compact_history, resolve_compaction_threshold,
};
use crate::agent::runloop::unified::context_manager::ContextManager;
use crate::agent::runloop::unified::inline_events::harness::HarnessEventEmitter;
use crate::agent::runloop::unified::state::SessionStats;
use async_trait::async_trait;
use hashbrown::HashMap;
use serde_json::json;
use std::fs;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::sync::RwLock;
use vtcode_commons::llm::Usage;
use vtcode_core::compaction::ManualCompactionOptions;
use vtcode_core::compaction::effective_session_context_budget;
use vtcode_core::compaction::memory_envelope::DEFAULT_OUTPUT_RESERVE_TOKENS;
use vtcode_core::config::constants::tools as tool_names;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::llm::provider::{
    LLMError, LLMProvider, LLMRequest, LLMResponse, Message, MessageRole, ResponsesCompactionOptions, ToolCall,
};

struct LocalCompactionProvider;

struct ProviderCompactionProvider;

struct NoOpProviderCompactionProvider;

struct FailingProviderCompactionProvider;

/// Inline-dispatched provider (Anthropic-shaped) that rejects the
/// `compact_20260112` inline edit but serves plain summary requests. Models a
/// provider where NativeInline cannot fire; the recovery path must fall back to
/// Local summarization rather than aborting.
struct InlineRejectingRecoveryProvider;

struct RecordingProviderCompactionProvider {
    seen_history: Arc<RwLock<Vec<Message>>>,
}

struct ContextSizedProvider {
    context_size: usize,
    provider_name: &'static str,
}

#[async_trait]
impl LLMProvider for ContextSizedProvider {
    fn name(&self) -> &str {
        self.provider_name
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
    }
}

#[async_trait]
impl LLMProvider for LocalCompactionProvider {
    fn name(&self) -> &str {
        "stub"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

#[async_trait]
impl LLMProvider for ProviderCompactionProvider {
    fn name(&self) -> &str {
        "provider-stub"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    async fn compact_history(&self, _model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        let mut compacted = Vec::new();
        compacted.push(Message::system("Previous conversation summary:\nProvider compacted history".to_string()));
        compacted.extend(history.iter().rev().take(2).cloned().collect::<Vec<_>>());
        compacted.reverse();
        Ok(compacted)
    }

    async fn compact_history_with_options(
        &self,
        model: &str,
        history: &[Message],
        _options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.compact_history(model, history).await
    }

    fn supports_responses_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

#[async_trait]
impl LLMProvider for NoOpProviderCompactionProvider {
    fn name(&self) -> &str {
        "noop-provider-stub"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    async fn compact_history(&self, _model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        Ok(history.to_vec())
    }

    async fn compact_history_with_options(
        &self,
        _model: &str,
        history: &[Message],
        _options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        Ok(history.to_vec())
    }

    fn supports_responses_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

#[async_trait]
impl LLMProvider for FailingProviderCompactionProvider {
    fn name(&self) -> &str {
        "failing-provider-stub"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        // Simulate a true infrastructure failure: the provider cannot produce any
        // response. Under the unified dispatch this provider routes to NativeInline
        // (it does not override `supports_manual_openai_compaction`), so `generate` is the
        // only call path; failing it here makes Local fallback impossible, so the
        // compaction propagates `Err` and the existing history is preserved.
        Err(LLMError::Provider {
            message: "provider unreachable".to_string(),
            metadata: None,
        })
    }

    async fn compact_history(&self, _model: &str, _history: &[Message]) -> Result<Vec<Message>, LLMError> {
        Err(LLMError::Provider {
            message: "provider compaction failed".to_string(),
            metadata: None,
        })
    }

    fn supports_responses_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supports_native_inline_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

#[async_trait]
impl LLMProvider for RecordingProviderCompactionProvider {
    fn name(&self) -> &str {
        "recording-provider-stub"
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    async fn compact_history(&self, _model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        *self.seen_history.write().await = history.to_vec();
        Ok(history.to_vec())
    }

    async fn compact_history_with_options(
        &self,
        _model: &str,
        history: &[Message],
        _options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.compact_history("stub-model", history).await
    }

    fn supports_responses_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
        // Routes to NativeStandalone (uses `compact_history_with_options`) so the
        // recorded history reflects the provider-native compaction input.
        true
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

#[async_trait]
impl LLMProvider for InlineRejectingRecoveryProvider {
    fn name(&self) -> &str {
        "inline-rejecting-recovery"
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        // Reject the inline compaction request (carries `context_management`);
        // serve the Local summary request (no `context_management`).
        if request.context_management.is_some() {
            return Err(LLMError::Provider {
                message: "provider rejected inline compact edit".to_string(),
                metadata: None,
            });
        }
        Ok(LLMResponse::new("stub-model", "summary"))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn supports_responses_compaction(&self, _model: &str) -> bool {
        true
    }

    fn supports_native_inline_compaction(&self, _model: &str) -> bool {
        true
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000
    }
}

fn test_history() -> Vec<Message> {
    vec![
        Message::user("message-0".to_string()),
        Message::assistant("assistant-0".to_string()),
        Message::tool_response("call-0".to_string(), "tool-0".to_string()),
        Message::user("message-1".to_string()),
        Message::assistant("assistant-1".to_string()),
        Message::tool_response("call-1".to_string(), "tool-1".to_string()),
        Message::user("message-2".to_string()),
        Message::assistant("assistant-2".to_string()),
        Message::tool_response("call-2".to_string(), "tool-2".to_string()),
        Message::user("message-3".to_string()),
        Message::assistant("assistant-3".to_string()),
        Message::tool_response("call-3".to_string(), "tool-3".to_string()),
    ]
}

fn test_history_with_memory_envelope() -> Vec<Message> {
    let mut history = vec![Message::system(
        "[Session Memory Envelope]\nSummary:\nExisting summary".to_string(),
    )];
    history.extend(test_history());
    history
}

fn assert_local_compaction_history(history: &[Message], _old_envelope_index: usize) {
    assert_local_compaction_history_with_user_count(history, 4);
}

fn assert_local_compaction_history_with_user_count(history: &[Message], _retained_user_messages: usize) {
    let envelope_found = history.iter().any(|message| {
        message.role == MessageRole::System && message.content.as_text().contains("[Session Memory Envelope]")
    });
    let summary_found = history.iter().any(|message| {
        message.role == MessageRole::System && message.content.as_text().contains("Previous conversation summary")
    });
    assert!(
        envelope_found || summary_found,
        "Expected either a memory envelope or a summary in the compacted history"
    );
    let user_count = history.iter().filter(|message| message.role == MessageRole::User).count();
    assert!(user_count >= 1, "Expected at least 1 user message, got {user_count}");
    assert!(history.len() >= 2, "history.len()={} should be at least 2", history.len());
}

fn assert_history_contains_messages(history: &[Message], expected_messages: &[Message]) {
    for expected in expected_messages {
        assert!(
            history.iter().any(|message| message == expected),
            "compacted history lost the {:?} message {:?}",
            expected.role,
            expected.content.as_text()
        );
    }
}

fn read_file_tool_call(id: &str, path: &str) -> ToolCall {
    ToolCall::function(id.to_string(), tool_names::READ_FILE.to_string(), json!({ "path": path }).to_string())
}

fn file_operation_read_tool_call(id: &str, path: &str) -> ToolCall {
    ToolCall::function(
        id.to_string(),
        tool_names::UNIFIED_FILE.to_string(),
        json!({ "action": "read", "path": path }).to_string(),
    )
}

fn assistant_with_tool_call(tool_call: ToolCall) -> Message {
    let mut message = Message::assistant(String::new());
    message.tool_calls = Some(vec![tool_call]);
    message
}

fn test_context_manager() -> ContextManager {
    ContextManager::new("You are VT Code.".to_string(), (), Arc::new(RwLock::new(HashMap::new())), None)
}

#[tokio::test]
async fn manual_compaction_succeeds_without_server_side_support() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    session_stats.set_previous_response_chain("stub", "stub-model", Some("resp_123"), &[]);
    let mut context_manager = test_context_manager();
    context_manager.update_token_usage(&Some(Usage {
        prompt_tokens: 900,
        completion_tokens: 10,
        total_tokens: 910,
        ..Usage::default()
    }));

    let outcome = compact_history_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &mut history,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("manual compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.original_len, 12);
    // This fixture is smaller than the internal continuity-tail target, so
    // local compaction keeps the complete protocol history and adds the
    // summary/envelope metadata without dropping any live context.
    assert!(outcome.compacted_len >= outcome.original_len);
    assert_local_compaction_history(&history, 0);
    assert_history_contains_messages(&history, &test_history());
    assert_eq!(session_stats.previous_response_id_for("stub", "stub-model"), None);
    assert!(context_manager.current_token_usage() <= 900);
    assert!(latest_memory_envelope_path_for_session(temp.path(), "session-alpha").is_some());
}

#[tokio::test]
async fn manual_compaction_emits_local_compaction_boundary_event() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let harness_path = temp.path().join("harness.jsonl");
    let harness_emitter = HarnessEventEmitter::new(harness_path.clone()).expect("emitter");
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_in_place_with_events(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            Some(&harness_emitter),
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
    .expect("compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Local);
    let content = fs::read_to_string(harness_path).expect("read harness log");
    assert!(content.contains("\"type\":\"thread.compact_boundary\""));
    assert!(content.contains("\"mode\":\"local\""));
    assert!(content.contains("\"new_segment_id\":\"segment-00000001\""));
}

#[tokio::test]
async fn provider_compaction_emits_provider_boundary_event() {
    let temp = tempdir().expect("tempdir");
    let provider = ProviderCompactionProvider;
    let harness_path = temp.path().join("provider-harness.jsonl");
    let harness_emitter = HarnessEventEmitter::new(harness_path.clone()).expect("emitter");
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_in_place_with_events(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            Some(&harness_emitter),
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
    .expect("compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Provider);
    let content = fs::read_to_string(harness_path).expect("read harness log");
    assert!(content.contains("\"type\":\"thread.compact_boundary\""));
    assert!(content.contains("\"mode\":\"provider\""));
}

#[tokio::test]
async fn manual_compaction_clears_previous_response_chain() {
    let temp = tempdir().expect("tempdir");
    let provider = ProviderCompactionProvider;
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    session_stats.set_previous_response_chain("provider-stub", "stub-model", Some("resp_123"), &[]);
    let mut context_manager = test_context_manager();

    let outcome = manual_compact_history_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        &ManualCompactionOptions::default(),
        false,
    )
    .await
    .expect("manual OpenAI compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Provider);
    assert_eq!(session_stats.previous_response_id_for("provider-stub", "stub-model"), None);
}

#[tokio::test]
async fn model_switch_compaction_tags_boundary_event_with_model_switch_trigger() {
    let temp = tempdir().expect("tempdir");
    let harness_path = temp.path().join("harness.log");
    let emitter = HarnessEventEmitter::new(harness_path.clone()).expect("emitter");
    let provider = ProviderCompactionProvider;
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_on_model_switch_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            Some(&emitter),
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
    )
    .await
    .expect("model-switch compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Provider);

    let log = fs::read_to_string(&harness_path).expect("harness log readable");
    assert!(
        log.contains("\"trigger\":\"model_switch\""),
        "compact boundary event should carry the model_switch trigger, got: {log}"
    );
}

#[tokio::test]
async fn manual_compaction_native_only_rejects_provider_without_standalone_compaction() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let err = manual_compact_history_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        &ManualCompactionOptions::default(),
        true,
    )
    .await
    .expect_err("native-only should reject a non-standalone provider");

    assert!(err.to_string().contains(
        "`--native-only` `/compact` requires a provider that exposes a native server-side compaction endpoint"
    ));
    assert!(err.to_string().contains("Run `/compact` without `--native-only`"));
    assert_eq!(history, original_history);
}

#[tokio::test]
async fn manual_compaction_compacts_locally_for_non_native_provider() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = manual_compact_history_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        &ManualCompactionOptions::default(),
        false,
    )
    .await
    .expect("local compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Local);
    assert_history_contains_messages(&history, &test_history());
    assert_local_compaction_history(&history, 0);
}

#[tokio::test]
async fn manual_compaction_noop_preserves_existing_history() {
    let temp = tempdir().expect("tempdir");
    let provider = NoOpProviderCompactionProvider;
    let mut history = test_history_with_memory_envelope();
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = manual_compact_history_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        &ManualCompactionOptions::default(),
        false,
    )
    .await
    .expect("noop compaction succeeds");

    assert!(outcome.is_none());
    assert_eq!(history, original_history);
}

#[tokio::test]
async fn provider_compaction_noop_preserves_existing_history() {
    let temp = tempdir().expect("tempdir");
    let provider = NoOpProviderCompactionProvider;
    let mut history = test_history_with_memory_envelope();
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_in_place_with_events(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
    .expect("noop compaction succeeds");

    assert!(outcome.is_none());
    assert_eq!(history, original_history);
}

#[tokio::test]
async fn provider_compaction_preserves_original_repeated_file_reads() {
    let temp = tempdir().expect("tempdir");
    let seen_history = Arc::new(RwLock::new(Vec::new()));
    let provider = RecordingProviderCompactionProvider { seen_history: Arc::clone(&seen_history) };
    let mut history = vec![
        assistant_with_tool_call(read_file_tool_call("call-1", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-1".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "older contents"
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
        assistant_with_tool_call(read_file_tool_call("call-2", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-2".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "newer contents"
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
    ];
    history.extend(test_history());
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_in_place_with_events(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
    .expect("provider compaction succeeds");

    assert!(outcome.is_none());
    assert_eq!(history, original_history);

    let seen = seen_history.read().await.clone();
    assert_eq!(seen.len(), original_history.len());
    assert!(seen[1].content.as_text().contains("older contents"));
    assert!(!seen[1].content.as_text().contains("deduped_read"));
}

#[test]
fn dedup_repeated_file_reads_rewrites_only_older_exact_matches() {
    let history = vec![
        assistant_with_tool_call(read_file_tool_call("call-1", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-1".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "older contents"
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
        assistant_with_tool_call(file_operation_read_tool_call("call-2", "src/lib.rs")),
        Message::tool_response(
            "call-2".to_string(),
            json!({
                "path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "newer contents"
            })
            .to_string(),
        ),
    ];

    let deduped = super::dedup_repeated_file_reads_for_local_compaction(&history);

    let older_payload: serde_json::Value =
        serde_json::from_str(deduped[1].content.as_text().as_ref()).expect("json payload");
    assert_eq!(older_payload.get("deduped_read").and_then(serde_json::Value::as_bool), Some(true));
    assert_eq!(older_payload.get("note").and_then(serde_json::Value::as_str), Some(super::DEDUPED_FILE_READ_NOTE));
    assert_eq!(older_payload.get("file_path").and_then(serde_json::Value::as_str), Some("src/lib.rs"));
    assert!(deduped[3].content.as_text().contains("newer contents"));
    assert!(!deduped[3].content.as_text().contains("deduped_read"));
}

#[test]
fn dedup_repeated_file_reads_keeps_different_slices_and_chunked_reads() {
    let different_slice_history = vec![
        assistant_with_tool_call(read_file_tool_call("call-1", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-1".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 20,
                "result": "slice one"
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
        assistant_with_tool_call(read_file_tool_call("call-2", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-2".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 21,
                "end_line": 40,
                "result": "slice two"
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
    ];
    let chunked_history = vec![
        assistant_with_tool_call(read_file_tool_call("call-3", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-3".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "first chunk",
                "spool_chunked": true,
                "has_more": true
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
        assistant_with_tool_call(read_file_tool_call("call-4", "src/lib.rs")),
        Message::tool_response_with_origin(
            "call-4".to_string(),
            json!({
                "file_path": "src/lib.rs",
                "start_line": 1,
                "end_line": 40,
                "result": "second chunk",
                "spool_chunked": true,
                "has_more": false
            })
            .to_string(),
            tool_names::READ_FILE.to_string(),
        ),
    ];

    assert_eq!(
        super::dedup_repeated_file_reads_for_local_compaction(&different_slice_history),
        different_slice_history
    );
    assert_eq!(super::dedup_repeated_file_reads_for_local_compaction(&chunked_history), chunked_history);
}

#[test]
fn recovery_context_previews_include_latest_user_request_and_recent_distinct_tool_outputs() {
    let history = vec![
        Message::user("first request".to_string()),
        Message::tool_response("call-1".to_string(), "duplicate output".to_string()),
        Message::tool_response("call-2".to_string(), "distinct output".to_string()),
        Message::tool_response("call-3".to_string(), "duplicate output".to_string()),
        Message::user("latest request".to_string()),
    ];

    let previews = super::build_recovery_context_previews_with_workspace(&history, None);

    assert_eq!(previews[0], "Latest user request: latest request");
    assert_eq!(previews[1], "Tool output 1: duplicate output");
    assert_eq!(previews[2], "Tool output 2: distinct output");
    assert_eq!(previews.len(), 3);
}

#[test]
fn recovery_context_previews_fall_back_to_latest_assistant_text_when_needed() {
    let history = vec![Message::assistant("assistant summary".to_string())];

    let previews = super::build_recovery_context_previews_with_workspace(&history, None);

    assert_eq!(previews, vec!["Latest assistant text: assistant summary"]);
}

#[test]
fn recovery_context_previews_extract_structured_tool_guidance() {
    let history = vec![
        Message::user("search for Widget".to_string()),
        Message::tool_response(
            "call-1".to_string(),
            json!({
                "results": [],
                "path": "src/agent",
                "is_recoverable": true,
                "hint": "Try narrowing the path.",
                "next_action": "Retry with narrower filters.",
                "fallback_tool": "code_search",
                "fallback_tool_args": {"query": "Widget", "path": "src/agent"}
            })
            .to_string(),
        ),
    ];

    let previews = super::build_recovery_context_previews_with_workspace(&history, None);

    assert_eq!(previews[0], "Latest user request: search for Widget");
    assert!(previews[1].contains("No matches found in src/agent"));
    assert!(previews[1].contains("Try narrowing the path."));
    assert!(previews[1].contains("Next action: Retry with narrower filters."));
    assert!(previews[1].contains("Fallback tool: code_search"));
}

#[test]
fn recovery_context_previews_extract_nested_error_guidance_and_spool_excerpt() {
    let temp = tempdir().expect("tempdir");

    let history = vec![
        Message::user("review the read failure".to_string()),
        Message::tool_response(
            "call-1".to_string(),
            json!({
                "path": "src/main.rs",
                "spool_path": ".vtcode/context/tool_outputs/read_1.txt",
                "preview": "spooled-line-1\nspooled-line-2\nSpool excerpt: nested diagnostic",
                "error": {
                    "message": "Read failed",
                    "hint": "Inspect the spooled content.",
                    "next_action": "Retry with a smaller slice."
                }
            })
            .to_string(),
        ),
    ];

    let previews = super::build_recovery_context_previews_with_workspace(&history, Some(temp.path()));

    assert_eq!(previews[0], "Latest user request: review the read failure");
    assert!(previews[1].contains("Read failed"));
    assert!(previews[1].contains("Inspect the spooled content."));
    assert!(previews[1].contains("Next action: Retry with a smaller slice."));
    assert!(previews[1].contains("source_path: src/main.rs"));
    assert!(previews[1].contains("Spool excerpt:"));
    assert!(previews[1].contains("spooled-line-1"));
    assert_eq!(previews[1].matches("Spool excerpt:").count(), 1);
    assert!(previews[1].contains("Output excerpt: nested diagnostic"));
}

#[test]
fn recovery_context_previews_prefer_substantive_reads_over_recent_low_signal_outputs() {
    let history = vec![
        Message::user("tell me more".to_string()),
        Message::tool_response(
            "call-1".to_string(),
            json!({
                "path": "README.md",
                "content": "VT Code is an open-source coding agent with LLM-native code understanding."
            })
            .to_string(),
        ),
        Message::tool_response(
            "call-2".to_string(),
            json!({
                "path": "docs/ARCHITECTURE.md",
                "content": "VT Code follows a modular architecture designed for maintainability and extensibility."
            })
            .to_string(),
        ),
        Message::tool_response(
            "call-3".to_string(),
            json!({
                "count": 20,
                "items": [{"path": "docs/ide"}]
            })
            .to_string(),
        ),
        Message::tool_response(
            "call-4".to_string(),
            json!({
                "error": "Repeated reads of 'docs/ARCHITECTURE.md' with limited progress detected.",
                "next_action": "Try an alternative tool or narrower scope."
            })
            .to_string(),
        ),
    ];

    let previews = super::build_recovery_context_previews_with_workspace(&history, None);

    assert_eq!(previews[0], "Latest user request: tell me more");
    assert!(previews[1].contains("VT Code follows a modular architecture"));
    assert!(previews[2].contains("VT Code is an open-source coding agent"));
    assert!(previews[3].contains("Repeated reads of 'docs/ARCHITECTURE.md'"));
    assert!(
        previews.iter().all(|preview| !preview.contains("Listed 20 items")),
        "low-signal listing should be dropped when richer previews exist: {previews:?}"
    );
}

#[test]
fn legacy_memory_envelope_deserializes_with_new_fields_defaulted() {
    let envelope: SessionMemoryEnvelope = serde_json::from_value(json!({
        "session_id": "session-alpha",
        "summary": "Persisted summary",
        "task_summary": "Task tracker",
        "spec_summary": null,
        "evaluation_summary": null,
        "grounded_facts": [{
            "fact": "fact",
            "source": "tool:read_file"
        }],
        "touched_files": ["src/lib.rs"],
        "history_artifact_path": ".vtcode/history/session-alpha.jsonl",
        "generated_at": "2026-03-14T00:00:00Z"
    }))
    .expect("legacy envelope should deserialize");

    assert_eq!(envelope.schema_version, None);
    assert_eq!(envelope.objective, None);
    assert_eq!(envelope.verification_summary, None);
    assert!(envelope.constraints.is_empty());
    assert!(envelope.open_questions.is_empty());
    assert!(envelope.verification_todo.is_empty());
    assert!(envelope.delegation_notes.is_empty());
}

#[test]
fn refresh_session_memory_envelope_merges_existing_continuity_fields() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    fs::create_dir_all(temp.path().join(".vtcode").join("tasks")).expect("tasks dir");
    fs::write(
        temp.path().join(".vtcode").join("tasks").join("current_task.md"),
        "# Ship compaction cleanup\n- [ ] Run cargo nextest\n- [x] Wire in config\n",
    )
    .expect("write task");
    fs::write(
        temp.path().join(".vtcode").join("tasks").join("current_spec.md"),
        "# Spec\nKeep local compaction aligned with summarized forks.\n",
    )
    .expect("write spec");
    fs::write(
        temp.path().join(".vtcode").join("tasks").join("current_evaluation.md"),
        "# Eval\nNeed a regression test for repeated reads.\n",
    )
    .expect("write eval");

    let prior_envelope = SessionMemoryEnvelope {
        session_id: "session-alpha".to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary: "Prior summary".to_string(),
        objective: Some("Keep continuity".to_string()),
        task_summary: Some("Older task summary".to_string()),
        spec_summary: None,
        evaluation_summary: None,
        verification_summary: Some("- [x] Prior verification passed".to_string()),
        constraints: vec!["Do not redesign the harness".to_string()],
        grounded_facts: vec![GroundedFactRecord {
            fact: "Existing grounded fact".to_string(),
            source: "tool:read_file".to_string(),
        }],
        touched_files: vec!["src/old.rs".to_string()],
        open_questions: vec!["What should summarized forks retain?".to_string()],
        verification_todo: vec!["Confirm refresh runs at turn boundaries.".to_string()],
        delegation_notes: vec!["explorer: looked at compaction flow".to_string()],
        pending_intents: Vec::new(),
        applied_intent_ids: Vec::new(),
        history_artifact_path: Some(".vtcode/history/session-alpha_0001.jsonl".to_string()),
        generated_at: "2026-03-14T00:00:00Z".to_string(),
    };
    fs::write(
        history_dir.join("session-alpha.memory.json"),
        serde_json::to_string_pretty(&prior_envelope).expect("serialize envelope"),
    )
    .expect("write envelope");

    let history = vec![
        Message::user("Continue the compaction work.".to_string()),
        Message::assistant("I will update the local compaction path.".to_string()),
    ];
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    session_stats.record_touched_files(["src/new.rs".to_string()]);

    let update = super::SessionMemoryEnvelopeUpdate {
        grounded_facts: vec![GroundedFactRecord {
            fact: "Child agent confirmed the parser contract.".to_string(),
            source: "subagent:reviewer".to_string(),
        }],
        touched_files: vec!["src/child.rs".to_string()],
        open_questions: vec!["Should dedup cover batch reads?".to_string()],
        verification_todo: vec!["Run cargo check".to_string()],
        delegation_notes: vec!["reviewer: parser contract validated".to_string()],
        ..Default::default()
    };

    let envelope = super::refresh_session_memory_envelope(
        temp.path(),
        "session-alpha",
        Some(&VTCodeConfig::default()),
        &history,
        &session_stats,
        Some(&update),
    )
    .expect("refresh succeeds")
    .expect("envelope should be refreshed");

    assert_eq!(envelope.objective.as_deref(), Some("Keep continuity"));
    assert!(envelope.constraints.contains(&"Do not redesign the harness".to_string()));
    assert!(
        envelope
            .spec_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("Keep local compaction aligned"))
    );
    assert!(
        envelope
            .evaluation_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("Need a regression test"))
    );
    assert!(
        envelope
            .verification_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("Run cargo nextest"))
    );
    assert!(envelope.open_questions.contains(&"Should dedup cover batch reads?".to_string()));
    assert!(envelope.verification_todo.iter().any(|item| item.contains("Run cargo nextest")));
    assert!(envelope.verification_todo.contains(&"Run cargo check".to_string()));
    assert!(
        envelope
            .delegation_notes
            .contains(&"reviewer: parser contract validated".to_string())
    );
    assert!(envelope.touched_files.contains(&"src/new.rs".to_string()));
    assert!(envelope.touched_files.contains(&"src/child.rs".to_string()));
    assert_eq!(history, original_history, "ordinary refresh should not mutate live history");

    let persisted_path =
        latest_memory_envelope_path_for_session(temp.path(), "session-alpha").expect("persisted envelope path");
    let persisted: SessionMemoryEnvelope =
        serde_json::from_str(&fs::read_to_string(persisted_path).expect("read persisted envelope"))
            .expect("deserialize persisted envelope");
    assert_eq!(persisted, envelope);
}

#[test]
fn refresh_session_memory_envelope_prefers_structured_verify_metadata() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    fs::create_dir_all(temp.path().join(".vtcode").join("tasks")).expect("tasks dir");
    fs::write(
        temp.path()
            .join(".vtcode")
            .join("tasks")
            .join("current_task.md"),
        "# Ship compaction cleanup\n- [x] Analyze current continuity path\n  outcome: Existing envelope flow reviewed.\n- [ ] Update verification preservation\n  verify: cargo check -p vtcode\n- [ ] Run focused regression\n  verify:\n    - cargo test -p vtcode --bin vtcode agent::runloop::unified::turn::compaction::tests::refresh_session_memory_envelope_prefers_structured_verify_metadata -- --exact\n",
    )
    .expect("write task");

    let history = vec![Message::user("Continue the compaction work.".to_string())];
    let original_history = history.clone();
    let session_stats = SessionStats::default();

    let envelope = super::refresh_session_memory_envelope(
        temp.path(),
        "session-alpha",
        Some(&VTCodeConfig::default()),
        &history,
        &session_stats,
        None,
    )
    .expect("refresh succeeds")
    .expect("envelope should be refreshed");

    assert_eq!(
        envelope.verification_summary.as_deref(),
        Some(
            "- cargo check -p vtcode\n- cargo test -p vtcode --bin vtcode agent::runloop::unified::turn::compaction::tests::refresh_session_memory_envelope_prefers_structured_verify_metadata -- --exact"
        )
    );
    assert_eq!(history, original_history, "ordinary refresh should not mutate live history");
    let persisted_path =
        latest_memory_envelope_path_for_session(temp.path(), "session-alpha").expect("persisted envelope path");
    let persisted: SessionMemoryEnvelope =
        serde_json::from_str(&fs::read_to_string(persisted_path).expect("read persisted envelope"))
            .expect("deserialize persisted envelope");
    assert_eq!(persisted, envelope);
    assert!(
        persisted
            .verification_summary
            .as_deref()
            .is_some_and(|summary| summary.contains("- cargo check -p vtcode"))
    );
    let focused_regression = "- cargo test -p vtcode --bin vtcode agent::runloop::unified::turn::compaction::tests::refresh_session_memory_envelope_prefers_structured_verify_metadata -- --exact";
    assert!(
        persisted
            .verification_summary
            .as_deref()
            .is_some_and(|summary| summary.contains(focused_regression))
    );
}

#[test]
fn refresh_session_memory_envelope_is_throttled_when_nothing_changes() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    fs::create_dir_all(temp.path().join(".vtcode").join("tasks")).expect("tasks dir");
    fs::write(
        temp.path().join(".vtcode").join("tasks").join("current_task.md"),
        "# Ship compaction cleanup\n- [ ] Run cargo nextest\n",
    )
    .expect("write task");

    let prior_envelope = SessionMemoryEnvelope {
        session_id: "session-alpha".to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary: "Prior summary".to_string(),
        objective: Some("Keep continuity".to_string()),
        task_summary: Some("Ship compaction cleanup: - [ ] Run cargo nextest".to_string()),
        spec_summary: None,
        evaluation_summary: None,
        verification_summary: None,
        constraints: vec![],
        grounded_facts: vec![],
        touched_files: vec![],
        open_questions: vec![],
        verification_todo: vec![],
        delegation_notes: vec![],
        pending_intents: Vec::new(),
        applied_intent_ids: Vec::new(),
        history_artifact_path: None,
        generated_at: "2026-03-14T00:00:00Z".to_string(),
    };
    fs::write(
        history_dir.join("session-alpha.memory.json"),
        serde_json::to_string_pretty(&prior_envelope).expect("serialize envelope"),
    )
    .expect("write envelope");

    let history = vec![Message::user("Continue the compaction work.".to_string())];
    let original_history = history.clone();
    let session_stats = SessionStats::default();

    // First refresh with matching state should still write once because the
    // summary derived from history differs from the prior one. After that, an
    // identical refresh must be throttled.
    let first = super::refresh_session_memory_envelope(
        temp.path(),
        "session-alpha",
        Some(&VTCodeConfig::default()),
        &history,
        &session_stats,
        None,
    )
    .expect("refresh succeeds");
    assert!(first.is_some(), "first refresh should produce an envelope");
    assert_eq!(history, original_history, "ordinary refresh should not mutate live history");
    let history_len_after_first = history.len();

    let second = super::refresh_session_memory_envelope(
        temp.path(),
        "session-alpha",
        Some(&VTCodeConfig::default()),
        &history,
        &session_stats,
        None,
    )
    .expect("refresh succeeds");
    assert!(second.is_none(), "second identical refresh should be throttled");
    assert_eq!(history.len(), history_len_after_first, "history should not grow when refresh is throttled");
    assert_eq!(history, original_history, "throttled refresh should leave live history unchanged");
}

#[test]
fn refresh_session_memory_envelope_summary_is_concise() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    fs::create_dir_all(temp.path().join(".vtcode").join("tasks")).expect("tasks dir");
    fs::write(
        temp.path().join(".vtcode").join("tasks").join("current_task.md"),
        "# Audit agent loop\n- [ ] Reduce duplicated work\n",
    )
    .expect("write task");

    let history = vec![
        Message::user("Check the log and reduce repeated duplicated work.".to_string()),
        Message::assistant("I will audit the agent loop.".to_string()),
        Message::tool_response("call_1".to_string(), json!({"error": "some tool error"}).to_string()),
    ];
    let original_history = history.clone();
    let session_stats = SessionStats::default();

    let envelope = super::refresh_session_memory_envelope(
        temp.path(),
        "session-alpha",
        Some(&VTCodeConfig::default()),
        &history,
        &session_stats,
        None,
    )
    .expect("refresh succeeds")
    .expect("envelope should be refreshed");

    assert!(!envelope.summary.contains("{\"error\""), "summary should not contain raw JSON tool output");
    assert!(envelope.summary.len() < 300, "summary should be concise, got: {}", envelope.summary);
    assert!(envelope.summary.contains("Audit agent loop"), "summary should reference the objective");
    assert_eq!(history, original_history, "ordinary refresh should not mutate live history");
}

#[tokio::test]
async fn provider_compaction_error_preserves_existing_history() {
    let temp = tempdir().expect("tempdir");
    let provider = FailingProviderCompactionProvider;
    let mut history = test_history_with_memory_envelope();
    let original_history = history.clone();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let err = compact_history_in_place_with_events(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        vtcode_core::exec::events::CompactionTrigger::Manual,
    )
    .await
    .expect_err("failing provider should fail");

    assert!(!err.to_string().is_empty());
    assert_eq!(history, original_history);
}

#[tokio::test]
async fn recovery_compaction_falls_back_to_local_when_inline_request_errors() {
    let temp = tempdir().expect("tempdir");
    let provider = InlineRejectingRecoveryProvider;
    let mut history = test_history();
    let original_len = history.len();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    // A near-exhausted-budget recovery on a NativeInline-dispatched provider that
    // rejects the inline compact edit must not abort; it falls back to Local
    // summarization and compacts the earlier history.
    let outcome = compact_history_for_recovery_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        original_len,
    )
    .await
    .expect("recovery compaction should fall back to local")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Local);
    assert!(outcome.compacted_len >= outcome.original_len);
    assert_history_contains_messages(&history, &test_history());
}

#[tokio::test]
async fn auto_compaction_replaces_history_and_clears_response_chain() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.agent.harness.auto_compaction_enabled = true;
    vt_cfg.agent.harness.auto_compaction_threshold_tokens = Some(700);

    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    session_stats.set_previous_response_chain("stub", "stub-model", Some("resp_123"), &[]);
    let mut context_manager = test_context_manager();
    context_manager.update_token_usage(&Some(Usage {
        prompt_tokens: 900,
        completion_tokens: 10,
        total_tokens: 910,
        ..Usage::default()
    }));

    let outcome = maybe_auto_compact_history(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&vt_cfg),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
    )
    .await
    .expect("auto compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.original_len, 12);
    // The complete fixture fits within the continuity tail, so compaction
    // preserves all protocol messages and only adds durable metadata.
    assert!(outcome.compacted_len >= outcome.original_len);
    assert_local_compaction_history(&history, 4);
    assert_history_contains_messages(&history, &test_history());
    assert!(history[0].content.as_text().contains("Previous conversation summary"));
    assert_eq!(session_stats.previous_response_id_for("stub", "stub-model"), None);
    assert!(context_manager.current_token_usage() <= 700);
    assert!(latest_memory_envelope_path_for_session(temp.path(), "session-alpha").is_some());
}

#[tokio::test]
async fn auto_compaction_uses_the_effective_session_safety_ceiling() {
    let temp = tempdir().expect("tempdir");
    let provider = ContextSizedProvider { context_size: 500_000, provider_name: "openai" };
    let vt_cfg = VTCodeConfig::default();
    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();
    context_manager.update_token_usage(&Some(Usage {
        prompt_tokens: 496_000,
        completion_tokens: 10,
        total_tokens: 496_010,
        ..Usage::default()
    }));

    let outcome = maybe_auto_compact_history(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&vt_cfg),
            None,
            None,
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
    )
    .await
    .expect("auto compaction succeeds")
    .expect("496k prompt pressure must cross the 495.9k provider boundary");

    assert!(outcome.compacted_len >= outcome.original_len);
    assert!(context_manager.current_token_usage() <= 495_904);
}

#[tokio::test]
async fn targeted_compaction_preserves_prefix_and_replaces_suffix() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    let preserved_prefix = history[..1].to_vec();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();
    context_manager.update_token_usage(&Some(Usage {
        prompt_tokens: 900,
        completion_tokens: 10,
        total_tokens: 910,
        ..Usage::default()
    }));

    let outcome = compact_history_from_index_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &mut history,
        1,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("targeted compaction succeeds")
    .expect("history should compact");

    assert_eq!(&history[..1], preserved_prefix.as_slice());
    assert_eq!(outcome.original_len, 12);
    // The suffix fits within the continuity tail, so it remains verbatim after
    // the preserved prefix rather than being reduced to a message-count-based
    // approximation.
    assert!(outcome.compacted_len >= outcome.original_len);
    // The suffix begins with the assistant/tool completion for the first
    // group; without its user anchor that partial group belongs in the
    // summary prefix. Newer complete groups remain verbatim.
    assert_history_contains_messages(&history, &test_history()[3..]);
    assert!(
        history
            .iter()
            .any(|m| m.content.as_text().contains("[Session Memory Envelope]"))
    );
    assert!(
        history
            .iter()
            .any(|m| { m.content.as_text().contains("Previous conversation summary") })
    );
    assert!(latest_memory_envelope_path_for_session(temp.path(), "session-alpha").is_none());
}

#[tokio::test]
async fn recovery_compaction_preserves_current_turn_suffix_and_emits_event() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let harness_path = temp.path().join("recovery-harness.jsonl");
    let harness_emitter = HarnessEventEmitter::new(harness_path.clone()).expect("emitter");
    let mut history = test_history();
    history.push(Message::system("Previous turn already completed tool execution. Reuse the latest tool outputs in history instead of rerunning the same exploration. If those tool outputs include `critical_note`, `hint`, `next_action`, `fallback_tool`, `fallback_tool_args`, or `rerun_hint`, follow that guidance first.".to_string()));
    history.push(Message::system("Model follow-up failed after tool activity. Tools are disabled on the next pass; provide a direct textual response from the current context and reuse the latest tool outputs already in history.".to_string()));
    history.push(Message::user("current-turn".to_string()));
    history.push(Message::assistant("".to_string()));
    history.push(Message::tool_response("call-current".to_string(), "{\"ok\":true}".to_string()));
    let preserved_suffix = history[12..].to_vec();
    let mut session_stats = SessionStats::default();
    session_stats.set_previous_response_chain("stub", "stub-model", Some("resp-recovery"), &[]);
    let mut context_manager = test_context_manager();
    context_manager.update_token_usage(&Some(Usage {
        prompt_tokens: 950,
        completion_tokens: 10,
        total_tokens: 960,
        ..Usage::default()
    }));

    let outcome = compact_history_for_recovery_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            Some(&harness_emitter),
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        12,
    )
    .await
    .expect("recovery compaction succeeds")
    .expect("history should compact");

    assert_eq!(history[history.len() - preserved_suffix.len()..], preserved_suffix);
    assert!(outcome.compacted_len >= outcome.original_len);
    assert_eq!(session_stats.previous_response_id_for("stub", "stub-model"), None);
    assert!(context_manager.current_token_usage() <= 900);
    assert_history_contains_messages(&history, &test_history());

    let content = fs::read_to_string(harness_path).expect("read harness log");
    assert_eq!(content.matches("\"type\":\"thread.compact_boundary\"").count(), 1);
    assert!(content.contains("\"trigger\":\"recovery\""));
    assert!(content.contains("\"mode\":\"local\""));
    assert!(history.iter().any(|message| message.content.as_text() == "current-turn"));
}

#[tokio::test]
async fn recovery_compaction_uses_provider_mode_when_supported() {
    let temp = tempdir().expect("tempdir");
    let provider = ProviderCompactionProvider;
    let harness_path = temp.path().join("provider-recovery-harness.jsonl");
    let harness_emitter = HarnessEventEmitter::new(harness_path.clone()).expect("emitter");
    let mut history = test_history();
    history.push(Message::user("current-turn".to_string()));
    history.push(Message::assistant("".to_string()));
    history.push(Message::tool_response("call-current".to_string(), "{\"ok\":true}".to_string()));
    let preserved_suffix = history[12..].to_vec();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_for_recovery_in_place(
        CompactionContext::new(
            &provider,
            "stub-model",
            "session-alpha",
            "thread-alpha",
            temp.path(),
            Some(&VTCodeConfig::default()),
            None,
            Some(&harness_emitter),
        ),
        CompactionState::new(&mut history, &mut session_stats, &mut context_manager),
        12,
    )
    .await
    .expect("provider recovery compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.mode, vtcode_core::exec::events::CompactionMode::Provider);
    assert_eq!(history[history.len() - preserved_suffix.len()..], preserved_suffix);

    let content = fs::read_to_string(harness_path).expect("read harness log");
    assert!(content.contains("\"trigger\":\"recovery\""));
    assert!(content.contains("\"mode\":\"provider\""));
}

#[test]
fn inject_latest_memory_envelope_rehydrates_resume_history() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    let envelope_path = history_dir.join("resume-session_001.memory.json");
    let envelope = SessionMemoryEnvelope {
        session_id: "resume-session".to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary: "Persisted summary".to_string(),
        objective: None,
        task_summary: Some("Tracker: - [ ] Follow up".to_string()),
        spec_summary: None,
        evaluation_summary: None,
        verification_summary: Some("- [ ] Run cargo nextest".to_string()),
        constraints: Vec::new(),
        grounded_facts: vec![GroundedFactRecord {
            fact: "Cargo.toml declares vtcode-core".to_string(),
            source: "tool:read_file".to_string(),
        }],
        touched_files: vec!["Cargo.toml".to_string()],
        open_questions: Vec::new(),
        verification_todo: Vec::new(),
        delegation_notes: Vec::new(),
        pending_intents: Vec::new(),
        applied_intent_ids: Vec::new(),
        history_artifact_path: Some(".vtcode/history/resume-session_001.jsonl".to_string()),
        generated_at: "2026-03-14T00:00:00Z".to_string(),
    };
    fs::write(&envelope_path, serde_json::to_string_pretty(&envelope).expect("serialize envelope"))
        .expect("write envelope");

    let mut history = vec![Message::user("resume".to_string())];
    assert!(inject_latest_memory_envelope(temp.path(), "resume-session", &mut history));
    assert!(history[0].content.as_text().contains("Persisted summary"));
    assert!(history[0].content.as_text().contains("Cargo.toml"));
    assert!(history[0].content.as_text().contains("Verification Status"));
}

#[test]
fn inject_latest_memory_envelope_is_session_scoped() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");

    for (session_id, summary) in [("session-alpha", "Alpha summary"), ("session-beta", "Beta summary")] {
        let envelope_path = history_dir.join(format!("{session_id}_0001.memory.json"));
        let envelope = SessionMemoryEnvelope {
            session_id: session_id.to_string(),
            schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
            summary: summary.to_string(),
            objective: None,
            task_summary: None,
            spec_summary: None,
            evaluation_summary: None,
            verification_summary: None,
            constraints: Vec::new(),
            grounded_facts: Vec::new(),
            touched_files: Vec::new(),
            open_questions: Vec::new(),
            verification_todo: Vec::new(),
            delegation_notes: Vec::new(),
            pending_intents: Vec::new(),
            applied_intent_ids: Vec::new(),
            history_artifact_path: None,
            generated_at: "2026-03-14T00:00:00Z".to_string(),
        };
        fs::write(envelope_path, serde_json::to_string_pretty(&envelope).expect("serialize envelope"))
            .expect("write envelope");
    }

    let mut history = vec![Message::user("resume".to_string())];
    assert!(inject_latest_memory_envelope(temp.path(), "session-beta", &mut history));
    assert!(history[0].content.as_text().contains("Beta summary"));
    assert!(!history[0].content.as_text().contains("Alpha summary"));
}

#[test]
fn inject_latest_memory_envelope_requires_exact_session_prefix_match() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");

    for (file_name, summary) in [
        ("session-a_0001.memory.json", "Exact summary"),
        ("session-alpha_0002.memory.json", "Wrong summary"),
    ] {
        let envelope = SessionMemoryEnvelope {
            session_id: "session-a".to_string(),
            schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
            summary: summary.to_string(),
            objective: None,
            task_summary: None,
            spec_summary: None,
            evaluation_summary: None,
            verification_summary: None,
            constraints: Vec::new(),
            grounded_facts: Vec::new(),
            touched_files: Vec::new(),
            open_questions: Vec::new(),
            verification_todo: Vec::new(),
            delegation_notes: Vec::new(),
            pending_intents: Vec::new(),
            applied_intent_ids: Vec::new(),
            history_artifact_path: None,
            generated_at: "2026-03-14T00:00:00Z".to_string(),
        };
        fs::write(history_dir.join(file_name), serde_json::to_string_pretty(&envelope).expect("serialize envelope"))
            .expect("write envelope");
    }

    let mut history = vec![Message::user("resume".to_string())];
    assert!(inject_latest_memory_envelope(temp.path(), "session-a", &mut history));
    assert!(history[0].content.as_text().contains("Exact summary"));
    assert!(!history[0].content.as_text().contains("Wrong summary"));
}

#[tokio::test]
async fn no_envelope_written_when_dynamic_history_is_disabled() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.context.dynamic.enabled = false;

    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    compact_history_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&vt_cfg),
        &mut history,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("compaction succeeds");

    assert!(latest_memory_envelope_path_for_session(temp.path(), "session-alpha").is_none());
    assert!(history[0].content.as_text().contains("Previous conversation summary"));
}

#[tokio::test]
async fn persisted_envelope_uses_recorded_touched_files_only() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    history.push(Message::user("Mentioning docs/example.md in prose should not populate touched files.".to_string()));
    let mut session_stats = SessionStats::default();
    session_stats.record_touched_files(["src/main.rs".to_string(), "Cargo.toml".to_string()]);
    let mut context_manager = test_context_manager();

    compact_history_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &mut history,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("compaction succeeds");

    let envelope_path = latest_memory_envelope_path_for_session(temp.path(), "session-alpha").expect("envelope path");
    let envelope: SessionMemoryEnvelope =
        serde_json::from_str(&fs::read_to_string(envelope_path).expect("read envelope")).expect("parse envelope");

    assert_eq!(envelope.touched_files, vec!["src/main.rs".to_string(), "Cargo.toml".to_string()]);
    assert_eq!(envelope.session_id, "session-alpha");
}

#[test]
fn inject_latest_memory_envelope_uses_exact_session_id_when_prefixes_collide() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");

    let session_alpha = "01234567890123456789012345678901-alpha";
    let session_beta = "01234567890123456789012345678901-beta";

    for (session_id, summary, suffix) in [
        (session_alpha, "Alpha summary", "0001"),
        (session_beta, "Beta summary", "0002"),
    ] {
        let envelope = SessionMemoryEnvelope {
            session_id: session_id.to_string(),
            schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
            summary: summary.to_string(),
            objective: None,
            task_summary: None,
            spec_summary: None,
            evaluation_summary: None,
            verification_summary: None,
            constraints: Vec::new(),
            grounded_facts: Vec::new(),
            touched_files: Vec::new(),
            open_questions: Vec::new(),
            verification_todo: Vec::new(),
            delegation_notes: Vec::new(),
            pending_intents: Vec::new(),
            applied_intent_ids: Vec::new(),
            history_artifact_path: None,
            generated_at: "2026-03-14T00:00:00Z".to_string(),
        };
        let file_name = format!("{}_{suffix}.memory.json", &session_id[..32]);
        fs::write(history_dir.join(file_name), serde_json::to_string_pretty(&envelope).expect("serialize envelope"))
            .expect("write envelope");
    }

    let mut history = vec![Message::user("resume".to_string())];
    assert!(inject_latest_memory_envelope(temp.path(), session_alpha, &mut history));
    assert!(history[0].content.as_text().contains("Alpha summary"));
    assert!(!history[0].content.as_text().contains("Beta summary"));
}

#[tokio::test]
async fn compaction_strips_existing_memory_envelope_before_recompacting() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut history = test_history();
    history.insert(0, Message::system("[Session Memory Envelope]\nSummary:\nPersisted summary".to_string()));
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    let outcome = compact_history_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &mut history,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("compaction succeeds")
    .expect("history should compact");

    assert_eq!(outcome.original_len, 12);
    assert!(outcome.compacted_len >= outcome.original_len);
    assert_history_contains_messages(&history, &test_history());
    assert_eq!(
        history
            .iter()
            .filter(|message| message.content.as_text().contains("[Session Memory Envelope]"))
            .count(),
        1
    );
}

#[tokio::test]
async fn summarized_fork_history_reuses_compaction_pipeline_and_prior_envelope() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("history dir");
    let source_envelope = SessionMemoryEnvelope {
        session_id: "session-source".to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary: "Prior source summary".to_string(),
        objective: Some("Keep the source session moving".to_string()),
        task_summary: Some("Tracker: keep going".to_string()),
        spec_summary: None,
        evaluation_summary: None,
        verification_summary: None,
        constraints: Vec::new(),
        grounded_facts: vec![GroundedFactRecord {
            fact: "src/lib.rs was updated".to_string(),
            source: "tool:write_file".to_string(),
        }],
        touched_files: vec!["src/lib.rs".to_string()],
        open_questions: Vec::new(),
        verification_todo: Vec::new(),
        delegation_notes: Vec::new(),
        pending_intents: Vec::new(),
        applied_intent_ids: Vec::new(),
        history_artifact_path: Some(".vtcode/history/session-source_0001.jsonl".to_string()),
        generated_at: "2026-03-14T00:00:00Z".to_string(),
    };
    fs::write(
        history_dir.join("session-source_0001.memory.json"),
        serde_json::to_string_pretty(&source_envelope).expect("serialize envelope"),
    )
    .expect("write envelope");

    let compacted = build_summarized_fork_history(
        &LocalCompactionProvider,
        "stub-model",
        "session-source",
        "session-target",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &test_history(),
        false,
    )
    .await
    .expect("summarized fork history");

    // The fixture fits within the continuity tail, so the fork keeps the
    // complete protocol history in addition to its summary metadata.
    assert!(compacted.len() >= 6);
    assert!(compacted[0].content.as_text().contains("[Session Memory Envelope]"));
    assert!(compacted[0].content.as_text().contains("src/lib.rs"));
    assert!(compacted[1].content.as_text().contains("Previous conversation summary"));
    assert_eq!(compacted.iter().filter(|message| message.role == MessageRole::User).count(), 4);
    assert_history_contains_messages(&compacted, &test_history());
}

#[tokio::test]
async fn budget_resume_summary_reuses_saved_envelope_without_provider_compaction() {
    let temp = tempdir().expect("tempdir");
    let history_dir = temp.path().join(".vtcode").join("history");
    fs::create_dir_all(&history_dir).expect("create history dir");

    let source_envelope = SessionMemoryEnvelope {
        session_id: "session-source".to_string(),
        schema_version: Some(SESSION_MEMORY_ENVELOPE_SCHEMA_VERSION),
        summary: "Budget-limited session summary".to_string(),
        objective: None,
        task_summary: None,
        spec_summary: None,
        evaluation_summary: None,
        verification_summary: None,
        constraints: Vec::new(),
        grounded_facts: Vec::new(),
        touched_files: vec!["src/lib.rs".to_string()],
        open_questions: Vec::new(),
        verification_todo: Vec::new(),
        delegation_notes: Vec::new(),
        pending_intents: Vec::new(),
        applied_intent_ids: Vec::new(),
        history_artifact_path: None,
        generated_at: "2026-03-14T00:00:00Z".to_string(),
    };
    fs::write(
        history_dir.join("session-source_0001.memory.json"),
        serde_json::to_string_pretty(&source_envelope).expect("serialize envelope"),
    )
    .expect("write envelope");

    let compacted = build_summarized_fork_history(
        &FailingProviderCompactionProvider,
        "stub-model",
        "session-source",
        "session-target",
        temp.path(),
        Some(&VTCodeConfig::default()),
        &test_history(),
        true,
    )
    .await
    .expect("saved summary fork history");

    assert!(compacted[0].content.as_text().contains("[Session Memory Envelope]"));
    assert!(compacted[1].content.as_text().contains("Budget-limited session summary"));
}

#[tokio::test]
async fn local_and_fork_compaction_preserve_continuity_tail() {
    let temp = tempdir().expect("tempdir");
    let provider = LocalCompactionProvider;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.context.dynamic.retained_user_messages = 2;

    let mut history = test_history();
    let mut session_stats = SessionStats::default();
    let mut context_manager = test_context_manager();

    compact_history_in_place(
        &provider,
        "stub-model",
        "session-alpha",
        temp.path(),
        Some(&vt_cfg),
        &mut history,
        &mut session_stats,
        &mut context_manager,
    )
    .await
    .expect("compaction succeeds")
    .expect("history should compact");

    assert_local_compaction_history_with_user_count(&history, 2);

    let compacted = build_summarized_fork_history(
        &provider,
        "stub-model",
        "session-alpha",
        "session-beta",
        temp.path(),
        Some(&vt_cfg),
        &test_history(),
        false,
    )
    .await
    .expect("summarized fork history");

    assert_eq!(compacted.iter().filter(|message| message.role == MessageRole::User).count(), 4);
    assert_history_contains_messages(&compacted, &test_history());
}

#[test]
fn grounded_fact_extraction_dedupes_caps_and_skips_errors() {
    let history = vec![
        Message::tool_response_with_origin(
            "call_1".to_string(),
            "{\"result\":\"Cargo.toml declares vtcode-core\"}".to_string(),
            "read_file".to_string(),
        ),
        Message::tool_response_with_origin(
            "call_2".to_string(),
            "{\"result\":\"cargo.toml declares vtcode-core\"}".to_string(),
            "read_file".to_string(),
        ),
        Message::tool_response_with_origin(
            "call_3".to_string(),
            "{\"error\":\"denied\"}".to_string(),
            "read_file".to_string(),
        ),
        Message::user("I prefer concise answers.".to_string()),
    ];

    let facts = super::dedup_latest_facts(&history, 5);
    assert_eq!(facts.len(), 2);
    assert!(facts.iter().any(|fact| fact.source == "tool:read_file"));
    assert!(facts.iter().any(|fact| fact.source == "user_assertion"));
}

#[test]
fn resolve_compaction_threshold_prefers_configured_value() {
    assert_eq!(resolve_compaction_threshold(Some(42), 200_000), Some(42));
}

#[test]
fn resolve_compaction_threshold_reserves_output_room_when_unset() {
    let reserve = DEFAULT_OUTPUT_RESERVE_TOKENS as u64;
    assert_eq!(resolve_compaction_threshold(None, 200_000), Some(200_000 - reserve));
}

#[test]
fn resolve_compaction_threshold_clamps_to_context_size() {
    let reserve = DEFAULT_OUTPUT_RESERVE_TOKENS as u64;
    assert_eq!(resolve_compaction_threshold(Some(300_000), 200_000), Some(200_000 - reserve));
}

#[test]
fn resolve_compaction_threshold_requires_context_or_override() {
    assert_eq!(resolve_compaction_threshold(None, 0), None);
}

#[test]
fn effective_compaction_threshold_follows_provider_capacity_when_session_budget_unset() {
    // Arrange: the default session budget is 0 (unset), so the provider
    // capacity drives the threshold and only the output reserve is subtracted.
    let provider = ContextSizedProvider { context_size: 500_000, provider_name: "openai" };
    let config = VTCodeConfig::default();

    // Act
    let threshold = effective_compaction_threshold(Some(&config), &provider, "stub-model");

    // Assert
    assert_eq!(threshold, Some(500_000 - DEFAULT_OUTPUT_RESERVE_TOKENS));
}

#[test]
fn effective_compaction_threshold_clamps_session_budget_to_provider_capacity() {
    // Arrange
    let provider = ContextSizedProvider { context_size: 100_000, provider_name: "openai" };
    let mut config = VTCodeConfig::default();
    config.context.max_context_tokens = 160_000;

    // Act
    let threshold = effective_compaction_threshold(Some(&config), &provider, "stub-model");

    // Assert: the 160k session budget is clamped to the 100k provider capacity,
    // and the output reserve is subtracted from the result.
    assert_eq!(threshold, Some(100_000 - DEFAULT_OUTPUT_RESERVE_TOKENS));
}

#[test]
fn effective_compaction_threshold_uses_default_session_budget_without_config() {
    // A zero (unset) session budget defers to the provider capacity minus the
    // output reserve.
    let threshold = effective_compaction_threshold(
        None,
        &ContextSizedProvider { context_size: 500_000, provider_name: "openai" },
        "stub-model",
    );

    assert_eq!(threshold, Some(500_000 - DEFAULT_OUTPUT_RESERVE_TOKENS));
}

#[test]
fn explicit_compaction_threshold_overrides_session_budget_but_not_provider_capacity() {
    // Arrange
    let mut config = VTCodeConfig::default();
    config.context.max_context_tokens = 300_000;
    config.agent.harness.auto_compaction_threshold_tokens = Some(200_000);

    // Act
    let session_override = effective_compaction_threshold(
        Some(&config),
        &ContextSizedProvider { context_size: 500_000, provider_name: "openai" },
        "stub-model",
    );
    let provider_cap = effective_compaction_threshold(
        Some(&config),
        &ContextSizedProvider { context_size: 150_000, provider_name: "openai" },
        "stub-model",
    );

    // Assert
    assert_eq!(session_override, Some(200_000));
    assert_eq!(provider_cap, Some(150_000 - DEFAULT_OUTPUT_RESERVE_TOKENS));
}

#[test]
fn zero_session_budget_preserves_provider_derived_threshold() {
    // Arrange
    let mut config = VTCodeConfig::default();
    config.context.max_context_tokens = 0;

    // Act
    let threshold = effective_compaction_threshold(
        Some(&config),
        &ContextSizedProvider { context_size: 200_000, provider_name: "openai" },
        "stub-model",
    );

    // Assert
    assert_eq!(threshold, Some(200_000 - DEFAULT_OUTPUT_RESERVE_TOKENS));
    assert_eq!(resolve_compaction_threshold(Some(0), 200_000), Some(200_000 - DEFAULT_OUTPUT_RESERVE_TOKENS as u64));
}

#[test]
fn effective_session_context_budget_preserves_known_limit_when_other_side_is_zero() {
    assert_eq!(effective_session_context_budget(500_000, 160_000), 160_000);
    assert_eq!(effective_session_context_budget(100_000, 160_000), 100_000);
    assert_eq!(effective_session_context_budget(500_000, 0), 500_000);
    assert_eq!(effective_session_context_budget(0, 160_000), 160_000);
    assert_eq!(effective_session_context_budget(0, 0), 0);
}

#[test]
fn build_server_compaction_context_management_creates_openai_payload() {
    assert_eq!(
        build_server_compaction_context_management(Some(512), 2_000_000, 160_000),
        Some(json!([{
            "type": "compaction",
            "compact_threshold": 512,
        }]))
    );
}

#[tokio::test]
async fn capability_driven_compaction_triggers_for_three_families_at_small_and_large_limits() {
    for provider_name in ["openai", "anthropic", "gemini"] {
        for context_size in [32_000, 1_000_000] {
            let provider = ContextSizedProvider { context_size, provider_name };
            let config = VTCodeConfig::default();
            let threshold = context_size - DEFAULT_OUTPUT_RESERVE_TOKENS;
            assert_eq!(
                vtcode_core::compaction::effective_context_budget(Some(&config), &provider, "dynamic-model"),
                context_size
            );
            assert_eq!(effective_compaction_threshold(Some(&config), &provider, "dynamic-model"), Some(threshold));
            for (prompt_tokens, should_compact) in [(threshold - 1, false), (threshold, true)] {
                let temp = tempdir().expect("temporary workspace");
                let mut history = test_history();
                let mut stats = SessionStats::default();
                let mut manager = test_context_manager();
                manager.update_token_usage(&Some(Usage {
                    prompt_tokens: prompt_tokens as u32,
                    total_tokens: prompt_tokens as u32,
                    ..Usage::default()
                }));
                let outcome = maybe_auto_compact_history(
                    CompactionContext::new(
                        &provider,
                        "dynamic-model",
                        "budget-session",
                        "budget-thread",
                        temp.path(),
                        Some(&config),
                        None,
                        None,
                    ),
                    CompactionState::new(&mut history, &mut stats, &mut manager),
                )
                .await
                .expect("offline compaction");
                assert_eq!(
                    outcome.is_some(),
                    should_compact,
                    "{provider_name}, capacity={context_size}, pressure={prompt_tokens}"
                );
            }
        }
    }
}

#[test]
fn explicit_threshold_cannot_bypass_session_safety_or_request_output_reserve() {
    use vtcode_core::compaction::memory_envelope::effective_compaction_threshold_with_reserve;
    let provider = ContextSizedProvider { context_size: 1_000_000, provider_name: "openai" };
    let mut config = VTCodeConfig::default();
    config.context.max_context_tokens = 32_000;
    config.agent.harness.auto_compaction_threshold_tokens = Some(900_000);
    assert_eq!(
        effective_compaction_threshold_with_reserve(Some(&config), &provider, "dynamic-model", 800),
        Some(31_200)
    );
    assert_eq!(
        effective_compaction_threshold_with_reserve(Some(&config), &provider, "dynamic-model", 8_000),
        Some(24_000)
    );
}

#[test]
fn discovered_resolved_context_reaches_runtime_budget_without_changing_other_models() {
    use vtcode_core::llm::model_resolver::{DynamicModelMeta, ModelResolver};
    use vtcode_core::llm::provider::ContextWindowProvider;
    for provider_name in ["openai", "anthropic", "gemini"] {
        let resolved = ModelResolver::resolve(
            Some(provider_name),
            "dynamic-32k",
            &[],
            Some(DynamicModelMeta {
                display_name: "Discovered model".into(),
                description: None,
                context_window: Some(32_000),
            }),
        )
        .expect("resolved dynamic model");
        let provider = ContextWindowProvider::wrap(
            Box::new(ContextSizedProvider { context_size: 1_000_000, provider_name }),
            &resolved.model_id,
            resolved.context_window(),
        );
        assert_eq!(vtcode_core::compaction::effective_context_budget(None, provider.as_ref(), "dynamic-32k"), 32_000);
        assert_eq!(
            effective_compaction_threshold(None, provider.as_ref(), "dynamic-32k"),
            Some(32_000 - DEFAULT_OUTPUT_RESERVE_TOKENS)
        );
        assert_eq!(provider.effective_context_size("other-model"), 1_000_000);
        assert_eq!(provider.effective_context_size(""), 32_000);
    }
}
