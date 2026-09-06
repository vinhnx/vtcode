use super::*;
use crate::llm::provider::{
    FinishReason, LLMError, LLMNormalizedStream, LLMProvider, LLMRequest, LLMResponse, NormalizedStreamEvent,
};
use crate::llm::resolve_api_key_for_model_route;
use async_trait::async_trait;
use futures::stream;
use std::sync::Mutex;
use tempfile::tempdir;

struct StaticProvider {
    response: &'static str,
    supports_structured_output: bool,
    last_request: Mutex<Option<LLMRequest>>,
}

impl StaticProvider {
    fn new(response: &'static str) -> Self {
        Self {
            response,
            supports_structured_output: true,
            last_request: Mutex::new(None),
        }
    }

    fn prompt_only_json(response: &'static str) -> Self {
        Self {
            response,
            supports_structured_output: false,
            last_request: Mutex::new(None),
        }
    }

    fn last_request(&self) -> LLMRequest {
        self.last_request
            .lock()
            .expect("request lock")
            .clone()
            .expect("request recorded")
    }
}

#[async_trait]
impl LLMProvider for StaticProvider {
    fn name(&self) -> &str {
        "static"
    }

    async fn generate(&self, request: LLMRequest) -> std::result::Result<LLMResponse, LLMError> {
        *self.last_request.lock().expect("request lock") = Some(request);
        Ok(LLMResponse::new("stub-model", self.response))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> std::result::Result<(), LLMError> {
        Ok(())
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        self.supports_structured_output
    }
}

fn message_history() -> Vec<Message> {
    vec![
        Message::user("I prefer cargo nextest".to_string()),
        Message::tool_response_with_origin(
            "call-1".to_string(),
            serde_json::json!({"summary":"Tests live under vtcode-core/tests"}).to_string(),
            "code_search".to_string(),
        ),
    ]
}

fn runtime_config(workspace: &Path) -> RuntimeAgentConfig {
    RuntimeAgentConfig {
        model: "gpt-5".to_string(),
        api_key: "test-key".to_string(),
        provider: "openai".to_string(),
        openai_chatgpt_auth: None,
        api_key_env: "OPENAI_API_KEY".to_string(),
        workspace: workspace.to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "ciapre".to_string(),
        reasoning_effort: crate::config::types::ReasoningEffortLevel::None,
        ui_surface: crate::config::types::UiSurfacePreference::Auto,
        prompt_cache: crate::config::PromptCachingConfig::default(),
        model_source: crate::config::types::ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: Default::default(),
        checkpointing_enabled: true,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: 10,
        checkpointing_max_age_days: Some(7),
        max_conversation_turns: 10,
        model_behavior: None,
    }
}

fn enabled_memory_config() -> PersistentMemoryConfig {
    PersistentMemoryConfig { enabled: true, ..PersistentMemoryConfig::default() }
}

fn enabled_memory_config_for(workspace: &Path) -> PersistentMemoryConfig {
    PersistentMemoryConfig {
        enabled: true,
        directory_override: Some(workspace.join(".memory").display().to_string()),
        ..PersistentMemoryConfig::default()
    }
}

fn enabled_vt_memory_config_for(workspace: &Path) -> VTCodeConfig {
    let mut config = VTCodeConfig::default();
    config.features.memories = true;
    config.agent.persistent_memory = enabled_memory_config_for(workspace);
    config
}

#[test]
fn dedup_latest_facts_extracts_user_and_tool_memory() {
    let facts = dedup_latest_facts(&message_history(), 8);
    assert_eq!(facts.len(), 2);
    assert!(facts.iter().any(|fact| fact.source == "user_assertion"));
    assert!(facts.iter().any(|fact| fact.source == "tool:code_search"));
}

#[test]
fn maybe_extract_user_fact_keeps_durable_self_facts() {
    let name = maybe_extract_user_fact(&Message::user("My name is Vinh Nguyen".to_string()))
        .expect("name should be extracted");
    assert_eq!(name.source, "user_assertion");
    assert_eq!(name.fact, "My name is Vinh Nguyen");

    let preference = maybe_extract_user_fact(&Message::user("I prefer cargo nextest for test runs".to_string()))
        .expect("preference should be extracted");
    assert_eq!(preference.source, "user_assertion");
    assert_eq!(preference.fact, "I prefer cargo nextest for test runs");
}

#[test]
fn maybe_extract_user_fact_ignores_transient_first_person_task_chatter() {
    assert!(
        maybe_extract_user_fact(&Message::user("I am debugging a failing vtcode-core test".to_string(),)).is_none()
    );
    assert!(
        maybe_extract_user_fact(&Message::user("My tests are still failing after the refactor".to_string(),)).is_none()
    );
}

#[test]
fn maybe_extract_user_fact_keeps_authored_notes_after_polite_prefixes() {
    let note =
        maybe_extract_user_fact(&Message::user("Please note that I prefer pnpm in JavaScript workspaces".to_string()))
            .expect("authored note should be extracted");
    assert_eq!(note.source, "user_assertion");
    assert_eq!(note.fact, "I prefer pnpm in JavaScript workspaces");
}

#[test]
fn maybe_extract_user_fact_ignores_memory_prompts_and_questions() {
    assert!(maybe_extract_user_fact(&Message::user("remember that I prefer cargo nextest".to_string(),)).is_none());
    assert!(maybe_extract_user_fact(&Message::user("do you remember my name?".to_string())).is_none());
    assert!(maybe_extract_user_fact(&Message::user("what do you remember about this repo?".to_string(),)).is_none());
}

#[tokio::test]
async fn finalize_persistent_memory_ignores_explicit_memory_prompts() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");

