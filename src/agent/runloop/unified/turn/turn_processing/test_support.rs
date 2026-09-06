use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use hashbrown::HashMap;
use serde_json::Value;
use tokio::sync::{Notify, RwLock};
use vtcode_config::core::PromptCachingConfig;
use vtcode_core::acp::ToolPermissionCache;
use vtcode_core::config::types::{AgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};
use vtcode_core::core::agent::runtime::RuntimeSteering;
use vtcode_core::core::agent::steering::SteeringMessage;
use vtcode_core::core::decision_tracker::DecisionTracker;
use vtcode_core::core::trajectory::TrajectoryLogger;
use vtcode_core::llm::provider::{self as uni, ToolDefinition};
use vtcode_core::primary_agent::ActivePrimaryAgentState;
use vtcode_core::tools::adaptive_rate_limiter::AdaptiveRateLimiter;
use vtcode_core::tools::circuit_breaker::CircuitBreaker;
use vtcode_core::tools::health::ToolHealthTracker;
use vtcode_core::tools::{ApprovalRecorder, ToolResultCache};
use vtcode_ui::tui::app::{InlineHandle, InlineSession};

use crate::agent::runloop::mcp_events::McpPanelState;
use crate::agent::runloop::unified::context_manager::ContextManager;
use crate::agent::runloop::unified::inline_events::harness::HarnessEventEmitter;
use crate::agent::runloop::unified::run_loop_context::{HarnessTurnState, RecoveryMode, TurnId, TurnRunId};
use crate::agent::runloop::unified::state::{CtrlCState, SessionStats};
use crate::agent::runloop::unified::status_line::InputStatusState;
use crate::agent::runloop::unified::tool_call_safety::ToolCallSafetyValidator;
use crate::agent::runloop::unified::tool_catalog::ToolCatalogState;
use crate::agent::runloop::unified::turn::context::{
    LLMContext, ToolContext, TurnProcessingContext, TurnProcessingContextParts, TurnProcessingState, UIContext,
};

#[derive(Clone)]
struct NoopProvider;

#[async_trait::async_trait]
impl uni::LLMProvider for NoopProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    async fn generate(&self, _request: uni::LLMRequest) -> Result<uni::LLMResponse, uni::LLMError> {
        Ok(uni::LLMResponse {
            content: Some(String::new()),
            model: "noop-model".to_string(),
            tool_calls: None,
            usage: None,
            finish_reason: uni::FinishReason::Stop,
            reasoning: None,
            reasoning_details: None,
            organization_id: None,
            request_id: None,
            tool_references: Vec::new(),
            compaction: None,
        })
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["noop-model".to_string()]
    }

    // The stub masquerades as the OpenAI provider, whose routes accept the
    // standard effort levels; without this the mapper cannot pass a configured
    // effort through.
    fn supports_reasoning_effort(&self, _model: &str) -> bool {
        true
    }

    fn supported_reasoning_efforts(&self, _model: &str) -> &'static [&'static str] {
        &["low", "medium", "high", "xhigh", "max"]
    }

    fn validate_request(&self, _request: &uni::LLMRequest) -> Result<(), uni::LLMError> {
        Ok(())
    }
}

