use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::config::types::{AgentConfig as CoreAgentConfig, ReasoningEffortLevel};
use vtcode_core::context::{ConversationMemory, EntityResolver, WorkspaceState};
use vtcode_core::llm::{
    LightweightFeature, collect_single_response, create_provider_for_model_route, provider as uni,
    resolve_lightweight_route,
};

const MIN_PROMPT_LENGTH_FOR_REFINEMENT: usize = 20;
const MIN_PROMPT_WORDS_FOR_REFINEMENT: usize = 4;
const SHORT_PROMPT_WORD_THRESHOLD: usize = 6;
const MAX_REFINED_WORD_MULTIPLIER: usize = 3;
const MIN_KEYWORD_LENGTH: usize = 3;
const MIN_KEYWORD_OVERLAP_RATIO: f32 = 0.5;

#[path = "prompt_refinement.rs"]
mod prompt_refinement;
#[cfg(test)]
#[path = "prompt_tests.rs"]
mod prompt_tests;
use prompt_refinement::{should_accept_refinement, should_attempt_refinement};

/// Combined refinement and enrichment function (Phase 3 integration)
pub(crate) async fn refine_and_enrich_prompt(
    raw: &str,
    cfg: &CoreAgentConfig,
    vt_cfg: Option<&VTCodeConfig>,
) -> String {
    // Step 1: Apply standard refinement first
    let refined = refine_user_prompt_if_enabled(raw, cfg, vt_cfg).await;

    // Step 2: Apply vibe coding enrichment if enabled
    if let Some(vtc) = vt_cfg
        && should_enrich_prompt(&refined, Some(vtc))
    {
        let enricher = PromptEnricher::new(cfg.workspace.clone(), vtc.clone());
        let enriched = enricher.enrich_vague_prompt(&refined).await;
        return enriched.to_llm_prompt();
    }

    refined
}

pub(crate) async fn refine_user_prompt_if_enabled(
    raw: &str,
    cfg: &CoreAgentConfig,
    vt_cfg: Option<&VTCodeConfig>,
) -> String {
    let Some(vtc) = vt_cfg else {
        return raw.to_string();
    };
    if !vtc.agent.refine_prompts_enabled {
        return raw.to_string();
    }
    if std::env::var("VTCODE_PROMPT_REFINER_STUB").is_ok() {
        return format!("[REFINED] {raw}");
    }

    if !should_attempt_refinement(raw) {
        return raw.to_string();
    }

    let routes = resolve_lightweight_route(
        cfg,
        Some(vtc),
        LightweightFeature::PromptRefinement,
        Some(vtc.agent.refine_prompts_model.as_str()),
    );
    if let Some(warning) = &routes.warning {
        tracing::warn!(warning = %warning, "prompt refinement route adjusted");
    }

    match try_refine_prompt_with_route(raw, cfg, Some(vtc), &routes.primary).await {
        Ok(Some(text)) => text,
        Ok(None) | Err(_) if routes.fallback_to_main_model().is_some() => {
            let fallback = routes.fallback_to_main_model().expect("checked fallback");
            tracing::warn!(
                model = %routes.primary.model,
                fallback_model = %fallback.model,
                "prompt refinement failed on lightweight route; retrying with main model"
            );
            match try_refine_prompt_with_route(raw, cfg, Some(vtc), fallback).await {
                Ok(Some(text)) => text,
                _ => raw.to_string(),
            }
        }
        _ => raw.to_string(),
    }
}

async fn try_refine_prompt_with_route(
    raw: &str,
    cfg: &CoreAgentConfig,
    vt_cfg: Option<&VTCodeConfig>,
    route: &vtcode_core::llm::ModelRoute,
) -> Result<Option<String>, anyhow::Error> {
    let Some(vt_cfg) = vt_cfg else {
        return Ok(None);
    };
    let refiner = create_provider_for_model_route(route, cfg, Some(vt_cfg))?;
    let reasoning_effort = if vt_cfg.agent.reasoning_effort != ReasoningEffortLevel::None {
        let mapping = vtcode_core::llm::reasoning_effort::ReasoningEffortMapper::resolve(
            refiner.as_ref(),
            &route.model,
            vt_cfg.agent.reasoning_effort,
            vt_cfg.agent.allow_reasoning_effort_downgrade,
        )?;
        if mapping.degraded() {
            tracing::warn!(
                requested = %mapping.requested,
                effective = %mapping.effective,
                model = %route.model,
                "Prompt-refinement reasoning effort explicitly downgraded"
            );
        }
        Some(mapping.effective)
    } else {
        None
    };
    let temperature = if reasoning_effort.is_some() && matches!(route.provider_name.as_str(), "anthropic" | "minimax") {
        None
    } else {
        Some(vt_cfg.agent.refine_temperature)
    };
    let req = uni::LLMRequest {
        messages: Arc::new(vec![uni::Message::user(raw.to_string())]),
        model: route.model.clone(),
        temperature,
        tool_choice: Some(uni::ToolChoice::none()),
        reasoning_effort,
        ..Default::default()
    };

    let text = collect_single_response(refiner.as_ref(), req)
        .await
        .map(|response| response.content.unwrap_or_default())?;
    if !should_accept_refinement(raw, &text) {
        return Ok(None);
    }

    Ok(Some(finalize_refined_prompt(text)))
}

