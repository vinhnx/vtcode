#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use std::sync::Arc;

use serde_json::json;
use tokio::sync::Notify;
use vtcode_config::core::permissions::{AgentPermissionsConfig, PermissionDefault};
use vtcode_core::acp::PermissionGrant;
use vtcode_core::acp::permission_cache::ToolPermissionCache;
use vtcode_core::config::constants::tools;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::core::decision_tracker::DecisionTracker;
use vtcode_core::core::trajectory::TrajectoryLogger;
use vtcode_core::tools::result_cache::ToolResultCache;

use super::*;
use vtcode_commons::canonicalize;

#[tokio::test]
async fn test_run_tool_call_unknown_tool_failure() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    {
        let mut cache = permission_cache_arc.write().await;
        cache.cache_grant("test_tool".to_string(), PermissionGrant::Permanent);
    }

    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call =
        vtcode_core::llm::provider::ToolCall::function("call_1".to_string(), "test_tool".to_string(), "{}".to_string());
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
        .await
        .expect("run_tool_call must run");

    assert!(matches!(outcome.status, ToolExecutionStatus::Failure { .. }));
}

#[tokio::test]
async fn test_run_tool_call_respects_max_tool_calls_budget() {
    let mut test_ctx = TestContext::new().await;
    test_ctx.session.set_skip_confirmations(false);
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state_with(1);
    harness_state.record_tool_call(); // Exhaust the budget (1/1)
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_budget".to_string(),
        "read_file".to_string(),
        "{}".to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, None, 0, false)
        .await
        .expect("run_tool_call must run");

    println!("Outcome status: {:?}", outcome.status);

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("Policy violation"));
            assert!(error.to_string().contains("exceeded max tool calls per turn"));
        }
        other => panic!("Expected permission denial, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_run_tool_call_hook_deny_blocks_before_safety() {
    use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
    use vtcode_core::hooks::LifecycleHookEngine;

    let mut test_ctx = TestContext::new().await;
    let hook_marker = test_ctx.workspace.join("deny-hook-ran");
    let hooks_config = HooksConfig {
        lifecycle: LifecycleHooksConfig {
            pre_tool_use: vec![HookGroupConfig {
                matcher: Some("read_file".into()),
                hooks: vec![HookCommandConfig {
                    kind: Default::default(),
                    // stderr text turns exit 2 into an explicit deny decision.
                    command: format!("touch \"{}\"; echo denied >&2; exit 2", hook_marker.display()),
                    timeout_seconds: None,
                }],
            }],
            ..Default::default()
        },
    };
    let hooks = LifecycleHookEngine::new_with_session_gated(
        test_ctx.workspace.clone(),
        &hooks_config,
        vtcode_core::hooks::SessionStartTrigger::Startup,
        "test-session",
        false,
    )
    .expect("hook engine")
    .unwrap();
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_deny".to_string(),
        tools::READ_FILE.to_string(),
        json!({"path": "README.md"}).to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, Some(&hooks), true, None, 0, false)
            .await
            .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("Tool permission denied"), "got: {error}");
        }
        other => panic!("Expected hook deny failure, got: {other:?}"),
    }
    assert!(hook_marker.exists(), "hook must have run exactly once (deny path)");
}

