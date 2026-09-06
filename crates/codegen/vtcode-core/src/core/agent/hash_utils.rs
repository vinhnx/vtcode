//! Hash utilities for tool catalog and system prompt caching.
//!
//! Provides hashing functions for tool definitions, system prompts, and
//! low-signal attempt deduplication keys.
//!
//! All hashes are stable across processes (FNV-1a) so cache keys remain
//! comparable after restarts and in persisted harness artifacts.

use std::hash::{Hash, Hasher};

use serde::Serialize;
use serde_json::Value;

use crate::llm::provider::ToolDefinition;

/// Resolved wire capabilities that define a cache-stable request segment.
/// Runtime counters and environment observations deliberately do not belong here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PromptCapabilityIdentity {
    provider: compact_str::CompactString,
    model: compact_str::CompactString,
    context_window: usize,
    reasoning_tag: compact_str::CompactString,
    reasoning_effort: bool,
    parallel_tools: bool,
    tools: bool,
    caching: bool,
    catalog_epoch: u64,
}

impl PromptCapabilityIdentity {
    /// Construct the cache identity available from the generated model catalog.
    ///
    /// The runtime provider path should prefer [`Self::resolve`], because it
    /// can observe provider-specific capabilities such as parallel tool
    /// configuration. Static prompt assembly does not own a provider object,
    /// so it uses this catalog-only identity and keeps missing capability bits
    /// explicit rather than falling back to a model-name hash.
    #[must_use]
    pub fn from_catalog(
        provider: &str,
        model: &str,
        reasoning: Option<crate::config::types::ReasoningEffortLevel>,
        catalog_epoch: u64,
        catalog: Option<vtcode_config::models::ModelCatalogEntry>,
    ) -> Self {
        let context_window = catalog.map(|entry| entry.context_window).unwrap_or_default();
        let reasoning_effort = catalog.is_some_and(|entry| !entry.reasoning_efforts.is_empty());
        let tools = catalog.map(|entry| entry.tool_call).unwrap_or(true);
        let caching = catalog.is_some_and(|entry| entry.caching);
        Self {
            provider: provider.into(),
            model: model.into(),
            context_window,
            reasoning_tag: reasoning.map_or_else(|| "unset".into(), |effort| effort.to_string().into()),
            reasoning_effort,
            // Static prompt assembly has no provider instance from which to
            // resolve this wire capability. Runtime request construction
            // replaces this with the Provider-trait value.
            parallel_tools: false,
            tools,
            caching,
            catalog_epoch,
        }
    }

    /// Prefer discovery/catalog capacity when the caller already resolved a model.
    #[must_use]
    pub fn with_resolved_model(mut self, resolved: &crate::llm::model_resolver::ResolvedModel) -> Self {
        self.context_window = resolved.context_window().unwrap_or(self.context_window);
        self
    }
    #[must_use]
    pub fn resolve(
        provider: &dyn crate::llm::provider::LLMProvider,
        model: &str,
        reasoning: Option<crate::config::types::ReasoningEffortLevel>,
        catalog_epoch: u64,
    ) -> Self {
        Self {
            provider: provider.name().into(),
            model: model.into(),
            context_window: provider.effective_context_size(model),
            reasoning_tag: reasoning.map_or_else(|| "unset".into(), |effort| effort.to_string().into()),
            reasoning_effort: !provider.supported_reasoning_efforts(model).is_empty(),
            parallel_tools: provider.supports_parallel_tool_config(model),
            tools: provider.supports_tools(model),
            caching: provider.supports_context_caching(model),
            catalog_epoch,
        }
    }

    #[must_use]
    pub fn prefix_hash(&self, prompt_hash: u64) -> u64 {
        hash_value(&(prompt_hash, self))
    }

