//! Tool Orchestrator (from Codex)
//!
//! Central place for approvals + sandbox selection + retry semantics. Drives a
//! simple sequence for any ToolRuntime: approval → select sandbox → attempt →
//! retry without sandbox on denial (no re-approval thanks to caching).

use crate::tools::handlers::sandboxing::{
    ApprovalCtx, AskForApproval, ExecApprovalRequirement, ReviewDecision, SandboxAttempt, SandboxManager,
    SandboxOverride, SandboxType, ToolCtx, ToolError, ToolRuntime, canonical_sandbox_policy,
    default_exec_approval_requirement,
};
use crate::tools::handlers::tool_handler::TurnContext;

/// Tool orchestrator for coordinating execution (from Codex)
///
/// The orchestrator handles:
/// 1. Approval flow with caching
/// 2. Sandbox creation and selection
/// 3. Retry logic for sandbox escalation
pub struct ToolOrchestrator {
    sandbox: SandboxManager,
}

impl Default for ToolOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolOrchestrator {
    pub fn new() -> Self {
        Self { sandbox: SandboxManager::new() }
    }

    /// Run a tool with the orchestrator managing sandbox and retries (from Codex)
    ///
    /// Flow:
    /// 1. Check exec approval requirement
    /// 2. Request approval if needed
    /// 3. Select initial sandbox
    /// 4. First attempt
    /// 5. On sandbox denial, retry without sandbox (with approval if needed)
    pub async fn run<Req, Out, T>(
        &mut self,
        tool: &mut T,
        req: &Req,
        tool_ctx: &ToolCtx,
        turn_ctx: &TurnContext,
        approval_policy: AskForApproval,
    ) -> Result<Out, ToolError>
    where
        Req: Send + Sync,
        Out: Send + Sync,
        T: ToolRuntime<Req, Out>,
    {
        // 1) Determine approval requirement
        let mut already_approved = false;

        let requirement = tool
            .exec_approval_requirement(req)
            .unwrap_or_else(|| default_exec_approval_requirement(approval_policy, turn_ctx.sandbox_policy.get()));

        match &requirement {
            ExecApprovalRequirement::Skip { .. } => {
                // No approval needed, continue
                tracing::debug!("Skipping approval for tool: {}", tool_ctx.tool_name);
            }
            ExecApprovalRequirement::Forbidden { reason } => {
                return Err(ToolError::Rejected(reason.clone()));
            }
            ExecApprovalRequirement::NeedsApproval { reason, .. } => {
                // Request approval
                let approval_ctx = ApprovalCtx {
                    session: tool_ctx.session.as_ref(),
                    turn: tool_ctx.turn.as_ref(),
                    call_id: &tool_ctx.call_id,
                    retry_reason: reason.clone(),
                };

                let decision = tool.start_approval_async(req, approval_ctx).await;

                match decision {
                    ReviewDecision::Denied | ReviewDecision::Abort => {
                        return Err(ToolError::Rejected("rejected by user".to_string()));
                    }
                    ReviewDecision::Approved
                    | ReviewDecision::ApprovedExecpolicyAmendment { .. }
                    | ReviewDecision::ApprovedForSession => {
                        already_approved = true;
                    }
                }
            }
        }

        // 2) Select initial sandbox
        let canonical_policy = canonical_sandbox_policy(turn_ctx);
        let initial_sandbox = match tool.sandbox_policy_override_for_first_attempt(req) {
            SandboxOverride::BypassSandboxFirstAttempt => SandboxType::None,
            SandboxOverride::NoOverride => self
                .sandbox
                .select_initial_for_canonical(&canonical_policy, tool.sandbox_preference()),
        };

        // 3) First attempt
        let initial_attempt = SandboxAttempt {
            sandbox: initial_sandbox,
            policy: turn_ctx.sandbox_policy.get(),
            sandbox_cwd: &turn_ctx.cwd,
            codex_linux_sandbox_exe: turn_ctx.codex_linux_sandbox_exe.as_ref(),
        };

        match tool.run(req, &initial_attempt, tool_ctx).await {
            Ok(out) => Ok(out),
            Err(ToolError::SandboxDenied(output)) => {
                // 4) Handle sandbox denial
                if !tool.escalate_on_failure() {
                    return Err(ToolError::SandboxDenied(output));
                }

                // Under `Never` or `OnRequest`, do not retry without sandbox
                if !tool.wants_no_sandbox_approval(approval_policy) {
                    return Err(ToolError::SandboxDenied(build_denial_reason_from_output(Some(&output))));
                }

                // Ask for approval before retrying without sandbox
                if !tool.should_bypass_approval(approval_policy, already_approved) {
                    let reason_msg = build_denial_reason_from_output(Some(&output));
                    let approval_ctx = ApprovalCtx {
                        session: tool_ctx.session.as_ref(),
                        turn: tool_ctx.turn.as_ref(),
                        call_id: &tool_ctx.call_id,
                        retry_reason: Some(reason_msg),
                    };

                    let decision = tool.start_approval_async(req, approval_ctx).await;

                    match decision {
                        ReviewDecision::Denied | ReviewDecision::Abort => {
                            return Err(ToolError::Rejected("rejected by user".to_string()));
                        }
                        ReviewDecision::Approved
                        | ReviewDecision::ApprovedExecpolicyAmendment { .. }
                        | ReviewDecision::ApprovedForSession => {}
                    }
                }

                // 5) Second attempt without sandbox
                let escalated_attempt = SandboxAttempt {
                    sandbox: SandboxType::None,
                    policy: turn_ctx.sandbox_policy.get(),
                    sandbox_cwd: &turn_ctx.cwd,
                    codex_linux_sandbox_exe: None,
                };

                tool.run(req, &escalated_attempt, tool_ctx).await
            }
            Err(other) => Err(other),
        }
    }
}