#[tokio::test]
async fn test_run_tool_call_hook_rewrite_runs_hooks_exactly_once() {
    use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
    use vtcode_core::hooks::LifecycleHookEngine;

    let mut test_ctx = TestContext::new().await;
    // The hook rewrites the path argument and fails closed (exit 2) if it is
    // invoked a second time for the same call. A regression that re-runs the
    // hook phase after forwarding would therefore deny the call instead of
    // executing the rewritten arguments.
    let hook_invocations = test_ctx.workspace.join("rewrite-hook-invocations");
    let hook_script = format!(
        "f=\"{}\"; [ -f \"$f\" ] && exit 2; touch \"$f\"; printf '%s' '{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"updatedInput\":{{\"path\":\"HOOK_REWROTE.md\"}}}}}}'",
        hook_invocations.display()
    );
    let hooks_config = HooksConfig {
        lifecycle: LifecycleHooksConfig {
            pre_tool_use: vec![HookGroupConfig {
                matcher: Some("read_file".into()),
                hooks: vec![HookCommandConfig {
                    kind: Default::default(),
                    command: hook_script,
                    timeout_seconds: None,
                }],
            }],
            ..Default::default()
        },
    };
    let hooks = LifecycleHookEngine::new_with_session_gated(
        test_ctx.workspace.clone(),
        &hooks_config,
        vtcode_core::hooks::SessionStartTrigger::Startup,
        "test-session",
        false,
    )
    .expect("hook engine")
    .unwrap();
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_rewrite".to_string(),
        tools::READ_FILE.to_string(),
        json!({"path": "README.md"}).to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    // The rewritten path must exist so execution confirms the rewrite reached
    // the tool.
    std::fs::write(test_ctx.workspace.join("HOOK_REWROTE.md"), "rewritten").expect("write rewritten file");

    let outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, Some(&hooks), true, None, 0, false)
            .await
            .expect("run_tool_call must run");

    assert!(hook_invocations.exists(), "hook must have run at least once");
    match outcome.status {
        ToolExecutionStatus::Success { output, .. } => {
            let output_text = output.to_string();
            assert!(output_text.contains("rewritten"), "expected rewritten file content, got: {output_text}");
        }
        other => panic!("Expected rewritten call to execute, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_run_tool_call_allows_unlimited_budget_when_disabled() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tool_defs = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state_with(0);
    for _ in 0..4 {
        harness_state.record_tool_call();
    }
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tool_defs,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_unlimited".to_string(),
        "read_file".to_string(),
        "{}".to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, None, 0, false)
        .await
        .expect("run_tool_call must run");

    assert!(!matches!(
        outcome.status,
        ToolExecutionStatus::Failure { ref error }
            if error
                .to_string()
                .contains("exceeded max tool calls per turn")
    ));
}

#[tokio::test]
async fn test_run_tool_call_forwards_runtime_agent_permissions_to_routing() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let agent_permissions = AgentPermissionsConfig::new(PermissionDefault::Deny);

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );
    let vt_cfg = VTCodeConfig {
        runtime_agent_permissions: Some(agent_permissions),
        ..Default::default()
    };

    let args = serde_json::to_string(&json!({
        "action": "write",
        "path": "notes.md",
        "content": "hello"
    }))
    .expect("serialize file_operation args");
    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_active_agent_deny".to_string(),
        tools::WRITE_FILE.to_string(),
        args,
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, Some(&vt_cfg), 0, false)
            .await
            .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("Tool permission denied"));
        }
        other => panic!("Expected permission denial, got: {other:?}"),
    }
    assert!(!test_ctx.workspace.join("notes.md").exists());
}

#[tokio::test]
async fn test_run_tool_call_prevalidated_blocks_mutation_in_planning_workflow() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    {
        let mut cache = permission_cache_arc.write().await;
        cache.cache_grant(tools::APPLY_PATCH.to_string(), PermissionGrant::Permanent);
    }

    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tool_defs = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    registry.enable_planning();
    registry.planning_workflow_state().enable();
    plan_session.enter(vtcode_core::core::interfaces::session::PlanningEntrySource::UserRequest);

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tool_defs,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let payload = serde_json::to_string(&json!({
        "input": "*** Begin Patch\n*** Add File: notes.txt\n+hello planning workflow\n*** End Patch\n"
    }))
    .expect("serialize tool args");
    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_plan_patch".to_string(),
        tools::APPLY_PATCH.to_string(),
        payload,
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, true)
        .await
        .expect("run_tool_call must run");

    println!("Planning workflow guard test outcome status: {:?}", outcome.status);

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("planning workflow"));
        }
        other => panic!("Expected planning workflow failure, got: {other:?}"),
    }
    assert!(!test_ctx.workspace.join("notes.txt").exists());
    assert!(registry.is_planning_active());
    assert!(registry.planning_workflow_state().is_active());
}

