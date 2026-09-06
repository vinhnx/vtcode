# Context Engineering in VT Code

## Overview

VT Code implements context engineering principles based on [Anthropic's research](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents) to manage the "attention budget" of large language models effectively. This document explains the strategies and features we use to prevent context rot and maintain agent coherence across long-horizon tasks.

## Context Engineering vs Prompt Engineering

### Single-Turn Prompt Engineering

Traditional prompt engineering focuses on crafting a single prompt for discrete tasks:

-   **Input**: System prompt + User message
-   **Output**: Assistant message
-   **Process**: One-shot, static

### Multi-Turn Context Engineering (Agents)

Context engineering is about **iterative curation** - deciding what context to pass to the model on each turn:

**Available Context:**

-   Documentation, tools, memory files
-   Comprehensive instructions, domain knowledge
-   Message history, previous tool results

↓ **Curation (happens each turn)** ↓

**Selected Context:**

-   System prompt
-   Relevant docs (not all docs)
-   Memory file summary
-   Relevant tools (not all tools)
-   User message
-   Recent message history (not full history)

→ [Model] → Assistant message → Tool call → Tool result → **Next turn curation**

**Key Insight:** Unlike prompt engineering where you craft a prompt once, context engineering is **iterative** - the curation phase happens each time we decide what to pass to the model.

## VT Code's Three Context Primitives

VT Code now treats long-horizon context management as three separate jobs:

- **Memory** keeps durable facts outside the live prompt. VT Code uses persistent per-repository memory under `/memories` (`memory_summary.md`, `preferences.md`, `repository-facts.md`, `notes/**`) plus the session-local `SessionMemoryEnvelope` used for resume and summarized forks.
- **Compaction** shrinks the whole in-session transcript when token pressure gets high. VT Code can use provider-native compaction where available, and otherwise falls back to local compaction that preserves a structured summary, recent user prompts, and the session memory envelope.
- **Tool-result clearing / offloading** keeps re-fetchable tool output out of the live window. VT Code does this locally with split tool results and output spooling, and on Anthropic it can now emit provider-native `clear_tool_uses_*` edits alongside native compaction.

Those layers compose, but they solve different problems:

- Use **memory** when information must survive a new session.
- Use **compaction** when the whole transcript is getting too large.
- Use **tool-result clearing/offloading** when the context is mostly bloated by large, recoverable tool payloads.

## Accuracy Optimization Loop

VT Code treats LLM optimization as a diagnosis problem, not a fixed ladder from prompt engineering to RAG to fine-tuning.

### Start with a baseline

Before adding more machinery:

-   Define what "correct" means for the task.
-   Start with the simplest prompt that can work.
-   Keep a small eval set of realistic prompts and expected outcomes.
-   Review failures before changing the system.

### Choose the right lever

When an eval fails, classify the issue before changing the stack:

-   **Context problem**: The model is missing domain knowledge, using stale facts, or needs proprietary information. Improve context selection, retrieval, or source quality.
-   **Behavior problem**: The model has the right facts but formats inconsistently, ignores instructions, uses the wrong tone, or reasons unreliably. Improve instructions, examples, decomposition, or training.

These levers stack, but they do different jobs. VT Code should not add retrieval or longer prompts unless the failure suggests a context problem.

### Retrieval and long-context guidance

-   Retrieved context should be relevant enough to change the answer.
-   Extra but irrelevant context can lower accuracy by drowning out the key facts.
-   Long-context setups must still be evaluated at different context sizes because important details can get lost in the middle.

### Production stance

For high-stakes flows, optimize not only for answer quality but for failure handling:

-   Ask clarifying questions when confidence is low.
-   Fall back to narrower, safer actions when context is incomplete.
-   Prefer human review or explicit escalation over confident guessing.

## Core Principles

### 1. **Minimal Token Usage ("Right Altitude" Prompts)**

Our system prompts strike a balance between specificity and flexibility:

-   **Concise Instructions**: Clear guidance without prescriptive micromanagement
-   **Progressive Disclosure**: Load information layer-by-layer as needed
-   **Heuristics Over Rules**: Provide strong patterns rather than exhaustive edge cases

Example from our default prompt:

```
## Context Strategy
- Use search tools (rg, grep, ripgrep) to find relevant code before reading files
- Load file metadata (paths, sizes) as references; read content only when necessary
- Summarize tool outputs; avoid echoing large results
- Preserve recent decisions and errors in your working memory
```

### 2. **Just-in-Time Context Loading**

Instead of pre-loading everything, we use lightweight references:

-   **File Paths as Metadata**: List files first, read content only when relevant
-   **Search Before Read**: Use shell `rg` through `exec_command.cmd` to identify relevant files
-   **Chunked Reading**: Auto-truncate large files (>2000 lines) to first/last portions
-   **Pagination**: Tools support `per_page` and `page` parameters for large results

### 3. **Token Budget Management**

We track token usage across the context window to prevent exceeding limits:

```rust
use vtcode_core::core::token_budget::{TokenBudgetManager, TokenBudgetConfig, ContextComponent};

// Initialize tracker - use latest models from docs/models.json
let config = TokenBudgetConfig::for_model("gpt-5-mini", 400_000);
let manager = TokenBudgetManager::new(config);

// Track token usage
let tokens = manager.count_tokens_for_component(
    text,
    ContextComponent::ToolResult,
    Some("file_read_1")
).await?;

// Check thresholds
if manager.is_alert_threshold_exceeded().await {
    // Issue alert/warning
}
```

**Token Budget Features:**

-   Real-time token counting using Hugging Face `tokenizers`
-   Component-level tracking (system prompt, user messages, tool results, etc.)
-   Configurable warning thresholds
-   Automatic deduction after context cleanup

### 4. **Decision Ledger (Structured Note-Taking)**

The decision tracker maintains persistent memory across turns:

```rust
use vtcode_core::core::decision_tracker::DecisionTracker;

let mut tracker = DecisionTracker::new();

// Record decisions
let decision_id = tracker.record_decision(
    "Reading config file to understand project structure".to_string(),
    Action::ToolCall {
        name: "exec_command".to_string(),
        args: json!({"cmd": "sed -n '1,120p' vtcode.toml"}),
        expected_outcome: "Configuration loaded".to_string(),
    },
    Some(0.9), // confidence score
);

// Generate compact ledger for prompt injection
let ledger_summary = tracker.render_ledger_brief(12);
```

The ledger is automatically injected into the system prompt if configured:

```toml
[context.ledger]
enabled = true
max_entries = 12
include_in_prompt = true
preserve_in_compression = true
```

### 5. **Tool Result Clearing and Summarization**

To prevent context pollution from verbose tool outputs:

-   **Auto-Truncation**: Command outputs >10k lines show first 5k + last 5k
-   **Concise Formats**: Tools default to `response_format="concise"`
-   **Split Results / Spooling**: Large tool outputs stay on disk or in UI-facing channels instead of being replayed verbatim to the model
-   **Provider-Native Clearing**: Anthropic sessions can drop stale tool results while retaining the fact that the tool call happened

### 7. **Tool Design for Efficiency**

Our tools are designed with context efficiency in mind:

#### Search Tools

-   **exec_command**: Shell file inspection and fast text matching through
    `rg`, with `grep` as a fallback when `rg` is unavailable
-   **code_search**: Advanced bounded search for recognised definitions, exact
    syntactic usages, literal text, and matching paths
-   Return metadata first (file paths, line numbers) before content

#### File Operations

-   **exec_command**: Inspect files with shell commands such as `rg`, `sed`,
    `cat`, `ls`, and `wc`
-   **apply_patch**: Precise repository edits through unified patches, avoiding
    whole-file rewrites for small changes

#### Command Execution

-   **exec_command**: Run shell commands with output limits and reusable
    sessions for long-running processes
-   **write_stdin**: Continue or interact with an existing live command session

## Configuration

### Token Budget Settings

```toml
[context.token_budget]
enabled = true
# Model for tokenizer - use latest models from docs/models.json
# Examples: "gpt-5-mini", "gpt-5-nano", "claude-sonnet-4", "deepseek-chat"
model = "gpt-5-nano"
warning_threshold = 0.75  # Warn at 75% usage

detailed_tracking = false  # Enable for debugging
```

### Context Management

```toml
[context]
max_context_tokens = 160000
trim_to_percent = 80
preserve_recent_turns = 5

[context.ledger]
enabled = true
max_entries = 12
include_in_prompt = true
preserve_in_compression = true
```

`context.max_context_tokens` is the optional session prompt safety ceiling. With
no explicit harness compaction threshold, VT Code compacts when the estimated
request input plus reserved output reaches the smaller of this ceiling, the
resolved model capacity, and the provider route capacity. Set it to `0` to use
the resolved capacity without an additional session ceiling.

## Best Practices

### For Users

1. **Start Broad, Drill Down**: Use search tools to explore before reading files
2. **Paginate Large Results**: Use `per_page=50` for directory listings
3. **Review Budget**: Check token usage with `/status` command
4. **Leverage Ledger**: Reference past decisions instead of re-explaining

### For Developers

1. **Tool Design**: Return lightweight metadata before full content
2. **Result Limits**: Always provide `max_results` parameters
3. **Format Options**: Offer `concise` vs `detailed` response formats
4. **Chunking**: Auto-chunk large outputs (files, logs, listings)
5. **Summarization**: Compress verbose outputs automatically

## Monitoring

### Token Budget Reports

```rust
let report = manager.generate_report().await;
println!("{}", report);
```

Output:

```
Token Budget Report
==================
Total Tokens: 45000/128000 (35.2%)
Remaining: 83000 tokens

Breakdown by Category:
- System Prompt: 2500 tokens
- User Messages: 8000 tokens
- Assistant Messages: 12000 tokens
- Tool Results: 20000 tokens
- Decision Ledger: 2500 tokens
```

### Component Tracking

Enable detailed tracking for debugging:

```toml
[context.token_budget]
detailed_tracking = true
```

Then inspect per-component usage:

```rust
let breakdown = manager.get_component_breakdown().await;
for (component, tokens) in breakdown {
    println!("{}: {} tokens", component, tokens);
}
```

## Performance Considerations

### Token Counting Overhead

-   Uses Hugging Face `tokenizers` with heuristic fallback when pretrained assets are unavailable
-   ~10μs per message for typical sizes
-   Caching minimizes repeated tokenization
-   Disable `detailed_tracking` in production for best performance

### Memory Efficiency

-   LRU caches for tokenizer instances
-   Incremental tracking (no full recount needed)
-   Deduplication of identical content

## Future Enhancements

### Planned Features

1. **Sub-Agent Architecture**: Specialized agents with focused context windows
2. **Semantic Chunking**: Content-aware splitting for better preservation
3. **Context Swapping**: Hot-swap between task-specific contexts
4. **Adaptive Thresholds**: Learn optimal warning points per task type
5. **Multi-Model Support**: Per-provider tokenizers (Claude, Gemini)

## References

-   [Anthropic: Effective Context Engineering for AI Agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)
-   [Hugging Face tokenizers Documentation](https://huggingface.co/docs/tokenizers/index)
-   [Context Rot Research (Chroma)](https://research.trychroma.com/context-rot)

## Related Documentation

- [Configuration Guide](../config/config.md)
- [Tool Development Guide](../tools/TOOL_SPECS.md)
- [Architecture Guide](../ARCHITECTURE.md)