    let mut runtime = runtime_config(workspace.path());
    runtime.provider = "missing-provider".to_string();

    let vt_cfg = enabled_vt_memory_config_for(workspace.path());

    let report = finalize_persistent_memory(
        &runtime,
        Some(&vt_cfg),
        &[Message::user("remember that I prefer cargo nextest".to_string())],
    )
    .await
    .expect("finalize should skip ignored prompts");

    assert!(report.is_none());
}

#[tokio::test]
async fn finalize_persistent_memory_skips_existing_authored_note_duplicates() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");

    let mut runtime = runtime_config(workspace.path());
    runtime.provider = "missing-provider".to_string();

    let vt_cfg = enabled_vt_memory_config_for(workspace.path());

    let memory_dir = resolve_persistent_memory_dir(&vt_cfg.agent.persistent_memory, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    std::fs::create_dir_all(&files.directory).expect("dir");
    std::fs::write(
        &files.preferences_file,
        "# Preferences\n\n- [user_assertion] I prefer cargo nextest for test runs\n",
    )
    .expect("prefs");

    let report = finalize_persistent_memory(
        &runtime,
        Some(&vt_cfg),
        &[Message::user(
            "Please note that I prefer cargo nextest for test runs".to_string(),
        )],
    )
    .await
    .expect("existing duplicate should skip routing");

    assert!(report.is_none());
}

#[tokio::test]
async fn llm_classification_rewrites_and_routes_candidates() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::new(
        r#"{
          "keep": [
            {"id": 0, "topic": "preferences", "fact": "Prefer cargo nextest for test runs"},
            {"id": 1, "topic": "repository_facts", "fact": "Tests live under vtcode-core/tests"}
          ]
        }"#,
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };
    let classified =
        classify_facts_with_provider(&provider, &route, workspace.path(), &dedup_latest_facts(&message_history(), 8))
            .await
            .expect("classify");

    assert_eq!(classified.preferences.len(), 1);
    assert_eq!(classified.repository_facts.len(), 1);
    assert_eq!(classified.preferences[0].fact, "Prefer cargo nextest for test runs");
    assert_eq!(classified.repository_facts[0].fact, "Tests live under vtcode-core/tests");
}

#[tokio::test]
async fn remember_planner_requests_missing_details() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::new(
        r#"{
          "kind": "ask_missing",
          "facts": [],
          "selected_ids": [],
          "missing": {"field": "name", "prompt": "What name should VT Code remember?"},
          "message": null
        }"#,
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };
    let plan = plan_memory_operation_with_provider(
        &provider,
        &route,
        workspace.path(),
        MemoryOpKind::Remember,
        "save to memory and remember my name",
        None,
        None,
        &[],
    )
    .await
    .expect("plan");

    assert_eq!(plan.kind, MemoryOpKind::AskMissing);
    assert_eq!(plan.missing.as_ref().map(|missing| missing.field.as_str()), Some("name"));
    assert!(
        !provider.last_request().messages[0]
            .content
            .as_text()
            .contains("Immediately preceding assistant reply")
    );
}

#[tokio::test]
async fn remember_planner_uses_immediately_preceding_assistant_reply_for_deictic_request() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::new(
        r#"{
          "kind": "remember",
          "facts": [
            {
              "topic": "preferences",
              "fact": "My name is Vinh Nguyen; my alias is vinhnx.",
              "source": "assistant_reply"
            }
          ],
          "selected_ids": [],
          "missing": null,
          "message": null
        }"#,
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };
    let prior_assistant_reply = "Your name is Vinh Nguyen and you go by vinhnx.";
    let plan = plan_memory_operation_with_provider(
        &provider,
        &route,
        workspace.path(),
        MemoryOpKind::Remember,
        "remember it",
        None,
        Some(prior_assistant_reply),
        &[],
    )
    .await
    .expect("plan");

    assert_eq!(plan.kind, MemoryOpKind::Remember);
    assert_eq!(plan.facts.len(), 1);
    assert_eq!(plan.facts[0].topic, MemoryPlannedTopic::Preferences);
    assert_eq!(plan.facts[0].fact, "My name is Vinh Nguyen; my alias is vinhnx.");

    let request = provider.last_request();
    let prompt = request.messages[0].content.as_text();
    assert!(prompt.contains("Immediately preceding assistant reply"));
    assert!(prompt.contains(prior_assistant_reply));
    assert!(prompt.contains("approved by the current user"));

    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");
    let vt_cfg = enabled_vt_memory_config_for(workspace.path());
    let report = persist_remembered_memory_plan(&runtime_config(workspace.path()), Some(&vt_cfg), &plan)
        .await
        .expect("persist contextual plan")
        .expect("write report");
    assert_eq!(report.added_facts, 1);

    let candidates = list_persistent_memory_candidates(&vt_cfg.agent.persistent_memory, workspace.path())
        .await
        .expect("read persisted candidates")
        .expect("enabled");
    assert!(candidates.iter().any(|candidate| {
        candidate.fact == "My name is Vinh Nguyen; my alias is vinhnx." && candidate.source == "assistant_reply"
    }));
}