#[tokio::test]
async fn test_run_tool_call_prevalidated_allows_task_tracker_in_planning_workflow() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tool_defs = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    registry.enable_planning();
    registry.planning_workflow_state().enable();
    plan_session.enter(vtcode_core::core::interfaces::session::PlanningEntrySource::UserRequest);

    let plans_dir = test_ctx.workspace.join(".vtcode").join("plans");
    std::fs::create_dir_all(&plans_dir).expect("create plans dir");
    let plan_file = plans_dir.join("tracker-test-task-tracker.md");
    std::fs::write(&plan_file, "# Tracker Test\n").expect("write plan file");
    registry.planning_workflow_state().set_plan_file(Some(plan_file)).await;

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tool_defs,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_task_tracker".to_string(),
        tools::TASK_TRACKER.to_string(),
        r#"{"action":"list"}"#.to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, true)
        .await
        .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Success { output, .. } => {
            assert!(output["status"] == "ok" || output["status"] == "empty", "unexpected status: {}", output["status"]);
        }
        other => panic!("Expected task_tracker success in planning workflow, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_run_tool_call_non_prevalidated_allows_task_tracker_in_planning_workflow_and_tracks_budget() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tool_defs = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    registry.enable_planning();
    registry.planning_workflow_state().enable();
    plan_session.enter(vtcode_core::core::interfaces::session::PlanningEntrySource::UserRequest);

    let plans_dir = test_ctx.workspace.join(".vtcode").join("plans");
    std::fs::create_dir_all(&plans_dir).expect("create plans dir");
    let plan_file = plans_dir.join("tracker-test-task-tracker-non-prevalidated.md");
    std::fs::write(&plan_file, "# Tracker Test\n").expect("write plan file");
    registry.planning_workflow_state().set_plan_file(Some(plan_file)).await;

    let mut harness_state = build_harness_state_with(2);
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tool_defs,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_task_tracker_non_prevalidated".to_string(),
        tools::TASK_TRACKER.to_string(),
        r#"{"action":"list"}"#.to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
        .await
        .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Success { output, .. } => {
            assert!(output["status"] == "ok" || output["status"] == "empty", "unexpected status: {}", output["status"]);
        }
        other => panic!("Expected task_tracker success in planning workflow, got: {other:?}"),
    }

    assert_eq!(ctx.harness_state.tool_calls, 1);
}

#[tokio::test]
async fn test_run_tool_call_invalid_preflight_does_not_consume_budget() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state_with(1);
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_invalid_preflight".to_string(),
        tools::READ_FILE.to_string(),
        r#"{"path":"/var/db/shadow"}"#.to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let first_outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, None, 0, false)
            .await
            .expect("first run_tool_call must run");

    assert!(matches!(first_outcome.status, ToolExecutionStatus::Failure { .. }));
    assert_eq!(ctx.harness_state.tool_calls, 0);

    let second_outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, None, 0, false)
            .await
            .expect("second run_tool_call must run");

    assert!(matches!(second_outcome.status, ToolExecutionStatus::Failure { .. }));
    assert_eq!(ctx.harness_state.tool_calls, 0);
}

