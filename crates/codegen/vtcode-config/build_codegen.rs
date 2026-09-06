use indexmap::IndexMap;
use proc_macro2::{Ident, Span, TokenStream};
use quote::quote;

use crate::{CapabilityEntry, EntryData, PricingSpec, Provider};

// ---------------------------------------------------------------------------
// Helper: dynamic Ident creation
// ---------------------------------------------------------------------------

fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

fn optional_f64_tokens(value: Option<f64>) -> TokenStream {
    match value {
        Some(v) => {
            let lit = proc_macro2::Literal::f64_suffixed(v);
            quote! { Some(#lit) }
        }
        None => quote! { None },
    }
}

// ---------------------------------------------------------------------------
// 1. openrouter_constants.rs
// ---------------------------------------------------------------------------

pub fn generate_openrouter_constants(entries: &[EntryData], provider: &Provider) -> anyhow::Result<String> {
    // Per-model constants
    let const_defs: TokenStream = entries
        .iter()
        .map(|entry| {
            let name = ident(&entry.const_name);
            let id = &entry.id;
            quote! {
                pub const #name: &str = #id;
            }
        })
        .collect();

    // DEFAULT_MODEL
    let default_id = provider
        .default_model
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("openrouter.default_model is missing in docs/models.json"))?;
    let default_entry = entries
        .iter()
        .find(|e| &e.id == default_id)
        .ok_or_else(|| anyhow::anyhow!("Default OpenRouter model '{default_id}' is not declared in vtcode metadata"))?;
    let default_const = ident(&default_entry.const_name);

    // SUPPORTED_MODELS
    let supported_models: TokenStream = entries
        .iter()
        .map(|entry| {
            let name = ident(&entry.const_name);
            quote! { #name, }
        })
        .collect();

    // REASONING_MODELS
    let reasoning_models: TokenStream = entries
        .iter()
        .filter(|e| e.reasoning)
        .map(|entry| {
            let name = ident(&entry.const_name);
            quote! { #name, }
        })
        .collect();

    // TOOL_UNAVAILABLE_MODELS
    let tool_unavailable_models: TokenStream = entries
        .iter()
        .filter(|e| !e.tool_call)
        .map(|entry| {
            let name = ident(&entry.const_name);
            quote! { #name, }
        })
        .collect();

    // Vendor modules
    let mut vendor_map: IndexMap<String, Vec<&EntryData>> = IndexMap::new();
    for entry in entries {
        vendor_map.entry(entry.vendor.clone()).or_default().push(entry);
    }

    let vendor_modules: TokenStream = vendor_map
        .iter()
        .map(|(vendor, vendor_entries)| {
            let mod_name = ident(&super::to_module_name(vendor));
            let model_refs: TokenStream = vendor_entries
                .iter()
                .map(|entry| {
                    let name = ident(&entry.const_name);
                    quote! { super::super::#name, }
                })
                .collect();
            quote! {
                pub mod #mod_name {
                    pub const MODELS: &[&str] = &[
                        #model_refs
                    ];
                }
            }
        })
        .collect();

    let output = quote! {
        #const_defs

        pub const DEFAULT_MODEL: &str = #default_const;

        pub const SUPPORTED_MODELS: &[&str] = &[
            #supported_models
        ];

        pub const REASONING_MODELS: &[&str] = &[
            #reasoning_models
        ];

        pub const TOOL_UNAVAILABLE_MODELS: &[&str] = &[
            #tool_unavailable_models
        ];

        pub mod vendor {
            #vendor_modules
        }
    };

    Ok(output.to_string())
}

// ---------------------------------------------------------------------------
// 2. openrouter_metadata.rs
// ---------------------------------------------------------------------------

pub fn generate_openrouter_metadata(entries: &[EntryData]) -> String {
    // ENTRIES array items
    let entry_literals: TokenStream = entries.iter().map(openrouter_entry_literal).collect();

    // VendorModels
    let mut vendor_map: IndexMap<String, Vec<&EntryData>> = IndexMap::new();
    for entry in entries {
        vendor_map.entry(entry.vendor.clone()).or_default().push(entry);
    }

    let vendor_models: TokenStream = vendor_map
        .iter()
        .map(|(vendor, vendor_entries)| {
            let vendor_str = vendor.as_str();
            let model_variants: TokenStream = vendor_entries
                .iter()
                .map(|entry| {
                    let variant = ident(&entry.variant);
                    quote! { super::ModelId::#variant, }
                })
                .collect();
            quote! {
                VendorModels {
                    vendor: #vendor_str,
                    models: &[
                        #model_variants
                    ],
                },
            }
        })
        .collect();

    // metadata_for() match arms
    let metadata_match_arms: TokenStream = entries
        .iter()
        .map(|entry| {
            let variant = ident(&entry.variant);
            let const_name = ident(&entry.const_name);
            let vendor = &entry.vendor;
            let display = &entry.display;
            let description = &entry.description;
            let efficient = entry.efficient;
            let top_tier = entry.top_tier;
            let generation = &entry.generation;
            let reasoning = entry.reasoning;
            let tool_call = entry.tool_call;
            quote! {
                super::ModelId::#variant => Some(super::OpenRouterMetadata {
                    id: crate::constants::models::openrouter::#const_name,
                    vendor: #vendor,
                    display: #display,
                    description: #description,
                    efficient: #efficient,
                    top_tier: #top_tier,
                    generation: #generation,
                    reasoning: #reasoning,
                    tool_call: #tool_call,
                }),
            }
        })
        .collect();

    // parse_model() match arms
    let parse_match_arms: TokenStream = entries
        .iter()
        .map(|entry| {
            let variant = ident(&entry.variant);
            let const_name = ident(&entry.const_name);
            quote! {
                crate::constants::models::openrouter::#const_name => Some(super::ModelId::#variant),
            }
        })
        .collect();

    let output = quote! {
        #[derive(Clone)]
        pub struct Entry {
            pub variant: super::ModelId,
            pub id: &'static str,
            pub vendor: &'static str,
            pub display: &'static str,
            pub description: &'static str,
            pub efficient: bool,
            pub top_tier: bool,
            pub generation: &'static str,
            pub reasoning: bool,
            pub tool_call: bool,
        }

        #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
        pub const ENTRIES: &[Entry] = &[
            #entry_literals
        ];

        #[derive(Clone)]
        pub struct VendorModels {
            pub vendor: &'static str,
            pub models: &'static [super::ModelId],
        }

        pub const VENDOR_MODELS: &[VendorModels] = &[
            #vendor_models
        ];

        pub fn metadata_for(model: super::ModelId) -> Option<super::OpenRouterMetadata> {
            match model {
                #metadata_match_arms
                _ => None,
            }
        }

        pub fn parse_model(value: &str) -> Option<super::ModelId> {
            match value {
                #parse_match_arms
                _ => None,
            }
        }

        pub fn vendor_groups() -> &'static [VendorModels] {
            VENDOR_MODELS
        }
    };

    output.to_string()
}

fn openrouter_entry_literal(entry: &EntryData) -> TokenStream {
    let variant = ident(&entry.variant);
    let const_name = ident(&entry.const_name);
    let vendor = &entry.vendor;
    let display = &entry.display;
    let description = &entry.description;
    let efficient = entry.efficient;
    let top_tier = entry.top_tier;
    let generation = &entry.generation;
    let reasoning = entry.reasoning;
    let tool_call = entry.tool_call;
    quote! {
        Entry {
            variant: super::ModelId::#variant,
            id: crate::constants::models::openrouter::#const_name,
            vendor: #vendor,
            display: #display,
            description: #description,
            efficient: #efficient,
            top_tier: #top_tier,
            generation: #generation,
            reasoning: #reasoning,
            tool_call: #tool_call,
        },
    }
}

// ---------------------------------------------------------------------------
// 3. model_capabilities.rs
// ---------------------------------------------------------------------------

pub fn generate_model_capabilities(entries: &[CapabilityEntry]) -> String {
    // ENTRIES array
    let entry_literals: TokenStream = entries.iter().map(capability_entry_literal).collect();

    // PROVIDERS list
    let mut provider_map: IndexMap<&str, Vec<&CapabilityEntry>> = IndexMap::new();
    for entry in entries {
        provider_map.entry(&entry.provider).or_default().push(entry);
    }

    let provider_names: TokenStream = provider_map
        .keys()
        .map(|provider| {
            quote! { #provider, }
        })
        .collect();

    // metadata_for() and models_for_provider()
    let (metadata_for_fn, models_for_provider_fn) = if provider_map.is_empty() {
        (
            quote! {
                pub fn metadata_for(_provider: &str, _id: &str) -> Option<Entry> {
                    None
                }
            },
            quote! {
                pub fn models_for_provider(_provider: &str) -> Option<&'static [&'static str]> {
                    None
                }
            },
        )
    } else {
        let provider_match_arms: TokenStream = provider_map
            .iter()
            .map(|(provider, provider_entries)| {
                let id_match_arms: TokenStream = provider_entries
                    .iter()
                    .map(|entry| {
                        let id = &entry.id;
                        let literal = capability_entry_literal_without_comma(entry);
                        quote! {
                            #id => Some(#literal),
                        }
                    })
                    .collect();
                quote! {
                    #provider => match id {
                        #id_match_arms
                        _ => None,
                    },
                }
            })
            .collect();

        let models_match_arms: TokenStream = provider_map
            .iter()
            .map(|(provider, provider_entries)| {
                let model_ids: TokenStream = provider_entries
                    .iter()
                    .map(|entry| {
                        let id = &entry.id;
                        quote! { #id, }
                    })
                    .collect();
                quote! {
                    #provider => Some(&[
                        #model_ids
                    ]),
                }
            })
            .collect();

        (
            quote! {
                pub fn metadata_for(provider: &str, id: &str) -> Option<Entry> {
                    match provider {
                        #provider_match_arms
                        _ => None,
                    }
                }
            },
            quote! {
                pub fn models_for_provider(provider: &str) -> Option<&'static [&'static str]> {
                    match provider {
                        #models_match_arms
                        _ => None,
                    }
                }
            },
        )
    };

    let output = quote! {
        #[derive(Clone, Copy)]
        pub struct Pricing {
            pub input: Option<f64>,
            pub output: Option<f64>,
            pub cache_read: Option<f64>,
            pub cache_write: Option<f64>,
        }

        #[derive(Clone, Copy)]
        pub struct Entry {
            pub provider: &'static str,
            pub id: &'static str,
            pub display_name: &'static str,
            pub description: &'static str,
            pub context_window: usize,
            pub max_output_tokens: Option<usize>,
            pub reasoning_efforts: &'static [&'static str],
            pub is_pro: bool,
            pub lightweight_model: Option<&'static str>,
            pub reasoning: bool,
            pub tool_call: bool,
            pub vision: bool,
            pub input_modalities: &'static [&'static str],
            pub caching: bool,
            pub structured_output: bool,
            pub supports_sampling: bool,
            pub supports_logprobs: bool,
            pub prompt_cache_ttl: Option<&'static str>,
            pub prompt_contract: Option<&'static str>,
            pub pricing: Pricing,
        }

        #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
        pub const ENTRIES: &[Entry] = &[
            #entry_literals
        ];

        pub const PROVIDERS: &[&str] = &[
            #provider_names
        ];

        #metadata_for_fn

        #models_for_provider_fn
    };

    output.to_string()
}

