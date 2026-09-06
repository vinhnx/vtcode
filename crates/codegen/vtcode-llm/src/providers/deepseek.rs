use serde_json::{Map, Value};
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::core::PromptCachingConfig;

use super::extract_reasoning_trace;
use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, SystemPromptPlacement, impl_openai_compat_provider};
use crate::provider::{LLMError, LLMRequest};

pub struct DeepSeekSpec;

fn deepseek_reasoning(message: &Value, choice: &Value) -> Option<String> {
    message
        .get("reasoning_content")
        .and_then(extract_reasoning_trace)
        .or_else(|| message.get("reasoning").and_then(extract_reasoning_trace))
        .or_else(|| choice.get("reasoning_content").and_then(extract_reasoning_trace))
}

impl OpenAiCompatSpec for DeepSeekSpec {
    const NAME: &'static str = "DeepSeek";
    const KEY: &'static str = "deepseek";
    const API_KEY_ENV: &'static str = "DEEPSEEK_API_KEY";
    const DEFAULT_MODEL: &'static str = models::deepseek::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::DEEPSEEK_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::DEEPSEEK_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::deepseek::SUPPORTED_MODELS;
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = Some(models::deepseek::SUPPORTED_MODELS);

    const SYSTEM_PROMPT: SystemPromptPlacement = SystemPromptPlacement::TopLevelField;
    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    const INCLUDE_USER_ID: bool = true;
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(deepseek_reasoning);

    fn prompt_cache_enabled(prompt_cache: Option<&PromptCachingConfig>) -> bool {
        prompt_cache.is_some_and(|cfg| {
            let settings = &cfg.providers.deepseek;
            cfg.enabled && settings.enabled && settings.surface_metrics
        })
    }

    fn response_cache_metrics(core: &OpenAiCompatCore<Self>) -> bool {
        core.prompt_cache_enabled
    }

    fn stream_cache_metrics(_core: &OpenAiCompatCore<Self>) -> bool {
        true
    }

    fn insert_reasoning(
        core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        if let Some(effort) = request.reasoning_effort {
            if effort == vtcode_config::types::ReasoningEffortLevel::None {
                payload.insert("thinking".to_owned(), serde_json::json!({"type": "disabled"}));
            } else {
                use crate::rig_adapter::RigProviderCapabilities;
                use vtcode_config::models::Provider;
                let supported = crate::provider::catalog_or_explicit_reasoning_efforts(
                    Self::KEY,
                    &request.model,
                    core.model_behavior
                        .as_ref()
                        .and_then(|behavior| behavior.model_supports_reasoning_effort)
                        .unwrap_or(false),
                );
                if let Some(params) = RigProviderCapabilities::new(Provider::DeepSeek, &request.model)
                    .reasoning_parameters_for_supported_efforts(effort, supported)?
                    && let Some(obj) = params.as_object()
                {
                    for (k, v) in obj {
                        payload.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        Ok(())
    }

    fn finish_payload(
        _core: &OpenAiCompatCore<Self>,
        _request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        // DeepSeek vision Files API requires flat `file_id`/`file_data` at top level
        // of the `file` content part, not nested under `file`. `common.rs` emits
        // nested `{file:{...}}` for provider-agnostic compat; promote to flat here.
        let Some(messages) = payload.get_mut("messages").and_then(|m| m.as_array_mut()) else {
            return Ok(());
        };
        for msg in messages {
            let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
                continue;
            };
            for part in content {
                if part.get("type").and_then(|t| t.as_str()) != Some("file") {
                    continue;
                }
                // Clone nested file object if present and promote keys to top level.
                if let Some(nested) = part.get("file").and_then(|f| f.as_object()).cloned() {
                    let Some(obj) = part.as_object_mut() else { continue };
                    for (k, v) in nested {
                        obj.entry(k).or_insert(v);
                    }
                }
            }
        }
        Ok(())
    }
}

impl_openai_compat_provider!(DeepSeekProvider, DeepSeekSpec, {
    fn supports_vision(&self, model: &str) -> bool {
        let raw = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };
        let normalized = raw.trim().rsplit('/').next().unwrap_or(raw).trim().to_ascii_lowercase();
        normalized == models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let raw = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };
        let normalized = raw.trim().rsplit('/').next().unwrap_or(raw).trim().to_ascii_lowercase();
        // Codex-inspired robustness: Setting model_supports_reasoning to false
        // does NOT disable it for known reasoning models.
        self.core
            .model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning)
            .unwrap_or(false)
            || vtcode_config::models::model_catalog_entry("deepseek", &normalized).is_some_and(|entry| entry.reasoning)
            || normalized == models::deepseek::DEEPSEEK_V4_PRO
            || normalized == models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let raw = if model.trim().is_empty() {
            &self.core.model
        } else {
            model
        };
        let normalized = raw.trim().rsplit('/').next().unwrap_or(raw).trim().to_ascii_lowercase();
        // Curated catalog effort levels are authoritative for known routes;
        // retain the legacy flags for explicitly configured or dynamic models.
        self.core
            .model_behavior
            .as_ref()
            .and_then(|b| b.model_supports_reasoning_effort)
            .unwrap_or(false)
            || vtcode_config::models::model_catalog_entry("deepseek", &normalized)
                .is_some_and(|entry| !entry.reasoning_efforts.is_empty())
            || normalized == "deepseek-reasoner"
    }

    async fn get_balance(&self) -> Result<Option<vtcode_commons::llm::BalanceInfo>, LLMError> {
        // Strip /v1 suffix to get the root API URL for the balance endpoint.
        let base = self.core.base_url.trim_end_matches('/');
        let root = base.strip_suffix("/v1").unwrap_or(base);
        let url = format!("{root}/user/balance");

        let response = self
            .core
            .http_client
            .get(&url)
            .bearer_auth(&self.core.api_key)
            .send()
            .await
            .map_err(|e| LLMError::Network {
                message: crate::error_display::format_llm_error(
                    <DeepSeekSpec as OpenAiCompatSpec>::NAME,
                    &format!("balance request failed: {e}"),
                ),
                metadata: None,
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = crate::providers::common::read_provider_error_body(response).await;
            return Err(LLMError::Provider {
                message: crate::error_display::format_llm_error(
                    <DeepSeekSpec as OpenAiCompatSpec>::NAME,
                    &format!("balance API returned {status}: {body}"),
                ),
                metadata: None,
            });
        }

        let balance_resp: vtcode_commons::llm::DeepSeekBalanceResponse =
            response.json().await.map_err(|e| LLMError::Provider {
                message: crate::error_display::format_llm_error(
                    <DeepSeekSpec as OpenAiCompatSpec>::NAME,
                    &format!("failed to parse balance response: {e}"),
                ),
                metadata: None,
            })?;

        Ok(Some(balance_resp.into()))
    }
});

#[cfg(test)]
mod tests {
    use super::DeepSeekProvider;
    use crate::provider::{ImageDetail, LLMProvider, LLMRequest, Message, ToolChoice};
    use crate::reasoning_effort::ReasoningEffortMapper;
    use std::sync::Arc;
    use vtcode_config::constants::models;
    use vtcode_config::types::ReasoningEffortLevel;

