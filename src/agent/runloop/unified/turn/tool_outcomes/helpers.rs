use std::path::{Path, PathBuf};
use std::time::Instant;

use rustc_hash::{FxHashMap, FxHashSet};
use vtcode_core::llm::provider as uni;
use vtcode_core::tools::names::canonical_tool_name;
use vtcode_core::tools::tool_intent::{
    ShellActivity, classify_shell_activity, shell_command_is_admitted_verification_attempt,
};

use crate::agent::runloop::unified::tool_pipeline::{ToolExecutionStatus, ToolPipelineOutcome};
use crate::agent::runloop::unified::turn::tool_outcomes::read_extent;
use crate::agent::runloop::unified::turn::tool_outcomes::{is_grep_style_no_match, output_field_is_empty};

/// Threshold: number of consecutive file mutations before the Anti-Blind-Editing
/// warning fires. NL2Repo-Bench recommends verifying after every few edits.
pub(crate) const BLIND_EDITING_THRESHOLD: usize = 4;
pub(crate) const ANTI_BLIND_EDITING_WARNING: &str = "[!] Anti-Blind-Editing: Pause to run verification/tests.";
pub(crate) const ANTI_BLIND_EDITING_DIRECTIVE: &str = "CRITICAL: Multiple edits were made without verification. Stop editing and run `exec_command` to compile or test before proceeding.";
/// Fix-up window granted after a failed verification attempt. A failed
/// `cargo check` / `cargo nextest run` must not deadlock the turn: the agent
/// needs a bounded number of edits to address the reported failure before
/// re-verifying. Each failed verifier refreshes this window, so blind editing
/// (many edits with no verifier attempt) stays blocked while fix-verify loops
/// can make progress.
pub(crate) const FAILED_VERIFICATION_FIX_ALLOWANCE: u8 = 2;
/// Warning rendered when a pending gate's verifier result was lost (the exec
/// session ended before the verifier's output was captured).
pub(crate) const VERIFICATION_RESULT_LOST_WARNING: &str =
    "[!] Verification result lost: the exec session ended before the verifier's output was captured.";
/// Model-facing directive paired with [`VERIFICATION_RESULT_LOST_WARNING`]:
/// a standalone verifier re-run is the only way to clear the pending gate.
pub(crate) const VERIFICATION_RESULT_LOST_DIRECTIVE: &str = "Verification result lost: the exec session ended before the verifier's output was captured. Re-run the verification command standalone (no pipes/truncation) to confirm or reject the recent edits.";
/// Warning rendered while the failed-verifier fix-up window is active. Distinct
/// from [`ANTI_BLIND_EDITING_WARNING`] so the pending-verification block notice
/// does not imply verification was never run when the verifier already failed.
pub(crate) const FAILED_VERIFICATION_FIX_WARNING: &str =
    "[!] Verification failed: bounded fix edits granted before the gate re-arms.";
/// Model-facing directive paired with [`FAILED_VERIFICATION_FIX_WARNING`]:
/// the verifier ran and reported failure, so text responses must repair the
/// reported failure and re-run a standalone verifier instead of claiming
/// completion.
pub(crate) const FAILED_VERIFICATION_FIX_DIRECTIVE: &str = "The last verification command ran and FAILED. A bounded fix window is active: apply fixes for the reported failure, then re-run the standalone verification command (no pipes/truncation). Verification success is still required before the work can be accepted.";

/// Threshold: number of consecutive read/search operations before the Navigation
/// Loop warning fires.
pub(crate) const NAVIGATION_LOOP_THRESHOLD: usize = 15;

/// Planning recovery thresholds for low-signal navigation. These are kept
/// below the hard planning tool-call ceiling so the model gets one bounded,
/// tool-free synthesis pass while the evidence is still useful.
pub(crate) const PLANNING_CONSECUTIVE_LOW_SIGNAL_THRESHOLD: u8 = 6;
pub(crate) const PLANNING_TOTAL_LOW_SIGNAL_THRESHOLD: u8 = 10;

/// Optimized loop detection with bounded signature keys and exponential backoff.
pub(crate) struct LoopTracker {
    attempts: FxHashMap<String, (usize, Instant)>,
    low_signal_attempts: FxHashMap<String, (usize, Instant)>,
    coarse_inspection_attempts: FxHashMap<String, (usize, Instant)>,
    /// Counter for consecutive mutating file operations without execution/verification
    pub consecutive_mutations: usize,
    /// True after the mutation threshold until a verification command completes.
    pub verification_pending: bool,
    /// Bounded fix-up edits allowed while verification stays pending.
    /// Set to [`FAILED_VERIFICATION_FIX_ALLOWANCE`] after a failed verifier so
    /// a broken build can be repaired; consumed by successful fix-up mutations.
    /// Persisted in `SessionStats` so `continue` turns keep the same window.
    pub fix_edits_remaining: u8,
    /// Prevent repeated warning output while verification remains pending.
    pub verification_warning_emitted: bool,
    /// Prevent repeated inline block notices for a single verification checkpoint.
    pub verification_block_notice_emitted: bool,
    /// Set when a pending gate's verifier result was lost (verifier-level
    /// Failure/Timeout, or a lost exec session). Consumed once by the
    /// tool-outcome handlers so the lost-result directive is surfaced after
    /// the tool response lands.
    pub verification_result_lost_notice_pending: bool,
    /// Counter for consecutive read/search operations without action or synthesis
    pub consecutive_navigations: usize,
    /// Number of times navigation-loop recovery has fired in this session.
    pub navigation_loop_recoveries: usize,
    /// Consecutive low-signal navigation outcomes in this turn.
    pub consecutive_low_signal_navigations: u8,
    /// Total low-signal navigation outcomes in this turn.
    pub total_low_signal_navigations: u8,
    /// Lifetime low-signal outcomes for checkpoint diagnostics. Unlike the
    /// adaptive window counters, this never resets within the turn.
    pub low_signal_tool_calls: u32,
    /// At most one adaptive planning synthesis pass is scheduled per turn.
    pub planning_low_signal_synthesis_triggered: bool,
    /// Unique navigation signatures in the current consecutive window.
    /// Used to distinguish legitimate exploration (all unique) from actual looping (many repeats).
    nav_signatures: FxHashSet<String>,
}

impl LoopTracker {
    pub(crate) fn new() -> Self {
        Self {
            attempts: FxHashMap::with_capacity_and_hasher(16, Default::default()),
            low_signal_attempts: FxHashMap::with_capacity_and_hasher(8, Default::default()),
            coarse_inspection_attempts: FxHashMap::with_capacity_and_hasher(8, Default::default()),
            consecutive_mutations: 0,
            verification_pending: false,
            fix_edits_remaining: 0,
            verification_warning_emitted: false,
            verification_block_notice_emitted: false,
            verification_result_lost_notice_pending: false,
            consecutive_navigations: 0,
            navigation_loop_recoveries: 0,
            consecutive_low_signal_navigations: 0,
            total_low_signal_navigations: 0,
            low_signal_tool_calls: 0,
            planning_low_signal_synthesis_triggered: false,
            nav_signatures: FxHashSet::default(),
        }
    }

    /// Tuple counterpart to `SessionStats::verification_snapshot`, so turn
    /// setup and persistence share one call shape instead of threading two
    /// loosely-coupled halves across five call sites. A zero-pending snapshot
    /// never carries fix-ups; the clamp keeps a stale caller from building an
    /// inconsistent gate.
    pub(crate) fn with_verification_snapshot(snapshot: (bool, u8)) -> Self {
        let mut tracker = Self::new();
        tracker.verification_pending = snapshot.0;
        tracker.fix_edits_remaining = if snapshot.0 { snapshot.1 } else { 0 };
        tracker
    }

    /// Record an attempt and return the count
    pub(crate) fn record(&mut self, signature: String) -> usize {
        let entry = self.attempts.entry(signature).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
        entry.0
    }

    fn record_low_signal(&mut self, signature: String) -> usize {
        let entry = self.low_signal_attempts.entry(signature).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.1 = Instant::now();
        entry.0
    }

