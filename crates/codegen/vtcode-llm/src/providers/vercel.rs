use vtcode_config::constants::{env_vars, models, urls};

use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, impl_openai_compat_provider};

pub struct VercelSpec;

impl OpenAiCompatSpec for VercelSpec {
    const NAME: &'static str = "Vercel AI Gateway";
    const KEY: &'static str = "vercel";
    const API_KEY_ENV: &'static str = "AI_GATEWAY_API_KEY";
    const DEFAULT_MODEL: &'static str = models::vercel::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::VERCEL_AI_GATEWAY_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::VERCEL_AI_GATEWAY_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::vercel::SUPPORTED_MODELS;
    // The gateway routes hundreds of models that change independently of this
    // catalog, so only request shape is validated.
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = None;

    const SUPPRESS_SAMPLING_WHEN_REASONING: bool = false;
    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    // The gateway forwards OpenAI-style reasoning payloads in both fields
    // depending on the upstream vendor.
    const STREAM_REASONING_FIELDS: &'static [&'static str] = &["reasoning", "reasoning_content"];

    fn resolve_api_key(api_key: Option<String>) -> String {
        api_key
            .or_else(|| std::env::var(Self::API_KEY_ENV).ok().filter(|key| !key.trim().is_empty()))
            .unwrap_or_default()
    }
}

impl_openai_compat_provider!(VercelProvider, VercelSpec, {
    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        use vtcode_config::constants::models;
        !models::vercel::NON_REASONING_MODELS.contains(&model)
    }

    fn effective_context_size(&self, model: &str) -> usize {
        crate::provider::catalog_context_window("vercel", model, 1_000_000)
    }
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{LLMRequest, Message, ToolChoice};
    use std::sync::Arc;
    use vtcode_config::types::ReasoningEffortLevel;

    fn provider() -> VercelProvider {
        VercelProvider::from_config(
            Some("test-key".to_string()),
            Some(models::vercel::ANTHROPIC_CLAUDE_SONNET_5.to_string()),
            Some("https://example.test/v1".to_string()),
            None,
            None,
            None,
            None,
        )
    }

    fn base_request() -> LLMRequest {
        LLMRequest {
            messages: vec![Message::user("hello".to_string())].into(),
            system_prompt: Some(Arc::from("system guidance")),
            model: models::vercel::ANTHROPIC_CLAUDE_SONNET_5.to_string(),
            max_tokens: Some(512),
            temperature: Some(0.5),
            stream: true,
            tool_choice: Some(ToolChoice::Auto),
            ..Default::default()
        }
    }

    #[test]
    fn golden_payload_basic_shape() {
        let payload = provider().core.convert_request(&base_request()).unwrap();

        assert_eq!(payload["model"], models::vercel::ANTHROPIC_CLAUDE_SONNET_5);
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "system guidance");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "hello");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["tool_choice"], "auto");
    }

    #[test]
    fn golden_payload_reasoning_keeps_sampling() {
        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::High);
        let payload = provider().core.convert_request(&request).unwrap();
        assert_eq!(payload["temperature"], 0.5);
        assert!(payload.get("reasoning").is_none());
    }

    #[test]
    fn golden_payload_omits_empty_system_prompt() {
        let mut request = base_request();
        request.system_prompt = Some(Arc::from("   "));
        request.stream = false;
        let payload = provider().core.convert_request(&request).unwrap();
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert!(payload.get("stream").is_none());
        assert!(payload.get("stream_options").is_none());
    }

    #[test]
    fn gateway_ids_are_forwarded_verbatim() {
        let model = "some-vendor/unlisted-model";
        let provider =
            VercelProvider::from_config(Some("k".to_string()), Some(model.to_string()), None, None, None, None, None);
        assert_eq!(provider.core.model, model);
        assert_eq!(provider.core.base_url, urls::VERCEL_AI_GATEWAY_API_BASE);
    }
}
