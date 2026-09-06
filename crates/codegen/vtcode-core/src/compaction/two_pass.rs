//! Pure builders for prefire two-pass compaction.
//!
//! Pass1 summarizes ~95% of history (by estimated-token weight) -> NOTE₁.
//! Pass2 rewrites NOTE₁ + the ~5% tail into the successor-visible NOTE₂.
//! Sampling lives in [`super`]; this module has no I/O.

use crate::llm::provider::Message;

/// Default history fraction covered by pass1; the remainder is the blocking
/// pass2 tail, so keep it small (prod pass2 latency is dominated by tail prefill).
pub const TWO_PASS_DEFAULT_SPLIT_FRACTION: f64 = 0.95;

/// Minimum char length for a closed `<summary>` block to be preferred as NOTE₁
/// over the full pass1 response.
const TWO_PASS_MIN_SUMMARY_BLOCK_CHARS: usize = 1000;

/// Cap on NOTE₁ text embedded in pass2 (carrier + special turn).
const TWO_PASS_MAX_NOTE1_CHARS: usize = 12_000;

/// Result of splitting a conversation for two-pass compaction.
#[derive(Debug, Clone, Copy)]
pub struct TwoPassSplit<'a> {
    pub prefix: &'a [Message],
    pub tail: &'a [Message],
    pub split_idx: usize,
}

/// Choose a split index so prefix weight is at least `fraction` of total.
#[allow(
    clippy::cast_sign_loss,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
fn split_index_by_token_fraction(weights: &[usize], fraction: f64) -> usize {
    if weights.is_empty() {
        return 0;
    }
    let frac = fraction.clamp(0.05, 0.95);
    let total_w: usize = weights.iter().copied().sum::<usize>().max(1);
    let target_w = (frac * total_w as f64).round() as u64 as usize;
    let mut acc = 0usize;
    let mut split_idx = weights.len().saturating_sub(1).max(1);
    for (i, w) in weights.iter().enumerate() {
        acc = acc.saturating_add(*w);
        if acc >= target_w {
            split_idx = (i + 1).max(1);
            break;
        }
    }
    if split_idx >= weights.len() && weights.len() > 1 {
        split_idx = weights.len() - 1;
    }
    split_idx
}

/// Never separate an assistant `tool_calls` turn from its following `tool_call_id` results.
fn snap_split_idx_to_tool_boundaries(conversation: &[Message], mut split_idx: usize) -> usize {
    let n = conversation.len();
    if n == 0 {
        return 0;
    }
    split_idx = split_idx.min(n);

    while split_idx < n {
        let Some(msg) = conversation.get(split_idx) else {
            break;
        };
        if msg.tool_call_id.is_some() {
            split_idx += 1;
        } else {
            break;
        }
    }
    if split_idx < n
        && let Some(msg) = conversation.get(split_idx)
        && msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    {
        split_idx += 1;
        while split_idx < n {
            let Some(msg) = conversation.get(split_idx) else {
                break;
            };
            if msg.tool_call_id.is_some() {
                split_idx += 1;
            } else {
                break;
            }
        }
    }
    while split_idx > 0 && split_idx < n {
        let Some(msg) = conversation.get(split_idx - 1) else {
            break;
        };
        if msg.tool_calls.as_ref().is_some_and(|tool_calls| !tool_calls.is_empty()) {
            if conversation.get(split_idx).map(|m| m.tool_call_id.is_some()) != Some(true) {
                break;
            }
            while split_idx < n {
                let Some(msg) = conversation.get(split_idx) else {
                    break;
                };
                if msg.tool_call_id.is_some() {
                    split_idx += 1;
                } else {
                    break;
                }
            }
        } else {
            break;
        }
    }

    if split_idx >= n && n > 1 {
        let mut candidate = n - 1;
        while candidate > 1 {
            if let Some(msg) = conversation.get(candidate) {
                if msg.tool_call_id.is_none() {
                    break;
                }
                candidate -= 1;
            } else {
                break;
            }
        }
        if candidate > 0
            && let Some(msg) = conversation.get(candidate)
            && msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
        {
            if conversation.get(candidate + 1).map(|m| m.tool_call_id.is_some()) == Some(true) {
                split_idx = candidate;
            }
        } else if candidate > 0 && conversation.get(candidate).map(|m| m.tool_call_id.is_some()) == Some(true) {
            let mut i = candidate;
            while i > 0 && conversation.get(i - 1).map(|m| m.tool_call_id.is_some()) == Some(true) {
                i -= 1;
            }
            if conversation
                .get(i)
                .is_some_and(|m| m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty()))
            {
                split_idx = i;
            }
        }
        if candidate >= 1 && candidate < n {
            split_idx = candidate;
        }
    }

    split_idx.min(n)
}

