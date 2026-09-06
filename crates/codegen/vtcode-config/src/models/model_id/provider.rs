use crate::models::{Provider, ProviderModelSupport};

use super::ModelId;

impl ModelId {
    /// Get the provider for this model
    pub fn provider(&self) -> Provider {
        if self.openrouter_metadata().is_some() {
            return Provider::OpenRouter;
        }
        if let Some(provider) = self.table_provider() {
            return provider;
        }
        match self {
            ModelId::Custom(provider_key, _) => {
                use std::str::FromStr;
                Provider::from_str(provider_key).unwrap_or(Provider::OpenAI)
            }
            // Every remaining variant is a hand-written OpenRouter model without
            // generated metadata; the table covers all other providers.
            _ => Provider::OpenRouter,
        }
    }

    /// Whether this model supports configurable reasoning effort levels
    pub fn supports_reasoning_effort(&self) -> bool {
        self.provider().supports_reasoning_effort(&self.as_str())
    }

    /// Whether this model exposes structured reasoning output.
    pub fn supports_reasoning(&self) -> bool {
        self.provider().supports_reasoning(&self.as_str())
    }
}
