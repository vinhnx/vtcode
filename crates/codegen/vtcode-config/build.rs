#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]

use anyhow::{Context as _, Result};
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

mod build_codegen;

const EMBEDDED_OPENROUTER_MODELS: &str = include_str!("build_data/openrouter_models.json");

fn main() {
    let is_docsrs = env::var_os("DOCS_RS").is_some();

    // Force rebuild when embedded OpenRouter models change
    println!("cargo:rerun-if-changed=build_data/openrouter_models.json");

    if is_docsrs {
        // When building on docs.rs, generate empty placeholder files to prevent compilation errors
        println!("cargo:warning=docs.rs build detected, generating placeholder files");
        generate_placeholder_artifacts();
    } else if let Err(error) = generate_artifacts() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn generate_placeholder_artifacts() {
    use std::path::PathBuf;

    let out_dir = match env::var("OUT_DIR") {
        Ok(path) => PathBuf::from(path),
        Err(error) => {
            eprintln!("warning: OUT_DIR not set during docs.rs placeholder generation: {error}");
            return;
        }
    };

    // Generate placeholder openrouter_constants.rs with all per-model constants
    // so that #[cfg(not(docsrs))] source code referencing these constants compiles.
    // The docsrs cfg is only set during the rustdoc pass, not during compilation,
    // so source code behind #[cfg(not(docsrs))] is active and needs these symbols.
    let constants_content = match generate_placeholder_openrouter_constants() {
        Ok(content) => content,
        Err(error) => {
            eprintln!("warning: failed to generate placeholder openrouter constants: {error:#}");
            // Fall back to minimal placeholder
            "    pub const DEFAULT_MODEL: &str = \"openrouter/auto\";\n    \
             pub const SUPPORTED_MODELS: &[&str] = &[];\n    \
             pub const REASONING_MODELS: &[&str] = &[];\n    \
             pub const TOOL_UNAVAILABLE_MODELS: &[&str] = &[];\n    \
             pub mod vendor {}\n"
                .to_string()
        }
    };
    if let Err(error) = fs::write(out_dir.join("openrouter_constants.rs"), constants_content) {
        eprintln!("warning: failed to write placeholder constants: {error}");
    }
    if let Err(error) = fs::write(
        out_dir.join("openrouter_metadata.rs"),
        "// Placeholder for docs.rs build\n#[derive(Clone, Copy)]\npub struct Entry {\n    pub variant: super::ModelId,\n    pub id: &'static str,\n    pub vendor: &'static str,\n    pub display: &'static str,\n    pub description: &'static str,\n    pub efficient: bool,\n    pub top_tier: bool,\n    pub generation: &'static str,\n    pub reasoning: bool,\n    pub tool_call: bool,\n}\n\npub const ENTRIES: &[Entry] = &[];\n\n#[derive(Clone, Copy)]\npub struct VendorModels {\n    pub vendor: &'static str,\n    pub models: &'static [super::ModelId],\n}\n\npub const VENDOR_MODELS: &[VendorModels] = &[];\n\npub fn metadata_for(_model: super::ModelId) -> Option<super::OpenRouterMetadata> { None }\n\npub fn parse_model(_value: &str) -> Option<super::ModelId> { None }\n\npub fn vendor_groups() -> &'static [VendorModels] { VENDOR_MODELS }\n",
    ) {
        eprintln!("warning: failed to write placeholder metadata: {error}");
    }
    if let Err(error) = fs::write(
        out_dir.join("model_capabilities.rs"),
        "// Placeholder for docs.rs build\n#[derive(Clone, Copy)]\npub struct Pricing {\n    pub input: Option<f64>,\n    pub output: Option<f64>,\n    pub cache_read: Option<f64>,\n    pub cache_write: Option<f64>,\n}\n\n#[derive(Clone, Copy)]\npub struct Entry {\n    pub provider: &'static str,\n    pub id: &'static str,\n    pub display_name: &'static str,\n    pub description: &'static str,\n    pub context_window: usize,\n    pub max_output_tokens: Option<usize>,\n    pub reasoning_efforts: &'static [&'static str],\n    pub is_pro: bool,\n    pub lightweight_model: Option<&'static str>,\n    pub reasoning: bool,\n    pub tool_call: bool,\n    pub vision: bool,\n    pub input_modalities: &'static [&'static str],\n    pub caching: bool,\n    pub structured_output: bool,\n    pub supports_sampling: bool,\n    pub supports_logprobs: bool,\n    pub prompt_cache_ttl: Option<&'static str>,\n    pub prompt_contract: Option<&'static str>,\n    pub pricing: Pricing,\n}\n\npub const ENTRIES: &[Entry] = &[];\npub const PROVIDERS: &[&str] = &[];\n\npub fn metadata_for(_provider: &str, _id: &str) -> Option<Entry> { None }\npub fn models_for_provider(_provider: &str) -> Option<&'static [&'static str]> { None }\n",
    ) {
        eprintln!("warning: failed to write capability metadata: {error}");
    }
}

/// Generate placeholder openrouter_constants.rs for docs.rs builds.
///
/// The `docsrs` cfg flag is only set during the final rustdoc invocation, not
/// during the cargo compilation step. Source code behind `#[cfg(not(docsrs))]`
/// is therefore active and references per-model constants (e.g.
/// `openrouter::QWEN3_CODER`). This function reads the embedded model metadata
/// and generates all per-model constants so compilation succeeds.
fn generate_placeholder_openrouter_constants() -> Result<String> {
    let root: Value = serde_json::from_str(EMBEDDED_OPENROUTER_MODELS)
        .context("Failed to parse embedded openrouter_models.json for placeholder generation")?;

    let models = root
        .get("models")
        .and_then(|v| v.as_object())
        .context("openrouter_models.json is missing 'models' object")?;

    let default_model_id = root
        .get("default_model")
        .and_then(|v| v.as_str())
        .unwrap_or("xiaomi/mimo-v2.5-pro");

    // Collect per-model entries: (constant_name, model_id, is_reasoning, tool_call, vendor)
    let mut entries: Vec<(String, String, bool, bool, String)> = Vec::new();
    let mut default_const_name: Option<String> = None;

    for (_key, model) in models {
        let Some(vtcode) = model.get("vtcode") else {
            continue;
        };
        let const_name = match vtcode.get("constant").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => continue,
        };
        let model_id = match model.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let is_reasoning = model.get("reasoning").and_then(|v| v.as_bool()).unwrap_or(false);
        let tool_call = model.get("tool_call").and_then(|v| v.as_bool()).unwrap_or(true);
        let vendor = vtcode.get("vendor").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

        if model_id == default_model_id {
            default_const_name = Some(const_name.clone());
        }

        entries.push((const_name, model_id, is_reasoning, tool_call, vendor));
    }

    let default_const = default_const_name.as_deref().unwrap_or("XIAOMI_MIMO_V2_5_PRO");

    let mut output = String::new();
    output.push_str("// Auto-generated placeholder for docs.rs build\n");

    // Per-model constants
    for (const_name, model_id, _, _, _) in &entries {
        output.push_str(&format!("pub const {const_name}: &str = \"{model_id}\";\n"));
    }

    // DEFAULT_MODEL
    output.push_str(&format!("pub const DEFAULT_MODEL: &str = {default_const};\n"));

    // SUPPORTED_MODELS
    output.push_str("pub const SUPPORTED_MODELS: &[&str] = &[");
    for (const_name, _, _, _, _) in &entries {
        output.push_str(&format!("{const_name}, "));
    }
    output.push_str("];\n");

    // REASONING_MODELS
    output.push_str("pub const REASONING_MODELS: &[&str] = &[");
    for (const_name, _, is_reasoning, _, _) in &entries {
        if *is_reasoning {
            output.push_str(&format!("{const_name}, "));
        }
    }
    output.push_str("];\n");

    // TOOL_UNAVAILABLE_MODELS
    output.push_str("pub const TOOL_UNAVAILABLE_MODELS: &[&str] = &[");
    for (const_name, _, _, tool_call, _) in &entries {
        if !tool_call {
            output.push_str(&format!("{const_name}, "));
        }
    }
    output.push_str("];\n");

    // Vendor modules
    let mut vendor_map: IndexMap<String, Vec<&str>> = IndexMap::new();
    for (const_name, _, _, _, vendor) in &entries {
        vendor_map.entry(vendor.clone()).or_default().push(const_name);
    }

    output.push_str("pub mod vendor {\n");
    for (vendor, constants) in &vendor_map {
        let mod_name = to_module_name(vendor);
        output.push_str(&format!("    pub mod {mod_name} {{\n"));
        output.push_str("        pub const MODELS: &[&str] = &[");
        for c in constants {
            output.push_str(&format!("super::super::{c}, "));
        }
        output.push_str("];\n");
        output.push_str("    }\n");
    }
    output.push_str("}\n");

    Ok(output)
}