/// Split `conversation` into pass1 prefix / pass2 tail by estimated-token weight.
pub fn split_conversation_for_two_pass(conversation: &[Message], split_fraction: f64) -> TwoPassSplit<'_> {
    let weights: Vec<usize> = conversation.iter().map(|m| m.estimate_tokens()).collect();
    let mut split_idx = split_index_by_token_fraction(&weights, split_fraction);
    split_idx = snap_split_idx_to_tool_boundaries(conversation, split_idx);
    let split_idx = split_idx.min(conversation.len());
    TwoPassSplit {
        prefix: &conversation[..split_idx],
        tail: &conversation[split_idx..],
        split_idx,
    }
}

fn extract_summary_block(text: &str, min_chars: usize) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let open = "<summary>";
    let close = "</summary>";
    let lower = text.to_ascii_lowercase();
    let mut blocks: Vec<String> = Vec::new();
    let text_bytes = text.as_bytes();
    let lower_bytes = lower.as_bytes();
    let mut search_from = 0usize;
    while search_from < lower_bytes.len() {
        let Some(rel) = find_bytes(&lower_bytes[search_from..], open.as_bytes()) else {
            break;
        };
        let start = search_from + rel + open.len();
        let Some(rel_close) = find_bytes(&lower_bytes[start..], close.as_bytes()) else {
            break;
        };
        let end = start + rel_close;
        let inner = std::str::from_utf8(&text_bytes[start..end]).unwrap_or("");
        blocks.push(inner.to_string());
        search_from = end + close.len();
    }
    for block in blocks.into_iter().rev() {
        let stripped = block.trim();
        if stripped.chars().count() > min_chars {
            return Some(stripped.to_string());
        }
    }
    None
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Prefer a substantive `<summary>` inner for NOTE₁; otherwise the full pass1 response.
pub fn note_for_two_pass_pass2(pass1_raw: &str) -> String {
    let mut note = extract_summary_block(pass1_raw, TWO_PASS_MIN_SUMMARY_BLOCK_CHARS)
        .unwrap_or_else(|| pass1_raw.trim().to_string());
    let n = note.chars().count();
    if n > TWO_PASS_MAX_NOTE1_CHARS {
        note = note.chars().take(TWO_PASS_MAX_NOTE1_CHARS).collect();
        note.push_str("\n\n[… NOTE₁ truncated for pass2 input budget …]");
    }
    note
}

fn format_two_pass_note1_carrier(note1: &str) -> String {
    let note1 = note1.trim();
    format!(
        "Your conversation was summarized due to context constraints. \
         Here is the summary of the conversation so far:\n\n\
         <summary_content>\n{note1}\n</summary_content>\n\n\
         Continue with the compaction task below."
    )
}

fn format_two_pass_special_pass2_user(note1: &str, compaction_prompt: &str) -> String {
    let note1 = note1.trim();
    let summary_block = format!("<summary_content>\n{note1}\n</summary_content>");
    let uq = if compaction_prompt.trim().is_empty() {
        "Please summarize the conversation so far."
    } else {
        compaction_prompt
    };
    format!(
        "This is a special compaction case (two-pass / hierarchical summarization).\n\
         You are writing the *final* compaction note that a successor assistant will \
         rely on as their only memory of the conversation.\n\n\
         Critical requirements:\n\
         - Incorporate the **entire** prior summary below into your final note — do not \
         omit sections, defer to \"see prior compaction\", or drop early history because \
         newer turns are in context.\n\
         - Merge that prior summary with the more recent conversation turns above into \
         one coherent, faithful, self-contained summary (same structure/sections you \
         normally use for compaction).\n\
         - Preserve concrete values, file paths, errors/blockers, operational how-tos, \
         key findings, and pending tasks from *both* the prior summary and the recent \
         turns when they still matter.\n\n\
         Prior summary to incorporate in full (duplicate of the summary_content above):\n\n\
         {summary_block}\n\n\
         Compaction instruction:\n\
         {uq}"
    )
}

