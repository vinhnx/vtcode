#![expect(
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "Shell parsing tracks byte offsets at character boundaries while validating operator pairs."
)]

//! Shell script parser for `bash -lc` and similar commands.
//!
//! This module parses shell commands like:
//! ```sh
//! bash -lc "git status && cargo check"
//! ```
//!
//! Into individual command vectors for independent safety checking:
//! ```text
//! [["git", "status"], ["cargo", "check"]]
//! ```
//!
//! **Phase 4 Implementation**: Uses tree-sitter for accurate bash AST parsing.
//! Falls back to basic tokenization for minimal shell syntax.

use std::sync::Mutex;
use std::sync::OnceLock;

use anyhow::Result;

/// Lazy-initialized tree-sitter bash parser (wrapped in Mutex for mutation)
static BASH_PARSER: OnceLock<Result<Mutex<tree_sitter::Parser>, String>> = OnceLock::new();

/// Returns whether a shell command contains syntax whose meaning depends on
/// shell expansion rather than the literal argument text.
///
/// Safety-sensitive classification must only operate on static command
/// shapes. Parameter expansion, command substitution, brace expansion,
/// globbing, and unquoted backslash escapes can otherwise turn a
/// harmless-looking token into a different executable argument at runtime.
/// Backslash escapes inside double-quoted arguments are consumed as literal
/// argument syntax so patterns such as `rg -n "\\[profile"` remain classifiable.
pub fn contains_dynamic_shell_syntax(command: &str) -> bool {
    enum ShellQuote {
        Single,
        Double,
    }

    let mut quote: Option<ShellQuote> = None;
    let mut characters = command.chars();

    while let Some(character) = characters.next() {
        match quote {
            Some(ShellQuote::Single) => {
                if character == '\'' {
                    quote = None;
                }
            }
            Some(ShellQuote::Double) => match character {
                '"' => quote = None,
                '$' | '`' => return true,
                '\\' => {
                    // Backslash escapes inside double quotes are literal
                    // argument syntax. Consume the escaped character so an
                    // escaped quote cannot incorrectly end the quoted region;
                    // unquoted escapes remain rejected below because they can
                    // alter the command token or its shell structure.
                    if characters.next().is_none() {
                        return true;
                    }
                }
                _ => {}
            },
            None => match character {
                '\'' => quote = Some(ShellQuote::Single),
                '"' => quote = Some(ShellQuote::Double),
                '\\' | '$' | '`' | '{' | '}' | '*' | '?' | '[' | ']' => return true,
                _ => {}
            },
        }
    }

    quote.is_some()
}

/// Returns whether a `find` command contains shell syntax that can change the
/// literal option tokens after approval-time tokenization.
pub fn contains_dynamic_find_syntax(script: &str) -> bool {
    if let Ok(commands) = parse_shell_commands_tree_sitter(script)
        && commands.iter().any(|command| {
            command
                .first()
                .map(|program| base_command_name(program) == "find")
                .unwrap_or(false)
                && command.iter().any(|word| contains_dynamic_shell_syntax(word))
        })
    {
        return true;
    }

    // Be conservative when the grammar cannot identify the command shape: a
    // raw script containing a find invocation and dynamic syntax must not pass
    // preflight just because parsing was incomplete.
    let has_find_word = script.split_whitespace().any(|word| {
        let command = word.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '/');
        base_command_name(command) == "find"
    });
    has_find_word && contains_dynamic_shell_syntax(script)
}

