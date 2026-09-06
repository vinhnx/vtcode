use std::fmt::Write as _;

use super::system::{
    PLANNING_WORKFLOW_EXIT_INSTRUCTION_LINE, PLANNING_WORKFLOW_INTERVIEW_POLICY_LINE,
    PLANNING_WORKFLOW_NO_AUTO_EXIT_LINE, PLANNING_WORKFLOW_NO_REQUEST_USER_INPUT_POLICY_LINE,
    PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE, PLANNING_WORKFLOW_PLAN_POLICY_LINE,
    PLANNING_WORKFLOW_PLAN_QUALITY_LINE, PLANNING_WORKFLOW_READ_ONLY_HEADER, PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE,
    PLANNING_WORKFLOW_RESEARCH_SCOPE_LINE, PLANNING_WORKFLOW_TASK_TRACKER_LINE,
};
use crate::config::constants::tool_limits;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimePromptContract {
    pub full_auto: bool,
    pub planning_active: bool,
    pub request_user_input_enabled: bool,
}

pub fn append_runtime_mode_sections(prompt: &mut String, contract: RuntimePromptContract) {
    if contract.full_auto {
        append_full_auto_notice(prompt, contract);
    }

    if contract.planning_active {
        append_planning_workflow_notice(prompt, contract.request_user_input_enabled);
    }
}

fn append_full_auto_notice(prompt: &mut String, contract: RuntimePromptContract) {
    let header = if contract.planning_active {
        "# FULL-AUTO (PLANNING WORKFLOW): Work autonomously within planning workflow constraints."
    } else {
        "# FULL-AUTO: Complete task autonomously until done or blocked."
    };

    if prompt.contains(header) {
        return;
    }

    let _ = writeln!(prompt, "\n{header}");
    let _ = writeln!(prompt, "- Stay within the exposed tool list and adapt when a tool is unavailable or denied.");
    // Checkpoint guidance is already in the operating profiles; omit here to
    // avoid token waste from duplication.
    if !contract.request_user_input_enabled {
        let _ = writeln!(
            prompt,
            "- `request_user_input` is unavailable in this runtime; make reasonable assumptions and continue with the available context."
        );
    }
}

fn append_planning_workflow_notice(prompt: &mut String, request_user_input_enabled: bool) {
    if prompt.contains(PLANNING_WORKFLOW_READ_ONLY_HEADER) {
        if !request_user_input_enabled && !prompt.contains(PLANNING_WORKFLOW_NO_REQUEST_USER_INPUT_POLICY_LINE) {
            let _ = writeln!(prompt, "{PLANNING_WORKFLOW_NO_REQUEST_USER_INPUT_POLICY_LINE}");
        }
        return;
    }

    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_READ_ONLY_HEADER);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_EXIT_INSTRUCTION_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_PLAN_PERSISTENCE_POLICY_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_PLAN_POLICY_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_PLAN_QUALITY_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_RESEARCH_SCOPE_LINE);
    prompt.push('\n');
    let _ = writeln!(
        prompt,
        "- Planning uses a nonzero per-turn tool-call research floor of {} that is separate from max_tool_loops and max_conversation_turns.",
        tool_limits::PLANNING_WORKFLOW_MIN_TOOL_CALLS_PER_TURN,
    );
    prompt.push_str(PLANNING_WORKFLOW_INTERVIEW_POLICY_LINE);
    prompt.push('\n');
    if !request_user_input_enabled {
        prompt.push_str(PLANNING_WORKFLOW_NO_REQUEST_USER_INPUT_POLICY_LINE);
        prompt.push('\n');
    }
    prompt.push_str(PLANNING_WORKFLOW_NO_AUTO_EXIT_LINE);
    prompt.push('\n');
    prompt.push_str(PLANNING_WORKFLOW_TASK_TRACKER_LINE);
    prompt.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{RuntimePromptContract, append_runtime_mode_sections};
    use crate::prompts::system::{
        PLANNING_WORKFLOW_INTERVIEW_POLICY_LINE, PLANNING_WORKFLOW_READ_ONLY_HEADER,
        PLANNING_WORKFLOW_RESEARCH_SCOPE_LINE,
    };

    #[test]
    fn planning_workflow_uses_plan_policy_unconditionally() {
        for request_user_input_enabled in [true, false] {
            let mut prompt = "Base prompt".to_string();

            append_runtime_mode_sections(
                &mut prompt,
                RuntimePromptContract {
                    planning_active: true,
                    request_user_input_enabled,
                    ..RuntimePromptContract::default()
                },
            );

            assert!(prompt.contains(PLANNING_WORKFLOW_READ_ONLY_HEADER));
            assert!(prompt.contains(PLANNING_WORKFLOW_INTERVIEW_POLICY_LINE));
            assert!(prompt.contains("Emit exactly one final `<proposed_plan>` block"));
            assert!(
                prompt.contains("Do not use shell commands or file-writing tools to create or modify `.vtcode/plans/`")
            );
            assert!(prompt.contains("runtime owns plan/tracker persistence and validation"));
            assert!(prompt.contains("approval controls only after successful persistence"));
        }
    }

    #[test]
    fn full_auto_notice_mentions_missing_request_user_input_when_disabled() {
        let mut prompt = "Base prompt".to_string();

        append_runtime_mode_sections(
            &mut prompt,
            RuntimePromptContract {
                full_auto: true,
                request_user_input_enabled: false,
                ..RuntimePromptContract::default()
            },
        );

        assert!(prompt.contains("# FULL-AUTO: Complete task autonomously until done or blocked."));
        assert!(prompt.contains("`request_user_input` is unavailable in this runtime"));
    }

    /// Regression guard for checkpoint turn_647: a simple planning request
    /// burned 70+ tool calls until the wall-clock budget was exhausted with
    /// no plan delivered. The research-scope line must always be present
    /// while planning is active, regardless of `request_user_input`
    /// availability, so the model has a concrete signal to stop researching
    /// and draft.
    #[test]
    fn planning_workflow_always_includes_research_scope_guidance() {
        for request_user_input_enabled in [true, false] {
            let mut prompt = "Base prompt".to_string();
            append_runtime_mode_sections(
                &mut prompt,
                RuntimePromptContract {
                    planning_active: true,
                    request_user_input_enabled,
                    ..RuntimePromptContract::default()
                },
            );
            assert!(prompt.contains(PLANNING_WORKFLOW_RESEARCH_SCOPE_LINE));
            assert!(prompt.contains("Planning uses a nonzero per-turn tool-call research floor of 120"));
        }
    }
}
