//! Output truncation limits inspired by OpenAI Codex multi-tier truncation.
//! Prevents OOM with three independent size limits.
//! Reference: <https://openai.com/index/unrolling-the-codex-agent-loop/>

/// Maximum size for single agent message payloads (bytes) - 4 MB.
pub const MAX_AGENT_MESSAGES_SIZE: usize = 4 * 1024 * 1024;

/// Maximum size for entire message history payloads (bytes) - 24 MB.
pub const MAX_ALL_MESSAGES_SIZE: usize = 24 * 1024 * 1024;

/// Maximum retained error-log payload bytes in the in-memory collector (10 MiB).
pub const ERROR_LOG_BUFFER_SIZE_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Maximum size per line (bytes) - 256 KB.
/// Prevents OOM on malformed output with very long lines.
pub const MAX_LINE_LENGTH: usize = 256 * 1024;

/// Default message count limit for history.
pub const DEFAULT_MESSAGE_LIMIT: usize = 4_000;

/// Maximum message count limit.
pub const MAX_MESSAGE_LIMIT: usize = 20_000;

/// Aggregate provider-visible tool preview budget across one turn (32 KiB).
///
/// Once a turn's tool previews exhaust this budget, later tool responses keep
/// bounded outcome/control metadata while payload bodies are truncated or
/// omitted. See the agent-loop contract ("Across a turn, provider-visible
/// tool previews are capped at 32 KiB").
pub const TURN_PREVIEW_BUDGET_BYTES: usize = 32 * 1024;

/// Truncation marker appended when content is cut off.
const TRUNCATION_MARKER: &str = "\n[... content truncated due to size limit ...]";

/// Collect content with lazy truncation (Codex pattern).
/// Marks truncated but continues draining to prevent pipe blocking.
///
/// # Arguments
/// * `output` - Accumulated output buffer
/// * `new_content` - New content to append
/// * `max_size` - Maximum allowed size
/// * `truncated` - Mutable flag tracking truncation state
///
/// # Returns
/// `true` if content was appended, `false` if truncated
#[inline]
fn collect_with_truncation(output: &mut String, new_content: &str, max_size: usize, truncated: &mut bool) -> bool {
    let new_size = output.len() + new_content.len();

    if new_size > max_size {
        if !*truncated {
            output.push_str(TRUNCATION_MARKER);
            *truncated = true;
        }
        return false;
    }

    output.push_str(new_content);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_within_limit() {
        let mut output = String::new();
        let mut truncated = false;

        assert!(collect_with_truncation(&mut output, "hello", 100, &mut truncated));
        assert_eq!(output, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_collect_at_limit_triggers_truncation() {
        let mut output = String::from("hello");
        let mut truncated = false;

        assert!(!collect_with_truncation(&mut output, " world that exceeds", 10, &mut truncated));
        assert!(output.contains(TRUNCATION_MARKER));
        assert!(truncated);
    }

    #[test]
    fn test_truncation_marker_appended_only_once() {
        let mut output = String::new();
        let mut truncated = false;

        // Triggers initial truncation
        collect_with_truncation(&mut output, "first content", 5, &mut truncated);
        let len_after_marker = output.len();
        assert!(truncated);

        // Should not append marker again
        collect_with_truncation(&mut output, "second content", 5, &mut truncated);
        assert_eq!(output.len(), len_after_marker);
    }
}