fn create_headless_session() -> (InlineSession, tokio::sync::mpsc::UnboundedReceiver<vtcode_core::ui::InlineCommand>) {
    let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
    let (_event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    (
        InlineSession {
            handle: InlineHandle::new_for_tests(command_tx),
            events: event_rx,
            worker: None,
        },
        command_rx,
    )
}

pub(crate) struct TestTurnProcessingBacking {
    _temp: tempfile::TempDir,
    tool_registry: vtcode_core::tools::ToolRegistry,
    tools: Arc<RwLock<Vec<ToolDefinition>>>,
    tool_result_cache: Arc<RwLock<ToolResultCache>>,
    tool_permission_cache: Arc<RwLock<ToolPermissionCache>>,
    permissions_state: Arc<RwLock<vtcode_core::config::PermissionsConfig>>,
    decision_ledger: Arc<RwLock<DecisionTracker>>,
    approval_recorder: Arc<ApprovalRecorder>,
    session_stats: SessionStats,
    plan_session: crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState,
    mcp_panel_state: McpPanelState,
    context_manager: ContextManager,
    last_forced_redraw: Instant,
    input_status_state: InputStatusState,
    session: InlineSession,
    command_receiver: tokio::sync::mpsc::UnboundedReceiver<vtcode_core::ui::InlineCommand>,
    handle: InlineHandle,
    renderer: vtcode_core::utils::ansi::AnsiRenderer,
    ctrl_c_state: Arc<CtrlCState>,
    ctrl_c_notify: Arc<Notify>,
    safety_validator: Arc<ToolCallSafetyValidator>,
    circuit_breaker: Arc<CircuitBreaker>,
    tool_health_tracker: Arc<ToolHealthTracker>,
    rate_limiter: Arc<AdaptiveRateLimiter>,
    telemetry: Arc<vtcode_core::core::telemetry::TelemetryManager>,
    autonomous_executor: Arc<vtcode_core::tools::autonomous_executor::AutonomousExecutor>,
    error_recovery: Arc<RwLock<vtcode_core::core::agent::error_recovery::ErrorRecoveryState>>,
    harness_state: HarnessTurnState,
    harness_emitter: Option<HarnessEventEmitter>,
    auto_finish_planning_attempted: bool,
    skip_confirmations_for_test: bool,
    working_history: Vec<uni::Message>,
    tool_catalog: Arc<ToolCatalogState>,
    default_placeholder: Option<String>,
    active_primary_agent: ActivePrimaryAgentState,
    runtime_steering: RuntimeSteering,
    config: AgentConfig,
    provider_client: Box<dyn uni::LLMProvider>,
    traj: TrajectoryLogger,
    turn_metadata_cache: Option<Option<Value>>,
}

impl TestTurnProcessingBacking {
    pub(crate) async fn new(max_tool_calls: usize) -> Self {
        let temp = tempfile::TempDir::new().expect("temp workspace");
        let workspace = temp.path().to_path_buf();

        let tool_registry = vtcode_core::tools::ToolRegistry::new(workspace.clone()).await;
        let tools = Arc::new(RwLock::new(Vec::new()));
        let tool_result_cache = Arc::new(RwLock::new(ToolResultCache::new(8)));
        let tool_permission_cache = Arc::new(RwLock::new(ToolPermissionCache::new()));
        let permissions_state = Arc::new(RwLock::new(vtcode_core::config::PermissionsConfig::default()));
        let decision_ledger = Arc::new(RwLock::new(DecisionTracker::new()));
        let approval_recorder = Arc::new(ApprovalRecorder::new(workspace.clone()));
        let session_stats = SessionStats::default();
        let plan_session =
            crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
        let mcp_panel_state = McpPanelState::default();
        let loaded_skills = Arc::new(RwLock::new(HashMap::new()));
        let context_manager = ContextManager::new("You are VT Code.".to_string(), (), loaded_skills, None);
        let last_forced_redraw = Instant::now();
        let input_status_state = InputStatusState::default();
        let (mut session, command_receiver) = create_headless_session();
        session.set_skip_confirmations(true);
        let handle = session.clone_inline_handle();
        let renderer = vtcode_core::utils::ansi::AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let ctrl_c_notify = Arc::new(Notify::new());
        let safety_validator = Arc::new(ToolCallSafetyValidator::new());
        safety_validator.start_turn();
        let circuit_breaker = Arc::new(CircuitBreaker::default());
        let tool_health_tracker = Arc::new(ToolHealthTracker::new(3));
        let rate_limiter = Arc::new(AdaptiveRateLimiter::default());
        let telemetry = Arc::new(vtcode_core::core::telemetry::TelemetryManager::new());
        let autonomous_executor = Arc::new(vtcode_core::tools::autonomous_executor::AutonomousExecutor::new());
        let error_recovery =
            Arc::new(RwLock::new(vtcode_core::core::agent::error_recovery::ErrorRecoveryState::default()));
        let harness_state = HarnessTurnState::new(
            TurnRunId("run-test".to_string()),
            TurnId("turn-test".to_string()),
            max_tool_calls,
            60,
            0,
        );
        let tool_catalog = Arc::new(ToolCatalogState::new());
        let config = AgentConfig {
            model: "noop-model".to_string(),
            api_key: "test-key".to_string(),
            provider: "openai".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            workspace,
            verbose: false,
            quiet: false,
            theme: "default".to_string(),
            reasoning_effort: ReasoningEffortLevel::Medium,
            ui_surface: UiSurfacePreference::Inline,
            prompt_cache: PromptCachingConfig::default(),
            model_source: ModelSelectionSource::WorkspaceConfig,
            custom_api_keys: BTreeMap::new(),
            checkpointing_enabled: false,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: 10,
            checkpointing_max_age_days: None,
            max_conversation_turns: 16,
            model_behavior: None,
            openai_chatgpt_auth: None,
        };

        Self {
            _temp: temp,
            tool_registry,
            tools,
            tool_result_cache,
            tool_permission_cache,
            permissions_state,
            decision_ledger,
            approval_recorder,
            session_stats,
            plan_session,
            mcp_panel_state,
            context_manager,
            last_forced_redraw,
            input_status_state,
            session,
            command_receiver,
            handle,
            renderer,
            ctrl_c_state,
            ctrl_c_notify,
            safety_validator,
            circuit_breaker,
            tool_health_tracker,
            rate_limiter,
            telemetry,
            autonomous_executor,
            error_recovery,
            harness_state,
            harness_emitter: None,
            auto_finish_planning_attempted: false,
            skip_confirmations_for_test: true,
            working_history: Vec::new(),
            tool_catalog,
            default_placeholder: None,
            active_primary_agent: ActivePrimaryAgentState::default(),
            runtime_steering: RuntimeSteering::default(),
            config,
            provider_client: Box::new(NoopProvider),
            traj: TrajectoryLogger::disabled(),
            turn_metadata_cache: None,
        }
    }

    pub(crate) async fn add_tool_definition(&self, tool: ToolDefinition) {
        self.tools.write().await.push(tool);
    }

    pub(crate) fn workspace_path(&self) -> &Path {
        self._temp.path()
    }

    pub(crate) fn enable_planning(&self) {
        self.tool_registry.enable_planning();
    }

    pub(crate) async fn persist_plan_for_test(&self, plan_text: &str) {
        crate::agent::runloop::unified::planning_workflow::persist_plan_draft(
            &self.tool_registry.planning_workflow_state(),
            plan_text,
        )
        .await
        .expect("test plan should persist");
    }

    /// Persist a plan and then return the registry to the post-approval state
    /// (planning finished, plan file retained) — mirrors what
    /// `complete_approved_plan_handoff` leaves behind for execution turns.
    pub(crate) async fn persist_approved_plan_for_test(&self, plan_text: &str) {
        self.enable_planning();
        self.persist_plan_for_test(plan_text).await;
        self.tool_registry.disable_planning();
        self.tool_registry.planning_workflow_state().disable();
    }

    pub(crate) fn select_primary_agent_from_specs(&mut self, specs: &[vtcode_config::SubagentSpec], requested: &str) {
        self.active_primary_agent
            .select_from_specs(specs, requested)
            .expect("test primary agent should resolve");
    }

    pub(crate) fn set_steering_receiver(&mut self, receiver: tokio::sync::mpsc::UnboundedReceiver<SteeringMessage>) {
        self.runtime_steering.set_receiver(Some(receiver));
    }

    pub(crate) fn deferred_follow_up_inputs(&mut self) -> Vec<String> {
        let mut inputs = Vec::new();
        let mut steering = std::mem::take(&mut self.runtime_steering);
        while let Some(input) = steering.pop_follow_up_input() {
            inputs.push(input);
        }
        self.runtime_steering = steering;
        inputs
    }

    pub(crate) fn set_loop_limit(&self, tool_name: &str, limit: usize) {
        self.autonomous_executor.set_loop_limit(tool_name, limit);
    }

    pub(crate) fn record_tool_call(&self, tool_name: &str, args: &Value) -> Option<String> {
        self.autonomous_executor.record_tool_call(tool_name, args)
    }

    pub(crate) fn is_hard_limit_exceeded(&self, tool_name: &str) -> bool {
        self.autonomous_executor.is_hard_limit_exceeded(tool_name)
    }

    pub(crate) fn recovery_is_tool_free(&self) -> bool {
        self.harness_state.recovery_is_tool_free()
    }

    /// Replace the backing's LLM provider (used by `run_turn_loop` integration tests).
    pub(crate) fn set_provider(&mut self, provider: Box<dyn uni::LLMProvider>) {
        self.provider_client = provider;
    }

    pub(crate) fn enable_harness_emitter(&mut self) -> std::path::PathBuf {
        let path = self._temp.path().join("harness-events.jsonl");
        self.harness_emitter = Some(HarnessEventEmitter::new(path.clone()).expect("test harness emitter"));
        path
    }

    pub(crate) fn emit_harness_assistant_message_for_test(&self, text: &str) {
        self.harness_emitter
            .as_ref()
            .expect("test harness emitter must be enabled")
            .emit_assistant_message("turn-test", text)
            .expect("test harness assistant message");
    }

    pub(crate) fn rendered_inline_output(&mut self) -> String {
        crate::agent::runloop::tool_output::collect_inline_output(&mut self.command_receiver)
    }

    /// Force the turn into a tool-free recovery pass before `run_turn_loop`
    /// starts, so the first request is already a recovery synthesis pass.
    pub(crate) fn activate_tool_free_recovery_for_test(&mut self, reason: &str) {
        self.harness_state
            .activate_recovery_with_mode(reason.to_string(), RecoveryMode::ToolFreeSynthesis);
    }

    /// Activate planning workflow for an integration test (mirrors what the
    /// `start_planning` tool does): enable planning on the registry and enter
    /// the planning session so `is_planning_active()` returns true.
    pub(crate) fn activate_planning_for_test(&mut self) {
        self.tool_registry.enable_planning();
        self.plan_session
            .enter(vtcode_core::core::interfaces::session::PlanningEntrySource::UserRequest);
    }

    pub(crate) fn mark_interview_denied_for_test(&mut self) {
        self.plan_session.mark_interview_denied();
    }

    /// Control the confirmation policy surfaced through `turn_processing_context`.
    /// Defaults to `true`; revision-routing tests set `false` to exercise the
    /// interactive confirmation-policy path without an overlay.
    pub(crate) fn set_skip_confirmations_for_test(&mut self, skip: bool) {
        self.skip_confirmations_for_test = skip;
    }

    /// Mark the turn as an approved-plan execution turn, mirroring what the
    /// session loop does for the scheduled implementation turn.
    pub(crate) fn set_approved_plan_execution_for_test(&mut self, active: bool) {
        self.harness_state.set_approved_plan_execution(active);
    }

    pub(crate) fn last_history_message_contains(&self, needle: &str) -> bool {
        self.working_history
            .last()
            .is_some_and(|message| message.content.as_text().contains(needle))
    }

    pub(crate) fn turn_loop_context(&mut self) -> crate::agent::runloop::unified::turn::turn_loop::TurnLoopContext<'_> {
        crate::agent::runloop::unified::turn::turn_loop::TurnLoopContext::new(
            &mut self.renderer,
            &self.handle,
            &mut self.session,
            &mut self.session_stats,
            &mut self.plan_session,
            &mut self.auto_finish_planning_attempted,
            &mut self.mcp_panel_state,
            &self.tool_result_cache,
            &self.approval_recorder,
            &self.decision_ledger,
            &mut self.tool_registry,
            &self.tools,
            &self.tool_catalog,
            &self.ctrl_c_state,
            &self.ctrl_c_notify,
            &mut self.context_manager,
            &mut self.last_forced_redraw,
            &mut self.input_status_state,
            None,
            &self.default_placeholder,
            &self.tool_permission_cache,
            &self.permissions_state,
            &self.safety_validator,
            &self.circuit_breaker,
            &self.tool_health_tracker,
            &self.rate_limiter,
            &self.telemetry,
            &self.autonomous_executor,
            &self.error_recovery,
            &mut self.harness_state,
            self.harness_emitter.as_ref(),
            &mut self.config,
            None,
            &mut self.turn_metadata_cache,
            &mut self.provider_client,
            &self.traj,
            &self.active_primary_agent,
            true,
            false,
            &mut self.runtime_steering,
        )
    }

    pub(crate) fn turn_processing_context(&mut self) -> TurnProcessingContext<'_> {
        let tool = ToolContext {
            tool_result_cache: &self.tool_result_cache,
            approval_recorder: &self.approval_recorder,
            tool_registry: &mut self.tool_registry,
            tools: &self.tools,
            tool_catalog: &self.tool_catalog,
            tool_permission_cache: &self.tool_permission_cache,
            permissions_state: &self.permissions_state,
            safety_validator: &self.safety_validator,
            circuit_breaker: &self.circuit_breaker,
            tool_health_tracker: &self.tool_health_tracker,
            rate_limiter: &self.rate_limiter,
            telemetry: &self.telemetry,
            autonomous_executor: &self.autonomous_executor,
            error_recovery: &self.error_recovery,
        };
        let llm = LLMContext {
            provider_client: &mut self.provider_client,
            config: &mut self.config,
            vt_cfg: None,
            context_manager: &mut self.context_manager,
            active_primary_agent: &self.active_primary_agent,
            decision_ledger: &self.decision_ledger,
            traj: &self.traj,
        };
        let ui = UIContext {
            renderer: &mut self.renderer,
            handle: &self.handle,
            session: &mut self.session,
            active_thread_label: "main",
            ctrl_c_state: &self.ctrl_c_state,
            ctrl_c_notify: &self.ctrl_c_notify,
            lifecycle_hooks: None,
            default_placeholder: &self.default_placeholder,
            last_forced_redraw: &mut self.last_forced_redraw,
            input_status_state: &mut self.input_status_state,
        };
        let state = TurnProcessingState {
            session_stats: &mut self.session_stats,
            plan_session: &mut self.plan_session,
            auto_finish_planning_attempted: &mut self.auto_finish_planning_attempted,
            mcp_panel_state: &mut self.mcp_panel_state,
            working_history: &mut self.working_history,
            turn_metadata_cache: &mut self.turn_metadata_cache,
            skip_confirmations: self.skip_confirmations_for_test,
            full_auto: false,
            harness_state: &mut self.harness_state,
            harness_emitter: self.harness_emitter.as_ref(),
            runtime_steering: &mut self.runtime_steering,
        };

        TurnProcessingContext::from_parts(TurnProcessingContextParts { tool, llm, ui, state })
    }
}