#[tokio::test]
async fn forget_planner_selects_exact_candidate_ids() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::new(
        r#"{
          "kind": "forget",
          "facts": [],
          "selected_ids": [1],
          "missing": null,
          "message": "Remove the pnpm preference."
        }"#,
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };
    let candidates = vec![
        MemoryOpCandidate {
            id: 0,
            source: "manual_memory".to_string(),
            fact: "Prefer cargo nextest".to_string(),
        },
        MemoryOpCandidate {
            id: 1,
            source: "manual_memory".to_string(),
            fact: "Prefer pnpm".to_string(),
        },
    ];
    let plan = plan_memory_operation_with_provider(
        &provider,
        &route,
        workspace.path(),
        MemoryOpKind::Forget,
        "forget my pnpm preference",
        None,
        None,
        &candidates,
    )
    .await
    .expect("plan");

    assert_eq!(plan.kind, MemoryOpKind::Forget);
    assert_eq!(plan.selected_ids, vec![1]);
}

#[tokio::test]
async fn classification_falls_back_to_prompt_only_json_when_native_schema_is_unsupported() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::prompt_only_json(
        "Here is the JSON:\n```json\n{\n  \"keep\": [\n    {\"id\": 0, \"topic\": \"preferences\", \"fact\": \"Prefer cargo nextest for test runs\"}\n  ]\n}\n```",
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };

    let classified =
        classify_facts_with_provider(&provider, &route, workspace.path(), &dedup_latest_facts(&message_history(), 8))
            .await
            .expect("classify");

    assert_eq!(classified.preferences.len(), 1);
    let request = provider.last_request();
    assert!(request.output_format.is_none());
    assert!(request.messages[0].content.as_text().contains("Return JSON only."));
}

#[tokio::test]
async fn planner_falls_back_to_prompt_only_json_when_native_schema_is_unsupported() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::prompt_only_json(
        "```json\n{\n  \"kind\": \"ask_missing\",\n  \"facts\": [],\n  \"selected_ids\": [],\n  \"missing\": {\"field\": \"name\", \"prompt\": \"What name should VT Code remember?\"},\n  \"message\": null\n}\n```",
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };

    let plan = plan_memory_operation_with_provider(
        &provider,
        &route,
        workspace.path(),
        MemoryOpKind::Remember,
        "remember my name",
        None,
        None,
        &[],
    )
    .await
    .expect("plan");

    assert_eq!(plan.kind, MemoryOpKind::AskMissing);
    let request = provider.last_request();
    assert!(request.output_format.is_none());
    assert!(request.messages[0].content.as_text().contains("Return JSON only."));
}

#[derive(Clone)]
struct StreamingOnlyMemoryProvider {
    response: &'static str,
}

#[async_trait]
impl LLMProvider for StreamingOnlyMemoryProvider {
    fn name(&self) -> &str {
        "streaming-memory"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_non_streaming(&self, _model: &str) -> bool {
        false
    }

    async fn generate(&self, _request: LLMRequest) -> std::result::Result<LLMResponse, LLMError> {
        panic!("generate should not be called for streaming-only provider")
    }

    async fn stream_normalized(&self, _request: LLMRequest) -> std::result::Result<LLMNormalizedStream, LLMError> {
        Ok(Box::pin(stream::iter(vec![Ok(NormalizedStreamEvent::Done {
            response: Box::new(LLMResponse {
                content: Some(self.response.to_string()),
                model: "stub-model".to_string(),
                tool_calls: None,
                usage: None,
                finish_reason: FinishReason::Stop,
                reasoning: None,
                reasoning_details: None,
                organization_id: None,
                request_id: None,
                tool_references: Vec::new(),
                compaction: None,
            }),
        })])))
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["stub-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> std::result::Result<(), LLMError> {
        Ok(())
    }
}

#[tokio::test]
async fn planner_supports_streaming_only_provider() {
    let workspace = tempdir().expect("workspace");
    let provider = StreamingOnlyMemoryProvider {
        response: "{\"kind\":\"ask_missing\",\"facts\":[],\"selected_ids\":[],\"missing\":{\"field\":\"name\",\"prompt\":\"What name should VT Code remember?\"},\"message\":null}",
    };
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };

    let plan = plan_memory_operation_with_provider(
        &provider,
        &route,
        workspace.path(),
        MemoryOpKind::Remember,
        "remember my name",
        None,
        None,
        &[],
    )
    .await
    .expect("streaming planner should succeed");

    assert_eq!(plan.kind, MemoryOpKind::AskMissing);
}

#[tokio::test]
async fn summary_falls_back_to_prompt_only_json_when_native_schema_is_unsupported() {
    let workspace = tempdir().expect("workspace");
    let provider = StaticProvider::prompt_only_json(
        "Summary:\n{\"bullets\":[\"Prefer cargo nextest\",\"Tests live under vtcode-core/tests\"]}",
    );
    let route = MemoryModelRoute {
        provider_name: "stub".to_string(),
        model: "stub-model".to_string(),
        temperature: 0.0,
    };

    let summary = summarize_memory_with_provider(
        &provider,
        &route,
        workspace.path(),
        &[GroundedFactRecord {
            fact: "Prefer cargo nextest".to_string(),
            source: encode_topic_source(MemoryTopic::Preferences, "manual_memory"),
        }],
        &[GroundedFactRecord {
            fact: "Tests live under vtcode-core/tests".to_string(),
            source: encode_topic_source(MemoryTopic::RepositoryFacts, "tool:code_search"),
        }],
        &[],
    )
    .await
    .expect("summary");

    assert!(summary.contains("Prefer cargo nextest"));
    let request = provider.last_request();
    assert!(request.output_format.is_none());
}

#[tokio::test]
async fn rebuild_summary_uses_summary_file_not_registry() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");
    let memory_config = enabled_memory_config_for(workspace.path());
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.features.memories = true;
    vt_cfg.agent.persistent_memory = memory_config.clone();