fn generate_artifacts() -> Result<()> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let provider = load_provider_metadata(&manifest_dir)?;
    let capability_entries = load_model_capability_entries(&manifest_dir)?;

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let entries = provider.collect_entries()?;

    // Debug: count entries and check for deprecated models
    for entry in &entries {
        if entry.id.contains("claude-sonnet-4.5") || entry.id.contains("deepseek-chat-v3.1") {
            println!("cargo:warning=DEPRECATED MODEL STILL IN BUILD DATA: {}", entry.id);
        }
    }

    write_constants(&out_dir, &provider, &entries)?;
    write_metadata(&out_dir, &entries)?;
    write_model_capabilities(&out_dir, &capability_entries)?;

    Ok(())
}

fn load_provider_metadata(manifest_dir: &Path) -> Result<Provider> {
    let docs_path = manifest_dir.join("../../../docs/models.json");
    if docs_path.exists() {
        println!("cargo:rerun-if-changed={}", docs_path.display());
        let models_source =
            fs::read_to_string(&docs_path).with_context(|| format!("Failed to read {}", docs_path.display()))?;

        let root: Value = serde_json::from_str(&models_source).context("Failed to parse docs/models.json as JSON")?;
        let openrouter_value = root
            .get("openrouter")
            .cloned()
            .context("docs/models.json is missing the openrouter provider section")?;

        serde_json::from_value(openrouter_value).context("Failed to deserialize openrouter provider metadata")
    } else {
        // Fallback to embedded models if docs/models.json is unavailable.
        // If docs/models.json exists but contains entries that we don't have enum variants for
        // (e.g., experimental listings), prefer the embedded set by returning an error early.
        serde_json::from_str(EMBEDDED_OPENROUTER_MODELS).context("Failed to parse embedded OpenRouter model metadata")
    }
}