/// Gets or initializes the bash parser
fn get_bash_parser() -> Result<&'static Mutex<tree_sitter::Parser>, String> {
    BASH_PARSER
        .get_or_init(|| {
            let mut parser = tree_sitter::Parser::new();
            let lang: tree_sitter::Language = tree_sitter_bash::LANGUAGE.into();
            parser
                .set_language(&lang)
                .map_err(|e| format!("Failed to load bash grammar: {e}"))?;
            Ok(Mutex::new(parser))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Ensures the bash tree-sitter parser is initialized.
pub fn prewarm_bash_parser() -> Result<(), String> {
    let _ = get_bash_parser()?;
    Ok(())
}

/// Parses a shell script into individual commands using tree-sitter bash grammar
///
/// # Example
/// ```text
/// Input:  "git status && cargo check"
/// Output: Ok([["git", "status"], ["cargo", "check"]])
/// ```
///
/// # Fallback
/// If tree-sitter parsing fails, falls back to simple tokenization
pub fn parse_shell_commands(script: &str) -> Result<Vec<Vec<String>>, String> {
    // Try tree-sitter parsing first
    match parse_with_tree_sitter(script, false) {
        Ok(commands) if !commands.is_empty() => return Ok(commands),
        Ok(_) => {} // Empty result, fall through to basic parsing
        Err(e) => {
            tracing::debug!("Tree-sitter bash parsing failed: {}, falling back to basic tokenization", e);
        }
    }

    // Fallback to simple tokenization
    parse_with_basic_tokenization(script)
}

/// Parses a shell script using tree-sitter bash grammar only (no fallback tokenization).
///
/// Use this when caller behavior must be strictly gated on bash grammar validity.
pub fn parse_shell_commands_tree_sitter(script: &str) -> Result<Vec<Vec<String>>, String> {
    parse_with_tree_sitter(script, true)
}

/// Returns whether every redirection in a static shell script only routes
/// command output. Input, heredoc, and descriptor-closing redirections remain
/// unsupported so progress classification can fail closed.
pub fn has_only_output_redirections(script: &str) -> bool {
    if contains_dynamic_shell_syntax(script) {
        return false;
    }
    if contains_background_operator(script) {
        return false;
    }

    let Ok(parser) = get_bash_parser() else {
        return false;
    };
    let Ok(mut parser) = parser.lock() else {
        return false;
    };
    let Some(tree) = parser.parse(script, None) else {
        return false;
    };
    if tree.root_node().has_error() {
        return false;
    }

    let mut saw_redirection = false;
    if !collect_output_redirections(tree.root_node(), script, &mut saw_redirection) {
        return false;
    }
    saw_redirection
}

/// Validate literal file-redirection targets before command classification drops them.
/// Descriptor routing carries no path; unresolved shell expansion fails closed.
pub(crate) fn validate_redirection_paths(script: &str) -> Result<()> {
    use anyhow::{Context, anyhow, ensure};
    if !script.contains(['<', '>']) {
        return Ok(());
    }
    let parser = get_bash_parser().map_err(anyhow::Error::msg)?;
    let mut parser = parser.lock().map_err(|error| anyhow!("shell parser lock poisoned: {error}"))?;
    let tree = parser.parse(script, None).context("failed to parse shell redirections")?;
    ensure!(!tree.root_node().has_error(), "cannot validate malformed shell redirections");
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.kind() == "file_redirect" {
            let text = node.utf8_text(script.as_bytes()).context("invalid shell redirection text")?;
            let operator = text.trim_start_matches(|character: char| character.is_ascii_digit());
            let descriptor_route = operator.starts_with(">&") || operator.starts_with("<&");
            let mut cursor = node.walk();
            let destinations = node.children_by_field_name("destination", &mut cursor);
            for destination in destinations {
                let raw = destination
                    .utf8_text(script.as_bytes())
                    .context("invalid redirection destination")?;
                ensure!(!contains_dynamic_shell_syntax(raw), "dynamic redirection destination is not allowed");
                let words = shell_words::split(raw).context("invalid quoted redirection destination")?;
                ensure!(words.len() == 1, "redirection destination must be one literal path");
                let path = words.first().context("missing redirection destination")?;
                if descriptor_route && (path == "-" || path.chars().all(|character| character.is_ascii_digit())) {
                    continue;
                }
                // `sh` expands a leading `~` after validation, so treating it
                // as a relative literal would let a redirection escape the
                // workspace (for example, `> ~/.config`). Require callers to
                // provide an explicit, policy-checked destination instead.
                ensure!(!path.starts_with('~'), "home-directory redirection destinations are not allowed");
                // The null sink is the one intentional device path used by normal commands.
                if path == "/dev/null" {
                    continue;
                }
                vtcode_commons::paths::validate_path_safety(path)
                    .with_context(|| format!("unsafe shell redirection destination: {path}"))?;
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Ok(())
}

fn contains_background_operator(script: &str) -> bool {
    let chars = script.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while index < chars.len() {
        let character = chars[index];
        if character == '\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            index += 1;
            continue;
        }
        if character == '"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            index += 1;
            continue;
        }
        if in_single_quote || in_double_quote {
            index += 1;
            continue;
        }

        if character == '&' {
            let previous = index.checked_sub(1).and_then(|position| chars.get(position));
            let next = chars.get(index + 1);
            if next == Some(&'&') {
                index += 2;
                continue;
            }
            let part_of_allowed_operator = next == Some(&'&')
                || next == Some(&'>')
                || previous == Some(&'>')
                || previous == Some(&'|')
                || previous == Some(&'<');
            if !part_of_allowed_operator {
                return true;
            }
        }
        index += 1;
    }

    false
}

fn collect_output_redirections(node: tree_sitter::Node, source: &str, saw_redirection: &mut bool) -> bool {
    match node.kind() {
        "file_redirect" => {
            *saw_redirection = true;
            let Ok(text) = node.utf8_text(source.as_bytes()) else {
                return false;
            };
            if !is_output_redirection(text) {
                return false;
            }
        }
        "heredoc_redirect" | "herestring_redirect" => return false,
        _ => {}
    }

    let mut cursor = node.walk();
    node.children(&mut cursor)
        .all(|child| collect_output_redirections(child, source, saw_redirection))
}

fn is_output_redirection(text: &str) -> bool {
    let redirect = text.trim_start_matches(|character: char| character.is_ascii_digit());
    if redirect.starts_with("&>") {
        return !redirect.starts_with("&>-");
    }
    if let Some(destination) = redirect.strip_prefix(">&") {
        return destination.trim().chars().all(|character| character.is_ascii_digit());
    }

    redirect.starts_with('>') && !redirect.starts_with(">&-")
}

/// Parses shell script using tree-sitter bash grammar.
fn parse_with_tree_sitter(script: &str, reject_syntax_errors: bool) -> Result<Vec<Vec<String>>, String> {
    let parser_guard = get_bash_parser()?;
    let mut parser = parser_guard.lock().map_err(|e| format!("Failed to lock parser: {e}"))?;

    let tree = parser.parse(script, None).ok_or_else(|| "Failed to parse script".to_string())?;

    let mut commands = Vec::new();
    let root = tree.root_node();
    if reject_syntax_errors && root.has_error() {
        return Err("Shell script contains syntax errors".to_string());
    }

    // Walk the full tree so commands inside loops/conditionals are remembered
    // for approval and checked for safety.  Top-level-only extraction misses
    // common read loops such as `for f in ...; do grep ...; done`.
    collect_commands_from_node(root, script, &mut commands);

    Ok(commands)
}

fn collect_commands_from_node(node: tree_sitter::Node, source: &str, commands: &mut Vec<Vec<String>>) {
    match node.kind() {
        "command" | "simple_command" => {
            if let Some(cmd) = extract_command_from_node(node, source)
                && !cmd.is_empty()
            {
                commands.push(cmd);
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_commands_from_node(child, source, commands);
            }
        }
    }
}

/// Extracts a command vector from a tree-sitter node
fn extract_command_from_node(node: tree_sitter::Node, source: &str) -> Option<Vec<String>> {
    let mut command = Vec::new();
    let mut cursor = node.walk();

    // For pipeline nodes, extract the first command in the pipeline
    if node.kind() == "pipeline" {
        for child in node.children(&mut cursor) {
            if child.kind() == "command" || child.kind() == "simple_command" {
                return extract_command_from_node(child, source);
            }
        }
    }

    // Extract arguments from command node
    for child in node.children(&mut cursor) {
        if child.kind() == "command_name" {
            if let Ok(arg) = child.utf8_text(source.as_bytes()) {
                let trimmed = arg.trim();
                if !trimmed.is_empty() {
                    command.push(trimmed.to_string());
                }
            }
            continue;
        }

        if matches!(
            child.kind(),
            "word" | "string" | "raw_string" | "ansi_c_string" | "simple_expansion" | "variable_expansion"
        ) {
            let text = child.utf8_text(source.as_bytes());
            if let Ok(arg) = text {
                let trimmed = arg.trim();
                if !trimmed.is_empty() {
                    command.push(trimmed.to_string());
                }
            }
        }
    }

    if command.is_empty() { None } else { Some(command) }
}

/// Fallback: Parses shell script with simple tokenization
fn parse_with_basic_tokenization(script: &str) -> Result<Vec<Vec<String>>, String> {
    let mut commands = Vec::new();
    let mut current_command = String::new();
    let mut in_quotes = false;
    let mut quote_char = ' ';
    let mut escaped = false;

    for ch in script.chars() {
        if escaped {
            current_command.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => {
                escaped = true;
            }
            '\'' | '"' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
            }
            '&' | '|' | ';' if !in_quotes => {
                if !current_command.trim().is_empty()
                    && let Ok(cmd) = tokenize_command(&current_command)
                {
                    commands.push(cmd);
                }
                current_command.clear();
            }
            _ => current_command.push(ch),
        }
    }

    if !current_command.trim().is_empty()
        && let Ok(cmd) = tokenize_command(&current_command)
    {
        commands.push(cmd);
    }

    Ok(commands)
}

/// Splits a command string into arguments
/// Respects quoted strings and escapes
fn tokenize_command(cmd: &str) -> Result<Vec<String>, String> {
    shell_words::split(cmd).map_err(|err| format!("failed to tokenize command: {err}"))
}

/// Parses `bash -lc "script"` style invocations
///
/// # Example
/// ```text
/// Input:  vec!["bash", "-lc", "git status && rm /"]
/// Output: Some([["git", "status"], ["rm", "/"]])
/// ```
pub fn parse_bash_lc_commands(command: &[String]) -> Option<Vec<Vec<String>>> {
    if command.is_empty() {
        return None;
    }

    let cmd_name = command[0].as_str();
    let base_cmd = std::path::Path::new(cmd_name)
        .file_name()
        .and_then(|osstr| osstr.to_str())
        .unwrap_or("");

    if base_cmd != "bash" && base_cmd != "zsh" && base_cmd != "sh" {
        return None;
    }

    // Look for -lc or -c pattern
    for window in command.windows(2) {
        if matches!(window[0].as_str(), "-lc" | "-c" | "-il" | "-ic") {
            let script = &window[1];
            return parse_shell_commands(script).ok();
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_simple_command() {
        let cmd = "git status";
        let tokens = tokenize_command(cmd).unwrap();
        assert_eq!(tokens, vec!["git", "status"]);
    }

    #[test]
    fn tokenize_quoted_arguments() {
        let cmd = r#"echo "hello world""#;
        let tokens = tokenize_command(cmd).unwrap();
        assert_eq!(tokens, vec!["echo", "hello world"]);
    }

    #[test]
    fn parse_single_command() {
        let script = "git status";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0][0], "git");
    }

    #[test]
    fn parse_chained_commands_with_and() {
        let script = "git status && cargo check";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0], "git");
        assert_eq!(commands[1][0], "cargo");
    }

    #[test]
    fn parse_loop_body_commands() {
        let script = "cd crates/codegen/vtcode-core/src/tools/registry && for f in *.rs; do echo \"=== $f ===\"; grep -nE '^(pub )?(struct|enum|fn)' \"$f\" | head -50; done";
        let commands = parse_shell_commands(script).unwrap();

        assert_eq!(commands[0], vec!["cd", "crates/codegen/vtcode-core/src/tools/registry"]);
        assert!(
            commands
                .iter()
                .any(|command| command.first().is_some_and(|name| name == "echo"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.first().is_some_and(|name| name == "grep"))
        );
        assert!(
            commands
                .iter()
                .any(|command| command.first().is_some_and(|name| name == "head"))
        );
    }

    #[test]
    fn parse_chained_commands_with_semicolon() {
        let script = "git status; cargo check";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn parse_bash_lc_git_status() {
        let cmd = vec!["bash".to_string(), "-lc".to_string(), "git status".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let commands = commands.unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0][0], "git");
    }

    #[test]
    fn parse_bash_lc_chained() {
        let cmd = vec![
            "bash".to_string(),
            "-lc".to_string(),
            "git status && cargo check".to_string(),
        ];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let commands = commands.unwrap();
        assert_eq!(commands.len(), 2);
    }

    #[test]
    fn parse_non_bash_command_returns_none() {
        let cmd = vec!["echo".to_string(), "hello".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_none());
    }

    #[test]
    fn parse_bash_without_lc_returns_none() {
        let cmd = vec!["bash".to_string(), "script.sh".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_none());
    }

    // Phase 4 tests: Tree-sitter based parsing

    #[test]
    fn parse_complex_pipeline() {
        let script = "cat file.txt | grep -i pattern | sort";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_with_pipes_and_redirects() {
        let script = "ls -la | grep file > output.txt";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_command_substitution_fallback() {
        let script = "echo $(git status)";
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_escaped_quotes() {
        let script = r#"echo "hello \"world\"""#;
        let commands = parse_shell_commands(script).unwrap();
        assert!(!commands.is_empty());
    }

    #[test]
    fn parse_tree_sitter_preserves_command_name_with_quoted_args() {
        let script = r#"echo "fish and chips""#;
        let commands = parse_shell_commands_tree_sitter(script).unwrap();
        assert!(!commands.is_empty());
        assert_eq!(commands[0][0], "echo");
    }

    #[test]
    fn parse_tree_sitter_preserves_single_and_ansi_quoted_args() {
        let script = r#"printf '\n' && git diff '--output=out.txt' && printf $'\n'"#;
        let commands = parse_shell_commands_tree_sitter(script).unwrap();
        assert!(
            commands
                .iter()
                .any(|command| command.iter().any(|word| word.contains("--output=out.txt")))
        );
        assert!(commands.iter().any(|command| command.iter().any(|word| word.contains("\\n"))));
    }

    #[test]
    fn dynamic_syntax_allows_literal_escapes_inside_double_quoted_arguments() {
        assert!(!contains_dynamic_shell_syntax(r#"rg -n "\[profile|lto|codegen-units|strip" Cargo.toml"#));
        assert!(!contains_dynamic_shell_syntax(r#"printf "\nTop-level:\n""#));
        assert!(!contains_dynamic_shell_syntax(r#"printf "quoted: \"value\"""#));
        assert!(contains_dynamic_shell_syntax(r#"echo "safe\"$(id)""#));
    }

    #[test]
    fn dynamic_syntax_rejects_unquoted_escapes() {
        assert!(contains_dynamic_shell_syntax(r"rg -n \[profile Cargo.toml"));
    }

    #[test]
    fn output_redirection_guard_rejects_input_and_heredoc_shapes() {
        assert!(has_only_output_redirections("cargo check > build.log 2>&1"));
        assert!(has_only_output_redirections("cargo check | head -40 > build.log"));
        assert!(has_only_output_redirections("cargo check &> build.log"));
        assert!(has_only_output_redirections("cargo check &>> build.log"));
        assert!(!has_only_output_redirections("cargo check < build-input.log"));
        assert!(!has_only_output_redirections("cargo check <<'EOF'\ninput\nEOF"));
        assert!(!has_only_output_redirections("cargo check > $(printf build.log)"));
        assert!(!has_only_output_redirections("cargo check > build.log &"));
        assert!(!has_only_output_redirections("cargo check 2>&-"));
    }

    #[test]
    fn strict_tree_sitter_parser_rejects_incomplete_shell_syntax() {
        assert!(parse_shell_commands_tree_sitter("cargo check &&").is_err());
        assert!(parse_shell_commands_tree_sitter("echo '").is_err());
    }

    #[test]
    fn parse_bash_lc_with_pipe() {
        let cmd = vec!["bash".to_string(), "-lc".to_string(), "ls -la | head -5".to_string()];
        let commands = parse_bash_lc_commands(&cmd);
        assert!(commands.is_some());
        let cmds = commands.unwrap();
        assert!(!cmds.is_empty());
    }

    #[test]
    fn parse_dangerous_shell_command() {
        let script = "rm -rf /; echo done";
        let commands = parse_shell_commands(script).unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0][0], "rm");
    }

    #[test]
    fn prewarm_bash_parser_initializes_successfully() {
        prewarm_bash_parser().expect("bash parser should initialize");
    }

    #[test]
    fn dynamic_find_syntax_is_detected_without_rejecting_quoted_globs() {
        assert!(contains_dynamic_find_syntax("find src -maxdepth 0 -exe$''c touch /tmp/VT_BYPASS_POC {} +"));
        assert!(!contains_dynamic_find_syntax("find src -type f -name '*.rs'"));
    }
}

// === Injection detection (moved from tools::validation::commands) ===

use anyhow::bail;

/// Quote state for shell segment splitting.
#[derive(Clone, Copy, Eq, PartialEq)]
enum QuoteState {
    None,
    Single,
    Double,
}

/// Split a shell command into segments on unquoted `|` and `&` boundaries,
/// while detecting injection patterns (`;`, backticks, `$()`, newlines).
pub(crate) fn split_shell_segments(command: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut state = QuoteState::None;
    let mut escaped = false;
    let mut segment_start = 0usize;
    let mut chars = command.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        match state {
            QuoteState::Single => {
                if ch == '\'' {
                    state = QuoteState::None;
                }
            }
            QuoteState::Double => {
                if escaped {
                    escaped = false;
                    continue;
                }

                match ch {
                    '\\' => escaped = true,
                    '"' => state = QuoteState::None,
                    '`' => bail!("Command injection pattern detected"),
                    '$' if matches!(chars.peek(), Some((_, '('))) => {
                        bail!("Command injection pattern detected");
                    }
                    _ => {}
                }
            }
            QuoteState::None => {
                if escaped {
                    escaped = false;
                    continue;
                }

                match ch {
                    '\\' => escaped = true,
                    '\'' => state = QuoteState::Single,
                    '"' => state = QuoteState::Double,
                    '`' => bail!("Command injection pattern detected"),
                    '$' if matches!(chars.peek(), Some((_, '('))) => {
                        bail!("Command injection pattern detected");
                    }
                    ';' => bail!("Unquoted command chaining detected"),
                    '\n' => bail!("Command injection pattern detected"),
                    '|' | '&' => {
                        push_segment(command, segment_start, idx, &mut segments);
                        segment_start = idx + ch.len_utf8();
                        if let Some((next_idx, next_ch)) = chars.peek().copied()
                            && next_ch == ch
                        {
                            let _next = chars.next();
                            segment_start = next_idx + next_ch.len_utf8();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    push_segment(command, segment_start, command.len(), &mut segments);
    Ok(segments)
}

fn push_segment(command: &str, start: usize, end: usize, segments: &mut Vec<String>) {
    let segment = command[start..end].trim();
    if !segment.is_empty() {
        segments.push(segment.to_string());
    }
}

/// Check for additional dangerous patterns not covered by the central dangerous-command detector.
pub(crate) fn additional_dangerous_pattern(segment: &str) -> Option<&'static str> {
    let segment_lower = segment.to_ascii_lowercase();
    if segment_lower.starts_with(":(){:|:&};:") {
        return Some(":(){:|:&};:");
    }

    let tokens =
        shell_words::split(segment).unwrap_or_else(|_| segment.split_whitespace().map(ToString::to_string).collect());
    let first = tokens.first()?;
    let command_name = base_command_name(strip_wrapping_quotes(first)).to_ascii_lowercase();

    match command_name.as_str() {
        "rmdir" => Some("rmdir"),
        "wget" => Some("wget"),
        "curl" => Some("curl"),
        "chmod" if tokens.iter().skip(1).any(|arg| strip_wrapping_quotes(arg).starts_with("777")) => Some("chmod 777"),
        "chown"
            if tokens.iter().skip(1).any(|arg| {
                let arg = strip_wrapping_quotes(arg).to_ascii_lowercase();
                arg == "root" || arg.starts_with("root:")
            }) =>
        {
            Some("chown root")
        }
        _ => None,
    }
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token
        .strip_prefix('\'')
        .and_then(|token| token.strip_suffix('\''))
        .or_else(|| token.strip_prefix('"').and_then(|token| token.strip_suffix('"')))
        .unwrap_or(token)
}

fn base_command_name(command: &str) -> &str {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}