/// Pass1 sample history: `prefix` + compaction instruction user turn.
pub fn build_two_pass_pass1_history(prefix: &[Message], compaction_prompt: &str) -> Vec<Message> {
    let mut history = prefix.to_vec();
    history.push(Message {
        role: crate::llm::provider::MessageRole::User,
        content: crate::llm::provider::MessageContent::Text(compaction_prompt.to_string()),
        reasoning: None,
        reasoning_details: None,
        tool_calls: None,
        tool_call_id: None,
        phase: None,
        origin_tool: None,
        metadata: None,
        clear_at: None,
    });
    history
}

/// Pass2 sample history: system (from prefix) + NOTE₁ carrier + tail + special turn.
///
/// Successor-visible artifact is the model output of *this* history only (NOTE₂).
pub fn build_two_pass_pass2_history(
    prefix: &[Message],
    tail: &[Message],
    note1: &str,
    compaction_prompt: &str,
) -> Vec<Message> {
    let mut history: Vec<Message> = Vec::new();

    for item in prefix {
        if item.role == crate::llm::provider::MessageRole::System {
            history.push(item.clone());
        }
    }
    if history.is_empty() {
        history.push(Message {
            role: crate::llm::provider::MessageRole::System,
            content: crate::llm::provider::MessageContent::Text("You are a helpful assistant.".to_string()),
            reasoning: None,
            reasoning_details: None,
            tool_calls: None,
            tool_call_id: None,
            phase: None,
            origin_tool: None,
            metadata: None,
            clear_at: None,
        });
    }

    history.push(Message {
        role: crate::llm::provider::MessageRole::User,
        content: crate::llm::provider::MessageContent::Text(format_two_pass_note1_carrier(note1)),
        reasoning: None,
        reasoning_details: None,
        tool_calls: None,
        tool_call_id: None,
        phase: None,
        origin_tool: None,
        metadata: None,
        clear_at: None,
    });
    history.extend(tail.iter().cloned());
    history.push(Message {
        role: crate::llm::provider::MessageRole::User,
        content: crate::llm::provider::MessageContent::Text(format_two_pass_special_pass2_user(
            note1,
            compaction_prompt,
        )),
        reasoning: None,
        reasoning_details: None,
        tool_calls: None,
        tool_call_id: None,
        phase: None,
        origin_tool: None,
        metadata: None,
        clear_at: None,
    });
    history
}

use std::hash::{Hash, Hasher};