#[derive(Deserialize)]
struct Provider {
    #[serde(default)]
    default_model: Option<String>,
    models: IndexMap<String, ModelSpec>,
}

#[derive(Deserialize)]
struct ModelSpec {
    id: String,
    #[serde(default)]
    reasoning: bool,
    #[serde(default = "default_tool_call_true")]
    tool_call: bool,
    vtcode: Option<VtcodeSpec>,
}

fn default_tool_call_true() -> bool {
    true
}

#[derive(Deserialize)]
struct VtcodeSpec {
    variant: String,
    constant: String,
    vendor: String,
    display: String,
    description: String,
    efficient: bool,
    top_tier: bool,
    generation: String,
}

struct EntryData {
    variant: String,
    const_name: String,
    id: String,
    vendor: String,
    display: String,
    description: String,
    efficient: bool,
    top_tier: bool,
    generation: String,
    reasoning: bool,
    tool_call: bool,
}

#[derive(Deserialize)]
struct ProviderCatalog {
    #[serde(default)]
    models: IndexMap<String, CapabilityModelSpec>,
}

#[derive(Deserialize)]
struct CapabilityModelSpec {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    context: usize,
    #[serde(default, alias = "output_tokens")]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_efforts: Vec<String>,
    #[serde(default)]
    is_pro: bool,
    #[serde(default)]
    lightweight_model: Option<String>,
    #[serde(default = "default_tool_call_true")]
    tool_call: bool,
    #[serde(default)]
    modalities: CapabilityModalities,
    #[serde(default)]
    capabilities: CapabilityFlags,
    #[serde(default)]
    cost: Option<PricingSpec>,
    #[serde(default = "default_true")]
    supports_sampling: bool,
    #[serde(default = "default_true")]
    supports_logprobs: bool,
    #[serde(default)]
    prompt_cache_ttl: Option<String>,
    #[serde(default)]
    prompt_contract: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Default, Deserialize)]
struct CapabilityModalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Default, Deserialize)]
struct CapabilityFlags {
    #[serde(default)]
    caching: bool,
    #[serde(default)]
    context_caching: bool,
    #[serde(default)]
    structured_output: bool,
}

