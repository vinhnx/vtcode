use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::str::FromStr;
use tracing::warn;

use vtcode_config::VTCodeConfig;
use vtcode_config::core::ProviderOverrideConfig;
use vtcode_core::config::constants::defaults;
use vtcode_core::config::models::{ModelId, Provider};

#[derive(Clone)]
pub(super) struct ModelOption {
    pub(super) model: ModelId,
    pub(super) provider: Provider,
    pub(super) id: String,
    pub(super) display: String,
    pub(super) description: String,
    pub(super) supports_reasoning: bool,
    pub(super) reasoning_alternative: Option<ModelId>,
    pub(super) api_key_env: String,
}

fn should_filter_model(provider: Provider, model: &ModelId) -> bool {
    provider == Provider::Copilot && !matches!(model, ModelId::CopilotAuto)
}

pub(super) static MODEL_OPTIONS: Lazy<Vec<ModelOption>> = Lazy::new(|| {
    let models = ModelId::all_models();
    let mut options = Vec::with_capacity(models.len());
    for model in models {
        let provider = model.provider();
        if should_filter_model(provider, &model) {
            continue;
        }
        options.push(ModelOption {
            id: model.as_str().into_owned(),
            display: model.display_name().into_owned(),
            description: model.description().into_owned(),
            supports_reasoning: model.supports_reasoning(),
            reasoning_alternative: model.non_reasoning_variant(),
            api_key_env: provider.default_api_key_env().to_string(),
            model: model.clone(),
            provider,
        });
    }
    options
});

static PROVIDER_OPTION_INDEXES: Lazy<HashMap<Provider, Box<[usize]>>> = Lazy::new(|| {
    let mut map = HashMap::<Provider, Vec<usize>>::with_capacity(64);
    for (index, option) in MODEL_OPTIONS.iter().enumerate() {
        map.entry(option.provider).or_default().push(index);
    }
    map.into_iter().map(|(k, v)| (k, v.into_boxed_slice())).collect()
});

static PICKER_PROVIDER_ORDER: Lazy<Box<[Provider]>> = Lazy::new(|| {
    Provider::all_providers()
        .into_iter()
        .filter(|p| !matches!(p, Provider::Ollama | Provider::LlamaCpp))
        .chain([Provider::LlamaCpp, Provider::Ollama])
        .collect::<Vec<_>>()
        .into_boxed_slice()
});

pub(super) fn build_model_options_with_overrides(
    overrides: &BTreeMap<String, ProviderOverrideConfig>,
) -> Cow<'static, [ModelOption]> {
    if overrides.is_empty() {
        return Cow::Borrowed(MODEL_OPTIONS.as_slice());
    }

    let models = ModelId::all_models_with_overrides(overrides);
    let mut options = Vec::with_capacity(models.len());
    for model in models {
        let provider = match &model {
            ModelId::Custom(provider_key, _) => match Provider::from_str(provider_key) {
                Ok(parsed) => parsed,
                Err(_) => {
                    warn!("Unknown provider key '{}' in provider_overrides; defaulting to OpenAI", provider_key);
                    Provider::OpenAI
                }
            },
            _ => model.provider(),
        };
        if should_filter_model(provider, &model) {
            continue;
        }
        let api_key_env = match &model {
            ModelId::Custom(provider_key, _) => overrides
                .get(provider_key)
                .and_then(|config| config.api_key_env.as_deref())
                .map(str::to_owned),
            _ => overrides
                .get(provider.as_ref())
                .and_then(|config| config.api_key_env.as_deref())
                .map(str::to_owned),
        }
        .filter(|env_key| !env_key.trim().is_empty())
        .unwrap_or_else(|| provider.default_api_key_env().to_string());
        options.push(ModelOption {
            id: model.as_str().into_owned(),
            display: model.display_name().into_owned(),
            description: model.description().into_owned(),
            supports_reasoning: model.supports_reasoning(),
            reasoning_alternative: model.non_reasoning_variant(),
            api_key_env,
            model: model.clone(),
            provider,
        });
    }
    Cow::Owned(options)
}

pub(super) fn option_indexes_for_provider(provider: Provider) -> &'static [usize] {
    PROVIDER_OPTION_INDEXES.get(&provider).map_or(&[], Box::as_ref)
}

pub(super) fn find_option_index(provider: Provider, model_id: &str, options: &[ModelOption]) -> Option<usize> {
    options.iter().enumerate().find_map(|(index, option)| {
        if option.provider == provider && option.id.eq_ignore_ascii_case(model_id) {
            Some(index)
        } else {
            None
        }
    })
}

pub(super) fn build_filtered_options(vt_cfg: Option<&VTCodeConfig>) -> Cow<'static, [ModelOption]> {
    let Some(cfg) = vt_cfg else {
        return Cow::Borrowed(MODEL_OPTIONS.as_slice());
    };
    let opts: Cow<'static, [ModelOption]> = if !cfg.provider_overrides.is_empty() {
        build_model_options_with_overrides(&cfg.provider_overrides)
    } else {
        Cow::Borrowed(MODEL_OPTIONS.as_slice())
    };
    let opts = apply_persisted_api_key_env(opts, cfg);
    filter_options_by_whitelist(opts, &cfg.providers_whitelist)
}

fn apply_persisted_api_key_env(
    options: Cow<'static, [ModelOption]>,
    cfg: &VTCodeConfig,
) -> Cow<'static, [ModelOption]> {
    let Some(provider) = Provider::from_str(&cfg.agent.provider).ok() else {
        return options;
    };
    let configured_env = cfg.agent.api_key_env.trim();
    if configured_env.is_empty()
        || configured_env.eq_ignore_ascii_case(defaults::DEFAULT_API_KEY_ENV)
        || configured_env.eq_ignore_ascii_case(provider.default_api_key_env())
        || cfg.configured_api_key_env(&cfg.agent.provider).is_some()
    {
        return options;
    }

    let mut options = options.into_owned();
    for option in &mut options {
        if option.provider == provider {
            option.api_key_env = configured_env.to_owned();
        }
    }
    Cow::Owned(options)
}

pub(super) fn picker_provider_order() -> &'static [Provider] {
    PICKER_PROVIDER_ORDER.as_ref()
}

pub(super) fn picker_provider_order_with_whitelist(whitelist: &[String]) -> Vec<Provider> {
    if whitelist.is_empty() {
        return PICKER_PROVIDER_ORDER.to_vec();
    }
    PICKER_PROVIDER_ORDER
        .iter()
        .copied()
        .filter(|p| whitelist.iter().any(|w| w.eq_ignore_ascii_case(p.as_ref())))
        .collect()
}

pub(super) fn filter_options_by_whitelist(
    options: Cow<'static, [ModelOption]>,
    whitelist: &[String],
) -> Cow<'static, [ModelOption]> {
    if whitelist.is_empty() {
        return options;
    }
    let allowed: HashSet<String> = whitelist.iter().map(|w| w.to_ascii_lowercase()).collect();
    let filtered: Vec<ModelOption> = options
        .iter()
        .filter(|opt| allowed.contains(&opt.provider.as_ref().to_ascii_lowercase()))
        .cloned()
        .collect();
    Cow::Owned(filtered)
}
