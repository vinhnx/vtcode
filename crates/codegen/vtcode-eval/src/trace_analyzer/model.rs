use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Aggregate token and prompt-cache usage found in a trace.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// Total prompt/input tokens.
    pub input_tokens: u64,
    /// Total generated/output tokens.
    pub output_tokens: u64,
    /// Total prompt tokens served from cache.
    pub cached_input_tokens: u64,
    /// Total tokens used to create cache entries.
    pub cache_creation_tokens: u64,
    /// Total generated reasoning tokens when the provider reports them.
    pub reasoning_tokens: u64,
}

/// Statistics over recorded latency samples, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct LatencyStatistics {
    /// Number of latency samples.
    pub count: u64,
    /// Sum of latency samples.
    pub total_ms: u64,
    /// Arithmetic mean, or `None` when no samples were recorded.
    pub mean_ms: Option<f64>,
    /// Median from the bounded latency reservoir, or `None` when no samples were recorded.
    pub p50_ms: Option<u64>,
    /// 95th percentile from the bounded latency reservoir, or `None` when no samples were recorded.
    pub p95_ms: Option<u64>,
    /// Largest recorded sample, or `None` when no samples were recorded.
    pub max_ms: Option<u64>,
}

/// Redacted aggregate facts extracted from DeepSeek or VT Code JSONL traces.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HarnessTraceSummary {
    /// Number of execution turns.
    pub turns: u64,
    /// Number of agent steps.
    pub steps: u64,
    /// Number of tool calls.
    pub tool_calls: u64,
    /// Tool name to invocation count.
    pub tool_counts: BTreeMap<String, u64>,
    /// Canonical error category to count.
    pub error_categories: BTreeMap<String, u64>,
    /// Latency aggregate for all recognized samples.
    pub latency: LatencyStatistics,
    /// Total UTF-8 byte length of tool outputs, without retaining output text.
    pub output_bytes: u64,
    /// Number of calls after the first call for each tool name.
    pub repeated_calls: u64,
    /// Repeated calls grouped by tool name.
    pub repeated_tool_counts: BTreeMap<String, u64>,
    /// Aggregate model token usage.
    pub token_usage: TokenUsage,
    /// Lines that were not valid JSON objects.
    pub malformed_lines: u64,
    /// Valid JSON objects with no recognized trace shape.
    pub unrecognized_lines: u64,
}

impl HarnessTraceSummary {
    /// Merge another privacy-preserving trace summary into this aggregate.
    /// Percentiles are retained only for a single source because they cannot
    /// be combined exactly without retaining raw latency samples.
    pub fn merge(&mut self, other: &Self) {
        self.turns = self.turns.saturating_add(other.turns);
        self.steps = self.steps.saturating_add(other.steps);
        self.tool_calls = self.tool_calls.saturating_add(other.tool_calls);
        self.output_bytes = self.output_bytes.saturating_add(other.output_bytes);
        self.repeated_calls = self.repeated_calls.saturating_add(other.repeated_calls);
        self.malformed_lines = self.malformed_lines.saturating_add(other.malformed_lines);
        self.unrecognized_lines = self.unrecognized_lines.saturating_add(other.unrecognized_lines);

        for (tool, count) in &other.tool_counts {
            let entry = self.tool_counts.entry(tool.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }
        for (tool, count) in &other.repeated_tool_counts {
            let entry = self.repeated_tool_counts.entry(tool.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }
        for (category, count) in &other.error_categories {
            let entry = self.error_categories.entry(category.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }

        let previous_latency_count = self.latency.count;
        let combined_count = previous_latency_count.saturating_add(other.latency.count);
        self.latency.total_ms = self.latency.total_ms.saturating_add(other.latency.total_ms);
        self.latency.count = combined_count;
        self.latency.mean_ms = (combined_count > 0).then_some(self.latency.total_ms as f64 / combined_count as f64);
        self.latency.max_ms = match (self.latency.max_ms, other.latency.max_ms) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        };
        if previous_latency_count > 0 && other.latency.count > 0 {
            self.latency.p50_ms = None;
            self.latency.p95_ms = None;
        } else if previous_latency_count == 0 {
            self.latency.p50_ms = other.latency.p50_ms;
            self.latency.p95_ms = other.latency.p95_ms;
        }

        self.token_usage.input_tokens = self.token_usage.input_tokens.saturating_add(other.token_usage.input_tokens);
        self.token_usage.output_tokens = self.token_usage.output_tokens.saturating_add(other.token_usage.output_tokens);
        self.token_usage.cached_input_tokens = self
            .token_usage
            .cached_input_tokens
            .saturating_add(other.token_usage.cached_input_tokens);
        self.token_usage.cache_creation_tokens = self
            .token_usage
            .cache_creation_tokens
            .saturating_add(other.token_usage.cache_creation_tokens);
        self.token_usage.reasoning_tokens = self
            .token_usage
            .reasoning_tokens
            .saturating_add(other.token_usage.reasoning_tokens);
    }
}
