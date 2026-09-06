//! Immutable, cache-stable request envelope for one harness segment.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::llm::provider::ToolDefinition;

use super::hash_utils::{hash_tool_definitions, hash_value};

/// A change that intentionally ends the current cache-stable segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentBoundaryReason {
    Compaction,
    Recovery,
    ToolCatalogEpoch,
    PrimaryAgent,
    Model,
    Provider,
    Mode,
    Instructions,
}

impl SegmentBoundaryReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Recovery => "recovery",
            Self::ToolCatalogEpoch => "tool_catalog_epoch",
            Self::PrimaryAgent => "primary_agent",
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Mode => "mode",
            Self::Instructions => "instructions",
        }
    }
}

/// Immutable request prefix shared by every request in a session segment.
#[derive(Debug, Clone)]
pub struct SessionRequestEnvelope {
    segment_id: Arc<str>,
    system_prompt: Arc<str>,
    ordered_tools: Arc<Vec<ToolDefinition>>,
    instruction_digest: u64,
    prefix_hash: u64,
    catalog_hash: Option<u64>,
}

impl SessionRequestEnvelope {
    /// Freeze a segment prefix. Tool ordering is canonicalized once here and
    /// must not be rebuilt by individual turns.
    #[must_use]
    pub fn new(
        segment_id: impl Into<Arc<str>>,
        system_prompt: impl Into<Arc<str>>,
        tools: Vec<ToolDefinition>,
        instruction_digest: u64,
    ) -> Self {
        let system_prompt = system_prompt.into();
        let prefix_hash = hash_value(&system_prompt);
        Self::with_prefix_hash(segment_id, system_prompt, tools, instruction_digest, prefix_hash)
    }

    /// Freeze a segment prefix while retaining a caller-provided instruction
    /// digest for compatibility with request identity tracking.
    #[must_use]
    pub fn with_prefix_hash(
        segment_id: impl Into<Arc<str>>,
        system_prompt: impl Into<Arc<str>>,
        mut tools: Vec<ToolDefinition>,
        instruction_digest: u64,
        prefix_hash: u64,
    ) -> Self {
        tools.sort_by(compare_tools);
        let system_prompt = system_prompt.into();
        let catalog_hash = if tools.is_empty() {
            None
        } else {
            hash_tool_definitions(Some(&tools))
        };
        Self {
            segment_id: segment_id.into(),
            system_prompt,
            ordered_tools: Arc::new(tools),
            instruction_digest,
            prefix_hash,
            catalog_hash,
        }
    }

    #[must_use]
    pub fn segment_id(&self) -> &str {
        &self.segment_id
    }

    #[must_use]
    pub fn system_prompt(&self) -> Arc<str> {
        Arc::clone(&self.system_prompt)
    }

    #[must_use]
    pub fn ordered_tools(&self) -> Arc<Vec<ToolDefinition>> {
        Arc::clone(&self.ordered_tools)
    }

    #[must_use]
    pub fn instruction_digest(&self) -> u64 {
        self.instruction_digest
    }

    #[must_use]
    pub fn prefix_hash(&self) -> u64 {
        self.prefix_hash
    }

    /// Include the resolved capability digest without changing frozen prompt bytes.
    #[must_use]
    pub fn with_capability_digest(mut self, digest: u64) -> Self {
        self.prefix_hash = hash_value(&(self.prefix_hash, digest));
        self
    }

    #[must_use]
    pub fn catalog_hash(&self) -> Option<u64> {
        self.catalog_hash
    }
}

fn core_priority(name: &str) -> u8 {
    match name {
        "exec_command" => 0,
        "write_stdin" => 1,
        "search_tools" | "mcp_search_tools" => 2,
        "apply_patch" => 3,
        _ => 4,
    }
}

fn compare_tools(left: &ToolDefinition, right: &ToolDefinition) -> Ordering {
    let left_name = left.function_name();
    let right_name = right.function_name();
    core_priority(left_name)
        .cmp(&core_priority(right_name))
        .then_with(|| left_name.cmp(right_name))
        .then_with(|| left.tool_type.cmp(&right.tool_type))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition::function(name.to_string(), name.to_string(), json!({"type": "object"}))
    }

    #[test]
    fn equivalent_cold_starts_are_byte_identical() {
        let left = SessionRequestEnvelope::new(
            "segment-1",
            "fixed prompt",
            vec![tool("zeta"), tool("exec_command"), tool("alpha")],
            42,
        );
        let right = SessionRequestEnvelope::new(
            "segment-1",
            "fixed prompt",
            vec![tool("alpha"), tool("zeta"), tool("exec_command")],
            42,
        );

        assert_eq!(left.system_prompt.as_bytes(), right.system_prompt.as_bytes());
        assert_eq!(
            serde_json::to_vec(left.ordered_tools.as_ref()).expect("serialize left catalog"),
            serde_json::to_vec(right.ordered_tools.as_ref()).expect("serialize right catalog")
        );
        assert_eq!(left.catalog_hash, right.catalog_hash);
    }

    #[test]
    fn core_priority_precedes_name_order() {
        let envelope = SessionRequestEnvelope::new(
            "segment-1",
            "fixed prompt",
            vec![
                tool("alpha"),
                tool("apply_patch"),
                tool("search_tools"),
                tool("exec_command"),
            ],
            42,
        );
        let names = envelope
            .ordered_tools
            .iter()
            .map(ToolDefinition::function_name)
            .collect::<Vec<_>>();

        assert_eq!(names, ["exec_command", "search_tools", "apply_patch", "alpha"]);
    }

    #[test]
    fn constructor_preserves_instruction_digest_and_prefix_override() {
        let envelope = SessionRequestEnvelope::new("segment-1", "fixed prompt", vec![], 42);
        assert_eq!(envelope.instruction_digest(), 42);
        assert_eq!(envelope.prefix_hash(), hash_value(&"fixed prompt"));

        let stable = SessionRequestEnvelope::with_prefix_hash("segment-1", "runtime prompt", vec![], 42, 7);
        assert_eq!(stable.instruction_digest(), 42);
        assert_eq!(stable.prefix_hash(), 7);
    }
}