    /// Get the maximum repetition count, optionally filtering by a predicate on the signature
    pub(crate) fn max_count_filtered<F>(&self, exclude: F) -> usize
    where
        F: Fn(&str) -> bool,
    {
        self.attempts
            .iter()
            .filter_map(|(sig, (count, _))| if exclude(sig) { None } else { Some(*count) })
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn max_low_signal_count(&self) -> usize {
        self.low_signal_attempts.values().map(|(count, _)| *count).max().unwrap_or(0)
    }

    /// Number of redundant navigations (total - unique) in the current window.
    /// At least 3 before the navigation loop guard considers firing.
    pub(crate) fn repeated_navigation_count(&self) -> usize {
        self.consecutive_navigations.saturating_sub(self.nav_signatures.len())
    }

    fn reset_low_signal_attempts(&mut self) {
        self.low_signal_attempts.clear();
        self.coarse_inspection_attempts.clear();
    }

    fn reset_low_signal_navigation_counters(&mut self) {
        self.consecutive_low_signal_navigations = 0;
        self.total_low_signal_navigations = 0;
    }

    /// Clear the per-turn navigation window after a non-navigation tool.
    /// Callers pass `low_signal_family.is_none()` so diverse productive reads
    /// keep their repetition history while low-signal churn resets.
    fn reset_navigation_window(&mut self, clear_low_signal_attempts: bool) {
        self.consecutive_navigations = 0;
        self.nav_signatures.clear();
        self.reset_low_signal_navigation_counters();
        if clear_low_signal_attempts {
            self.reset_low_signal_attempts();
        }
    }

    fn record_navigation_signal(&mut self, is_low_signal: bool) {
        if is_low_signal {
            self.low_signal_tool_calls = self.low_signal_tool_calls.saturating_add(1);
            self.consecutive_low_signal_navigations = self.consecutive_low_signal_navigations.saturating_add(1);
            self.total_low_signal_navigations = self.total_low_signal_navigations.saturating_add(1);
        } else {
            // Productive inspection breaks only the consecutive streak. The
            // total remains turn-scoped so diverse empty searches still
            // converge on synthesis.
            self.consecutive_low_signal_navigations = 0;
        }
    }

    pub(crate) fn reset_after_balancer_recovery(&mut self) {
        self.attempts.clear();
        self.reset_low_signal_attempts();
        self.nav_signatures.clear();
        self.consecutive_mutations = 0;
        // The mutation history is wiped, so a still-pending gate would demand
        // verification for edits the tracker can no longer attribute — and
        // with the fix window intact it stays empty. Resetting only the
        // counters while keeping the gate left the turn deadlocked; clear the
        // gate together with the history it was derived from.
        self.verification_pending = false;
        self.fix_edits_remaining = 0;
        self.verification_block_notice_emitted = false;
        self.verification_result_lost_notice_pending = false;
        self.consecutive_navigations = 0;
        self.reset_low_signal_navigation_counters();
    }

    pub(crate) fn verification_is_pending(&self) -> bool {
        self.verification_pending || self.consecutive_mutations >= BLIND_EDITING_THRESHOLD
    }

    /// Snapshot the session-persisted gate state for `SessionStats`.
    /// Persist both halves together so resumed turns reconstruct the same
    /// gate instead of drifting (a pending gate with a lost fix window
    /// deadlocks a broken build).
    pub(crate) fn verification_snapshot(&self) -> (bool, u8) {
        (self.verification_is_pending(), self.fix_edits_remaining)
    }

    pub(crate) fn mark_verification_pending(&mut self) {
        self.verification_pending = true;
    }

    /// One-shot accessor for the lost-verification-result notice queued by
    /// [`update_repetition_tracker`]. Handlers consume it after the tool
    /// response lands so the directive never splits an assistant batch.
    pub(crate) fn take_verification_result_lost_notice(&mut self) -> bool {
        std::mem::take(&mut self.verification_result_lost_notice_pending)
    }

    /// Grant a bounded fix-up window after a failed verifier. The gate stays
    /// pending (completion still requires a successful standalone verifier),
    /// but the next [`FAILED_VERIFICATION_FIX_ALLOWANCE`] successful mutations
    /// are admitted so a broken build can be repaired instead of deadlocking.
    pub(crate) fn record_failed_verification(&mut self) {
        self.verification_pending = true;
        self.fix_edits_remaining = FAILED_VERIFICATION_FIX_ALLOWANCE;
    }

    fn record_successful_mutation(&mut self) {
        // Consume the fix-up window first: repair edits must not grow the
        // blind-editing counter while the gate already requires re-verify.
        if self.verification_pending && self.fix_edits_remaining > 0 {
            self.fix_edits_remaining = self.fix_edits_remaining.saturating_sub(1);
            return;
        }
        self.consecutive_mutations = self.consecutive_mutations.saturating_add(1);
        if self.consecutive_mutations >= BLIND_EDITING_THRESHOLD {
            self.verification_pending = true;
        }
    }

    fn mark_verification_complete(&mut self) {
        self.consecutive_mutations = 0;
        self.verification_pending = false;
        self.fix_edits_remaining = 0;
        self.verification_warning_emitted = false;
        self.verification_block_notice_emitted = false;
    }
}

/// Check if an identical tool call (same name + same args) was already executed
/// recently in the working history. Returns the output of the most recent
/// matching tool response if found.
///
/// This catches cross-turn duplicates that the per-turn `LoopTracker` misses
/// because it is reset at the start of each turn. Scans the last
/// `MAX_HISTORY_SCAN` messages to keep the check bounded.
///
/// File-read pagination is normalised so that re-reading the same file with a
/// different `offset` or `limit` is recognised as the same logical read.
/// `code_search` uses a separate replay identity that retains the effective
/// `max_results`; its loop identity is separate.
///
/// Tool-call IDs are scoped to the nearest preceding Assistant batch. A later
/// batch may reuse an ID for another tool, so both the batch and tool name must
/// match before its Tool response can satisfy this replay lookup.
pub(crate) fn find_duplicate_in_history(
    history: &[uni::Message],
    tool_name: &str,
    args: &serde_json::Value,
    workspace_root: &Path,
) -> Option<String> {
    const MAX_HISTORY_SCAN: usize = 120;
    let target_signature = read_normalized_signature_key(tool_name, args);

    let scan_start = history.len().saturating_sub(MAX_HISTORY_SCAN);
    let target_tool_name = canonical_tool_name(tool_name);
    let mut current_batch: FxHashMap<String, (String, serde_json::Value)> = FxHashMap::default();
    let mut matching_responses = Vec::new();

    for (offset, msg) in history[scan_start..].iter().enumerate() {
        let abs_idx = scan_start + offset;
        match msg.role {
            uni::MessageRole::Assistant => {
                current_batch.clear();
                if let Some(ref tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        if let Some(ref func) = tc.function {
                            let tc_args: serde_json::Value = serde_json::from_str(&func.arguments)
                                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                            current_batch.insert(tc.id.clone(), (canonical_tool_name(&func.name).to_string(), tc_args));
                        }
                    }
                }
            }
            uni::MessageRole::Tool => {
                let Some(call_id) = msg.tool_call_id.as_deref() else {
                    continue;
                };
                let Some((batch_tool_name, tc_args)) = current_batch.get(call_id) else {
                    continue;
                };
                if batch_tool_name == target_tool_name
                    && read_normalized_signature_key(batch_tool_name, tc_args) == target_signature
                    && read_extent::extent_covers(tc_args, args)
                    && tool_response_is_replayable(msg)
                {
                    matching_responses.push((abs_idx, tc_args.clone(), msg));
                }
            }
            _ => {}
        }
    }

    for (response_index, tc_args, msg) in matching_responses.into_iter().rev() {
        let invalidated = tool_name == vtcode_core::config::constants::tools::CODE_SEARCH
            && history_has_scoped_mutation_after(history, response_index, &tc_args, workspace_root);
        if !invalidated {
            return Some(msg.content.as_text().to_string());
        }
    }
    None
}

fn tool_response_is_replayable(message: &uni::Message) -> bool {
    let content = message.content.as_text();
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.len() > 128 * 1024 {
        return false;
    }

    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(output)) => {
            if output.contains_key("error") || output.contains_key("error_type") || output.contains_key("failure_kind")
            {
                return false;
            }
            if output.get("blocked").and_then(serde_json::Value::as_bool) == Some(true)
                || output.get("verification_required").and_then(serde_json::Value::as_bool) == Some(true)
            {
                return false;
            }
            if matches!(output.get("success"), Some(serde_json::Value::Bool(false)) | Some(serde_json::Value::Null)) {
                return false;
            }
            if output.get("success").is_some_and(|value| !value.is_boolean()) {
                return false;
            }
            !output.get("status").and_then(serde_json::Value::as_str).is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "failed"
                        | "failure"
                        | "error"
                        | "denied"
                        | "permission_denied"
                        | "rejected"
                        | "timeout"
                        | "timed_out"
                        | "cancelled"
                        | "canceled"
                        | "interrupted"
                        | "aborted"
                        | "blocked"
                        | "skipped"
                        | "not_started"
                        | "not_executed"
                        | "pending"
                        | "in_progress"
                        | "not_run"
                )
            })
        }
        Ok(serde_json::Value::String(value)) => text_response_is_replayable(&value),
        Ok(serde_json::Value::Array(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)) => true,
        Ok(serde_json::Value::Null) => false,
        Err(_) => text_response_is_replayable(trimmed),
    }
}