    /// Return the digest for the resolved capabilities alone. Callers that
    /// also have a stable prompt hash can combine the two with
    /// [`Self::prefix_hash`].
    #[must_use]
    pub fn digest(&self) -> u64 {
        hash_value(self)
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn stable_hasher_is_deterministic_across_instances() {
        assert_eq!(hash_value(&"cache-key"), hash_value(&"cache-key"));
        assert_ne!(hash_value(&"cache-key-a"), hash_value(&"cache-key-b"));
    }

    #[test]
    fn stable_prefix_hash_strips_all_runtime_sections() {
        let base = "Base prompt\n[Harness Limits]\n- max_tool_calls_per_turn: 5";
        for suffix in [
            "\n\n## Active Tools\n- Capabilities: read-only.",
            "\n\n[Runtime Tool Catalog]\n- version: 1",
            "\n\n[Deferred Tools]\n- code_search (2 tools): search",
            "\n\n[Runtime Context]\n- turns: 1",
            "\n\n[History Directives]\n- reuse prior tool outputs",
            "\n\n[Context]\n- workspace: /tmp",
        ] {
            let with_section = format!("{base}{suffix}");
            assert_eq!(
                stable_system_prefix_hash(base),
                stable_system_prefix_hash(&with_section),
                "runtime section should not affect prefix hash: {suffix:?}"
            );
        }
    }

    #[test]
    fn capability_identity_resolves_three_provider_families_without_transport() {
        use crate::config::constants::models;
        use crate::config::types::ReasoningEffortLevel;
        use crate::llm::provider::LLMProvider;
        use crate::llm::providers::{AnthropicProvider, GeminiProvider, OpenAIProvider};

        let providers: [(Box<dyn LLMProvider>, &str); 3] = [
            (Box::new(OpenAIProvider::new("offline-fixture".into())), models::openai::DEFAULT_MODEL),
            (Box::new(AnthropicProvider::new("offline-fixture".into())), models::anthropic::DEFAULT_MODEL),
            (Box::new(GeminiProvider::new("offline-fixture".into())), models::google::DEFAULT_MODEL),
        ];
        for (provider, model) in providers {
            let identity =
                PromptCapabilityIdentity::resolve(provider.as_ref(), model, Some(ReasoningEffortLevel::High), 1);
            assert_eq!(identity.context_window, provider.effective_context_size(model));
            assert_eq!(identity.parallel_tools, provider.supports_parallel_tool_config(model));
            assert_eq!(identity.reasoning_effort, !provider.supported_reasoning_efforts(model).is_empty());
            let repeat =
                PromptCapabilityIdentity::resolve(provider.as_ref(), model, Some(ReasoningEffortLevel::High), 1);
            assert_eq!(identity.prefix_hash(7), repeat.prefix_hash(7));
            let refreshed =
                PromptCapabilityIdentity::resolve(provider.as_ref(), model, Some(ReasoningEffortLevel::High), 2);
            assert_ne!(identity.prefix_hash(7), refreshed.prefix_hash(7));
        }
    }

    #[test]
    fn capability_prefix_tracks_changes_for_three_provider_families() {
        for (provider, model, context_window) in [
            ("openai", "openai-fixture", 1_000_000),
            ("anthropic", "claude-fixture", 200_000),
            ("gemini", "gemini-fixture", 32_000),
        ] {
            let identity = PromptCapabilityIdentity {
                provider: provider.into(),
                model: model.into(),
                context_window,
                reasoning_tag: "high".into(),
                reasoning_effort: true,
                parallel_tools: true,
                tools: true,
                caching: true,
                catalog_epoch: 1,
            };
            let baseline = identity.prefix_hash(7);
            assert_eq!(baseline, identity.clone().prefix_hash(7));
            let mut changed = identity.clone();
            changed.catalog_epoch += 1;
            assert_ne!(baseline, changed.prefix_hash(7));
            changed = identity.clone();
            changed.reasoning_tag = "low".into();
            assert_ne!(baseline, changed.prefix_hash(7));
            changed = identity.clone();
            changed.context_window /= 2;
            assert_ne!(baseline, changed.prefix_hash(7));
            changed = identity;
            changed.parallel_tools = false;
            assert_ne!(baseline, changed.prefix_hash(7));
        }
    }
}

/// Stable FNV-1a 64-bit hasher with fixed offset basis.
///
/// `std::collections::hash_map::DefaultHasher` uses per-process random keys,
/// so hashes differ across restarts. Cache and harness keys must be comparable
/// across processes, hence this deterministic alternative shared by
/// [`hash_value`], [`hash_json_value`], and execution-cache builders.
#[derive(Debug, Clone)]
pub struct StableHasher {
    hash: u64,
}

impl Default for StableHasher {
    fn default() -> Self {
        Self { hash: 0xcbf29ce484222325 }
    }
}

impl StableHasher {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Hasher for StableHasher {
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.hash ^= u64::from(*byte);
            self.hash = self.hash.wrapping_mul(0x100000001b3);
        }
    }

