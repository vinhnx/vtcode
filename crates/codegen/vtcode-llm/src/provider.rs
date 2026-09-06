#![expect(
    unused_results,
    reason = "Provider abstraction caches and compatibility adapters intentionally discard replacement results after state updates."
)]

//! Universal LLM provider abstraction with API-specific role handling
//!
//! This module provides a unified interface for different LLM providers (OpenAI, Anthropic, Gemini)
//! while properly handling their specific requirements for message roles and tool calling.
//!
//! ## Message Role Mapping
//!
//! Different LLM providers have varying support for message roles, especially for tool calling:
//!
//! ### OpenAI API
//! - **Full Support**: `system`, `user`, `assistant`, `tool`
//! - **Tool Messages**: Must include `tool_call_id` to reference the original tool call
//! - **Tool Calls**: Only `assistant` messages can contain `tool_calls`
//!
//! ### Anthropic API
//! - **Standard Roles**: `user`, `assistant`
//! - **System Messages**: Can be hoisted to system parameter or treated as user messages
//! - **Tool Responses**: Converted to `user` messages (no separate tool role)
//! - **Tool Choice**: Supports `auto`, `any`, `tool`, `none` modes
//!
//! ### Gemini API
//! - **Conversation Roles**: Only `user` and `model` (not `assistant`)
//! - **System Messages**: Handled separately as `systemInstruction` parameter
//! - **Tool Responses**: Converted to `user` messages with `functionResponse` format
//! - **Function Calls**: Uses `functionCall` in `model` messages
//!
//! ## Best Practices
//!
//! 1. Always use `MessageRole::tool_response()` constructor for tool responses
//! 2. Validate messages using `validate_for_provider()` before sending
//! 3. Use appropriate role mapping methods for each provider
//! 4. Handle provider-specific constraints (e.g., Gemini's system instruction requirement)
//!
//! ## Example Usage
//!
//! ```rust,ignore
//! use vtcode_llm::provider::{Message, MessageRole};
//!
//! // Create a proper tool response message
//! let tool_response = Message::tool_response(
//!     "call_123".to_string(),
//!     "Tool execution completed successfully".to_string()
//! );
//!
//! // Validate for specific provider
//! tool_response.validate_for_provider("openai").unwrap();
//! ```

mod call;
mod message;
mod provider_trait;
mod request;
mod response;
mod responses_continuation;
#[cfg(test)]
mod tests;
mod tool;

pub use call::{FunctionCall, ToolCall};
pub use message::{AssistantPhase, ContentPart, ImageDetail, Message, MessageClearAt, MessageContent, MessageRole};
pub use provider_trait::{
    ContextWindowProvider, LLMError, LLMErrorMetadata, LLMProvider, ProviderCapabilities, get_cached_capabilities,
};
pub(crate) use provider_trait::{
    GENERIC_REASONING_EFFORTS, catalog_context_window, catalog_or_explicit_reasoning_efforts,
    catalog_or_generic_reasoning_efforts, catalog_reasoning_efforts,
};
pub use request::{
    AnthropicOptionalStringOverride, AnthropicOptionalU32Override, AnthropicRequestOverrides, AnthropicThinkingConfig,
    AnthropicThinkingDisplayOverride, AnthropicThinkingModeOverride, CodingAgentSettings, FallbackModel, LLMRequest,
    ParallelToolConfig, PromptCacheProfile, ResponsesCompactionOptions, SamplingOverrides, SpecificFunctionChoice,
    SpecificToolChoice, ToolChoice,
};
pub use response::{
    BorrowedLLMStream, FinishReason, LLMNormalizedStream, LLMResponse, LLMStream, LLMStreamEvent,
    NormalizedStreamEvent, Usage,
};
pub use responses_continuation::{
    PreparedResponsesRequest, ResponsesContinuationState, prepare_openai_responses_request,
    prepare_responses_continuation_request, records_responses_continuation_state, responses_continuation_key,
    supports_responses_chaining, uses_incremental_responses_history,
};
pub use tool::{
    FunctionDefinition, GrammarDefinition, ShellToolDefinition, ToolDefinition, ToolNamespace, ToolSearchAlgorithm,
};