#[tokio::test]
async fn test_run_tool_call_command_session_git_diff_uses_cache_on_repeat() {
    let mut test_ctx = TestContext::new().await;
    std::fs::create_dir_all(&test_ctx.workspace).expect("create workspace directory");
    std::fs::write(test_ctx.workspace.join("a.txt"), "same-content\n").expect("write a.txt");
    std::fs::write(test_ctx.workspace.join("b.txt"), "same-content\n").expect("write b.txt");

    let mut registry = test_ctx.registry;
    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    {
        let mut cache = permission_cache_arc.write().await;
        cache.cache_grant(tools::EXEC_COMMAND.to_string(), PermissionGrant::Permanent);
    }

    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(32)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let args = serde_json::to_string(&json!({
        "action": "run",
        "command": "git diff --no-index ./a.txt ./b.txt"
    }))
    .expect("serialize command_session args");

    let first_call = vtcode_core::llm::provider::ToolCall::function(
        "call_command_session_1".to_string(),
        tools::EXEC_COMMAND.to_string(),
        args.clone(),
    );
    let second_call = vtcode_core::llm::provider::ToolCall::function(
        "call_command_session_2".to_string(),
        tools::EXEC_COMMAND.to_string(),
        args,
    );

    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let first_outcome =
        run_tool_call(&mut ctx, &first_call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
            .await
            .expect("first command_session call must run");

    let second_outcome =
        run_tool_call(&mut ctx, &second_call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
            .await
            .expect("second command_session call must run");

    let extract_session_id = |status: &ToolExecutionStatus| -> String {
        match status {
            ToolExecutionStatus::Success { output, command_success, .. } => {
                assert!(*command_success);
                output
                    .get("session_id")
                    .or_else(|| output.get("id"))
                    .and_then(|value| value.as_str())
                    .expect("command output should include session id")
                    .to_string()
            }
            other => panic!("Expected success status, got: {other:?}"),
        }
    };

    let first_session_id = extract_session_id(&first_outcome.status);
    let second_session_id = extract_session_id(&second_outcome.status);
    assert_eq!(first_session_id, second_session_id);

    let first_output = match &first_outcome.status {
        ToolExecutionStatus::Success { output, .. } => output,
        other => panic!("expected Success, got: {other:?}"),
    };
    let second_output = match &second_outcome.status {
        ToolExecutionStatus::Success { output, .. } => output,
        other => panic!("expected Success, got: {other:?}"),
    };

    let mut first_stable = first_output.clone();
    let mut second_stable = second_output.clone();
    let first_wall_time = first_stable
        .get("wall_time")
        .and_then(|value| value.as_f64())
        .expect("first output should include wall_time");
    let second_wall_time = second_stable
        .get("wall_time")
        .and_then(|value| value.as_f64())
        .expect("second output should include wall_time");
    assert!(first_wall_time >= 0.0);
    assert!(second_wall_time >= 0.0);
    first_stable.as_object_mut().map(|object| object.remove("wall_time"));
    second_stable.as_object_mut().map(|object| object.remove("wall_time"));
    assert_eq!(first_stable, second_stable);
}

#[tokio::test]
async fn successful_apply_patch_invalidates_cached_code_search() {
    let mut test_ctx = TestContext::new().await;
    let source_dir = test_ctx.workspace.join("src");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    std::fs::write(source_dir.join("widget.rs"), "pub struct Widget;\n").expect("write search fixture");

    let mut registry = test_ctx.registry;
    registry.allow_all_tools().await.expect("allow test tools");
    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    {
        let mut cache = permission_cache_arc.write().await;
        cache.cache_grant(tools::CODE_SEARCH.to_string(), PermissionGrant::Permanent);
        cache.cache_grant(tools::APPLY_PATCH.to_string(), PermissionGrant::Permanent);
        cache.cache_grant(tools::EXEC_COMMAND.to_string(), PermissionGrant::Permanent);
    }

    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));
    let mut harness_state = build_harness_state_with(8);
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());
    let search_args = json!({
        "query": "Widget",
        "path": "src",
        "file_types": ["rust"],
        "result_types": ["text"]
    });
    let search_call = |id: &str| {
        vtcode_core::llm::provider::ToolCall::function(
            id.to_string(),
            tools::CODE_SEARCH.to_string(),
            search_args.to_string(),
        )
    };

    let first = run_tool_call(
        &mut ctx,
        &search_call("search_before_patch"),
        &ctrl_c_state,
        &ctrl_c_notify,
        None,
        None,
        true,
        None,
        0,
        false,
    )
    .await
    .expect("initial code_search should run");
    match first.status {
        ToolExecutionStatus::Success { output, .. } => {
            assert_eq!(output["returned"], json!(1));
        }
        other => panic!("expected initial search success, got: {other:?}"),
    }
    assert_eq!(result_cache.read().await.stats().current_size, 1);

    let failed_patch = vtcode_core::llm::provider::ToolCall::function(
        "failed_patch".to_string(),
        tools::APPLY_PATCH.to_string(),
        json!({"input": "*** Begin Patch\n*** Not An Operation\n*** End Patch\n"}).to_string(),
    );
    let failed =
        run_tool_call(&mut ctx, &failed_patch, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
            .await
            .expect("failed apply_patch should produce a pipeline outcome");
    assert!(matches!(failed.status, ToolExecutionStatus::Failure { .. }));
    assert_eq!(result_cache.read().await.stats().current_size, 1, "a failed patch must preserve cached reads");

    let cached = run_tool_call(
        &mut ctx,
        &search_call("search_after_failed_patch"),
        &ctrl_c_state,
        &ctrl_c_notify,
        None,
        None,
        true,
        None,
        0,
        false,
    )
    .await
    .expect("search after failed patch should run");
    assert!(matches!(
        cached.status,
        ToolExecutionStatus::Success { ref output, .. } if output["returned"] == json!(1)
    ));
    assert_eq!(result_cache.read().await.stats().hits, 1);

    let patch = "*** Begin Patch\n*** Update File: src/widget.rs\n@@\n-pub struct Widget;\n+pub struct Gadget;\n*** End Patch\n";
    let successful_patch = vtcode_core::llm::provider::ToolCall::function(
        "successful_patch".to_string(),
        tools::APPLY_PATCH.to_string(),
        json!({"input": patch}).to_string(),
    );
    let patched =
        run_tool_call(&mut ctx, &successful_patch, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
            .await
            .expect("successful apply_patch should run");
    match patched.status {
        ToolExecutionStatus::Success { modified_files, .. } => {
            assert_eq!(
                modified_files,
                vec![
                    canonicalize(source_dir.join("widget.rs"))
                        .expect("canonical widget.rs")
                        .to_string_lossy()
                ]
            );
        }
        other => panic!("expected patch success, got: {other:?}"),
    }
    assert_eq!(
        result_cache.read().await.stats().current_size,
        0,
        "successful patch metadata must invalidate the scoped search cache"
    );

    let fresh = run_tool_call(
        &mut ctx,
        &search_call("search_after_successful_patch"),
        &ctrl_c_state,
        &ctrl_c_notify,
        None,
        None,
        true,
        None,
        0,
        false,
    )
    .await
    .expect("search after successful patch should run");
    match fresh.status {
        ToolExecutionStatus::Success { output, .. } => {
            assert_eq!(output["returned"], json!(0));
        }
        other => panic!("expected fresh search success, got: {other:?}"),
    }
    assert_eq!(
        result_cache.read().await.stats().hits,
        1,
        "the post-success search must execute freshly instead of hitting stale cache"
    );

    let shell_call = vtcode_core::llm::provider::ToolCall::function(
        "successful_shell_mutation".to_string(),
        tools::EXEC_COMMAND.to_string(),
        json!({
            "action": "run",
            "command": "printf marker > shell-marker.txt"
        })
        .to_string(),
    );
    let shell = run_tool_call(&mut ctx, &shell_call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
        .await
        .expect("shell mutation should run");
    assert!(matches!(shell.status, ToolExecutionStatus::Success { .. }));
    assert_eq!(
        result_cache.read().await.stats().current_size,
        0,
        "a shell command must invalidate cached filesystem results"
    );
}

#[tokio::test]
async fn test_run_tool_call_rejects_escalated_shell_when_hitl_disabled() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.security.human_in_the_loop = false;

    let args = serde_json::to_string(&json!({
        "action": "run",
        "command": "echo hi",
        "sandbox_permissions": "require_escalated",
        "justification": "Do you want to run this command without sandbox restrictions?"
    }))
    .expect("serialize command_session args");

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_command_session_escalated".to_string(),
        tools::EXEC_COMMAND.to_string(),
        args,
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, Some(&vt_cfg), 0, false)
            .await
            .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("Tool permission denied"));
        }
        other => panic!("Expected permission denial, got: {other:?}"),
    }
    assert_eq!(ctx.harness_state.tool_calls, 0);
}