#[derive(Clone, Copy, Default, Deserialize)]
struct PricingSpec {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

struct CapabilityEntry {
    provider: String,
    id: String,
    display_name: String,
    description: String,
    context_window: usize,
    max_output_tokens: Option<usize>,
    reasoning: bool,
    reasoning_efforts: Vec<String>,
    is_pro: bool,
    lightweight_model: Option<String>,
    tool_call: bool,
    vision: bool,
    input_modalities: Vec<String>,
    caching: bool,
    structured_output: bool,
    supports_sampling: bool,
    supports_logprobs: bool,
    prompt_cache_ttl: Option<String>,
    prompt_contract: Option<String>,
    pricing: PricingSpec,
}

impl Provider {
    fn collect_entries(&self) -> Result<Vec<EntryData>> {
        let mut seen_constants = HashMap::new();
        let mut entries = Vec::with_capacity(self.models.len());

        for (model_id, spec) in &self.models {
            let Some(vtcode) = spec.vtcode.as_ref() else {
                println!("cargo:warning=Skipping openrouter model '{model_id}' without vtcode metadata");
                continue;
            };

            let const_name = vtcode.constant.trim().to_string();
            if const_name.is_empty() {
                anyhow::bail!("vtcode constant name missing for model '{model_id}'");
            }
            if let Some(existing_id) = seen_constants.insert(const_name.clone(), model_id) {
                anyhow::bail!("Duplicate constant '{const_name}' for models '{existing_id}' and '{model_id}'");
            }

            entries.push(EntryData {
                variant: vtcode.variant.clone(),
                const_name: const_name.clone(),
                id: spec.id.clone(),
                vendor: vtcode.vendor.to_lowercase(),
                display: vtcode.display.clone(),
                description: vtcode.description.clone(),
                efficient: vtcode.efficient,
                top_tier: vtcode.top_tier,
                generation: vtcode.generation.clone(),
                reasoning: spec.reasoning,
                tool_call: spec.tool_call,
            });
        }

        Ok(entries)
    }
}

fn load_model_capability_entries(manifest_dir: &Path) -> Result<Vec<CapabilityEntry>> {
    let docs_path = manifest_dir.join("../../../docs/models.json");
    if !docs_path.exists() {
        return Ok(Vec::new());
    }

    println!("cargo:rerun-if-changed={}", docs_path.display());
    let models_source =
        fs::read_to_string(&docs_path).with_context(|| format!("Failed to read {}", docs_path.display()))?;
    let providers: IndexMap<String, ProviderCatalog> =
        serde_json::from_str(&models_source).context("Failed to deserialize docs/models.json providers")?;

    let mut entries = Vec::new();
    for (provider_key, provider) in providers {
        let provider_key = canonical_provider_key(&provider_key);
        for spec in provider.models.into_values() {
            let vision = spec
                .modalities
                .input
                .iter()
                .any(|modality| matches!(modality.as_str(), "image" | "video"));
            entries.push(CapabilityEntry {
                provider: provider_key.to_string(),
                id: spec.id,
                display_name: spec.name,
                description: spec.description,
                context_window: spec.context,
                max_output_tokens: spec.max_output_tokens,
                reasoning: spec.reasoning,
                reasoning_efforts: spec.reasoning_efforts,
                is_pro: spec.is_pro,
                lightweight_model: spec.lightweight_model,
                tool_call: spec.tool_call,
                input_modalities: spec.modalities.input,
                vision,
                caching: spec.capabilities.caching || spec.capabilities.context_caching,
                structured_output: spec.capabilities.structured_output,
                supports_sampling: spec.supports_sampling,
                supports_logprobs: spec.supports_logprobs,
                prompt_cache_ttl: spec.prompt_cache_ttl,
                prompt_contract: spec.prompt_contract,
                pricing: spec.cost.unwrap_or_default(),
            });
        }
    }

    entries.sort_by(|left, right| left.provider.cmp(&right.provider).then(left.id.cmp(&right.id)));

    Ok(entries)
}

fn write_constants(out_dir: &Path, provider: &Provider, entries: &[EntryData]) -> Result<()> {
    let content = build_codegen::generate_openrouter_constants(entries, provider)?;
    fs::write(out_dir.join("openrouter_constants.rs"), content)
        .context("Failed to write generated OpenRouter constants")
}

fn write_metadata(out_dir: &Path, entries: &[EntryData]) -> Result<()> {
    let content = build_codegen::generate_openrouter_metadata(entries);
    fs::write(out_dir.join("openrouter_metadata.rs"), content).context("Failed to write generated OpenRouter metadata")
}

fn write_model_capabilities(out_dir: &Path, entries: &[CapabilityEntry]) -> Result<()> {
    let content = build_codegen::generate_model_capabilities(entries);
    fs::write(out_dir.join("model_capabilities.rs"), content)
        .context("Failed to write generated model capability metadata")
}

fn to_module_name(vendor: &str) -> String {
    let mut output = String::with_capacity(vendor.len());
    for ch in vendor.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        return "vendor".to_string();
    }
    if output.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("vendor_{output}")
    } else {
        output
    }
}

fn canonical_provider_key(provider: &str) -> &str {
    match provider {
        "google" => "gemini",
        other => other,
    }
}
