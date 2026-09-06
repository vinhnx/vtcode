//! Metrics-related accessors for ToolRegistry.

use super::ToolRegistry;

impl ToolRegistry {
    /// Return the shared metrics collector for this registry instance.
    pub fn metrics_collector(&self) -> std::sync::Arc<crate::metrics::MetricsCollector> {
        self.metrics.clone()
    }

    /// Get total tool calls made in current session (for observability).
    pub fn tool_call_count(&self) -> u64 {
        self.tool_call_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get total PTY poll iterations (for CPU monitoring).
    pub fn pty_poll_count(&self) -> u64 {
        self.pty_poll_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Reset the aggregate provider-visible preview ledger at a turn boundary.
    ///
    /// Call once per turn from each runloop so the 32 KiB budget in
    /// `TURN_PREVIEW_BUDGET_BYTES` applies per turn rather than per session.
    pub fn begin_turn_preview_window(&self) {
        self.turn_preview_bytes.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Charge `bytes` against the per-turn preview budget, returning the
    /// total before the charge. Consulted by `process_tool_output` to
    /// truncate or strip payload bodies once the budget is exhausted.
    pub(super) fn charge_turn_preview_bytes(&self, bytes: usize) -> usize {
        self.turn_preview_bytes.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed)
    }
}
