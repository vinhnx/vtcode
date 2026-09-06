//! Universal LLM provider abstraction with API-specific role handling.
//!
//! The canonical trait, request/response types, and tool definitions live in
//! [`vtcode_llm::provider`]. This module re-exports them so the historical
//! `crate::llm::provider::*` import paths continue to resolve throughout
//! `vtcode-core`.

pub use vtcode_llm::provider::{
    AnthropicOptionalStringOverride, AnthropicOptionalU32Override, AnthropicRequestOverrides, AnthropicThinkingConfig,
    AnthropicThinkingDisplayOverride, AnthropicThinkingModeOverride, AssistantPhase, BorrowedLLMStream,
    CodingAgentSettings, ContentPart, ContextWindowProvider, FallbackModel, FinishReason, FunctionCall,
    FunctionDefinition, GrammarDefinition, LLMError, LLMErrorMetadata, LLMNormalizedStream, LLMProvider, LLMRequest,
    LLMResponse, LLMStream, LLMStreamEvent, Message, MessageClearAt, MessageContent, MessageRole,
    NormalizedStreamEvent, ParallelToolConfig, PreparedResponsesRequest, PromptCacheProfile, ProviderCapabilities,
    ResponsesCompactionOptions, ResponsesContinuationState, ShellToolDefinition, SpecificFunctionChoice,
    SpecificToolChoice, ToolCall, ToolChoice, ToolDefinition, ToolNamespace, ToolSearchAlgorithm, Usage,
    get_cached_capabilities, prepare_openai_responses_request, prepare_responses_continuation_request,
    records_responses_continuation_state, responses_continuation_key, supports_responses_chaining,
    uses_incremental_responses_history,
};