fn text_response_is_replayable(content: &str) -> bool {
    let trimmed = content.trim();
    const FAILURE_PREFIXES: &[&str] = &[
        "error:",
        "execution denied",
        "permission denied",
        "timeout",
        "timed out",
        "cancelled",
        "canceled",
        "failed",
        "failure",
        "denied",
        "rejected",
        "blocked",
        "aborted",
        "interrupted",
        "skipped",
        "not started",
        "not executed",
        "not run",
        "pending",
        "in progress",
    ];
    !FAILURE_PREFIXES.iter().any(|prefix| {
        trimmed
            .get(..prefix.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
    })
}

fn history_has_scoped_mutation_after(
    history: &[uni::Message],
    response_index: usize,
    search_args: &serde_json::Value,
    workspace_root: &Path,
) -> bool {
    let mut pending_mutations: FxHashMap<String, Vec<PathBuf>> = FxHashMap::default();
    for message in history.iter().skip(response_index.saturating_add(1)) {
        match message.role {
            uni::MessageRole::Assistant => {
                // Tool-call IDs are scoped to one Assistant batch and may be
                // reused later. Unanswered calls from an earlier batch were
                // never executed, so they must not survive this boundary.
                pending_mutations.clear();
                let Some(tool_calls) = message.tool_calls.as_ref() else {
                    continue;
                };
                for tool_call in tool_calls {
                    let Some(function) = tool_call.function.as_ref() else {
                        continue;
                    };
                    let Ok(args) = serde_json::from_str::<serde_json::Value>(&function.arguments) else {
                        continue;
                    };
                    if !vtcode_core::tools::tool_intent::classify_tool_intent(&function.name, &args).mutating {
                        continue;
                    }
                    let paths = vtcode_core::tools::mutation_target_paths(&function.name, &args);
                    if !paths.is_empty() {
                        pending_mutations.insert(tool_call.id.clone(), paths);
                    }
                }
            }
            uni::MessageRole::Tool => {
                let Some(call_id) = message.tool_call_id.as_deref() else {
                    continue;
                };
                let Some(paths) = pending_mutations.remove(call_id) else {
                    continue;
                };
                if tool_response_is_success(message)
                    && paths.iter().any(|path| {
                        vtcode_core::tools::code_search_scope_contains_mutated_path(search_args, path, workspace_root)
                    })
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn tool_response_is_success(message: &uni::Message) -> bool {
    let Ok(output) = serde_json::from_str::<serde_json::Value>(&message.content.as_text()) else {
        return false;
    };
    let Some(output) = output.as_object() else {
        return false;
    };
    if output.contains_key("error") || output.contains_key("error_type") || output.contains_key("failure_kind") {
        return false;
    }
    if output.get("status").is_some_and(|status| status.as_str() != Some("success")) {
        return false;
    }

    match output.get("success") {
        Some(serde_json::Value::Bool(success)) => *success,
        Some(_) => false,
        None => output
            .get("status")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|status| status == "success"),
    }
}

fn output_has_empty_search_results(output: &serde_json::Value) -> bool {
    output
        .get("results")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|results| results.is_empty())
        && !output_has_actionable_recovery_guidance(output)
        && !output_has_error_signal(output)
}

fn output_has_actionable_recovery_guidance(output: &serde_json::Value) -> bool {
    ["hint", "next_action", "critical_note", "warning"].iter().any(|key| {
        output
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    }) || output
        .get("fallback_tool")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        || output.get("hints").and_then(serde_json::Value::as_array).is_some_and(|hints| {
            hints
                .iter()
                .any(|hint| hint.as_str().is_some_and(|value| !value.trim().is_empty()))
        })
}

fn output_has_error_signal(output: &serde_json::Value) -> bool {
    ["error", "error_type", "stderr", "stderr_preview", "message"]
        .iter()
        .any(|key| !output_field_is_empty(output.get(*key)))
}

fn output_reuses_recent_result(output: &serde_json::Value) -> bool {
    [
        "loop_detected",
        "reused_recent_result",
        "spool_ref_only",
        "result_ref_only",
    ]
    .iter()
    .any(|key| output.get(*key).and_then(serde_json::Value::as_bool) == Some(true))
}

fn error_is_missing_resource(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "not found",
        "no such file",
        "resource not found",
        "spool file not found",
        "session output file not found",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Detect the exec-session-loss error text emitted by the exec session
/// manager ("exec session '<id>' not found. ..."), mirroring the phrasing in
/// `vtcode-core` exec_session tool errors.
fn error_text_indicates_lost_exec_session(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("exec session") && lower.contains("not found")
}

fn is_low_signal_outcome(outcome: &ToolPipelineOutcome, canonical_tool_name: &str, args: &serde_json::Value) -> bool {
    match &outcome.status {
        ToolExecutionStatus::Success { output, command_success, .. } => {
            output_has_empty_search_results(output)
                || output_reuses_recent_result(output)
                || (matches!(
                    canonical_tool_name,
                    vtcode_core::config::constants::tools::UNIFIED_EXEC
                        | vtcode_core::config::constants::tools::EXEC_COMMAND
                ) && !*command_success
                    && is_grep_style_no_match(canonical_tool_name, args, output))
        }
        ToolExecutionStatus::Failure { error } => error_is_missing_resource(&error.message),
        ToolExecutionStatus::Timeout { .. } | ToolExecutionStatus::Cancelled => false,
    }
}

/// Coarse inspection family for duplicate-listing detection. Unlike the exact
/// `low_signal_family_key` (full normalized command), this groups overlapping
/// scans such as three `find` invocations with different paths so successful
/// but redundant exploration still counts toward diagnostics.
fn coarse_inspection_family_key(canonical_tool_name: &str, args: &serde_json::Value) -> Option<String> {
    use vtcode_core::config::constants::tools;
    // Only shell listing/scanning commands suffer from overlapping-but-distinct
    // invocations (e.g. three `find` calls with different paths) that the
    // exact family key never groups. File reads (`cat`/`head`/`tail` via shell
    // included) and semantic search already carry precise family keys;
    // grouping them coarsely would mislabel diverse productive exploration
    // (different files/queries) as looping.
    match canonical_tool_name {
        tools::UNIFIED_EXEC | tools::EXEC_COMMAND => {
            let command = vtcode_core::tools::command_args::command_text(args).ok()??;
            let first = command.split_whitespace().next().unwrap_or("");
            let base = first
                .rsplit('/')
                .next()
                .unwrap_or(first)
                .trim_matches(|ch| ch == '\'' || ch == '"')
                .to_ascii_lowercase();
            if matches!(base.as_str(), "find" | "ls" | "rg" | "grep" | "fd") {
                Some(format!("exec::inspection::{base}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Upsert a tool result into `history`, keyed on `tool_call_id`.
///
/// This is a **bounded** upsert: the reverse scan stops as soon as it reaches
/// ANY Assistant message (regardless of its tool_calls). This is critical:
/// Assistant messages represent turn boundaries. Tool responses from before an
/// Assistant must never be overwritten by Tool responses from after it, even
/// when fabricated tool_call_ids collide across turns.
///
/// If a Tool message with a matching id is found *before* the nearest
/// Assistant boundary, it is a legitimate same-call update (e.g. an
/// auto-permission probe replaying a result) and gets overwritten in place.
/// If the boundary is hit first, the id has been reused across turns, so we
/// append instead of clobbering an unrelated, earlier Tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolResponseHistoryUpdate {
    Appended,
    Replaced { previous_text_len: usize },
}

pub(crate) fn push_tool_response<S>(
    history: &mut Vec<uni::Message>,
    tool_call_id: S,
    tool_name: Option<&str>,
    content: String,
) -> ToolResponseHistoryUpdate
where
    S: AsRef<str> + Into<String>,
{
    let tool_call_id_ref = tool_call_id.as_ref();
    let mut overwrite_index = None;
    for (index, message) in history.iter().enumerate().rev() {
        match message.role {
            uni::MessageRole::Tool => {
                if message.tool_call_id.as_deref() == Some(tool_call_id_ref) {
                    overwrite_index = Some(index);
                    break;
                }
            }
            // Stop at ANY Assistant message — it marks a turn boundary.
            // Tool responses from before this Assistant must not be overwritten.
            uni::MessageRole::Assistant => {
                break;
            }
            _ => {}
        }
    }

    if let Some(index) = overwrite_index {
        let previous_text_len = history[index].content.as_text().len();
        history[index].content = uni::MessageContent::Text(content);
        if let Some(tool_name) = tool_name {
            history[index].origin_tool = Some(tool_name.to_string());
        }
        return ToolResponseHistoryUpdate::Replaced { previous_text_len };
    }

    let tool_call_id = tool_call_id.into();
    history.push(match tool_name {
        Some(name) => uni::Message::tool_response_with_origin(tool_call_id, content, name.to_string()),
        None => uni::Message::tool_response(tool_call_id, content),
    });
    ToolResponseHistoryUpdate::Appended
}

/// Generate a tool signature key with predictable structure for loop tracking.
pub(crate) fn signature_key_for(name: &str, args: &serde_json::Value) -> String {
    // Keep keys compact on hot paths: hash bounded argument bytes instead of
    // allocating full JSON payloads for large tool arguments.
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut input_len = 0usize;
    let mutability_tag = if vtcode_core::tools::tool_intent::classify_tool_intent(name, args).mutating {
        "rw"
    } else {
        "ro"
    };

    if serde_json::to_writer(HashingWriter::new(&mut hash, &mut input_len), args).is_err() {
        for byte in b"{}" {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
            input_len = input_len.saturating_add(1);
        }
    }

    format!("{name}:{mutability_tag}:len{input_len}-fnv{hash:016x}")
}

/// Generate a read-normalized signature key for cross-turn dedup.
///
/// File-read tools (`file_operation` with `read` action, `read_file`,
/// `grep_file`, `list_files`) omit pagination and read-offset fields so that
/// re-reading the same target groups under one logical read. `code_search`
/// uses its normalised result-replay identity, which preserves the effective
/// `max_results`; its separate loop identity may group searches across limits.
///
/// For mutating tools the original `signature_key_for` is returned unchanged.
pub(crate) fn read_normalized_signature_key(name: &str, args: &serde_json::Value) -> String {
    if name == vtcode_core::config::constants::tools::CODE_SEARCH
        && let Some(identity) = vtcode_core::tools::normalised_code_search_identity(args)
    {
        return format!("{name}:ro:{identity}");
    }

    if !is_read_only_tool_args(name, args) {
        return signature_key_for(name, args);
    }

    let Some(mut obj) = args.as_object().cloned() else {
        return signature_key_for(name, args);
    };

    // Strip pagination / read-offset fields that don't change *what* is read.
    for key in read_extent::normalization_strip_keys() {
        obj.remove(key);
    }

    let normalized = serde_json::Value::Object(obj);
    signature_key_for(name, &normalized)
}

/// Returns `true` when `(name, args)` describe a read-only tool invocation.
fn is_read_only_tool_args(name: &str, args: &serde_json::Value) -> bool {
    use vtcode_core::config::constants::tools;
    match name {
        tools::READ_FILE | tools::GREP_FILE | tools::LIST_FILES => true,
        tools::CODE_SEARCH => true,
        tools::UNIFIED_SEARCH | "search_dispatch" => true,
        tools::UNIFIED_FILE | "file_operation" => {
            matches!(args.get("action").and_then(|v| v.as_str()), Some("read"))
        }
        _ => false,
    }
}

struct HashingWriter<'a> {
    hash: &'a mut u64,
    input_len: &'a mut usize,
}

impl<'a> HashingWriter<'a> {
    fn new(hash: &'a mut u64, input_len: &'a mut usize) -> Self {
        Self { hash, input_len }
    }
}

impl std::io::Write for HashingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        for byte in buf {
            *self.hash ^= u64::from(*byte);
            *self.hash = self.hash.wrapping_mul(0x100000001b3);
            *self.input_len = self.input_len.saturating_add(1);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn resolve_max_tool_retries(
    _tool_name: &str,
    vt_cfg: Option<&vtcode_core::config::loader::VTCodeConfig>,
) -> usize {
    vt_cfg
        .map(|cfg| cfg.agent.harness.max_tool_retries as usize)
        .unwrap_or(vtcode_config::constants::defaults::DEFAULT_MAX_TOOL_RETRIES as usize)
}

fn path_targets_plan_artifact(path: &str) -> bool {
    let normalized = path.trim().replace('\\', "/");
    normalized == ".vtcode/plans"
        || normalized.starts_with(".vtcode/plans/")
        || normalized.contains("/.vtcode/plans/")
        || normalized == "/tmp/vtcode-plans"
        || normalized.starts_with("/tmp/vtcode-plans/")
        || normalized.contains("/tmp/vtcode-plans/")
}

pub(crate) fn is_plan_artifact_write(name: &str, args: &serde_json::Value) -> bool {
    use vtcode_core::config::constants::tools as tool_names;
    use vtcode_core::tools::names::canonical_tool_name;
    use vtcode_core::tools::tool_intent::file_operation_action;

    let canonical = canonical_tool_name(name);
    match canonical {
        tool_names::TASK_TRACKER => true,
        tool_names::UNIFIED_FILE => {
            if !file_operation_action(args)
                .map(|action| action.eq_ignore_ascii_case("read"))
                .unwrap_or(false)
            {
                [
                    "path",
                    "file_path",
                    "filepath",
                    "filePath",
                    "target_path",
                    "destination",
                    "destination_path",
                ]
                .iter()
                .filter_map(|key| args.get(*key).and_then(|value| value.as_str()))
                .any(path_targets_plan_artifact)
            } else {
                false
            }
        }
        tool_names::WRITE_FILE | tool_names::EDIT_FILE | tool_names::CREATE_FILE | tool_names::SEARCH_REPLACE => {
            ["path", "file_path", "filepath", "filePath"]
                .iter()
                .filter_map(|key| args.get(*key).and_then(|value| value.as_str()))
                .any(path_targets_plan_artifact)
        }
        _ => false,
    }
}

fn is_execution_tool(name: &str) -> bool {
    use vtcode_core::config::constants::tools as tool_names;

    matches!(
        name,
        tool_names::UNIFIED_EXEC
            | tool_names::EXEC_COMMAND
            | tool_names::EXEC_PTY_CMD
            | tool_names::RUN_PTY_CMD
            | tool_names::EXECUTE_CODE
            | tool_names::SHELL
    )
}

/// Return whether a tool call must wait for a successful verification step.
///
/// Reads, inspections, verification commands, task tracking, and dedicated
/// plan-artifact writes remain available while the checkpoint is pending.
/// A failed verifier grants a bounded fix-up window ([`FAILED_VERIFICATION_FIX_ALLOWANCE`])
/// so a broken build can be repaired, and piped verifier attempts
/// (e.g. `cargo check 2>&1 | head`) are admitted to run even though only a
/// standalone success clears the gate.
pub(crate) fn mutation_blocked_until_verification(
    loop_tracker: &LoopTracker,
    name: &str,
    args: &serde_json::Value,
) -> bool {
    if !loop_tracker.verification_is_pending() || is_plan_artifact_write(name, args) {
        return false;
    }

    let canonical_name = canonical_tool_name(name);
    if is_execution_tool(canonical_name) {
        // Truncation-only verifier attempts (`cargo check 2>&1 | head`) must
        // run so the model can see the failure; they never clear the gate
        // (see update_repetition_tracker). The admission predicate requires
        // every shell segment to be verification-or-readonly, so a smuggled
        // mutation such as `cargo check && rm -rf target` stays blocked.
        if shell_command_is_admitted_verification_attempt(args)
            && matches!(classify_shell_activity(canonical_name, args), ShellActivity::Mutation)
        {
            return false;
        }
        if !matches!(classify_shell_activity(canonical_name, args), ShellActivity::Mutation) {
            return false;
        }
        // Fix-up window: allow bounded repair edits after a failed verifier.
        return loop_tracker.fix_edits_remaining == 0;
    }

    if !vtcode_core::tools::tool_intent::classify_tool_intent(canonical_name, args).mutating {
        return false;
    }
    loop_tracker.fix_edits_remaining == 0
}

/// Updates the tool repetition tracker based on the execution outcome.
///
/// Count completed attempts for repetition detection, but only successful
/// mutations contribute to anti-blind-editing verification pressure.
///
/// Returns `true` when this outcome freshly granted (or refreshed) the
/// failed-verifier fix-up window. Callers must reset the assistant text-response
/// streak so the model gets one diagnostic explanation before the
/// pending-verification text cap re-applies; otherwise a failed build blocks
/// the turn before the agent can describe the failure and use its fix-up edits.
///
/// A `true` return also covers *lost* verifier results: while the gate is
/// pending, a verifier-level Failure/Timeout (or a `write_stdin` failure
/// reporting a dead exec session) grants the same bounded window because the
/// verifier never produced an observable verdict. In that case the tracker
/// queues [`VERIFICATION_RESULT_LOST_DIRECTIVE`] for the handlers to surface.
pub(crate) fn update_repetition_tracker(
    loop_tracker: &mut LoopTracker,
    outcome: &ToolPipelineOutcome,
    name: &str,
    args: &serde_json::Value,
) -> bool {
    if matches!(&outcome.status, ToolExecutionStatus::Cancelled) {
        return false;
    }

    let canonical_name = canonical_tool_name(name);
    let signature_key = signature_key_for(canonical_name, args);
    loop_tracker.record(signature_key.clone());
    let low_signal_family =
        crate::agent::runloop::unified::turn::tool_outcomes::handlers::low_signal_family_key(canonical_name, args)
            .filter(|_| is_low_signal_outcome(outcome, canonical_name, args));
    // Successful but redundant scans (e.g. three overlapping `find` calls)
    // never match the exact family key. Track a coarse inspection family so
    // the third repeat still surfaces in diagnostics without changing
    // admission or blocking behavior.
    let coarse_family = coarse_inspection_family_key(canonical_name, args);
    let coarse_repeat = coarse_family.as_ref().map(|family| {
        let entry = loop_tracker
            .coarse_inspection_attempts
            .entry(family.clone())
            .or_insert((0, Instant::now()));
        entry.0 = entry.0.saturating_add(1);
        entry.1 = Instant::now();
        entry.0
    });
    let is_coarse_duplicate =
        coarse_repeat.is_some_and(|count| count >= 3) && matches!(&outcome.status, ToolExecutionStatus::Success { .. });
    let mut low_signal_family = low_signal_family;
    if low_signal_family.is_none() && is_coarse_duplicate {
        low_signal_family = coarse_family.clone();
    }
    let is_low_signal_navigation = low_signal_family.is_some();
    if let Some(low_signal_family) = low_signal_family.as_ref() {
        loop_tracker.record_low_signal(low_signal_family.clone());
    }

    // Lost verifier results via the exec-session stdin tool: a `write_stdin`
    // failure that reports a missing session means the session (and its
    // pending verifier output) died before the result was captured, so
    // `write_stdin` never classifies as ShellActivity::Verification. While
    // the gate is pending, treat it like a failed verifier so the model gets
    // a bounded fix/diagnostic window instead of deadlocking behind a gate
    // that can no longer observe a successful verifier.
    if canonical_name == vtcode_core::config::constants::tools::WRITE_STDIN
        && loop_tracker.verification_is_pending()
        && let ToolExecutionStatus::Failure { error } = &outcome.status
        && error_text_indicates_lost_exec_session(&error.message)
    {
        loop_tracker.verification_result_lost_notice_pending = true;
        loop_tracker.record_failed_verification();
        loop_tracker.reset_navigation_window(low_signal_family.is_none());
        return true;
    }

    // Update NL2Repo-Bench metrics based on tool intent.
    //
    // IMPORTANT: Check execution tools FIRST. `classify_tool_intent` marks
    // `command_session(action=run)` as `mutating: true` because shell commands *can*
    // mutate state, but for the Edit-Test heuristic, any execution/verification
    // step (cargo check, cargo test, etc.) should RESET the mutation counter,
    // not increment it.
    if is_execution_tool(canonical_name) {
        match classify_shell_activity(canonical_name, args) {
            ShellActivity::Inspection => {
                loop_tracker.consecutive_navigations = loop_tracker.consecutive_navigations.saturating_add(1);
                loop_tracker.nav_signatures.insert(signature_key);
                loop_tracker.record_navigation_signal(is_low_signal_navigation);
            }
            ShellActivity::Verification => {
                if matches!(&outcome.status, ToolExecutionStatus::Success { command_success: true, .. }) {
                    loop_tracker.mark_verification_complete();
                } else if matches!(&outcome.status, ToolExecutionStatus::Success { command_success: false, .. }) {
                    // Only a verifier that actually ran and reported non-zero
                    // opens the fix-up window. Tool-level Failure/Timeout (never
                    // executed, e.g. argument errors) must not grant edits.
                    loop_tracker.record_failed_verification();
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                    return true;
                } else if loop_tracker.verification_is_pending()
                    && matches!(
                        &outcome.status,
                        ToolExecutionStatus::Failure { .. } | ToolExecutionStatus::Timeout { .. }
                    )
                {
                    // While the gate is pending, a verifier-level
                    // Failure/Timeout almost always means the result was lost
                    // (e.g. the exec session ended before the verifier
                    // finished) rather than a genuine non-zero exit: the
                    // verifier never produced an observable verdict. A bounded
                    // fix window that still requires a successful standalone
                    // verifier to clear is strictly better than a permanent
                    // stall. Without the gate pending this stays a no-grant
                    // arg-error path.
                    loop_tracker.verification_result_lost_notice_pending = true;
                    loop_tracker.record_failed_verification();
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                    return true;
                }
                loop_tracker.reset_navigation_window(low_signal_family.is_none());
            }
            ShellActivity::Mutation => {
                // Truncation-only verifier attempts (e.g. `cargo check 2>&1 | head`)
                // are admitted to run but never clear the gate: the pipeline
                // exit status is the truncator's, not the verifier's. Don't
                // count them as blind edits; a failed piped attempt still
                // opens the fix window so the agent can repair and re-run a
                // standalone verifier. Chained mutations smuggled behind a
                // verifier prefix are rejected by the admission predicate and
                // take the blind-edit path below.
                if shell_command_is_admitted_verification_attempt(args) {
                    let ran_and_failed =
                        matches!(&outcome.status, ToolExecutionStatus::Success { command_success: false, .. });
                    if ran_and_failed {
                        loop_tracker.record_failed_verification();
                        loop_tracker.reset_navigation_window(low_signal_family.is_none());
                        return true;
                    }
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                } else {
                    if mutation_was_applied(outcome) {
                        loop_tracker.record_successful_mutation();
                    }
                    loop_tracker.reset_navigation_window(low_signal_family.is_none());
                }
            }
        }
    } else if is_plan_artifact_write(canonical_name, args) {
        // Plan artifact writes in dedicated plan storage are allowed in Planning workflow and
        // should not trigger anti-blind-editing verification pressure.
        // Low-signal repetition history is preserved: plan writes are not
        // navigation, so they neither advance nor clear that window.
        loop_tracker.reset_navigation_window(false);
    } else {
        let intent = vtcode_core::tools::tool_intent::classify_tool_intent(canonical_name, args);
        if intent.mutating {
            if mutation_was_applied(outcome) {
                loop_tracker.record_successful_mutation();
            }
            loop_tracker.reset_navigation_window(low_signal_family.is_none());
        } else {
            // Read-only / navigation tool
            loop_tracker.consecutive_navigations += 1;
            loop_tracker.nav_signatures.insert(signature_key);
            loop_tracker.record_navigation_signal(is_low_signal_navigation);
        }
    }
    false
}

fn mutation_was_applied(outcome: &ToolPipelineOutcome) -> bool {
    match &outcome.status {
        ToolExecutionStatus::Success { output, command_success, modified_files, .. } => {
            if let Some(effective_change) = vtcode_core::tools::file_ops::diff_output_has_effective_change(output) {
                return effective_change;
            }
            *command_success || !modified_files.is_empty()
        }
        ToolExecutionStatus::Failure { .. } | ToolExecutionStatus::Timeout { .. } | ToolExecutionStatus::Cancelled => {
            false
        }
    }
}
pub(crate) fn serialize_output(output: &serde_json::Value) -> String {
    if let Some(s) = output.as_str() {
        s.to_string()
    } else {
        serde_json::to_string(output).unwrap_or_else(|_| "{}".to_string())
    }
}

pub(crate) fn check_is_argument_error(error_str: &str) -> bool {
    error_str.contains("Missing required")
        || error_str.contains("Invalid arguments")
        || error_str.contains("Tool argument validation failed")
        || error_str.contains("required path parameter")
        || error_str.contains("is required for '")
        || error_str.contains("is required for \"")
        || error_str.contains("'index' is required")
        || error_str.contains("'index_path' is required")
        || error_str.contains("'status' is required")
        || error_str.contains("expected ")
        || error_str.contains("Expected:")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vtcode_core::config::constants::tools;

    use super::*;

    #[test]
    fn push_tool_response_replaces_existing_tool_call_entry() {
        let mut history = vec![uni::Message::tool_response(
            "call_1".to_string(),
            "{\"output\":\"first\"}".to_string(),
        )];

        let update =
            push_tool_response(&mut history, "call_1".to_string(), None, "{\"output\":\"latest\"}".to_string());

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].content.as_text_borrowed(), Some("{\"output\":\"latest\"}"));
        assert_eq!(update, ToolResponseHistoryUpdate::Replaced { previous_text_len: "{\"output\":\"first\"}".len() });
    }

    #[test]
    fn push_tool_response_sets_origin_tool_when_provided() {
        let mut history = Vec::new();

        let update = push_tool_response(
            &mut history,
            "call_1".to_string(),
            Some("read_file"),
            "{\"output\":\"first\"}".to_string(),
        );

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].origin_tool.as_deref(), Some("read_file"));
        assert_eq!(update, ToolResponseHistoryUpdate::Appended);
    }

    #[test]
    fn push_tool_response_refreshes_origin_tool_when_replacing_same_call() {
        let mut history = vec![uni::Message::tool_response("call_1".to_string(), "old".to_string())];

        let update = push_tool_response(&mut history, "call_1".to_string(), Some("exec_command"), "new".to_string());

        assert_eq!(update, ToolResponseHistoryUpdate::Replaced { previous_text_len: 3 });
        assert_eq!(history[0].origin_tool.as_deref(), Some("exec_command"));
    }

    #[test]
    fn push_tool_response_appends_when_id_reused_across_assistant_boundary() {
        // Fabricated ids can collide across turns (e.g. index-based fallbacks).
        // A later assistant message re-declaring the same id must not cause a
        // new result to clobber the earlier, unrelated Tool response.
        let mut history = vec![
            uni::Message::assistant_with_tools(
                "first".into(),
                vec![uni::ToolCall::function(
                    "call_1".into(),
                    "file_operation".into(),
                    "{}".into(),
                )],
            ),
            uni::Message::tool_response("call_1".to_string(), "{\"output\":\"first\"}".into()),
            uni::Message::assistant_with_tools(
                "second".into(),
                vec![uni::ToolCall::function(
                    "call_1".into(),
                    tools::CODE_SEARCH.into(),
                    "{}".into(),
                )],
            ),
        ];

        let update = push_tool_response(
            &mut history,
            "call_1".to_string(),
            Some(tools::CODE_SEARCH),
            "{\"output\":\"second\"}".to_string(),
        );

        let tool_messages: Vec<&uni::Message> = history
            .iter()
            .filter(|message| matches!(message.role, uni::MessageRole::Tool))
            .collect();
        assert_eq!(tool_messages.len(), 2, "must append, not overwrite");
        assert_eq!(
            tool_messages[0].content.as_text_borrowed(),
            Some("{\"output\":\"first\"}"),
            "earlier unrelated Tool result must remain intact"
        );
        assert_eq!(tool_messages[1].content.as_text_borrowed(), Some("{\"output\":\"second\"}"));
        assert_eq!(update, ToolResponseHistoryUpdate::Appended);
    }

    #[test]
    fn push_tool_response_appends_when_assistant_has_no_tool_calls() {
        // When an Assistant message has no tool_calls (e.g. commentary-only
        // message between tool calls), the boundary must STILL stop the scan.
        // Otherwise a later Tool response with a colliding fabricated id would
        // overwrite an earlier, unrelated Tool result.
        let mut history = vec![
            uni::Message::assistant_with_tools(
                String::new(),
                vec![uni::ToolCall::function(
                    "call_0".into(),
                    "file_operation".into(),
                    "{}".into(),
                )],
            ),
            uni::Message::tool_response("call_0".to_string(), "{\"output\":\"file content\"}".into()),
            // Commentary Assistant with no tool_calls — must act as boundary
            uni::Message::assistant("I need to retry.".into()),
            uni::Message::assistant_with_tools(
                String::new(),
                vec![uni::ToolCall::function(
                    "call_0".into(),
                    "apply_patch".into(),
                    "{}".into(),
                )],
            ),
        ];

        let update = push_tool_response(
            &mut history,
            "call_0".to_string(),
            Some("apply_patch"),
            "{\"output\":\"patch result\"}".to_string(),
        );

        let tool_messages: Vec<&uni::Message> = history
            .iter()
            .filter(|message| matches!(message.role, uni::MessageRole::Tool))
            .collect();
        assert_eq!(tool_messages.len(), 2, "must append, not overwrite the earlier file read");
        assert_eq!(
            tool_messages[0].content.as_text_borrowed(),
            Some("{\"output\":\"file content\"}"),
            "earlier file read result must remain intact"
        );
        assert_eq!(tool_messages[1].content.as_text_borrowed(), Some("{\"output\":\"patch result\"}"));
        assert_eq!(update, ToolResponseHistoryUpdate::Appended);
    }

    #[test]
    fn repetition_tracker_counts_failures() {
        let mut tracker = LoopTracker::new();
        let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                "edit_file".to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "boom".to_string(),
            ),
        });

        update_repetition_tracker(&mut tracker, &outcome, "edit_file", &json!({"path":"src/main.rs"}));

        assert_eq!(tracker.max_count_filtered(|_| false), 1);
    }

    #[test]
    fn failed_file_mutations_do_not_trigger_verification_pressure() {
        let mut tracker = LoopTracker::new();
        let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                "apply_patch".to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "invalid patch path".to_string(),
            ),
        });

        update_repetition_tracker(
            &mut tracker,
            &outcome,
            tools::APPLY_PATCH,
            &json!({"input":"*** Begin Patch\n*** Update File: /absolute/path\n*** End Patch"}),
        );

        assert_eq!(tracker.consecutive_mutations, 0);
    }

    #[test]
    fn no_op_write_does_not_trigger_verification_pressure() {
        let mut tracker = LoopTracker::new();
        let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({
                "success": true,
                "path": "README.md",
                "diff_preview": {
                    "content": "",
                    "truncated": false,
                    "omitted_line_count": 0,
                    "skipped": false,
                    "is_empty": true
                },
                "diff": [{
                    "path": "README.md",
                    "content": "",
                    "truncated": false,
                    "omitted_line_count": 0,
                    "skipped": false,
                    "is_empty": true
                }]
            }),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &outcome,
            tools::WRITE_FILE,
            &json!({"path":"README.md","content":"same\n","mode":"overwrite"}),
        );

        assert_eq!(tracker.consecutive_mutations, 0);
    }

    #[test]
    fn skipped_write_does_not_trigger_verification_pressure() {
        let mut tracker = LoopTracker::new();
        let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({
                "success": true,
                "skipped": true,
                "reason": "File already exists"
            }),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &outcome,
            tools::WRITE_FILE,
            &json!({"path":"README.md","content":"same\n","mode":"skip_if_exists"}),
        );

        assert_eq!(tracker.consecutive_mutations, 0);
    }

    #[test]
    fn verification_gate_blocks_mutations_but_allows_reads_checks_and_plan_artifacts() {
        let mut tracker = LoopTracker::new();
        tracker.verification_pending = true;

        assert!(mutation_blocked_until_verification(
            &tracker,
            tools::WRITE_FILE,
            &json!({"path":"README.md","content":"new"})
        ));
        assert!(mutation_blocked_until_verification(
            &tracker,
            tools::EXEC_COMMAND,
            &json!({"cmd":"sed -i '' 's/old/new/' README.md"})
        ));
        assert!(!mutation_blocked_until_verification(&tracker, tools::READ_FILE, &json!({"path":"README.md"})));
        assert!(!mutation_blocked_until_verification(
            &tracker,
            tools::EXEC_COMMAND,
            &json!({"cmd":"cargo check --locked"})
        ));
        assert!(!mutation_blocked_until_verification(
            &tracker,
            tools::WRITE_FILE,
            &json!({"path":".vtcode/plans/next.md","content":"plan"})
        ));
        assert!(!mutation_blocked_until_verification(&tracker, tools::TASK_TRACKER, &json!({"action":"update"})));
    }

    #[test]
    fn inspection_does_not_clear_mutations_waiting_for_verification() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;

        for command in ["git diff -- README.md", "git diff --check"] {
            update_repetition_tracker(&mut tracker, &success, tools::EXEC_COMMAND, &json!({"cmd":command}));
        }

        assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
    }

    #[test]
    fn failed_verification_does_not_clear_mutations_waiting_for_verification() {
        let mut tracker = LoopTracker::new();
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        tracker.verification_pending = true;
        let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 1}),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });

        update_repetition_tracker(&mut tracker, &failed_check, tools::EXEC_COMMAND, &json!({"cmd":"cargo check"}));

        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
        // A failed verifier keeps the gate but opens a bounded fix-up window
        // so the broken build can be repaired instead of deadlocking.
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);
        assert!(!mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
    }

    #[test]
    fn failed_verification_fix_window_is_consumed_by_repair_edits() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 1}),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });
        update_repetition_tracker(&mut tracker, &failed_check, tools::EXEC_COMMAND, &json!({"cmd":"cargo check"}));
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);

        let edit = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        for _ in 0..FAILED_VERIFICATION_FIX_ALLOWANCE {
            assert!(!mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
            update_repetition_tracker(&mut tracker, &edit, tools::EDIT_FILE, &json!({"path": "src/lib.rs"}));
            assert!(tracker.verification_is_pending());
        }
        // Window exhausted: further mutations block again until a standalone
        // verifier succeeds.
        assert_eq!(tracker.fix_edits_remaining, 0);
        assert!(mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
    }

    #[test]
    fn piped_verifier_is_admitted_but_does_not_clear_gate() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        // Piped verifiers must run (not block) so the model sees output, but
        // the pipeline status is the truncator's — only standalone success clears.
        assert!(!mutation_blocked_until_verification(
            &tracker,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked 2>&1 | head -c 4000"})
        ));
        let piped_success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 0}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        update_repetition_tracker(
            &mut tracker,
            &piped_success,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked 2>&1 | head -c 4000"}),
        );
        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
    }

    #[test]
    fn smuggled_mutation_behind_verifier_prefix_stays_blocked() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        for command in [
            "cargo check && rm -rf target",
            "cargo check; rm foo.txt",
            "cargo check --locked && cargo test && rm foo.txt",
        ] {
            assert!(
                mutation_blocked_until_verification(&tracker, tools::EXEC_COMMAND, &json!({"cmd": command})),
                "smuggled mutation must stay blocked: {command}"
            );
        }
    }

    #[test]
    fn pure_and_chained_verifiers_are_admitted_and_clear_gate() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        for command in [
            "cargo fmt --all -- --check && cargo check --locked",
            "cargo check --locked && cargo nextest run --locked -p vtcode-ui",
            "cargo check --locked && cargo clippy --locked -p vtcode-ui -- -D warnings",
        ] {
            assert!(
                !mutation_blocked_until_verification(&tracker, tools::EXEC_COMMAND, &json!({"cmd": command})),
                "pure && verifier chain must be admitted: {command}"
            );
        }

        let chained_success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 0}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        assert!(!update_repetition_tracker(
            &mut tracker,
            &chained_success,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo fmt --all -- --check && cargo check --locked"}),
        ));
        assert!(!tracker.verification_is_pending());
        assert_eq!(tracker.consecutive_mutations, 0);
    }

    #[test]
    fn non_and_chained_verifiers_do_not_clear_gate() {
        for command in [
            "cargo check --locked; cargo nextest run --locked -p vtcode-ui",
            "cargo check --locked || cargo nextest run --locked -p vtcode-ui",
            "cargo check --locked | head -40",
        ] {
            let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
            tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
            let chained_success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
                output: serde_json::json!({"exit_code": 0}),
                stdout: None,
                modified_files: vec![],
                command_success: true,
            });
            update_repetition_tracker(&mut tracker, &chained_success, tools::EXEC_COMMAND, &json!({"cmd": command}));
            assert!(tracker.verification_is_pending(), "`;`/`||`/`|` chains must not clear the gate: {command}");
        }
    }

    #[test]
    fn fmt_check_clears_gate_but_plain_fmt_does_not() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        assert!(!mutation_blocked_until_verification(
            &tracker,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo fmt --all -- --check"})
        ));

        let fmt_check_success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 0}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        update_repetition_tracker(
            &mut tracker,
            &fmt_check_success,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo fmt --all -- --check"}),
        );
        assert!(!tracker.verification_is_pending());

        // Plain `cargo fmt` rewrites files: it stays a mutation and never
        // clears the gate.
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        assert!(mutation_blocked_until_verification(&tracker, tools::EXEC_COMMAND, &json!({"cmd": "cargo fmt"})));
    }

    #[test]
    fn failed_verifier_reports_fix_window_for_text_streak_reset() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 1}),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });
        assert!(update_repetition_tracker(
            &mut tracker,
            &failed_check,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked"}),
        ));
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);

        let successful_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 0}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        assert!(!update_repetition_tracker(
            &mut tracker,
            &successful_check,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked"}),
        ));
    }

    #[test]
    fn lost_verification_tool_failure_while_pending_grants_fix_window() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let tool_failure = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::EXEC_COMMAND.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "check could not start".to_string(),
            ),
        });
        assert!(update_repetition_tracker(
            &mut tracker,
            &tool_failure,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked"}),
        ));
        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);
        assert!(
            !mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})),
            "the lost-result grant must open the bounded fix window"
        );
        assert!(tracker.take_verification_result_lost_notice(), "the lost-result directive must be queued");
        assert!(!tracker.take_verification_result_lost_notice(), "the notice is one-shot");
    }

    #[test]
    fn lost_verification_tool_timeout_while_pending_grants_fix_window() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let tool_timeout = ToolPipelineOutcome::from_status(ToolExecutionStatus::Timeout {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::EXEC_COMMAND.to_string(),
                vtcode_core::tools::registry::ToolErrorType::Timeout,
                "verification command timed out".to_string(),
            ),
        });
        assert!(update_repetition_tracker(
            &mut tracker,
            &tool_timeout,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo nextest run"}),
        ));
        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);
    }

    #[test]
    fn verification_tool_failure_without_pending_gate_grants_no_fix_window() {
        let mut tracker = LoopTracker::new();
        let tool_failure = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::EXEC_COMMAND.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "check could not start".to_string(),
            ),
        });
        assert!(!update_repetition_tracker(
            &mut tracker,
            &tool_failure,
            tools::EXEC_COMMAND,
            &json!({"cmd": "cargo check --locked"}),
        ));
        assert!(!tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, 0);
        assert!(!tracker.take_verification_result_lost_notice());
    }

    #[test]
    fn write_stdin_lost_exec_session_failure_while_pending_grants_fix_window() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let lost_session = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::WRITE_STDIN.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "exec session 'run-7' not found. Copy the exact `session_id` from the original run response"
                    .to_string(),
            ),
        });
        assert!(update_repetition_tracker(
            &mut tracker,
            &lost_session,
            tools::WRITE_STDIN,
            &json!({"session_id": "run-7", "chars": ""}),
        ));
        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);
        assert!(!mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
        assert!(tracker.take_verification_result_lost_notice());
    }

    #[test]
    fn write_stdin_unrelated_failure_while_pending_grants_no_fix_window() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
        let unrelated = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::WRITE_STDIN.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "session is not writable".to_string(),
            ),
        });
        assert!(!update_repetition_tracker(
            &mut tracker,
            &unrelated,
            tools::WRITE_STDIN,
            &json!({"session_id": "run-7", "chars": "q"}),
        ));
        assert!(tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, 0);
        assert!(!tracker.take_verification_result_lost_notice());
    }

    #[test]
    fn verification_snapshot_bundle_round_trips_without_drift() {
        let tracker = LoopTracker::with_verification_snapshot((true, FAILED_VERIFICATION_FIX_ALLOWANCE));
        assert_eq!(tracker.verification_snapshot(), (true, FAILED_VERIFICATION_FIX_ALLOWANCE));
        let cleared = LoopTracker::with_verification_snapshot((false, FAILED_VERIFICATION_FIX_ALLOWANCE));
        assert_eq!(cleared.verification_snapshot(), (false, 0));
    }

    #[test]
    fn logged_compound_inspections_do_not_trigger_anti_blind_pressure() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        for command in [
            "cat README.md && printf '\\n--- git status ---\\n' && git status --short",
            "wc -l README.md; rg -n '^#' README.md",
            "git diff --stat; find docs -maxdepth 2 -type f | sort | head -40",
        ] {
            update_repetition_tracker(&mut tracker, &success, tools::EXEC_COMMAND, &json!({"cmd":command}));
        }

        assert_eq!(tracker.consecutive_mutations, 0);
        assert!(!tracker.verification_is_pending());
        assert_eq!(tracker.consecutive_navigations, 3);
    }

    #[cfg(unix)]
    #[test]
    fn logged_compound_inspection_with_unix_stderr_suppression_does_not_trigger_pressure() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        let command = r###"git diff --stat; find docs -maxdepth 2 -type f | sort | head -40; rg -n "vtcode init|vtcode models|full-auto|run-debug|cargo install" docs/user-guide docs/installation docs/development 2>/dev/null | head -50"###;
        update_repetition_tracker(&mut tracker, &success, tools::EXEC_COMMAND, &json!({"cmd":command}));

        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 1);
    }

    #[test]
    fn only_a_completed_verification_clears_pending_mutation_pressure() {
        let mut tracker = LoopTracker::new();
        let edit = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        for _ in 0..BLIND_EDITING_THRESHOLD {
            update_repetition_tracker(&mut tracker, &edit, tools::EDIT_FILE, &json!({"path":"README.md"}));
        }
        assert!(tracker.verification_is_pending());

        let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::EXEC_COMMAND.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ExecutionError,
                "check could not start".to_string(),
            ),
        });
        update_repetition_tracker(
            &mut tracker,
            &failed_check,
            tools::EXEC_COMMAND,
            &json!({"cmd":"cargo nextest run"}),
        );
        assert!(tracker.verification_is_pending());

        update_repetition_tracker(&mut tracker, &edit, tools::EXEC_COMMAND, &json!({"cmd":"cargo nextest run"}));
        assert!(!tracker.verification_is_pending());
        assert_eq!(tracker.consecutive_mutations, 0);
    }

    #[test]
    fn carried_verification_checkpoint_clears_after_successful_check() {
        let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
        let successful_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"exit_code": 0}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &successful_check,
            tools::EXEC_COMMAND,
            &json!({"cmd":"cargo check --locked"}),
        );

        assert!(!tracker.verification_is_pending());
    }

    #[test]
    fn verification_snapshot_round_trips_through_session_state() {
        let tracker = LoopTracker::with_verification_snapshot((true, FAILED_VERIFICATION_FIX_ALLOWANCE));
        assert_eq!(tracker.verification_snapshot(), (true, FAILED_VERIFICATION_FIX_ALLOWANCE));
        assert_eq!(LoopTracker::new().verification_snapshot(), (false, 0));
    }

    #[test]
    fn repetition_tracker_ignores_cancellations() {
        let mut tracker = LoopTracker::new();
        let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Cancelled);

        update_repetition_tracker(&mut tracker, &outcome, "edit_file", &json!({"path":"src/main.rs"}));

        assert_eq!(tracker.max_count_filtered(|_| false), 0);
    }

    #[test]
    fn reset_after_balancer_recovery_clears_attempts_and_counters() {
        let mut tracker = LoopTracker::new();
        tracker.record("code_search:{\"query\":\"Widget\"}".to_string());
        tracker.record("code_search:{\"query\":\"Widget\"}".to_string());
        tracker.consecutive_mutations = 2;
        tracker.verification_pending = true;
        tracker.fix_edits_remaining = FAILED_VERIFICATION_FIX_ALLOWANCE;
        tracker.consecutive_navigations = 4;
        tracker.consecutive_low_signal_navigations = 3;
        tracker.total_low_signal_navigations = 7;
        tracker.record_low_signal("code_search::Widget::src".to_string());
        tracker.navigation_loop_recoveries = 3;

        tracker.reset_after_balancer_recovery();

        assert_eq!(tracker.max_count_filtered(|_| false), 0);
        assert_eq!(tracker.max_low_signal_count(), 0);
        assert_eq!(tracker.consecutive_mutations, 0);
        // The mutation history is wiped, so the pending gate must be cleared
        // with it: an untracked pending gate has an empty fix window and
        // deadlocks the turn.
        assert!(!tracker.verification_pending);
        assert!(!tracker.verification_is_pending());
        assert_eq!(tracker.fix_edits_remaining, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 0);
        assert_eq!(tracker.navigation_loop_recoveries, 3);
    }

    #[test]
    fn shell_activity_distinguishes_inspection_verification_and_mutation() {
        for command in [
            "rg -n 'LoopTracker' src",
            "find src -name '*.rs'",
            "cat Cargo.toml",
            "sed -n '1,80p' src/main.rs",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &json!({"cmd":command})),
                ShellActivity::Inspection,
                "{command}"
            );
        }

        for command in [
            "cargo check --locked",
            "cargo nextest run -p vtcode",
            "cargo clippy --all-targets",
            "cargo build --release",
            "./scripts/check-dev.sh --changed",
            "cargo check --locked > build.log",
            "cargo check &> build.log",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &json!({"cmd":command})),
                ShellActivity::Verification,
                "{command}"
            );
        }

        for command in [
            "cargo nextest run -p vtcode 2>&1 | head -c 4000",
            "cargo check | head -40",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &json!({"cmd":command})),
                ShellActivity::Mutation,
                "verification pipelines require reliable aggregate status: {command}"
            );
        }

        assert_eq!(
            classify_shell_activity(tools::EXEC_COMMAND, &json!({"cmd":"sed -i '' 's/a/b/' src/lib.rs"})),
            ShellActivity::Mutation
        );
        assert_eq!(
            classify_shell_activity(tools::EXEC_COMMAND, &json!({"cmd":"rm output && cargo check"})),
            ShellActivity::Mutation
        );
    }

    #[test]
    fn inspection_commands_increment_navigation_instead_of_resetting_it() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        for command in [
            "rg LoopTracker src",
            "find src -name '*.rs'",
            "cat Cargo.toml",
            "sed -n '1,20p' src/main.rs",
        ] {
            update_repetition_tracker(&mut tracker, &success, tools::EXEC_COMMAND, &json!({"cmd":command}));
        }

        assert_eq!(tracker.consecutive_navigations, 4);
    }

    #[test]
    fn productive_navigation_resets_only_consecutive_low_signal_count() {
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({"results":[]}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });
        let hit = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({"results":[{"path":"src/lib.rs"}]}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        for query in ["missing-a", "missing-b"] {
            update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &json!({"query":query, "path":"src"}));
        }
        update_repetition_tracker(
            &mut tracker,
            &hit,
            tools::CODE_SEARCH,
            &json!({"query":"LoopTracker", "path":"src"}),
        );

        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 2);
    }

    #[test]
    fn verification_resets_all_low_signal_navigation_counts() {
        let mut tracker = LoopTracker::new();
        tracker.consecutive_low_signal_navigations = 6;
        tracker.total_low_signal_navigations = 10;
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::UNIFIED_EXEC,
            &json!({"action":"run", "command":"cargo check --locked"}),
        );

        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 0);
    }

    #[test]
    fn consecutive_mutations_increments_on_edit() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        // edit_file is classified as mutating
        update_repetition_tracker(
            &mut tracker,
            &success,
            "edit_file",
            &json!({"path":"src/lib.rs","old_str":"a","new_str":"b"}),
        );
        assert_eq!(tracker.consecutive_mutations, 1);
        assert_eq!(tracker.consecutive_navigations, 0);

        update_repetition_tracker(&mut tracker, &success, "write_to_file", &json!({"path":"src/lib.rs","content":"x"}));
        assert_eq!(tracker.consecutive_mutations, 2);
    }

    #[test]
    fn execution_tool_resets_mutation_counter() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        // Two mutations
        update_repetition_tracker(
            &mut tracker,
            &success,
            "edit_file",
            &json!({"path":"a","old_str":"x","new_str":"y"}),
        );
        update_repetition_tracker(
            &mut tracker,
            &success,
            "edit_file",
            &json!({"path":"b","old_str":"x","new_str":"y"}),
        );
        assert_eq!(tracker.consecutive_mutations, 2);

        // Execution tool resets
        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::UNIFIED_EXEC,
            &json!({"action":"run","command":"cargo check"}),
        );
        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[test]
    fn reads_increment_navigation_counter() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(&mut tracker, &success, tools::READ_FILE, &json!({"path":"src/main.rs"}));
        assert_eq!(tracker.consecutive_navigations, 1);
        assert_eq!(tracker.consecutive_mutations, 0);

        update_repetition_tracker(&mut tracker, &success, tools::GREP_FILE, &json!({"pattern":"foo","path":"src/"}));
        assert_eq!(tracker.consecutive_navigations, 2);
    }

    #[test]
    fn mutation_resets_navigation_counter() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        // Several reads
        for _ in 0..5 {
            update_repetition_tracker(&mut tracker, &success, tools::READ_FILE, &json!({"path":"src/main.rs"}));
        }
        assert_eq!(tracker.consecutive_navigations, 5);

        // A mutation resets navigation counter
        update_repetition_tracker(
            &mut tracker,
            &success,
            "edit_file",
            &json!({"path":"src/lib.rs","old_str":"a","new_str":"b"}),
        );
        assert_eq!(tracker.consecutive_navigations, 0);
        assert_eq!(tracker.consecutive_mutations, 1);
    }

    #[test]
    fn task_tracker_does_not_increment_mutations_in_planning() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::TASK_TRACKER,
            &json!({"action":"create","items":["step"]}),
        );
        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[test]
    fn task_tracker_does_not_increment_mutations() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::TASK_TRACKER,
            &json!({"action":"create","items":["step"]}),
        );
        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[test]
    fn plan_file_write_does_not_increment_mutations() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::UNIFIED_FILE,
            &json!({"action":"write","path":".vtcode/plans/my-plan.md","content":"text"}),
        );
        assert_eq!(tracker.consecutive_mutations, 0);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[test]
    fn non_plan_file_write_still_increments_mutations() {
        let mut tracker = LoopTracker::new();
        let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &success,
            tools::UNIFIED_FILE,
            &json!({"action":"write","path":"src/lib.rs","content":"text"}),
        );
        assert_eq!(tracker.consecutive_mutations, 1);
        assert_eq!(tracker.consecutive_navigations, 0);
    }

    #[test]
    fn argument_error_detection_includes_required_update_fields() {
        assert!(check_is_argument_error("Tool execution failed: 'index' is required for 'update' (1-indexed)"));
    }

    #[test]
    fn low_signal_tracker_groups_empty_search_results_by_family() {
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"results":[]}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        // Different queries produce separate family keys, so each counts as its
        // own family while the agent explores one path.
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::CODE_SEARCH,
            &json!({"query":"Widget", "path":"src", "result_types":["definition"]}),
        );
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::CODE_SEARCH,
            &json!({"query":"Result", "path":"src", "result_types":["usage"]}),
        );
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::CODE_SEARCH,
            &json!({"query":"Result<", "path":"src", "result_types":["text"]}),
        );

        assert_eq!(tracker.max_low_signal_count(), 1);
    }

    #[test]
    fn low_signal_tracker_groups_identical_searches_in_same_family() {
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({"results":[]}),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        let args = json!({"query":"TODO","path":"src","file_types":["rust"]});
        update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);
        update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);
        update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);

        assert_eq!(tracker.max_low_signal_count(), 3);
    }

    #[test]
    fn low_signal_tracker_ignores_empty_search_results_with_recovery_guidance() {
        let mut tracker = LoopTracker::new();
        let guided = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({
                "results": [],
                "hint": "Try narrowing the path.",
                "is_recoverable": true,
                "next_action": "Retry with narrower filters."
            }),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &guided,
            tools::CODE_SEARCH,
            &json!({"query":"run", "path":"src/agent", "result_types":["definition"]}),
        );

        assert_eq!(tracker.max_low_signal_count(), 0);
    }

    #[test]
    fn low_signal_tracker_does_not_hide_structured_search_errors_as_empty_results() {
        let mut tracker = LoopTracker::new();
        let failure_like = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({
                "results": [],
                "error": "permission denied while searching the workspace"
            }),
            stdout: None,
            modified_files: vec![],
            command_success: true,
        });

        update_repetition_tracker(
            &mut tracker,
            &failure_like,
            tools::CODE_SEARCH,
            &json!({"query":"secret", "path":"src"}),
        );

        assert_eq!(tracker.max_low_signal_count(), 0);
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
    }

    #[test]
    fn low_signal_tracker_counts_missing_read_failures() {
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::UNIFIED_FILE.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ResourceNotFound,
                "Resource not found: vtcode-tui/src/main.rs".to_string(),
            ),
        });

        // Two reads of the same path with different offsets are *different*
        // slices (paginated exploration), not a retry loop. The slice-aware
        // family key keeps them as distinct families, each with count 1.
        // Regression: previously both collapsed into one family with count 2,
        // which falsely tripped the family cap when the model paginated a
        // missing file (checkpoint turn_613 pattern).
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"vtcode-tui/src/main.rs"}),
        );
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"vtcode-tui/src/main.rs","offset":40}),
        );

        assert_eq!(
            tracker.max_low_signal_count(),
            1,
            "paginated reads (different offset) must be distinct families, not one family with count 2"
        );
    }

    #[test]
    fn low_signal_tracker_counts_identical_missing_read_failures() {
        // True retry loop: same path + same slice, repeated. The low-signal
        // count must accumulate so the turn balancer can stop the churn.
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
            error: vtcode_core::tools::registry::ToolExecutionError::new(
                tools::UNIFIED_FILE.to_string(),
                vtcode_core::tools::registry::ToolErrorType::ResourceNotFound,
                "Resource not found: vtcode-tui/src/main.rs".to_string(),
            ),
        });

        let identical_args = json!({"action":"read","path":"vtcode-tui/src/main.rs"});
        update_repetition_tracker(&mut tracker, &miss, tools::UNIFIED_FILE, &identical_args);
        update_repetition_tracker(&mut tracker, &miss, tools::UNIFIED_FILE, &identical_args);

        assert_eq!(
            tracker.max_low_signal_count(),
            2,
            "identical retry reads must accumulate into one family with count 2"
        );
    }

    #[test]
    fn low_signal_tracker_counts_grep_style_shell_misses() {
        let mut tracker = LoopTracker::new();
        let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({
                "command": "grep -n 'missing' vtcode-tui/src/main.rs",
                "exit_code": 1,
                "output": ""
            }),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::EXEC_COMMAND,
            &json!({"cmd":"grep -n 'missing' vtcode-tui/src/main.rs"}),
        );
        update_repetition_tracker(
            &mut tracker,
            &miss,
            tools::EXEC_COMMAND,
            &json!({"cmd":"grep -n \"missing\" vtcode-tui/src/main.rs"}),
        );

        assert_eq!(tracker.max_low_signal_count(), 2);
        assert_eq!(tracker.consecutive_low_signal_navigations, 2);
        assert_eq!(tracker.total_low_signal_navigations, 2);
    }

    #[test]
    fn low_signal_tracker_does_not_count_grep_style_errors_as_no_match() {
        let mut tracker = LoopTracker::new();
        let error = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({
                "command": "rg missing restricted",
                "exit_code": 2,
                "output": ""
            }),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });

        update_repetition_tracker(&mut tracker, &error, tools::EXEC_COMMAND, &json!({"cmd":"rg missing restricted"}));

        assert_eq!(tracker.max_low_signal_count(), 0);
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
        assert_eq!(tracker.total_low_signal_navigations, 0);
    }

    #[test]
    fn low_signal_tracker_does_not_hide_grep_errors_as_no_match() {
        let mut tracker = LoopTracker::new();
        let failure = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
            output: serde_json::json!({
                "command": "rg missing restricted",
                "exit_code": 1,
                "stdout": "",
                "stderr": "permission denied",
            }),
            stdout: None,
            modified_files: vec![],
            command_success: false,
        });

        update_repetition_tracker(&mut tracker, &failure, tools::EXEC_COMMAND, &json!({"cmd":"rg missing restricted"}));

        assert_eq!(tracker.max_low_signal_count(), 0);
        assert_eq!(tracker.consecutive_low_signal_navigations, 0);
    }

    // --- read_normalized_signature_key tests ---

    #[test]
    fn read_normalized_signature_key_normalizes_file_operation_read_offset() {
        let args_a = json!({"action": "read", "path": "src/lib.rs", "offset": 0, "limit": 100});
        let args_b = json!({"action": "read", "path": "src/lib.rs", "offset": 50, "limit": 200});
        let key_a = read_normalized_signature_key("file_operation", &args_a);
        let key_b = read_normalized_signature_key("file_operation", &args_b);
        assert_eq!(key_a, key_b, "same file read with different offset/limit should produce the same normalized key");
    }

    #[test]
    fn read_normalized_signature_key_preserves_encoding() {
        let utf8 = json!({"action": "read", "path": "src/lib.rs", "encoding": "utf8"});
        let base64 = json!({"action": "read", "path": "src/lib.rs", "encoding": "base64"});

        assert_ne!(
            read_normalized_signature_key("file_operation", &utf8),
            read_normalized_signature_key("file_operation", &base64),
            "different encodings produce different tool output and must not reuse one another"
        );
    }

    #[test]
    fn read_normalized_signature_key_differentiates_different_paths() {
        let args_a = json!({"action": "read", "path": "src/lib.rs"});
        let args_b = json!({"action": "read", "path": "src/main.rs"});
        let key_a = read_normalized_signature_key("file_operation", &args_a);
        let key_b = read_normalized_signature_key("file_operation", &args_b);
        assert_ne!(key_a, key_b, "different paths must produce different keys");
    }

    #[test]
    fn read_normalized_signature_key_includes_code_search_limit_and_normalises_filter_order() {
        let args_a = json!({
            "query": "Widget",
            "path": "src",
            "file_types": ["rust", "typescript"],
            "result_types": ["text", "definition"],
            "max_results": 10
        });
        let args_b = json!({
            "query": "Widget",
            "path": "src",
            "file_types": ["typescript", "rs"],
            "result_types": ["definition", "text"],
            "max_results": 100
        });
        let key_a = read_normalized_signature_key(tools::CODE_SEARCH, &args_a);
        let key_b = read_normalized_signature_key(tools::CODE_SEARCH, &args_b);
        assert_ne!(key_a, key_b, "different effective limits must not share one code-search replay identity");

        let args_default = json!({
            "query": " Widget ",
            "path": "src",
            "file_types": ["rs", "typescript"],
            "result_types": ["definition", "text"]
        });
        let args_explicit_default = json!({
            "query": "Widget",
            "path": "src",
            "file_types": ["typescript", "rust"],
            "result_types": ["text", "definition"],
            "max_results": 20
        });
        assert_eq!(
            read_normalized_signature_key(tools::CODE_SEARCH, &args_default),
            read_normalized_signature_key(tools::CODE_SEARCH, &args_explicit_default),
            "omitted and explicit default limits must share replay identity"
        );
    }

    #[test]
    fn read_normalized_signature_key_preserves_mutation_for_write() {
        let args_a = json!({"path": "src/lib.rs", "content": "old"});
        let args_b = json!({"path": "src/lib.rs", "content": "new"});
        let key_a = read_normalized_signature_key("file_operation", &args_a);
        let key_b = read_normalized_signature_key("file_operation", &args_b);
        assert_ne!(key_a, key_b, "mutating writes must NOT be normalized away");
    }

    #[test]
    fn find_duplicate_in_history_matches_normalized_read() {
        use vtcode_core::llm::provider as uni;

        // find_duplicate_in_history uses read_normalized_signature_key, which
        // strips offset/limit for file reads. A later unrelated Assistant batch
        // must not obscure the earlier matching call and result pair.

        // Verify normalization: same file + different offset/limit → same key
        let key_a = read_normalized_signature_key(
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"src/lib.rs","offset":0,"limit":100}),
        );
        let key_b = read_normalized_signature_key(
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"src/lib.rs","offset":50,"limit":500}),
        );
        assert_eq!(key_a, key_b, "same file read with different offset/limit should normalize to the same key");

        // Verify: different file → different key
        let key_c = read_normalized_signature_key(
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"src/main.rs","offset":0,"limit":100}),
        );
        assert_ne!(key_a, key_c, "different files must produce different normalized keys");

        // Verify: code-search result limits remain distinct while filter ordering normalises away.
        let s_key_a = read_normalized_signature_key(
            tools::CODE_SEARCH,
            &json!({"query":"Widget","path":"src","file_types":["rust","typescript"],"result_types":["text","definition"],"max_results":10}),
        );
        let s_key_b = read_normalized_signature_key(
            tools::CODE_SEARCH,
            &json!({"query":"Widget","path":"src","file_types":["typescript","rs"],"result_types":["definition","text"],"max_results":100}),
        );
        assert_ne!(s_key_a, s_key_b, "different effective limits must not share one code-search replay identity");

        // Verify: write NOT normalized
        let w_key_a = read_normalized_signature_key(
            tools::UNIFIED_FILE,
            &json!({"action":"write","path":"src/lib.rs","content":"old"}),
        );
        let w_key_b = read_normalized_signature_key(
            tools::UNIFIED_FILE,
            &json!({"action":"write","path":"src/lib.rs","content":"new"}),
        );
        assert_ne!(w_key_a, w_key_b, "writes must not be normalized away");

        // Verify: find_duplicate_in_history still works for EXACT match
        let mut history: Vec<uni::Message> = Vec::new();
        history.push(uni::Message::assistant_with_tools(
            "read".into(),
            vec![uni::ToolCall::function(
                "tc_exact".into(),
                tools::UNIFIED_FILE.into(),
                serde_json::to_string(&json!({"action":"read","path":"src/lib.rs","offset":0,"limit":100})).unwrap(),
            )],
        ));
        history.push(uni::Message {
            role: uni::MessageRole::Tool,
            content: uni::MessageContent::text("exact content".into()),
            tool_call_id: Some("tc_exact".into()),
            ..Default::default()
        });
        // Second pair (different file) so the scan finds A₀'s Tool after A₁:
        history.push(uni::Message::assistant_with_tools(
            "read other".into(),
            vec![uni::ToolCall::function(
                "tc_other".into(),
                tools::UNIFIED_FILE.into(),
                serde_json::to_string(&json!({"action":"read","path":"src/main.rs"})).unwrap(),
            )],
        ));
        history.push(uni::Message {
            role: uni::MessageRole::Tool,
            content: uni::MessageContent::text("other content".into()),
            tool_call_id: Some("tc_other".into()),
            ..Default::default()
        });

        let result = find_duplicate_in_history(
            &history,
            tools::UNIFIED_FILE,
            &json!({"action":"read","path":"src/lib.rs","offset":0,"limit":50}),
            Path::new("."),
        );
        assert_eq!(result.as_deref(), Some("exact content"));
    }

    #[test]
    fn find_duplicate_in_history_respects_normalised_code_search_limit() {
        let original_args = json!({
            "query": "Widget",
            "path": "src",
            "file_types": ["rust", "typescript"],
            "result_types": ["text", "definition"],
            "max_results": 10
        });
        let history = vec![
            uni::Message::assistant_with_tools(
                "search".into(),
                vec![uni::ToolCall::function(
                    "tc_search".into(),
                    tools::CODE_SEARCH.into(),
                    serde_json::to_string(&original_args).unwrap(),
                )],
            ),
            uni::Message {
                role: uni::MessageRole::Tool,
                content: uni::MessageContent::text("{\"results\":[]}".into()),
                tool_call_id: Some("tc_search".into()),
                ..Default::default()
            },
        ];

        let different_limit = find_duplicate_in_history(
            &history,
            tools::CODE_SEARCH,
            &json!({
                "query": "Widget",
                "path": "src",
                "file_types": ["typescript", "rs"],
                "result_types": ["definition", "text"],
                "max_results": 100
            }),
            Path::new("."),
        );

        assert_eq!(different_limit, None);

        let equivalent_default_history = vec![
            uni::Message::assistant_with_tools(
                "search".into(),
                vec![uni::ToolCall::function(
                    "tc_default".into(),
                    tools::CODE_SEARCH.into(),
                    serde_json::to_string(&json!({
                        "query": "Widget",
                        "path": "src",
                        "max_results": 20
                    }))
                    .unwrap(),
                )],
            ),
            uni::Message {
                role: uni::MessageRole::Tool,
                content: uni::MessageContent::text("{\"results\":[1]}".into()),
                tool_call_id: Some("tc_default".into()),
                ..Default::default()
            },
        ];
        let reused = find_duplicate_in_history(
            &equivalent_default_history,
            tools::CODE_SEARCH,
            &json!({"query": " Widget ", "path": "src"}),
            Path::new("."),
        );
        assert_eq!(reused.as_deref(), Some("{\"results\":[1]}"));
    }

    #[test]
    fn working_history_code_search_replay_stops_at_in_scope_mutation() {
        let search_args = json!({"query": "Widget", "path": "src"});
        let search_call = uni::Message::assistant_with_tools(
            "search".into(),
            vec![uni::ToolCall::function(
                "search_call".into(),
                tools::CODE_SEARCH.into(),
                serde_json::to_string(&search_args).unwrap(),
            )],
        );
        let search_result = uni::Message {
            role: uni::MessageRole::Tool,
            content: uni::MessageContent::text("{\"results\":[\"cached\"]}".into()),
            tool_call_id: Some("search_call".into()),
            ..Default::default()
        };
        let mutation = |path: &str, result: serde_json::Value| {
            let patch = format!("*** Begin Patch\n*** Update File: {path}\n@@\n-Widget\n+Gadget\n*** End Patch\n");
            vec![
                uni::Message::assistant_with_tools(
                    "edit".into(),
                    vec![uni::ToolCall::function(
                        "edit_call".into(),
                        tools::APPLY_PATCH.into(),
                        serde_json::to_string(&json!({"patch": patch})).unwrap(),
                    )],
                ),
                uni::Message::tool_response("edit_call".into(), result.to_string()),
            ]
        };

        let mut in_scope_history = vec![search_call.clone(), search_result.clone()];
        in_scope_history.extend(mutation("src/widget.rs", json!({"success": true})));
        assert!(
            find_duplicate_in_history(&in_scope_history, tools::CODE_SEARCH, &search_args, Path::new("."),).is_none(),
            "editing src/widget.rs after searching src must force a fresh search"
        );

        let mut status_success_history = vec![search_call.clone(), search_result.clone()];
        status_success_history
            .extend(mutation("src/widget.rs", json!({"status": "success", "output": "patch applied"})));
        assert!(
            find_duplicate_in_history(&status_success_history, tools::CODE_SEARCH, &search_args, Path::new("."),)
                .is_none(),
            "the established successful status shape must invalidate replay"
        );

        let mut unrelated_history = vec![search_call.clone(), search_result.clone()];
        unrelated_history.extend(mutation("tests/widget.rs", json!({"success": true})));
        assert_eq!(
            find_duplicate_in_history(&unrelated_history, tools::CODE_SEARCH, &search_args, Path::new("."),).as_deref(),
            Some("{\"results\":[\"cached\"]}"),
            "an unrelated edit may reuse the prior scoped search"
        );

        for failure in [
            json!({"success": false, "error": "patch rejected"}),
            json!({"error": {"message": "execution denied by policy"}}),
            json!({"failure_kind": "timeout"}),
            json!({"status": "failed"}),
            json!({"status": "denied"}),
            json!({"success": null}),
            json!({"output": "patch output without an outcome"}),
            json!(["non-object mutation output"]),
        ] {
            let mut failed_history = vec![search_call.clone(), search_result.clone()];
            failed_history.extend(mutation("src/widget.rs", failure));
            assert_eq!(
                find_duplicate_in_history(&failed_history, tools::CODE_SEARCH, &search_args, Path::new("."),)
                    .as_deref(),
                Some("{\"results\":[\"cached\"]}"),
                "a mutation without explicit positive success evidence must preserve reuse"
            );
        }

        let mut unexecuted_history = vec![search_call, search_result];
        let unexecuted_mutation = mutation("src/widget.rs", json!({"success": true}));
        unexecuted_history.push(unexecuted_mutation[0].clone());
        assert_eq!(
            find_duplicate_in_history(&unexecuted_history, tools::CODE_SEARCH, &search_args, Path::new("."),)
                .as_deref(),
            Some("{\"results\":[\"cached\"]}"),
            "an unexecuted mutation call must preserve reuse"
        );
    }

    #[test]
    fn mutation_tool_response_success_rejects_malformed_and_conflicting_shapes() {
        let response = |content: &str| uni::Message::tool_response("edit_call".into(), content.into());

        assert!(tool_response_is_success(&response(r#"{"success":true}"#)));
        assert!(tool_response_is_success(&response(r#"{"status":"success","output":"patch applied"}"#,)));

        for content in [
            "not json",
            "null",
            r#"{"success":null,"status":"success"}"#,
            r#"{"success":true,"status":"failed"}"#,
            r#"{"success":true,"failure_kind":"timeout"}"#,
            r#"{"success":true,"error":"execution denied"}"#,
        ] {
            assert!(
                !tool_response_is_success(&response(content)),
                "mutation outcome must not count as successful: {content}"
            );
        }
    }

    #[test]
    fn duplicate_history_reuse_rejects_failed_results() {
        let args = json!({"query": "needle", "path": "src"});
        let call = || {
            uni::Message::assistant_with_tools(
                "search".into(),
                vec![uni::ToolCall::function(
                    "search_call".into(),
                    tools::CODE_SEARCH.into(),
                    serde_json::to_string(&args).unwrap(),
                )],
            )
        };

        for failure in [
            r#"{"success":false,"output":"partial"}"#,
            r#"{"status":"timeout","output":"partial"}"#,
            r#"{"error":"permission denied"}"#,
            "Error: command failed",
            "timed out while reading",
            "failed to execute command",
            "denied by policy",
            "blocked until verification",
            "not executed",
        ] {
            let history = vec![
                call(),
                uni::Message::tool_response("search_call".into(), failure.into()),
            ];
            assert!(
                find_duplicate_in_history(&history, tools::CODE_SEARCH, &args, Path::new(".")).is_none(),
                "failed result must not be replayed: {failure}"
            );
        }

        for success in [r#"{"results":[]}"#, "[]", "plain successful output"] {
            let history = vec![
                call(),
                uni::Message::tool_response("search_call".into(), success.into()),
            ];
            assert_eq!(
                find_duplicate_in_history(&history, tools::CODE_SEARCH, &args, Path::new(".")).as_deref(),
                Some(success)
            );
        }
    }

    #[test]
    fn working_history_code_search_replay_rejects_reused_patch_call_id() {
        let search_args = json!({"query": "Widget", "path": "src"});
        let shared_call_id = "call_0";
        let search_call = uni::Message::assistant_with_tools(
            "search".into(),
            vec![uni::ToolCall::function(
                shared_call_id.into(),
                tools::CODE_SEARCH.into(),
                serde_json::to_string(&search_args).unwrap(),
            )],
        );
        let search_result =
            uni::Message::tool_response(shared_call_id.into(), "{\"results\":[\"genuine search output\"]}".into());
        let patch = "*** Begin Patch\n*** Update File: src/widget.rs\n@@\n-Widget\n+Gadget\n*** End Patch\n";
        let patch_call = uni::Message::assistant_with_tools(
            "edit".into(),
            vec![uni::ToolCall::function(
                shared_call_id.into(),
                tools::APPLY_PATCH.into(),
                serde_json::to_string(&json!({"patch": patch})).unwrap(),
            )],
        );

        let mut successful_history = vec![
            search_call.clone(),
            search_result.clone(),
            patch_call.clone(),
            uni::Message::tool_response(
                shared_call_id.into(),
                json!({"success": true, "output": "patch output"}).to_string(),
            ),
        ];
        assert!(
            find_duplicate_in_history(&successful_history, tools::CODE_SEARCH, &search_args, Path::new("."),).is_none(),
            "a successful in-scope patch must invalidate the genuine earlier search result"
        );

        successful_history.pop();
        successful_history.push(uni::Message::tool_response(
            shared_call_id.into(),
            json!({"success": false, "error": "patch rejected", "output": "patch output"}).to_string(),
        ));
        assert_eq!(
            find_duplicate_in_history(&successful_history, tools::CODE_SEARCH, &search_args, Path::new("."),)
                .as_deref(),
            Some("{\"results\":[\"genuine search output\"]}"),
            "a failed patch must preserve the earlier search without returning patch output"
        );
    }

    #[test]
    fn read_extent_covers_query_rejects_larger_limit() {
        // Cached limit=200 must NOT cover query limit=220
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":220}),
        ));

        // Cached limit=200 covers query limit=200 (same)
        assert!(read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        ));

        // Cached limit=200 covers query limit=100 (subset)
        assert!(read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":100}),
        ));

        // Different offset must not match
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
            &json!({"action":"read","path":"AGENTS.md","offset":50,"limit":200}),
        ));
    }

    #[test]
    fn read_extent_covers_query_rejects_different_raw_mode() {
        // Non-raw cached must NOT cover raw=true query
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
        ));

        // Raw cached covers raw query
        assert!(read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
        ));

        // Raw cached must NOT cover non-raw query
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200,"raw":true}),
            &json!({"action":"read","path":"AGENTS.md","offset":0,"limit":200}),
        ));
    }

    #[test]
    fn read_extent_covers_query_handles_missing_limit() {
        // Both missing limit → matches (same default read)
        assert!(read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md"}),
            &json!({"action":"read","path":"AGENTS.md"}),
        ));

        // Cached has limit, query doesn't → mismatch
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md","limit":200}),
            &json!({"action":"read","path":"AGENTS.md"}),
        ));

        // Cached has no limit, query does → mismatch
        assert!(!read_extent::extent_covers(
            &json!({"action":"read","path":"AGENTS.md"}),
            &json!({"action":"read","path":"AGENTS.md","limit":200}),
        ));
    }
}