/// Build a denial reason message from output (from Codex)
fn build_denial_reason_from_output(output: Option<&str>) -> String {
    match output {
        Some(o) if !o.is_empty() => format!("Sandbox denied with output: {}", truncate_output(o)),
        _ => "Sandbox denied execution".to_string(),
    }
}

/// Truncate output for display
fn truncate_output(output: &str) -> &str {
    const MAX_LEN: usize = 500;
    if output.len() <= MAX_LEN {
        return output;
    }
    // Round down to the nearest UTF-8 char boundary so non-ASCII output
    // (e.g. command stderr) never panics on a mid-codepoint byte slice.
    let mut end = MAX_LEN;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    &output[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::tools::handlers::sandboxing::{
        Approvable, BoxFuture, NetworkAccess, SandboxConfig, SandboxPolicy, Sandboxable, SandboxablePreference,
    };
    use crate::tools::handlers::tool_handler::{ApprovalPolicy, Constrained, ShellEnvironmentPolicy, ToolSession};

    struct TestSession {
        cwd: PathBuf,
    }

    impl TestSession {
        fn new(cwd: PathBuf) -> Self {
            Self { cwd }
        }
    }

    #[async_trait]
    impl ToolSession for TestSession {
        fn cwd(&self) -> &PathBuf {
            &self.cwd
        }

        fn workspace_root(&self) -> &PathBuf {
            &self.cwd
        }

        async fn record_warning(&self, _message: String) {}

        fn user_shell(&self) -> &str {
            "/bin/zsh"
        }
    }

    struct TestRuntime {
        calls: usize,
        escalate: bool,
    }

    impl TestRuntime {
        fn new(escalate: bool) -> Self {
            Self { calls: 0, escalate }
        }
    }

    impl Sandboxable for TestRuntime {
        fn sandbox_preference(&self) -> SandboxablePreference {
            SandboxablePreference::Auto
        }

        fn escalate_on_failure(&self) -> bool {
            self.escalate
        }
    }

    impl Approvable<()> for TestRuntime {
        type ApprovalKey = String;

        fn approval_key(&self, _req: &()) -> Self::ApprovalKey {
            "test-runtime".to_string()
        }

        fn start_approval_async<'a>(
            &'a mut self,
            _req: &'a (),
            _ctx: ApprovalCtx<'a>,
        ) -> BoxFuture<'a, ReviewDecision> {
            Box::pin(async { ReviewDecision::Approved })
        }
    }

    #[async_trait]
    impl ToolRuntime<(), &'static str> for TestRuntime {
        async fn run(
            &mut self,
            _req: &(),
            attempt: &SandboxAttempt<'_>,
            _ctx: &ToolCtx,
        ) -> Result<&'static str, ToolError> {
            self.calls += 1;
            if attempt.sandbox == SandboxType::None {
                Ok("ok")
            } else {
                Err(ToolError::SandboxDenied("denied".to_string()))
            }
        }
    }

    struct FirstAttemptProbeRuntime {
        first_sandbox: Option<SandboxType>,
        preference: SandboxablePreference,
    }

    impl FirstAttemptProbeRuntime {
        fn new(preference: SandboxablePreference) -> Self {
            Self { first_sandbox: None, preference }
        }
    }

    impl Sandboxable for FirstAttemptProbeRuntime {
        fn sandbox_preference(&self) -> SandboxablePreference {
            self.preference
        }
    }

    impl Approvable<()> for FirstAttemptProbeRuntime {
        type ApprovalKey = String;

        fn approval_key(&self, _req: &()) -> Self::ApprovalKey {
            "probe-runtime".to_string()
        }

        fn start_approval_async<'a>(
            &'a mut self,
            _req: &'a (),
            _ctx: ApprovalCtx<'a>,
        ) -> BoxFuture<'a, ReviewDecision> {
            Box::pin(async { ReviewDecision::Approved })
        }
    }

    #[async_trait]
    impl ToolRuntime<(), &'static str> for FirstAttemptProbeRuntime {
        async fn run(
            &mut self,
            _req: &(),
            attempt: &SandboxAttempt<'_>,
            _ctx: &ToolCtx,
        ) -> Result<&'static str, ToolError> {
            if self.first_sandbox.is_none() {
                self.first_sandbox = Some(attempt.sandbox);
            }
            Ok("ok")
        }
    }

    fn test_turn_context(cwd: PathBuf, sandbox_policy: SandboxConfig) -> TurnContext {
        TurnContext {
            cwd,
            turn_id: "turn-1".to_string(),
            sub_id: None,
            shell_environment_policy: ShellEnvironmentPolicy::Inherit,
            approval_policy: Constrained::allow_any(ApprovalPolicy::Never),
            codex_linux_sandbox_exe: None,
            sandbox_policy: Constrained::allow_any(sandbox_policy),
        }
    }

    fn test_tool_ctx(turn: Arc<TurnContext>, session: Arc<TestSession>) -> ToolCtx {
        ToolCtx {
            session,
            turn,
            call_id: "call-1".to_string(),
            tool_name: "test".to_string(),
        }
    }

    #[test]
    fn test_build_denial_reason_empty() {
        assert_eq!(build_denial_reason_from_output(None), "Sandbox denied execution");
        assert_eq!(build_denial_reason_from_output(Some("")), "Sandbox denied execution");
    }

    #[test]
    fn test_build_denial_reason_with_output() {
        let reason = build_denial_reason_from_output(Some("permission denied"));
        assert!(reason.contains("permission denied"));
    }

    #[test]
    fn test_truncate_output() {
        let short = "hello";
        assert_eq!(truncate_output(short), short);

        let long = "a".repeat(1000);
        assert_eq!(truncate_output(&long).len(), 500);

        // Multi-byte chars must not panic and must stay within the byte budget.
        let multibyte = "あ".repeat(1000); // 3 bytes each
        let truncated = truncate_output(&multibyte);
        assert!(truncated.len() <= 500);
        assert!(multibyte.starts_with(truncated));
    }

    #[tokio::test]
    async fn orchestrator_escalates_when_runtime_allows_it() {
        let cwd = PathBuf::from(".");
        let session = Arc::new(TestSession::new(cwd.clone()));
        let turn = Arc::new(test_turn_context(cwd, SandboxConfig::default()));
        let tool_ctx = test_tool_ctx(turn.clone(), session);
        let mut runtime = TestRuntime::new(true);
        let mut orchestrator = ToolOrchestrator::new();

        let out = orchestrator
            .run(&mut runtime, &(), &tool_ctx, turn.as_ref(), AskForApproval::OnFailure)
            .await
            .expect("expected escalated retry to succeed");

        assert_eq!(out, "ok");
        assert_eq!(runtime.calls, 2);
    }

    #[tokio::test]
    async fn orchestrator_stops_on_sandbox_denial_when_runtime_disables_retry() {
        let cwd = PathBuf::from(".");
        let session = Arc::new(TestSession::new(cwd.clone()));
        let turn = Arc::new(test_turn_context(cwd, SandboxConfig::default()));
        let tool_ctx = test_tool_ctx(turn.clone(), session);
        let mut runtime = TestRuntime::new(false);
        let mut orchestrator = ToolOrchestrator::new();

        let err = orchestrator
            .run(&mut runtime, &(), &tool_ctx, turn.as_ref(), AskForApproval::OnFailure)
            .await
            .expect_err("expected sandbox denial without retry");

        assert!(matches!(err, ToolError::SandboxDenied(_)));
        assert_eq!(runtime.calls, 1);
    }

    #[tokio::test]
    async fn canonical_full_access_turn_starts_without_sandbox() {
        let cwd = PathBuf::from(".");
        let session = Arc::new(TestSession::new(cwd.clone()));
        let turn = Arc::new(test_turn_context(
            cwd,
            SandboxConfig {
                policy: SandboxPolicy::DangerFullAccess,
                network_access: NetworkAccess::Full,
            },
        ));
        let tool_ctx = test_tool_ctx(turn.clone(), session);
        let mut runtime = FirstAttemptProbeRuntime::new(SandboxablePreference::Auto);
        let mut orchestrator = ToolOrchestrator::new();

        orchestrator
            .run(&mut runtime, &(), &tool_ctx, turn.as_ref(), AskForApproval::Never)
            .await
            .expect("expected run to succeed");

        assert_eq!(runtime.first_sandbox, Some(SandboxType::None));
    }
}