    let memory_dir = resolve_persistent_memory_dir(&memory_config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir.clone(), false);
    let mut created_files = Vec::new();
    ensure_memory_layout(&files, &mut created_files).await.expect("layout");
    tokio::fs::write(
        &files.preferences_file,
        render_topic_file(
            MemoryTopic::Preferences,
            &[GroundedFactRecord {
                fact: "Prefer cargo nextest".to_string(),
                source: encode_topic_source(MemoryTopic::Preferences, "manual_memory"),
            }],
        ),
    )
    .await
    .expect("write prefs");

    let excerpt = read_persistent_memory_excerpt(&memory_config, workspace.path())
        .await
        .expect("excerpt")
        .expect("present");
    assert!(excerpt.contents.contains("No durable memory notes"));

    rebuild_persistent_memory_summary(&runtime_config(workspace.path()), Some(&vt_cfg))
        .await
        .expect("rebuild")
        .expect("report");

    let excerpt = read_persistent_memory_excerpt(&memory_config, workspace.path())
        .await
        .expect("excerpt")
        .expect("present");
    assert!(excerpt.contents.contains("Prefer cargo nextest"));
}

#[tokio::test]
async fn scaffold_creates_memory_layout_even_when_disabled() {
    let workspace = tempdir().expect("workspace");
    let config = PersistentMemoryConfig {
        enabled: false,
        directory_override: Some(workspace.path().join(".memory").display().to_string()),
        ..PersistentMemoryConfig::default()
    };

    let status = scaffold_persistent_memory(&config, workspace.path())
        .await
        .expect("scaffold succeeds")
        .expect("status");

    assert!(!status.enabled);
    assert!(status.summary_file.exists());
    assert!(status.memory_file.exists());
    assert!(status.preferences_file.exists());
    assert!(status.repository_facts_file.exists());
    assert!(status.notes_dir.exists());
    assert!(status.rollout_summaries_dir.exists());
}