    #[test]
    fn catalog_efforts_are_exposed_to_resolution_and_request_building() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        let model = models::deepseek::DEEPSEEK_V4_PRO;

        assert_eq!(provider.supported_reasoning_efforts(model), &["low", "high", "max"]);
        let mapping = ReasoningEffortMapper::resolve(&provider, model, ReasoningEffortLevel::High, false)
            .expect("catalog-supported DeepSeek effort should resolve");
        assert_eq!(mapping.effective, ReasoningEffortLevel::High);
    }

    fn base_request() -> LLMRequest {
        LLMRequest {
            messages: vec![Message::user("hello".to_string())].into(),
            system_prompt: Some(Arc::from("system guidance")),
            model: models::deepseek::DEFAULT_MODEL.to_string(),
            max_tokens: Some(512),
            temperature: Some(0.5),
            top_p: Some(0.25),
            stream: true,
            tool_choice: Some(ToolChoice::Auto),
            metadata: Some(serde_json::json!({"user_id": "user-42"})),
            ..Default::default()
        }
    }

    #[test]
    fn golden_payload_basic_shape() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        let payload = provider.core.convert_request(&base_request()).unwrap();

        assert_eq!(payload["model"], models::deepseek::DEFAULT_MODEL);
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        // DeepSeek sends the system prompt as a top-level field, not a message.
        assert_eq!(payload["system"], "system guidance");
        assert_eq!(payload["max_tokens"], 512);
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["tool_choice"], "auto");
        assert_eq!(payload["user_id"], "user-42");
        assert!(payload.get("thinking").is_none());
    }

    #[test]
    fn golden_payload_thinking_disabled_and_sampling_suppression() {
        let provider = DeepSeekProvider::new("test-key".to_string());

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::None);
        let payload = provider.core.convert_request(&request).unwrap();
        assert_eq!(payload["thinking"]["type"], "disabled");
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["top_p"], 0.25);

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::High);
        let payload = provider.core.convert_request(&request).unwrap();
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["reasoning_effort"], "high");
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::Max);
        let payload = provider.core.convert_request(&request).unwrap();
        assert_eq!(payload["reasoning_effort"], "max");
    }

    #[test]
    fn vision_capability_is_exclusive_to_flash_vision_exp() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        assert!(provider.supports_vision(models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP));
        assert!(!provider.supports_vision(models::deepseek::DEEPSEEK_V4_FLASH));
        assert!(!provider.supports_vision(models::deepseek::DEEPSEEK_V4_PRO));
        assert!(!provider.supports_vision(""));
        // empty string should check default model (pro) => false
        let provider_flash =
            DeepSeekProvider::with_model("k".to_string(), models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string());
        assert!(provider_flash.supports_vision(""));
    }

    #[test]
    fn golden_payload_vision_base64_inline() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        let msg = Message::user_with_parts(vec![
            crate::provider::ContentPart::text("What is in this image?".to_string()),
            crate::provider::ContentPart::image("abc123b64".to_string(), "image/jpeg".to_string()),
        ]);
        let req = LLMRequest {
            messages: vec![msg].into(),
            model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
            ..Default::default()
        };
        let payload = provider.core.convert_request(&req).unwrap();
        assert_eq!(payload["model"], models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP);
        let content = &payload["messages"][0]["content"];
        let arr = content.as_array().expect("vision content should be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[1]["type"], "image_url");
        assert_eq!(arr[1]["image_url"]["url"], "data:image/jpeg;base64,abc123b64");
        assert!(arr[1]["image_url"].get("detail").is_none());
    }

    #[test]
    fn golden_payload_vision_detail_and_external_url() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        // detail low for base64
        let msg_low = Message::user_with_parts(vec![crate::provider::ContentPart::image_with_detail(
            "b64".to_string(),
            "image/png".to_string(),
            ImageDetail::Low,
        )]);
        let payload_low = provider
            .core
            .convert_request(&LLMRequest {
                messages: vec![msg_low].into(),
                model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(payload_low["messages"][0]["content"][0]["image_url"]["detail"], "low");

        // external URL with high detail
        let msg_url = Message::user_with_parts(vec![
            crate::provider::ContentPart::image_from_url(
                "https://example.com/image.jpg".to_string(),
                Some(ImageDetail::High),
            )
            .unwrap(),
        ]);
        let payload_url = provider
            .core
            .convert_request(&LLMRequest {
                messages: vec![msg_url].into(),
                model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(payload_url["messages"][0]["content"][0]["image_url"]["url"], "https://example.com/image.jpg");
        assert_eq!(payload_url["messages"][0]["content"][0]["image_url"]["detail"], "high");

        // original and auto also accepted
        for (detail, expected) in [(ImageDetail::Original, "original"), (ImageDetail::Auto, "auto")] {
            let m = Message::user_with_parts(vec![
                crate::provider::ContentPart::image_from_url("https://example.com/a.png".to_string(), Some(detail))
                    .unwrap(),
            ]);
            let p = provider
                .core
                .convert_request(&LLMRequest {
                    messages: vec![m].into(),
                    model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(p["messages"][0]["content"][0]["image_url"]["detail"], expected);
        }
    }

    #[test]
    fn golden_payload_vision_files_api() {
        let provider = DeepSeekProvider::new("test-key".to_string());
        // file_id reference (preferred for >32MiB or reused images)
        let msg_file_id = Message::user_with_parts(vec![
            crate::provider::ContentPart::text("What is in this image?".to_string()),
            crate::provider::ContentPart::File {
                content_type: "file".to_string(),
                filename: None,
                file_id: Some("file-api-xxxxxxxxxxxxxxxx".to_string()),
                file_data: None,
                file_url: None,
            },
        ]);
        let payload = provider
            .core
            .convert_request(&LLMRequest {
                messages: vec![msg_file_id].into(),
                model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
                ..Default::default()
            })
            .unwrap();
        let arr = payload["messages"][0]["content"].as_array().unwrap();
        assert_eq!(arr[1]["type"], "file");
        assert_eq!(arr[1]["file_id"], "file-api-xxxxxxxxxxxxxxxx");
        // legacy nested still present
        assert_eq!(arr[1]["file"]["file_id"], "file-api-xxxxxxxxxxxxxxxx");

        // inline file_data with filename
        let msg_file_data = Message::user_with_parts(vec![crate::provider::ContentPart::File {
            content_type: "file".to_string(),
            filename: Some("image.jpg".to_string()),
            file_id: None,
            file_data: Some("data:image/jpeg;base64,abc".to_string()),
            file_url: None,
        }]);
        let payload2 = provider
            .core
            .convert_request(&LLMRequest {
                messages: vec![msg_file_data].into(),
                model: models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string(),
                ..Default::default()
            })
            .unwrap();
        let arr2 = payload2["messages"][0]["content"].as_array().unwrap();
        assert_eq!(arr2[0]["type"], "file");
        assert_eq!(arr2[0]["filename"], "image.jpg");
        assert!(arr2[0].get("file_data").is_some());
    }

    #[test]
    fn supported_models_includes_vision() {
        let provider = DeepSeekProvider::new("k".to_string());
        let models = provider.supported_models();
        assert!(models.contains(&models::deepseek::DEEPSEEK_V4_FLASH_VISION_EXP.to_string()));
        assert!(models.contains(&models::deepseek::DEEPSEEK_V4_FLASH.to_string()));
        assert!(models.contains(&models::deepseek::DEEPSEEK_V4_PRO.to_string()));
    }
}