fn finalize_refined_prompt(text: String) -> String {
    let lower = text.to_lowercase();
    let debug_triggers = ["debug", "analyze", "error", "fix", "issue", "troubleshoot", "diagnose"];
    if debug_triggers.iter().any(|token| lower.contains(token)) {
        format!(
            "{text}\n\nNote: For diagnostics, inspect files and run verification commands through `exec_command`. When advanced `code_search` is available, call it with `query` and optional `path`, `file_types`, `result_types`, or `max_results`."
        )
    } else {
        text
    }
}

// ============================================================================
// Vibe Coding Support - Lazy/Vague Request Enrichment
// ============================================================================

/// Vague patterns that indicate casual, imprecise requests
const VAGUE_PATTERNS: &[&str] = &[
    r"\bit\b",    // "make it blue"
    r"\bthat\b",  // "fix that bug"
    r"\bthe\b",   // "decrease the padding"
    r"\bthis\b",  // "update this"
    r"\bhere\b",  // "add here"
    r"\bthese\b", // "remove these"
    r"\bthose\b", // "change those"
];

/// A vague reference detected in the prompt
#[derive(Debug, Clone)]
pub(crate) struct VagueReference {
    pub(crate) term: String,
}

/// Resolution of a vague reference to a concrete entity
#[derive(Debug, Clone)]
pub(crate) struct EntityResolution {
    pub(crate) original: String,
    pub(crate) resolved: String,
    pub(crate) file: String,
    pub(crate) line: usize,
    pub(crate) confidence: f32,
}

/// Enriched prompt with context and resolutions
#[derive(Debug, Clone)]
pub(crate) struct EnrichedPrompt {
    pub(crate) original: String,
    pub(crate) resolutions: Vec<EntityResolution>,
    pub(crate) recent_files: Vec<String>,
    pub(crate) inferred_values: Vec<(String, String)>,
    pub(crate) context_hints: Vec<String>,
}

impl EnrichedPrompt {
    /// Create new enriched prompt
    pub(crate) fn new(original: String) -> Self {
        Self {
            original,
            resolutions: Vec::new(),
            recent_files: Vec::new(),
            inferred_values: Vec::new(),
            context_hints: Vec::new(),
        }
    }

    /// Add an entity resolution
    pub(crate) fn add_resolution(&mut self, resolution: EntityResolution) {
        self.resolutions.push(resolution);
    }

    /// Add a recent file
    pub(crate) fn add_recent_file(&mut self, file: String) {
        if !self.recent_files.contains(&file) {
            self.recent_files.push(file);
        }
    }

    /// Add an inferred value
    pub(crate) fn add_inferred_value(&mut self, expression: String, value: String) {
        self.inferred_values.push((expression, value));
    }

    /// Add a context hint
    pub(crate) fn add_context_hint(&mut self, hint: String) {
        self.context_hints.push(hint);
    }

    /// Convert to LLM prompt format
    pub(crate) fn to_llm_prompt(&self) -> String {
        let mut prompt = format!("User request: {}\n\n", self.original);

        if !self.resolutions.is_empty() {
            prompt.push_str("Resolved references:\n");
            for resolution in &self.resolutions {
                let _ = writeln!(
                    prompt,
                    "- \"{}\" → {} in {}:{} (confidence: {:.0}%)",
                    resolution.original,
                    resolution.resolved,
                    resolution.file,
                    resolution.line,
                    resolution.confidence * 100.0
                );
            }
            prompt.push('\n');
        }

        if !self.inferred_values.is_empty() {
            prompt.push_str("Inferred values:\n");
            for (expr, value) in &self.inferred_values {
                let _ = writeln!(prompt, "- \"{expr}\" → {value}");
            }
            prompt.push('\n');
        }

        if !self.recent_files.is_empty() {
            prompt.push_str("Recent context:\n");
            for file in self.recent_files.iter().take(5) {
                let _ = writeln!(prompt, "- Last edited: {file}");
            }
            prompt.push('\n');
        }

        if !self.context_hints.is_empty() {
            prompt.push_str("Context hints:\n");
            for hint in &self.context_hints {
                let _ = writeln!(prompt, "- {hint}");
            }
            prompt.push('\n');
        }

        prompt.push_str("Please interpret the user's request using this context.");
        prompt
    }
}