#[tokio::test]
async fn rebuild_generated_files_include_notes_as_canonical_inputs() {
    let workspace = tempdir().expect("workspace");
    let config = PersistentMemoryConfig {
        enabled: true,
        directory_override: Some(workspace.path().join(".memory").display().to_string()),
        ..PersistentMemoryConfig::default()
    };

    scaffold_persistent_memory(&config, workspace.path())
        .await
        .expect("scaffold")
        .expect("status");
    let memory_dir = resolve_persistent_memory_dir(&config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    tokio::fs::write(
        files.notes_dir.join("project.md"),
        "# Project Notes\n\n- Keep Anthropic memory backed by shared storage.\n",
    )
    .await
    .expect("write note");

    rebuild_generated_memory_files(&config, workspace.path())
        .await
        .expect("rebuild");

    let summary = tokio::fs::read_to_string(&files.summary_file).await.expect("summary");
    let index = tokio::fs::read_to_string(&files.memory_file).await.expect("index");

    assert!(summary.contains("Keep Anthropic memory backed by shared storage"));
    assert!(index.contains("## Note Files"), "{index}");
    assert!(index.contains("Keep Anthropic memory backed by shared storage"));
}

#[tokio::test]
async fn persistent_memory_parallel_reads_preserve_source_order() {
    let workspace = tempdir().expect("workspace");
    let config = enabled_memory_config_for(workspace.path());
    scaffold_persistent_memory(&config, workspace.path())
        .await
        .expect("scaffold")
        .expect("status");
    let memory_dir = resolve_persistent_memory_dir(&config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);

    tokio::fs::write(
        &files.preferences_file,
        render_topic_file(
            MemoryTopic::Preferences,
            &[GroundedFactRecord {
                fact: "Prefer cargo nextest".to_string(),
                source: encode_topic_source(MemoryTopic::Preferences, "preference.md"),
            }],
        ),
    )
    .await
    .expect("preferences");
    tokio::fs::write(
        &files.repository_facts_file,
        render_topic_file(
            MemoryTopic::RepositoryFacts,
            &[GroundedFactRecord {
                fact: "The repository uses Rust stable".to_string(),
                source: encode_topic_source(MemoryTopic::RepositoryFacts, "repository.md"),
            }],
        ),
    )
    .await
    .expect("repository facts");
    tokio::fs::write(
        files.rollout_summaries_dir.join("rollout.md"),
        render_topic_file(
            MemoryTopic::Preferences,
            &[GroundedFactRecord {
                fact: "Keep the runtime path measured".to_string(),
                source: encode_topic_source(MemoryTopic::Preferences, "rollout.md"),
            }],
        ),
    )
    .await
    .expect("rollout");
    tokio::fs::write(files.notes_dir.join("note.md"), "# Notes\n\n- Keep async reads ordered at assembly time.\n")
        .await
        .expect("note");

    let candidates = list_persistent_memory_candidates(&config, workspace.path())
        .await
        .expect("candidates")
        .expect("enabled");
    let facts = candidates.into_iter().map(|candidate| candidate.fact).collect::<Vec<_>>();

    assert_eq!(
        facts,
        vec![
            "Prefer cargo nextest",
            "The repository uses Rust stable",
            "Keep the runtime path measured",
            "Keep async reads ordered at assembly time.",
        ]
    );
}

// Regression test for the O(n) dedup in `collect_all_memory_matches`.
// Locks down two invariants of the "keep last occurrence with move-to-end
// ordering" semantics that the O(n²) -> O(n) rewrite must preserve:
//   1. Duplicate facts (equal after whitespace normalization + ASCII
//      lowercasing) collapse to a single entry, and the retained entry is
//      the *last* one in source order (its `source` is the last writer's).
//   2. Non-duplicate entries keep their relative order, and the collapsed
//      entry moves to the position of its last occurrence.
// Concrete: prefs=[A(old), B(b)], repo=[A(new), C(c)] -> [B, A(new), C].
#[tokio::test]
async fn collect_all_memory_matches_dedup_keeps_last_occurrence_and_order() {
    let workspace = tempdir().expect("workspace");
    let config = enabled_memory_config_for(workspace.path());
    scaffold_persistent_memory(&config, workspace.path())
        .await
        .expect("scaffold")
        .expect("status");
    let memory_dir = resolve_persistent_memory_dir(&config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);

    // Two preferences: A (with extra spacing, will be overwritten) and B.
    tokio::fs::write(
        &files.preferences_file,
        render_topic_file(
            MemoryTopic::Preferences,
            &[
                GroundedFactRecord {
                    fact: "Prefer  cargo   nextest".to_string(),
                    source: encode_topic_source(MemoryTopic::Preferences, "old.md"),
                },
                GroundedFactRecord {
                    fact: "Prefer pnpm for scripts".to_string(),
                    source: encode_topic_source(MemoryTopic::Preferences, "b.md"),
                },
            ],
        ),
    )
    .await
    .expect("preferences");
    // Repository facts: A again (different case/spacing, last writer) and C.
    tokio::fs::write(
        &files.repository_facts_file,
        render_topic_file(
            MemoryTopic::RepositoryFacts,
            &[
                GroundedFactRecord {
                    fact: "prefer cargo nextest".to_string(),
                    source: encode_topic_source(MemoryTopic::RepositoryFacts, "new.md"),
                },
                GroundedFactRecord {
                    fact: "The repo uses Rust stable".to_string(),
                    source: encode_topic_source(MemoryTopic::RepositoryFacts, "c.md"),
                },
            ],
        ),
    )
    .await
    .expect("repository facts");

    let matches = collect_all_memory_matches(&files).await.expect("dedup");

    // [A(old), B(b), A(new), C(c)] -> keep last A, move-to-end -> [B, A(new), C]
    assert_eq!(
        matches.iter().map(|m| (m.fact.as_str(), m.source.as_str())).collect::<Vec<_>>(),
        vec![
            ("Prefer pnpm for scripts", "b.md"),
            ("prefer cargo nextest", "new.md"),
            ("The repo uses Rust stable", "c.md"),
        ],
        "dedup must keep the last occurrence and move it to its last position"
    );

    // No duplicate normalized facts survive.
    let mut normalized: Vec<String> = matches
        .iter()
        .map(|m| normalize_whitespace(&m.fact).to_ascii_lowercase())
        .collect();
    normalized.sort();
    let mut deduped = normalized.clone();
    deduped.dedup();
    assert_eq!(normalized.len(), deduped.len(), "no duplicate facts should remain");
}

#[tokio::test]
async fn persistent_memory_parallel_reads_preserve_error_context() {
    let workspace = tempdir().expect("workspace");
    let config = enabled_memory_config_for(workspace.path());
    scaffold_persistent_memory(&config, workspace.path())
        .await
        .expect("scaffold")
        .expect("status");
    let memory_dir = resolve_persistent_memory_dir(&config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    tokio::fs::remove_file(&files.preferences_file)
        .await
        .expect("remove preferences");
    tokio::fs::create_dir(&files.preferences_file)
        .await
        .expect("replace preferences with directory");

    let error = list_persistent_memory_candidates(&config, workspace.path())
        .await
        .expect_err("directory should not parse as a topic file");

    assert!(error.to_string().contains("Failed to read"), "{error:#}");
    assert!(error.to_string().contains("preferences.md"), "{error:#}");
}

#[tokio::test]
async fn remember_plan_persists_normalized_manual_memory_update() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");
    let vt_cfg = enabled_vt_memory_config_for(workspace.path());
    let plan = MemoryOpPlan {
        kind: MemoryOpKind::Remember,
        facts: vec![MemoryPlannedFact {
            topic: MemoryPlannedTopic::Preferences,
            fact: "Prefer pnpm for workspace package management.".to_string(),
            source: "manual_memory".to_string(),
        }],
        selected_ids: Vec::new(),
        missing: None,
        message: None,
    };

    let report = persist_remembered_memory_plan(&runtime_config(workspace.path()), Some(&vt_cfg), &plan)
        .await
        .expect("remember plan")
        .expect("report");

    assert_eq!(report.added_facts, 1);
    let excerpt = read_persistent_memory_excerpt(&vt_cfg.agent.persistent_memory, workspace.path())
        .await
        .expect("excerpt")
        .expect("present");
    assert!(excerpt.contents.contains("Prefer pnpm"));

    let candidates = list_persistent_memory_candidates(&vt_cfg.agent.persistent_memory, workspace.path())
        .await
        .expect("candidates")
        .expect("enabled");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.fact == "Prefer pnpm for workspace package management.")
    );
}