fn capability_entry_literal(entry: &CapabilityEntry) -> TokenStream {
    let expr = capability_entry_expr(entry);
    quote! { #expr, }
}

fn capability_entry_literal_without_comma(entry: &CapabilityEntry) -> TokenStream {
    capability_entry_expr(entry)
}

fn capability_entry_expr(entry: &CapabilityEntry) -> TokenStream {
    let provider = &entry.provider;
    let id = &entry.id;
    let display_name = &entry.display_name;
    let description = &entry.description;
    let context_window = entry.context_window;
    let max_output_tokens = optional_usize_tokens(entry.max_output_tokens);
    let reasoning = entry.reasoning;
    let reasoning_efforts = &entry.reasoning_efforts;
    let is_pro = entry.is_pro;
    let lightweight_model = match &entry.lightweight_model {
        Some(model) => quote! { Some(#model) },
        None => quote! { None },
    };
    let tool_call = entry.tool_call;
    let vision = entry.vision;
    let modalities: TokenStream = entry.input_modalities.iter().map(|m| quote! { #m, }).collect();
    let caching = entry.caching;
    let structured_output = entry.structured_output;
    let supports_sampling = entry.supports_sampling;
    let supports_logprobs = entry.supports_logprobs;
    let prompt_cache_ttl = match &entry.prompt_cache_ttl {
        Some(value) => quote! { Some(#value) },
        None => quote! { None },
    };
    let prompt_contract = match &entry.prompt_contract {
        Some(value) => quote! { Some(#value) },
        None => quote! { None },
    };
    let pricing = pricing_literal(&entry.pricing);

    quote! {
        Entry {
            provider: #provider,
            id: #id,
            display_name: #display_name,
            description: #description,
            context_window: #context_window,
            max_output_tokens: #max_output_tokens,
            reasoning: #reasoning,
            reasoning_efforts: &[#(#reasoning_efforts),*],
            is_pro: #is_pro,
            lightweight_model: #lightweight_model,
            tool_call: #tool_call,
            vision: #vision,
            input_modalities: &[
                #modalities
            ],
            caching: #caching,
            structured_output: #structured_output,
            supports_sampling: #supports_sampling,
            supports_logprobs: #supports_logprobs,
            prompt_cache_ttl: #prompt_cache_ttl,
            prompt_contract: #prompt_contract,
            pricing: #pricing,
        }
    }
}

fn pricing_literal(pricing: &PricingSpec) -> TokenStream {
    let input = optional_f64_tokens(pricing.input);
    let output = optional_f64_tokens(pricing.output);
    let cache_read = optional_f64_tokens(pricing.cache_read);
    let cache_write = optional_f64_tokens(pricing.cache_write);
    quote! {
        Pricing {
            input: #input,
            output: #output,
            cache_read: #cache_read,
            cache_write: #cache_write,
        }
    }
}

fn optional_usize_tokens(value: Option<usize>) -> TokenStream {
    match value {
        Some(v) => quote! { Some(#v) },
        None => quote! { None },
    }
}