    fn finish(&self) -> u64 {
        self.hash
    }
}

/// Hash a value with a stable cross-process hash.
pub fn hash_value<T: Hash>(value: &T) -> u64 {
    let mut hasher = StableHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Hash a serializable value as JSON with a stable cross-process hash.
pub fn hash_json_value<T: Serialize + ?Sized>(value: &T) -> Option<u64> {
    let mut hasher = StableHasher::new();
    serde_json::to_writer(HasherWriter::new(&mut hasher), value).ok().map(|_| {
        hasher.write_u8(0xff);
        hasher.finish()
    })
}

/// Hash tool definitions for cache key computation.
pub fn hash_tool_definitions(tools: Option<&[ToolDefinition]>) -> Option<u64> {
    tools.and_then(hash_json_value)
}

/// Compute a stable hash of the system prompt prefix.
///
/// Strips runtime sections (tool catalog, context, active tools) so the hash
/// remains stable across turns even as runtime context changes.
///
/// Section boundaries share [`crate::prompts::sections::find_prompt_section_bounds`]
/// semantics with prompt construction (`BracketOrMarkdown`), so a renamed
/// runtime header cannot silently change cache identity.
///
/// Keep `RUNTIME_HEADERS` in sync with the Anthropic wire split in
/// `vtcode-llm/src/providers/anthropic/request_builder/system.rs`
/// (`split_runtime_context_section`): both must treat `[History Directives]`
/// as dynamic per-turn content or cached-prefix identity diverges from the
/// actual cache breakpoint.
pub fn stable_system_prefix_hash(system_prompt: &str) -> u64 {
    const RUNTIME_HEADERS: &[&str] = &[
        "## Active Tools",
        "[Runtime Tool Catalog]",
        "[Deferred Tools]",
        "[Runtime Context]",
        "[History Directives]",
        "[Context]",
    ];
    let earliest = RUNTIME_HEADERS
        .iter()
        .filter_map(|header| {
            crate::prompts::sections::find_prompt_section_bounds(
                system_prompt,
                header,
                crate::prompts::sections::SectionBoundaryMode::BracketOrMarkdown,
            )
            .map(|(start, _)| start)
        })
        .min();
    let stable_prefix = earliest.map(|start| &system_prompt[..start]).unwrap_or(system_prompt);
    hash_value(&stable_prefix.trim_end())
}

/// Generate a deduplication key for low-signal tool attempts.
pub fn low_signal_attempt_key(name: &str, args: &Value) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut input_len = 0usize;
    if serde_json::to_writer(HashingWriter::new(&mut hash, &mut input_len), args).is_err() {
        for byte in b"{}" {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
            input_len = input_len.saturating_add(1);
        }
    }

    format!("{name}:len{input_len}-fnv{hash:016x}")
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

struct HasherWriter<'a, H> {
    hasher: &'a mut H,
}

impl<'a, H> HasherWriter<'a, H> {
    fn new(hasher: &'a mut H) -> Self {
        Self { hasher }
    }
}

impl<H: Hasher> std::io::Write for HasherWriter<'_, H> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
