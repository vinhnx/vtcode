/// Head ratio percentage for code files (legacy, kept for compatibility)
pub const CODE_HEAD_RATIO_PERCENT: usize = 60;

/// Head ratio percentage for log files (legacy, kept for compatibility)
pub const LOG_HEAD_RATIO_PERCENT: usize = 20;

// =========================================================================
// Context Window Sizes
// =========================================================================

/// Standard context window size (200K tokens) - default for most models
pub const STANDARD_CONTEXT_WINDOW: usize = 200_000;

/// Extended context window size (1M tokens) - beta feature
/// Available for Claude Sonnet 4, Sonnet 4.5 in usage tier 4
/// Requires beta header: "context-1m-2025-08-07"
pub const EXTENDED_CONTEXT_WINDOW: usize = 1_000_000;

/// Claude.ai Enterprise context window (500K tokens)
pub const ENTERPRISE_CONTEXT_WINDOW: usize = 500_000;

// =========================================================================
// Compaction Trigger Ratios
// =========================================================================

/// Legacy auto-compaction ratio retained for compatibility with older strategy
/// configuration. Runtime threshold resolution is capacity- and reserve-driven.
pub const DEFAULT_COMPACTION_TRIGGER_RATIO: f64 = 0.90;

// =========================================================================
// Extended Thinking Token Management
// =========================================================================

/// Minimum budget tokens for extended thinking (Anthropic requirement)
pub const MIN_THINKING_BUDGET: u32 = 1_024;

/// Recommended budget tokens for complex reasoning tasks
pub const RECOMMENDED_THINKING_BUDGET: u32 = 10_000;

/// Default thinking budget for production use (64K output models: Opus 4.5, Sonnet 4.5, Haiku 4.5)
/// Extended thinking is now auto-enabled by default as of January 2026
pub const DEFAULT_THINKING_BUDGET: u32 = 31_999;

/// Maximum thinking budget for 64K output models (Opus 4.5, Sonnet 4.5, Haiku 4.5)
/// Use MAX_THINKING_TOKENS=63999 environment variable to enable this
pub const MAX_THINKING_BUDGET_64K: u32 = 63_999;

/// Maximum thinking budget for 32K output models (Opus 4)
pub const MAX_THINKING_BUDGET_32K: u32 = 31_999;

// =========================================================================
// Beta Headers
// =========================================================================

/// Beta header for 1M token context window
/// Include in requests to enable extended context for Sonnet 4/4.5
pub const BETA_CONTEXT_1M: &str = "context-1m-2025-08-07";

/// Models eligible for 1M context window (beta)
/// Requires usage tier 4 or custom rate limits
pub const EXTENDED_CONTEXT_ELIGIBLE_MODELS: &[&str] = &[
    crate::constants::models::anthropic::CLAUDE_SONNET_5,
    crate::constants::models::anthropic::CLAUDE_OPUS_5,
];

/// Check if a model is eligible for 1M context window
pub fn supports_extended_context(model: &str) -> bool {
    EXTENDED_CONTEXT_ELIGIBLE_MODELS.iter().any(|m| model.contains(m))
}