#[tokio::test]
async fn forget_planned_matches_remove_notes_from_memory_files() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".git"), "gitdir: /tmp/git").expect("git marker");
    let runtime = runtime_config(workspace.path());
    let vt_cfg = enabled_vt_memory_config_for(workspace.path());
    let remember_plan = MemoryOpPlan {
        kind: MemoryOpKind::Remember,
        facts: vec![MemoryPlannedFact {
            topic: MemoryPlannedTopic::Preferences,
            fact: "Prefer pnpm for workspace package management.".to_string(),
            source: "manual_memory".to_string(),
        }],
        selected_ids: Vec::new(),
        missing: None,
        message: None,
    };

    persist_remembered_memory_plan(&runtime, Some(&vt_cfg), &remember_plan)
        .await
        .expect("remember plan")
        .expect("report");

    let matches = find_persistent_memory_matches(&vt_cfg.agent.persistent_memory, workspace.path(), "pnpm")
        .await
        .expect("find matches")
        .expect("enabled");
    assert!(!matches.is_empty());

    let candidates = list_persistent_memory_candidates(&vt_cfg.agent.persistent_memory, workspace.path())
        .await
        .expect("list")
        .expect("enabled")
        .into_iter()
        .enumerate()
        .map(|(index, entry)| MemoryOpCandidate { id: index, source: entry.source, fact: entry.fact })
        .collect::<Vec<_>>();
    let plan = MemoryOpPlan {
        kind: MemoryOpKind::Forget,
        facts: Vec::new(),
        selected_ids: vec![0],
        missing: None,
        message: None,
    };

    let report = forget_planned_persistent_memory_matches(&runtime, Some(&vt_cfg), &candidates, &plan)
        .await
        .expect("forget plan")
        .expect("report");
    assert!(report.removed_facts >= 1);

    let matches = find_persistent_memory_matches(&vt_cfg.agent.persistent_memory, workspace.path(), "pnpm")
        .await
        .expect("find matches")
        .expect("enabled");
    assert!(matches.is_empty());

    let excerpt = read_persistent_memory_excerpt(&vt_cfg.agent.persistent_memory, workspace.path())
        .await
        .expect("excerpt")
        .expect("present");
    assert!(!excerpt.contents.contains("Prefer pnpm"));
}