/// Detect vague references in a prompt
pub(crate) fn detect_vague_references(prompt: &str) -> Vec<VagueReference> {
    let mut references = Vec::new();
    let prompt_lower = prompt.to_lowercase();

    for pattern in VAGUE_PATTERNS {
        // Simple word boundary check (not full regex for now)
        let pattern_word = pattern.trim_start_matches(r"\b").trim_end_matches(r"\b");

        for word in prompt_lower.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric());
            if cleaned == pattern_word {
                references.push(VagueReference { term: cleaned.to_string() });
            }
        }
    }

    references
}

/// Check if prompt should be enriched (vibe coding enabled)
pub(crate) fn should_enrich_prompt(prompt: &str, vt_cfg: Option<&VTCodeConfig>) -> bool {
    let Some(vtc) = vt_cfg else {
        return false;
    };

    // Vibe coding must be enabled
    if !vtc.agent.vibe_coding.enabled {
        return false;
    }

    // Check minimum thresholds
    let char_len = prompt.trim().chars().count();
    let word_count = prompt.split_whitespace().count();

    if char_len < vtc.agent.vibe_coding.min_prompt_length {
        return false;
    }

    if word_count < vtc.agent.vibe_coding.min_prompt_words {
        return false;
    }

    // Check if prompt contains vague references
    let references = detect_vague_references(prompt);
    !references.is_empty()
}

/// Orchestrator that ties together all vibe coding components
pub(crate) struct PromptEnricher {
    /// Entity resolver for fuzzy matching
    entity_resolver: Arc<RwLock<EntityResolver>>,

    /// Workspace state tracker
    workspace_state: Arc<RwLock<WorkspaceState>>,

    /// Conversation memory for pronoun resolution
    conversation_memory: Arc<RwLock<ConversationMemory>>,

    /// Configuration
    vt_cfg: VTCodeConfig,
}

impl PromptEnricher {
    /// Create new enricher
    pub(crate) fn new(_workspace_root: PathBuf, vt_cfg: VTCodeConfig) -> Self {
        let workspace_state = Arc::new(RwLock::new(WorkspaceState::new()));
        let entity_resolver = Arc::new(RwLock::new(EntityResolver::new()));
        let conversation_memory = Arc::new(RwLock::new(ConversationMemory::new()));

        Self {
            entity_resolver,
            workspace_state,
            conversation_memory,
            vt_cfg,
        }
    }

    /// Enrich a vague/lazy prompt with contextual information
    pub(crate) async fn enrich_vague_prompt(&self, prompt: &str) -> EnrichedPrompt {
        let mut enriched = EnrichedPrompt::new(prompt.to_string());

        // Step 1: Detect vague patterns
        let vague_refs = detect_vague_references(prompt);

        if vague_refs.is_empty() {
            // No vague references, return original
            return enriched;
        }

        // Step 2: Resolve entities (if enabled)
        if self.vt_cfg.agent.vibe_coding.enable_entity_resolution {
            let resolver = self.entity_resolver.read().await;
            for vague_ref in &vague_refs {
                if let Some(entity_match) = resolver.resolve(&vague_ref.term)
                    && let Some(location) = entity_match.locations.first()
                {
                    enriched.add_resolution(EntityResolution {
                        original: vague_ref.term.clone(),
                        resolved: entity_match.entity.clone(),
                        file: location.path.display().to_string(),
                        line: location.line_start,
                        confidence: entity_match.total_score(),
                    });
                }
            }
        }

        // Step 3: Add recent files from workspace state (if enabled)
        if self.vt_cfg.agent.vibe_coding.track_workspace_state {
            let state = self.workspace_state.read().await;
            let recent_files = state.recent_files(5);
            for file_activity in recent_files {
                enriched.add_recent_file(file_activity.path.display().to_string());
            }
        }

        // Step 4: Resolve pronouns from conversation memory (if enabled)
        if self.vt_cfg.agent.vibe_coding.enable_conversation_memory {
            let memory = self.conversation_memory.read().await;
            for vague_ref in &vague_refs {
                // Check if it's a pronoun
                let is_pronoun = matches!(vague_ref.term.as_str(), "it" | "that" | "this");
                if is_pronoun {
                    // Use turn 0 as placeholder - will be improved in Phase 3 integration
                    if let Some(entity_name) = memory.resolve_pronoun(&vague_ref.term, 0) {
                        enriched.add_context_hint(format!("\"{}\" likely refers to: {}", vague_ref.term, entity_name));
                    }
                }
            }
        }

        // Step 5: Infer values for relative expressions (if enabled)
        if self.vt_cfg.agent.vibe_coding.enable_relative_value_inference {
            let state = self.workspace_state.read().await;
            if let Some(resolved_value) = state.resolve_relative_value(prompt) {
                enriched.add_inferred_value(prompt.to_string(), resolved_value);
            }
        }

        enriched
    }

    #[cfg(test)]
    async fn record_workspace_change_for_test(
        &self,
        path: PathBuf,
        content_before: Option<String>,
        content_after: String,
    ) {
        let mut state = self.workspace_state.write().await;
        state.record_change(path, content_before, content_after);
    }
}