#[tokio::test]
async fn test_run_tool_call_requires_operator_preapproval_for_escalated_shell() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.security.human_in_the_loop = false;
    vt_cfg.runtime_agent_permissions = Some(AgentPermissionsConfig::new(PermissionDefault::Allow));
    vt_cfg
        .commands
        .approval_prefixes
        .push("echo hi|sandbox_permissions=\"require_escalated\"|additional_permissions=null".to_string());
    ctx.tool_registry.apply_commands_config(&vt_cfg.commands);

    let args = serde_json::to_string(&json!({
        "action": "run",
        "command": "echo hi",
        "sandbox_permissions": "require_escalated",
        "justification": "Do you want to run this command without sandbox restrictions?"
    }))
    .expect("serialize command_session args");

    let call = vtcode_core::llm::provider::ToolCall::function(
        "call_command_session_escalated_saved_prefix".to_string(),
        tools::EXEC_COMMAND.to_string(),
        args,
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome =
        run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, false, Some(&vt_cfg), 0, false)
            .await
            .expect("run_tool_call must run");

    match outcome.status {
        ToolExecutionStatus::Failure { error } => {
            assert!(error.to_string().contains("requires an enforced operator approval decision"));
        }
        other => panic!("Expected operator preapproval denial, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_run_tool_call_reuses_streamed_invocation_item_without_duplicate_start() {
    let mut test_ctx = TestContext::new().await;
    std::fs::create_dir_all(&test_ctx.workspace).expect("create workspace");
    std::fs::write(test_ctx.workspace.join("note.txt"), "hello\n").expect("write note.txt");
    let mut registry = test_ctx.registry;

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    {
        let mut cache = permission_cache_arc.write().await;
        cache.cache_grant(tools::READ_FILE.to_string(), PermissionGrant::Permanent);
    }

    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let log_dir = tempfile::TempDir::new().expect("log dir");
    let emitter = crate::agent::runloop::unified::inline_events::harness::HarnessEventEmitter::new(
        log_dir.path().join("harness.jsonl"),
    )
    .expect("harness emitter");

    let mut harness_state = build_harness_state();
    let tool_call_id = "call_streamed".to_string();
    let streamed_item_id = "streamed-tool-item".to_string();
    harness_state.remember_streamed_tool_call_items([(
        tool_call_id.clone(),
        crate::agent::runloop::unified::run_loop_context::StreamedToolCallItem {
            item_id: streamed_item_id.clone(),
            tool_name: tools::READ_FILE.to_string(),
        },
    )]);
    emitter
        .emit(crate::agent::runloop::unified::inline_events::harness::tool_started_event(
            streamed_item_id.clone(),
            tools::READ_FILE,
            Some(&json!({"path":"note.txt"})),
            Some(tool_call_id.as_str()),
        ))
        .expect("emit tool started");

    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        Some(&emitter),
    );

    let call = vtcode_core::llm::provider::ToolCall::function(
        tool_call_id.clone(),
        tools::READ_FILE.to_string(),
        r#"{"path":"note.txt"}"#.to_string(),
    );
    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, false)
        .await
        .expect("run_tool_call must run");

    assert!(matches!(outcome.status, ToolExecutionStatus::Success { .. }));
    assert!(ctx.harness_state.take_streamed_tool_call_item_id(&tool_call_id).is_none());

    let payload = std::fs::read_to_string(log_dir.path().join("harness.jsonl")).expect("read harness log");
    let mut started_count = 0usize;
    let mut completed_count = 0usize;

    for line in payload.lines() {
        let value: Value = serde_json::from_str(line).expect("json line");
        let event = value.get("event").expect("event");
        let event_type = event.get("type").and_then(|kind| kind.as_str()).unwrap_or_default();
        let item = event.get("item").expect("item");
        let item_id = item.get("id").and_then(|id| id.as_str()).unwrap_or_default();
        let item_type = item.get("type").and_then(|kind| kind.as_str()).unwrap_or_default();

        if item_id == streamed_item_id && item_type == "tool_invocation" {
            if event_type == "item.started" {
                started_count += 1;
            }
            if event_type == "item.completed" {
                completed_count += 1;
            }
        }
    }

    assert_eq!(started_count, 1);
    assert_eq!(completed_count, 1);
}