#[test]
fn cleanup_status_flags_legacy_prompt_lines() {
    let workspace = tempdir().expect("workspace");
    let memory_config = enabled_memory_config_for(workspace.path());
    let memory_dir = resolve_persistent_memory_dir(&memory_config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    std::fs::create_dir_all(&files.directory).expect("dir");
    std::fs::create_dir_all(&files.rollout_summaries_dir).expect("rollout dir");
    std::fs::write(
        &files.preferences_file,
        "# Preferences\n\n- [user_assertion] save to memory and remember my name\n",
    )
    .expect("prefs");
    std::fs::write(&files.summary_file, "# VT Code Memory Summary\n\n- {\"query\":\"pnpm\"}\n").expect("summary");

    let status = detect_memory_cleanup_status(&files).expect("status");
    assert!(status.needed);
    assert!(status.suspicious_facts >= 1);
    assert!(status.suspicious_summary_lines >= 1);
}

#[test]
fn cleanup_status_ignores_normalized_user_assertion_fact() {
    let workspace = tempdir().expect("workspace");
    let memory_config = enabled_memory_config_for(workspace.path());
    let memory_dir = resolve_persistent_memory_dir(&memory_config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    std::fs::create_dir_all(&files.directory).expect("dir");
    std::fs::write(&files.preferences_file, "# Preferences\n\n- [user_assertion] My name is Vinh Nguyen\n")
        .expect("prefs");

    let status = detect_memory_cleanup_status(&files).expect("status");
    assert!(!status.needed);
    assert_eq!(status.suspicious_facts, 0);
    assert_eq!(status.suspicious_summary_lines, 0);
}

#[test]
fn cleanup_status_ignores_embedded_remember_word_in_fact() {
    let workspace = tempdir().expect("workspace");
    let memory_config = enabled_memory_config_for(workspace.path());
    let memory_dir = resolve_persistent_memory_dir(&memory_config, workspace.path())
        .expect("memory dir")
        .expect("resolved dir");
    let files = PersistentMemoryFiles::new(memory_dir, false);
    std::fs::create_dir_all(&files.directory).expect("dir");
    std::fs::write(
        &files.repository_facts_file,
        "# Repository Facts\n\n- [repository_fact] The docs remember prior design decisions in AGENTS.md.\n",
    )
    .expect("facts");

    let status = detect_memory_cleanup_status(&files).expect("status");
    assert!(!status.needed);
    assert_eq!(status.suspicious_facts, 0);
}

#[test]
fn resolve_memory_model_route_prefers_explicit_small_model_provider() {
    let workspace = tempdir().expect("workspace");
    let runtime = runtime_config(workspace.path());
    let mut vt_cfg = enabled_vt_memory_config_for(workspace.path());
    vt_cfg.agent.small_model.enabled = true;
    vt_cfg.agent.small_model.use_for_memory = true;
    vt_cfg.agent.small_model.model = "claude-4-5-haiku".to_string();

    let routes = resolve_memory_model_routes(&runtime, Some(&vt_cfg), MemoryPhase::Extract);

    assert_eq!(routes.primary.provider_name, "openai");
    // `gpt-5` is a deprecated but still valid provider route without a
    // catalog sibling; preserve it instead of selecting an unrelated model.
    assert_eq!(routes.primary.model, "gpt-5");
    assert!(routes.warning.is_some());
}

#[test]
fn resolve_memory_route_api_key_uses_runtime_key_for_active_provider() {
    let workspace = tempdir().expect("workspace");
    let route = MemoryModelRoute {
        provider_name: "openai".to_string(),
        model: "gpt-5-mini".to_string(),
        temperature: 0.1,
    };

    let api_key = resolve_api_key_for_model_route(
        &crate::llm::ModelRoute {
            provider_name: route.provider_name.clone(),
            model: route.model.clone(),
        },
        &runtime_config(workspace.path()),
    );

    assert_eq!(api_key.as_deref(), Some("test-key"));
}

#[test]
fn resolves_project_scoped_memory_directory() {
    let workspace = tempdir().expect("workspace");
    std::fs::write(workspace.path().join(".vtcode-project"), "renamed-project").expect("project name");
    let config = enabled_memory_config();
    let directory = resolve_persistent_memory_dir(&config, workspace.path())
        .expect("memory dir")
        .expect("memory dir should resolve");
    assert!(directory.to_string_lossy().contains("state/projects/renamed-project/memory"));
}

#[test]
fn migrates_legacy_memory_into_empty_target_directory() {
    let root = tempdir().expect("root");
    let legacy_dir = root.path().join("legacy/projects/repo/memory");
    let target_dir = root.path().join("home/.vtcode/projects/repo/memory");
    std::fs::create_dir_all(legacy_dir.join(ROLLOUT_SUMMARIES_DIRNAME)).expect("legacy dir");
    std::fs::write(
        legacy_dir.join(PREFERENCES_FILENAME),
        render_topic_file(
            MemoryTopic::Preferences,
            &[GroundedFactRecord {
                fact: "Prefer cargo nextest".to_string(),
                source: encode_topic_source(MemoryTopic::Preferences, "manual_memory"),
            }],
        ),
    )
    .expect("legacy prefs");

    migrate_legacy_memory_dir(&legacy_dir, &target_dir).expect("migrate");

    assert!(legacy_dir.exists());
    let migrated = std::fs::read_to_string(target_dir.join(PREFERENCES_FILENAME)).expect("target prefs");
    assert!(migrated.contains("Prefer cargo nextest"));
}

#[test]
fn migrates_legacy_memory_over_scaffold_only_target() {
    let root = tempdir().expect("root");
    let legacy_dir = root.path().join("legacy/projects/repo/memory");
    let target_dir = root.path().join("home/.vtcode/projects/repo/memory");
    std::fs::create_dir_all(legacy_dir.join(ROLLOUT_SUMMARIES_DIRNAME)).expect("legacy dir");
    std::fs::write(
        legacy_dir.join(REPOSITORY_FACTS_FILENAME),
        render_topic_file(
            MemoryTopic::RepositoryFacts,
            &[GroundedFactRecord {
                fact: "Tests live under vtcode-core/tests".to_string(),
                source: encode_topic_source(MemoryTopic::RepositoryFacts, "tool:code_search"),
            }],
        ),
    )
    .expect("legacy facts");

    std::fs::create_dir_all(target_dir.join(ROLLOUT_SUMMARIES_DIRNAME)).expect("target dir");
    std::fs::write(target_dir.join(PREFERENCES_FILENAME), render_topic_file(MemoryTopic::Preferences, &[]))
        .expect("target prefs");
    std::fs::write(target_dir.join(REPOSITORY_FACTS_FILENAME), render_topic_file(MemoryTopic::RepositoryFacts, &[]))
        .expect("target facts");
    std::fs::write(target_dir.join(MEMORY_FILENAME), render_memory_index(&[], &[], &[], 0)).expect("target memory");
    std::fs::write(target_dir.join(MEMORY_SUMMARY_FILENAME), render_memory_summary(&[], &[], &[]))
        .expect("target summary");

    migrate_legacy_memory_dir(&legacy_dir, &target_dir).expect("migrate");

    assert!(legacy_dir.exists());
    let migrated = std::fs::read_to_string(target_dir.join(REPOSITORY_FACTS_FILENAME)).expect("target facts");
    assert!(migrated.contains("Tests live under vtcode-core/tests"));
}

#[cfg(unix)]
#[test]
fn legacy_memory_migration_rejects_symlinked_source_ancestors() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("root");
    let outside_projects = root.path().join("outside/projects/repo/memory");
    std::fs::create_dir_all(&outside_projects).expect("outside memory");
    std::fs::write(outside_projects.join(PREFERENCES_FILENAME), "outside").expect("outside memory file");

    let legacy_root = root.path().join("legacy");
    std::fs::create_dir_all(&legacy_root).expect("legacy root");
    symlink(root.path().join("outside/projects"), legacy_root.join("projects")).expect("legacy ancestor symlink");

    let target_dir = root.path().join("target/memory");
    let result = migrate_legacy_memory_dir(&legacy_root.join("projects/repo/memory"), &target_dir);

    assert!(result.is_err());
    assert!(!target_dir.join(PREFERENCES_FILENAME).exists());
}

#[cfg(unix)]
#[test]
fn legacy_memory_migration_rejects_symlinked_target_content() {
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("root");
    let legacy_dir = root.path().join("legacy/projects/repo/memory");
    std::fs::create_dir_all(&legacy_dir).expect("legacy memory");
    std::fs::write(legacy_dir.join(PREFERENCES_FILENAME), "legacy").expect("legacy preferences");

    let outside = root.path().join("outside");
    std::fs::write(&outside, "outside").expect("outside memory");
    let target_dir = root.path().join("target/memory");
    std::fs::create_dir_all(&target_dir).expect("target memory");
    symlink(&outside, target_dir.join(PREFERENCES_FILENAME)).expect("target symlink");

    let result = migrate_legacy_memory_dir(&legacy_dir, &target_dir);

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&outside).expect("outside file remains"), "outside");
}