/// Cheap fingerprint of a conversation prefix for prefire NOTE₁ validity.
/// A mismatch means the prefix changed (edit / rewind / branch) since pass-1,
/// so the cached NOTE₁ no longer summarizes the current prefix.
pub fn fingerprint_prefix(prefix: &[Message]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut h = DefaultHasher::new();
    prefix.len().hash(&mut h);
    for msg in prefix {
        let tag: u8 = match msg.role {
            crate::llm::provider::MessageRole::System => 0,
            crate::llm::provider::MessageRole::User => 1,
            crate::llm::provider::MessageRole::Assistant => 2,
            crate::llm::provider::MessageRole::Tool => 3,
        };
        tag.hash(&mut h);
        msg.content.as_text().hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::{FunctionCall, MessageRole, ToolCall};

    fn make_tool_call(id: &str, name: &str) -> Option<Vec<ToolCall>> {
        Some(vec![ToolCall {
            id: id.to_string(),
            call_type: "function".to_string(),
            function: Some(FunctionCall {
                namespace: None,
                name: name.to_string(),
                arguments: "{}".to_string(),
            }),
            text: None,
            thought_signature: None,
        }])
    }

    #[test]
    fn split_leaves_tail_when_possible() {
        let weights = vec![10usize, 10, 10, 10, 10];
        let idx = split_index_by_token_fraction(&weights, 0.9);
        assert!(idx < weights.len());
        assert!(idx >= 1);
    }

    #[test]
    fn default_split_fraction_leaves_five_percent_tail() {
        assert!((TWO_PASS_DEFAULT_SPLIT_FRACTION - 0.95).abs() < f64::EPSILON);
        let weights = vec![10usize; 40];
        let idx = split_index_by_token_fraction(&weights, TWO_PASS_DEFAULT_SPLIT_FRACTION);
        assert_eq!(idx, 38);
        assert!(idx < weights.len());
    }

    #[test]
    fn split_does_not_sever_tool_pairs() {
        let assistant = Message {
            role: MessageRole::Assistant,
            content: crate::llm::provider::MessageContent::Text("call".to_string()),
            reasoning: None,
            reasoning_details: None,
            tool_calls: make_tool_call("tc1", "bash"),
            tool_call_id: None,
            phase: None,
            origin_tool: None,
            metadata: None,
            clear_at: None,
        };
        let conv = vec![
            Message {
                role: MessageRole::User,
                content: crate::llm::provider::MessageContent::Text("a".repeat(400)),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: crate::llm::provider::MessageContent::Text("b".repeat(400)),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            assistant.clone(),
            Message {
                role: MessageRole::Tool,
                content: crate::llm::provider::MessageContent::Text("ok".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: Some("tc1".to_string()),
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::User,
                content: crate::llm::provider::MessageContent::Text("tail".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
        ];
        let split = split_conversation_for_two_pass(&conv, 0.9);
        if let Some(msg) = split.prefix.last() {
            if msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty()) {
                assert!(!matches!(split.tail.first(), Some(m) if m.tool_call_id.is_some()));
            }
        }
        assert!(!matches!(split.tail.first(), Some(m) if m.tool_call_id.is_some()));
        let prefix_has_call = split.prefix.iter().any(|m| {
            m.tool_calls
                .as_ref()
                .map(|tc| tc.iter().any(|t| t.id == "tc1"))
                .unwrap_or(false)
        });
        let prefix_has_result = split.prefix.iter().any(|m| m.tool_call_id.as_deref() == Some("tc1"));
        let tail_has_call = split.tail.iter().any(|m| {
            m.tool_calls
                .as_ref()
                .map(|tc| tc.iter().any(|t| t.id == "tc1"))
                .unwrap_or(false)
        });
        let tail_has_result = split.tail.iter().any(|m| m.tool_call_id.as_deref() == Some("tc1"));
        assert_eq!(prefix_has_call, prefix_has_result);
        assert_eq!(tail_has_call, tail_has_result);
    }

    #[test]
    fn note_prefers_summary_block_and_caps_huge_raw() {
        let text = format!("<summary>short</summary>\n<summary>\n{}\n</summary>", "x".repeat(1001));
        assert_eq!(note_for_two_pass_pass2(&text), "x".repeat(1001));
        assert_eq!(note_for_two_pass_pass2("no tags"), "no tags");
        let huge = "n".repeat(TWO_PASS_MAX_NOTE1_CHARS + 500);
        let note = note_for_two_pass_pass2(&huge);
        assert!(note.chars().count() <= TWO_PASS_MAX_NOTE1_CHARS + 80);
        assert!(note.contains("truncated"));
    }

    #[test]
    fn pass_histories_shape() {
        let conv = vec![
            Message {
                role: MessageRole::System,
                content: crate::llm::provider::MessageContent::Text("You are VT Code.".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::User,
                content: crate::llm::provider::MessageContent::Text("early".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: crate::llm::provider::MessageContent::Text("early-a".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::User,
                content: crate::llm::provider::MessageContent::Text("late".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
            Message {
                role: MessageRole::Assistant,
                content: crate::llm::provider::MessageContent::Text("late-a".to_string()),
                reasoning: None,
                reasoning_details: None,
                tool_calls: None,
                tool_call_id: None,
                phase: None,
                origin_tool: None,
                metadata: None,
                clear_at: None,
            },
        ];
        let split = split_conversation_for_two_pass(&conv, 0.5);
        let prompt = "1. Primary Request and Intent: x\n5. Optional Next Step: y\n";
        let pass1 = build_two_pass_pass1_history(split.prefix, prompt);
        assert!(matches!(pass1.last(), Some(m) if m.role == MessageRole::User));

        let note1 = "x".repeat(1001);
        let pass2 = build_two_pass_pass2_history(split.prefix, split.tail, &note1, prompt);
        assert!(pass2.iter().any(|m| m.role == MessageRole::System));
        let texts: Vec<String> = pass2
            .iter()
            .map(|m| match &m.content {
                crate::llm::provider::MessageContent::Text(t) => t.clone(),
                crate::llm::provider::MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        crate::llm::provider::ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
            })
            .collect();
        assert!(texts.iter().any(|t| t.contains("<summary_content>")));
        assert!(texts.last().is_some_and(|t| t.contains("special compaction case")));
    }
}