/// Regression: the interactive runloop validates (safety-admits) each tool
/// call through the shared SafetyGateway BEFORE invoking the pipeline with
/// prevalidated = true. The pipeline must not let the registry re-admit the
/// same call on that shared gateway — the double count halved the effective
/// per-turn budget (checkpoint turn_942/943: 16 admitted calls then
/// "Per-turn tool limit reached (max: 32)" with max_tool_calls_per_turn = 32).
#[tokio::test]
async fn test_prevalidated_runloop_execution_consumes_safety_budget_once() {
    let mut test_ctx = TestContext::new().await;
    let mut registry = test_ctx.registry;
    // TestContext drops its TempDir (the workspace directory is deleted), so
    // re-create it before writing the fixture.
    std::fs::create_dir_all(&test_ctx.workspace).expect("recreate workspace");
    let read_path = test_ctx.workspace.join("budget.txt");
    std::fs::write(&read_path, "hello").expect("write fixture");

    let gateway = registry.safety_gateway();
    gateway.set_limits(2, 10);
    gateway.start_turn();

    let permission_cache_arc = Arc::new(tokio::sync::RwLock::new(ToolPermissionCache::new()));
    let result_cache = Arc::new(tokio::sync::RwLock::new(ToolResultCache::new(10)));
    let decision_ledger = Arc::new(tokio::sync::RwLock::new(DecisionTracker::new()));
    let mut session_stats = crate::agent::runloop::unified::state::SessionStats::default();
    let mut plan_session =
        crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState::default();
    let mut mcp_panel = crate::agent::runloop::mcp_events::McpPanelState::new(10, true);
    let approval_recorder = test_ctx.approval_recorder;
    let traj = TrajectoryLogger::new(&test_ctx.workspace);
    let tools = Arc::new(tokio::sync::RwLock::new(Vec::new()));

    let mut harness_state = build_harness_state();
    let mut ctx = crate::agent::runloop::unified::run_loop_context::RunLoopContext::new(
        &mut test_ctx.renderer,
        &test_ctx.handle,
        &mut registry,
        &tools,
        &result_cache,
        &permission_cache_arc,
        &test_ctx.permissions_state,
        &decision_ledger,
        &mut session_stats,
        &mut plan_session,
        &mut mcp_panel,
        &approval_recorder,
        &mut test_ctx.session,
        None,
        &traj,
        &mut harness_state,
        None,
    );

    let ctrl_c_state = Arc::new(CtrlCState::new());
    let ctrl_c_notify = Arc::new(Notify::new());

    for index in 0..2 {
        let args = json!({ "path": read_path });

        // The runloop handlers safety-admit each call BEFORE the pipeline.
        let admission = gateway
            .check_and_record_with_id(
                &vtcode_core::tools::SafetyContext::new("runloop-safety-validator"),
                tools::READ_FILE,
                &args,
                Some(vtcode_core::tools::ToolInvocationId::new()),
            )
            .await;
        assert!(
            matches!(admission.decision, vtcode_core::tools::SafetyDecision::Allow),
            "admission {index} should be allowed: {:?}",
            admission.decision
        );

        let call = vtcode_core::llm::provider::ToolCall::function(
            format!("call_budget_{index}"),
            tools::READ_FILE.to_string(),
            serde_json::to_string(&args).expect("serialize args"),
        );
        let outcome = run_tool_call(&mut ctx, &call, &ctrl_c_state, &ctrl_c_notify, None, None, true, None, 0, true)
            .await
            .expect("run_tool_call must run");
        assert!(
            matches!(outcome.status, ToolExecutionStatus::Success { .. }),
            "prevalidated call {index} must succeed: {:?}",
            outcome.status
        );
    }

    // Each call consumed exactly one budget slot (the runloop admission).
    // Before the fix, the registry re-admitted every call on the shared
    // gateway, so the second admission was rejected with "Per-turn tool limit
    // reached (max: 2)" and only one call could execute.
    assert_eq!(gateway.get_stats().turn_count, 2);
}