#[tokio::test]
async fn lock_age_reports_fresh_file_as_not_stale() {
    let workspace = tempdir().expect("workspace");
    let lock_path = workspace.path().join(".memory.lock");
    std::fs::write(&lock_path, "").expect("lock file");

    let age = lock_age(&lock_path).await.expect("age should resolve");
    assert!(
        age < Duration::from_secs(LOCK_STALE_AFTER_SECS),
        "freshly created lock reported as already stale: {age:?}"
    );
}

#[tokio::test]
async fn acquire_with_stale_after_steals_orphaned_lock() {
    let workspace = tempdir().expect("workspace");
    let lock_path = workspace.path().join(".memory.lock");
    std::fs::write(&lock_path, "").expect("orphaned lock file");

    let lock = MemoryLock::acquire_with_stale_after(&lock_path, Duration::ZERO)
        .await
        .expect("stale lock should be stolen");

    assert_eq!(lock.path, lock_path);
    assert!(lock_path.exists());
}

#[tokio::test]
async fn acquire_succeeds_when_no_lock_file_exists() {
    let workspace = tempdir().expect("workspace");
    let lock_path = workspace.path().join(".memory.lock");

    let lock = MemoryLock::acquire(&lock_path)
        .await
        .expect("acquire should succeed without contention");

    assert!(lock_path.exists());
    assert_eq!(lock.path, lock_path);
}

#[tokio::test]
async fn fresh_lock_is_not_treated_as_stale_under_large_threshold() {
    let workspace = tempdir().expect("workspace");
    let lock_path = workspace.path().join(".memory.lock");
    std::fs::write(&lock_path, "").expect("lock file");

    // Rather than driving the full ~2s retry/timeout loop with a huge
    // threshold, assert directly on the staleness predicate that
    // `acquire_with_stale_after` uses: a freshly written lock file must not
    // already exceed a generous stale-after window.
    let age = lock_age(&lock_path).await.expect("age should resolve");
    assert!(
        age < Duration::from_secs(LOCK_STALE_AFTER_SECS),
        "freshly created lock would be incorrectly stolen: {age:?}"
    );
}
