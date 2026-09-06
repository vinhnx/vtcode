//! Input management for terminal sessions
//!
//! Encapsulates user input state, including text editing, cursor movement,
//! and command history navigation.  Text editing and cursor positioning are
//! delegated to [`ratatui_textarea::TextArea`], which provides undo/redo,
//! proper UTF-8 handling, and a battle-tested editing model.

/// Exclusive destination for composer input in the core session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputOwner {
    Composer,
    Overlay,
    Runtime,
}

use std::collections::HashSet;
use std::ops::Range;

use chrono::{DateTime, Utc};
use ratatui_textarea::{CursorMove, DataCursor, TextArea};

use super::super::types::ContentPart;
use super::mouse_selection::MouseSelectionState;
use super::textarea_bridge;

fn configure_textarea(textarea: &mut TextArea<'static>) {
    textarea.set_max_histories(50);
    textarea.set_tab_length(4);
}

fn image_attachment_placeholder(number: usize) -> String {
    format!("[Image #{number}]")
}

fn image_attachment_placeholders(content: &str) -> Vec<(usize, String)> {
    let marker = "[Image #";
    let mut placeholders = Vec::new();
    let mut search_start = 0;

    while let Some(offset) = content[search_start..].find(marker) {
        let start = search_start + offset;
        let digits_start = start + marker.len();
        let rest = &content[digits_start..];
        let digits_len = rest.bytes().take_while(|byte| byte.is_ascii_digit()).count();
        let placeholder_end = digits_start + digits_len + 1;

        if digits_len > 0 && content.as_bytes().get(placeholder_end - 1) == Some(&b']') {
            if let Ok(number) = content[digits_start..digits_start + digits_len].parse::<usize>()
                && number > 0
            {
                placeholders.push((number, content[start..placeholder_end].to_owned()));
            }
            search_start = placeholder_end;
        } else {
            search_start = digits_start;
        }
    }

    placeholders
}

fn attachment_placeholders_for_content(content: &str, attachment_count: usize) -> Vec<Option<String>> {
    let placeholders = image_attachment_placeholders(content);
    let mut sorted_visible_placeholders = placeholders.clone();
    sorted_visible_placeholders.sort_by_key(|(number, _)| *number);
    sorted_visible_placeholders.dedup_by_key(|(number, _)| *number);

    if sorted_visible_placeholders.len() >= attachment_count {
        return sorted_visible_placeholders
            .into_iter()
            .take(attachment_count)
            .map(|(_, placeholder)| Some(placeholder))
            .collect();
    }

    (0..attachment_count)
        .map(|index| {
            let expected_number = index + 1;
            placeholders
                .iter()
                .find_map(|(number, placeholder)| (*number == expected_number).then(|| placeholder.clone()))
                .or_else(|| {
                    if attachment_count == 1 && placeholders.len() == 1 {
                        Some(placeholders[0].1.clone())
                    } else {
                        None
                    }
                })
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct InputHistoryEntry {
    content: String,
    elements: Vec<ContentPart>,
    created_at: DateTime<Utc>,
}

impl InputHistoryEntry {
    pub fn from_content_and_attachments(content: String, attachments: Vec<ContentPart>) -> Self {
        let mut elements = Vec::new();
        if !content.is_empty() {
            elements.push(ContentPart::text(content.clone()));
        }
        elements.extend(attachments.into_iter().filter(ContentPart::is_image));
        Self { content, elements, created_at: Utc::now() }
    }

    /// Create an entry with an explicit timestamp (used for archived history).
    pub fn from_content_and_timestamp(content: String, created_at: DateTime<Utc>) -> Self {
        let elements = if content.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::text(&content)]
        };
        Self { content, elements, created_at }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn has_attachments(&self) -> bool {
        self.elements.iter().any(|part| part.is_image())
    }

    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty() && !self.has_attachments()
    }

    pub fn attachment_elements(&self) -> Vec<ContentPart> {
        self.elements.iter().filter(|part| part.is_image()).cloned().collect()
    }

    pub fn timestamp(&self) -> DateTime<Utc> {
        self.created_at
    }
}

/// Manages user input state including text, cursor, and history.
///
/// The text buffer and cursor are managed by [`TextArea`], giving us
/// undo/redo and proper character-boundary handling for free.  Selection is
/// tracked via an anchor model (`selection_anchor` + cursor) so that the
/// existing rendering pipeline (which reads byte-offset ranges) continues to
/// work unchanged.
#[derive(Clone, Debug)]
pub struct InputManager {
    textarea: TextArea<'static>,
    /// Byte-offset selection anchor.  `Some(anchor)` when a range selection is
    /// active; `None` otherwise.
    selection_anchor: Option<usize>,
    /// Whether the current selection has already been copied to the clipboard.
    selection_copied: bool,
    /// Non-text input elements (e.g. image attachments)
    attachments: Vec<ContentPart>,
    /// Inline placeholder text for pasted image attachments.
    ///
    /// `None` means the attachment was restored or set without inline
    /// placeholder provenance and should keep the legacy attachment behaviour.
    attachment_placeholders: Vec<Option<String>>,
    /// Byte range for a large paste that should render as a compact marker.
    compact_paste_range: Option<Range<usize>>,
    /// Command history entries
    history: Vec<InputHistoryEntry>,
    /// Current position in history (None = viewing current input)
    history_index: Option<usize>,
    /// Unsaved draft when navigating history
    history_draft: Option<InputHistoryEntry>,
    /// Number of archived entries prepended at the front of `history`.
    /// Down-arrow restores the draft as soon as the index falls into this
    /// range instead of cycling through archived items.
    archived_count: usize,
    /// Cached joined content to avoid repeated allocation and `leak`.
    content_cache: String,
}

impl InputManager {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        configure_textarea(&mut textarea);
        Self {
            textarea,
            selection_anchor: None,
            selection_copied: false,
            attachments: Vec::new(),
            attachment_placeholders: Vec::new(),
            compact_paste_range: None,
            history: Vec::new(),
            history_index: None,
            history_draft: None,
            archived_count: 0,
            content_cache: String::new(),
        }
    }

    fn refresh_content_cache(&mut self) {
        self.content_cache = self.textarea.lines().join("\n");
    }

    // ------------------------------------------------------------------
    // Content access
    // ------------------------------------------------------------------

    pub fn content(&self) -> &str {
        &self.content_cache
    }

    pub fn set_content(&mut self, content: String) {
        self.textarea = TextArea::from(content.split('\n'));
        configure_textarea(&mut self.textarea);
        self.textarea.move_cursor(CursorMove::End);
        self.compact_paste_range = None;
        self.clear_selection();
        self.reset_history_navigation();
        self.refresh_content_cache();
    }

    pub(crate) fn compact_paste_range(&self) -> Option<Range<usize>> {
        self.compact_paste_range.clone()
    }

    pub(crate) fn set_compact_paste_range(&mut self, range: Range<usize>) {
        self.compact_paste_range = Some(range);
    }

    fn track_compact_paste_replace(&mut self, start: usize, end: usize, replacement_len: usize) {
        let Some(range) = self.compact_paste_range.as_mut() else {
            return;
        };

        if end <= range.start {
            let removed_len = end.saturating_sub(start);
            if replacement_len >= removed_len {
                let delta = replacement_len - removed_len;
                range.start = range.start.saturating_add(delta);
                range.end = range.end.saturating_add(delta);
            } else {
                let delta = removed_len - replacement_len;
                range.start = range.start.saturating_sub(delta);
                range.end = range.end.saturating_sub(delta);
            }
            return;
        }

        if start >= range.end {
            return;
        }

        self.compact_paste_range = None;
    }

    pub fn cursor(&self) -> usize {
        let DataCursor(row, col) = self.textarea.cursor();
        textarea_bridge::row_col_to_byte_offset(self.textarea.lines(), row, col)
    }

    fn set_textarea_cursor(&mut self, row: usize, col: usize) {
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let col = u16::try_from(col).unwrap_or(u16::MAX);
        self.textarea.move_cursor(CursorMove::Jump(row, col));
    }

    pub fn set_cursor(&mut self, pos: usize) {
        let (row, col) = textarea_bridge::byte_offset_to_row_col(self.textarea.lines(), pos);
        self.set_textarea_cursor(row, col);
        self.clear_selection();
    }

    pub fn set_cursor_with_selection(&mut self, pos: usize) {
        let previous_selection = self.selection_range();
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor());
            self.textarea.start_selection();
        }
        let (row, col) = textarea_bridge::byte_offset_to_row_col(self.textarea.lines(), pos);
        self.set_textarea_cursor(row, col);
        if self.selection_range() != previous_selection {
            self.selection_copied = false;
        }
    }

    // ------------------------------------------------------------------
    // Selection
    // ------------------------------------------------------------------

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        let cursor = self.cursor();
        if anchor == cursor {
            return None;
        }
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        // Build directly from TextArea lines to avoid leaking via content().
        let mut result = String::new();
        let mut byte_pos = 0;
        for line in self.textarea.lines() {
            let line_end = byte_pos + line.len();
            if start < line_end && end > byte_pos {
                let lo = start.saturating_sub(byte_pos);
                let hi = end.min(line_end) - byte_pos;
                result.push_str(&line[lo..hi]);
            }
            if end <= line_end {
                break;
            }
            if start <= line_end && end > line_end {
                result.push('\n');
            }
            byte_pos = line_end + 1; // +1 for '\n'
        }
        Some(result)
    }

    pub fn copy_selected_text_to_clipboard(&mut self) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        let copied = MouseSelectionState::copy_to_clipboard(&text);
        // Mark attempted even on failure: render-driven copies call this every frame
        // while selection_needs_copy() is true, and a false flag would retry forever.
        self.selection_copied = true;
        copied
    }

    pub fn selection_needs_copy(&self) -> bool {
        self.has_selection() && !self.selection_copied
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.selection_copied = false;
        self.textarea.cancel_selection();
    }

    // ------------------------------------------------------------------
    // Editing
    // ------------------------------------------------------------------

    pub(crate) fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        self.track_compact_paste_replace(start, end, replacement.len());
        let lines: Vec<String> = self.textarea.lines().to_vec();
        let (start_line, start_col) = textarea_bridge::byte_offset_to_row_col(&lines, start);
        let (end_line, end_col) = textarea_bridge::byte_offset_to_row_col(&lines, end);

        let rep_parts: Vec<&str> = replacement.split('\n').collect();
        let has_newlines = rep_parts.len() > 1;

        if start_line == end_line {
            let line = &lines[start_line];
            let start_byte = textarea_bridge::char_col_to_byte_offset(line, start_col);
            let end_byte = textarea_bridge::char_col_to_byte_offset(line, end_col);
            let remaining = &line[end_byte..];

            if !has_newlines {
                // Single-line replacement: fast path.
                let mut new_line = String::with_capacity(start_byte + replacement.len() + remaining.len());
                new_line.push_str(&line[..start_byte]);
                new_line.push_str(replacement);
                new_line.push_str(remaining);
                self.textarea = TextArea::from(
                    lines
                        .iter()
                        .enumerate()
                        .map(|(i, l)| if i == start_line { new_line.as_str() } else { l.as_str() }),
                );
            } else {
                // Multi-line replacement: split across lines.
                let mut new_lines: Vec<String> = Vec::with_capacity(lines.len() + rep_parts.len() - 1);
                for (i, l) in lines.iter().enumerate() {
                    if i < start_line {
                        new_lines.push(l.clone());
                    } else if i == start_line {
                        let mut first = String::with_capacity(start_byte + rep_parts[0].len());
                        first.push_str(&line[..start_byte]);
                        first.push_str(rep_parts[0]);
                        new_lines.push(first);
                        for mid in &rep_parts[1..rep_parts.len() - 1] {
                            new_lines.push(mid.to_string());
                        }
                        // has_newlines is true (rep_parts.len() > 1), so .last() is guaranteed Some
                        let last = rep_parts.last().expect("rep_parts has at least 2 elements when has_newlines");
                        let mut last_line = String::with_capacity(last.len() + remaining.len());
                        last_line.push_str(last);
                        last_line.push_str(remaining);
                        new_lines.push(last_line);
                    } else {
                        new_lines.push(l.clone());
                    }
                }
                self.textarea = TextArea::from(new_lines);
            }
        } else {
            let end_line_obj = &lines[end_line];
            let end_byte = textarea_bridge::char_col_to_byte_offset(end_line_obj, end_col);
            let remaining = &end_line_obj[end_byte..];

            let mut new_lines: Vec<String> = Vec::with_capacity(lines.len() + rep_parts.len().saturating_sub(1));
            for (i, line) in lines.iter().enumerate() {
                if i < start_line {
                    new_lines.push(line.clone());
                } else if i == start_line {
                    let start_byte = textarea_bridge::char_col_to_byte_offset(line, start_col);
                    if !has_newlines {
                        let mut merged = String::with_capacity(start_byte + replacement.len() + remaining.len());
                        merged.push_str(&line[..start_byte]);
                        merged.push_str(replacement);
                        merged.push_str(remaining);
                        new_lines.push(merged);
                    } else {
                        let mut first = String::with_capacity(start_byte + rep_parts[0].len());
                        first.push_str(&line[..start_byte]);
                        first.push_str(rep_parts[0]);
                        new_lines.push(first);
                        for mid in &rep_parts[1..rep_parts.len() - 1] {
                            new_lines.push(mid.to_string());
                        }
                        // has_newlines is true (rep_parts.len() > 1), so .last() is guaranteed Some
                        let last = rep_parts.last().expect("rep_parts has at least 2 elements when has_newlines");
                        let mut last_line = String::with_capacity(last.len() + remaining.len());
                        last_line.push_str(last);
                        last_line.push_str(remaining);
                        new_lines.push(last_line);
                    }
                } else if i > end_line {
                    new_lines.push(line.clone());
                }
            }
            self.textarea = TextArea::from(new_lines);
        }

        configure_textarea(&mut self.textarea);
        let cursor_byte = start + replacement.len();
        let (row, col) = textarea_bridge::byte_offset_to_row_col(self.textarea.lines(), cursor_byte);
        self.set_textarea_cursor(row, col);
        self.clear_selection();
        self.refresh_content_cache();
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        self.replace_range(start, end, "");
        true
    }

    pub fn move_cursor_left(&mut self) {
        if let Some((start, _)) = self.selection_range() {
            let (row, col) = textarea_bridge::byte_offset_to_row_col(self.textarea.lines(), start);
            self.set_textarea_cursor(row, col);
            self.clear_selection();
            return;
        }
        self.textarea.cancel_selection();
        self.textarea.move_cursor(CursorMove::Back);
    }

    pub fn move_cursor_right(&mut self) {
        if let Some((_, end)) = self.selection_range() {
            let (row, col) = textarea_bridge::byte_offset_to_row_col(self.textarea.lines(), end);
            self.set_textarea_cursor(row, col);
            self.clear_selection();
            return;
        }
        self.textarea.cancel_selection();
        self.textarea.move_cursor(CursorMove::Forward);
    }

    pub fn move_cursor_to_start(&mut self) {
        self.set_textarea_cursor(0, 0);
        self.clear_selection();
    }

    pub fn move_cursor_to_end(&mut self) {
        let last_row = self.textarea.lines().len().saturating_sub(1);
        let last_col = self.textarea.lines().last().map_or(0, |l| l.chars().count());
        self.set_textarea_cursor(last_row, last_col);
        self.clear_selection();
    }

    pub fn insert_char(&mut self, ch: char) {
        if let Some((start, end)) = self.selection_range() {
            let mut buf = [0_u8; 4];
            self.replace_range(start, end, ch.encode_utf8(&mut buf));
        } else {
            let cursor = self.cursor();
            self.track_compact_paste_replace(cursor, cursor, ch.len_utf8());
            self.textarea.insert_char(ch);
        }
        self.refresh_content_cache();
    }

    pub fn insert_text(&mut self, text: &str) {
        if let Some((start, end)) = self.selection_range() {
            self.replace_range(start, end, text);
        } else {
            let cursor = self.cursor();
            self.track_compact_paste_replace(cursor, cursor, text.len());
            self.textarea.insert_str(text);
        }
        self.refresh_content_cache();
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let cursor = self.cursor();
        if cursor > 0 {
            let content = self.content();
            let start = content[..cursor].char_indices().next_back().map_or(0, |(idx, _)| idx);
            self.track_compact_paste_replace(start, cursor, 0);
        }
        self.textarea.delete_char();
        self.refresh_content_cache();
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let cursor = self.cursor();
        let content = self.content();
        if cursor < content.len() {
            let end = content[cursor..]
                .char_indices()
                .nth(1)
                .map_or(content.len(), |(idx, _)| cursor + idx);
            self.track_compact_paste_replace(cursor, end, 0);
        }
        self.textarea.delete_next_char();
        self.refresh_content_cache();
    }

    pub fn delete_word_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let prefix_len = self.cursor();
        let before_len = self.content().len();
        self.textarea.delete_next_word();
        // Refresh before reading the new length: content() serves the cache.
        self.refresh_content_cache();
        // Deleting a forward span preserves everything before the cursor, so
        // the removed span is exactly [cursor, cursor + shrink). Track it like
        // the other mutators so compact_paste_range stays aligned.
        let after_len = self.content().len();
        if after_len < before_len {
            self.track_compact_paste_replace(prefix_len, prefix_len + (before_len - after_len), 0);
        }
    }

    pub fn delete_whitespace_around_cursor(&mut self) {
        let content = self.content().to_string();
        let cursor = self.cursor();
        if content.is_empty() || cursor >= content.len() {
            return;
        }

        let before = &content[..cursor];
        let after = &content[cursor..];

        let new_before = before.trim_end();
        let new_after = after.trim_start();

        let removed_before = before.len() - new_before.len();
        let removed_after = after.len() - new_after.len();

        if removed_before == 0 && removed_after == 0 {
            return;
        }

        let new_content = format!("{new_before}{new_after}");
        let new_cursor = cursor - removed_before;

        self.set_content(new_content);
        self.set_cursor(new_cursor);
    }

    pub fn transpose_chars(&mut self) {
        let content = self.content().to_string();
        let cursor = self.cursor();
        if content.len() < 2 {
            return;
        }

        let mut chars: Vec<char> = content.chars().collect();
        let char_pos = content[..cursor].chars().count();

        if char_pos == 0 {
            // At start: swap first two chars
            chars.swap(0, 1);
            let new_content: String = chars.into_iter().collect();
            // Move to just after the char now at index 0 (byte-correct for
            // multi-byte UTF-8; cursor + 1 would panic on non-ASCII).
            let new_cursor = new_content.chars().next().map_or(0, |c| c.len_utf8());
            self.set_content(new_content);
            self.set_cursor(new_cursor);
        } else if char_pos >= chars.len() {
            // At end: swap last two chars
            let last = chars.len() - 1;
            chars.swap(last - 1, last);
            let new_content: String = chars.into_iter().collect();
            self.set_content(new_content);
        } else {
            // In middle: swap char at cursor with char before. The byte length
            // of the string is unchanged, so the cursor stays at the boundary
            // of the (now-swapped) character at char_pos.
            chars.swap(char_pos - 1, char_pos);
            let new_content: String = chars.into_iter().collect();
            let new_cursor = new_content
                .char_indices()
                .nth(char_pos)
                .map_or(new_content.len(), |(idx, c)| idx + c.len_utf8());
            self.set_content(new_content);
            self.set_cursor(new_cursor);
        }
    }

    pub fn transpose_words(&mut self) {
        let content = self.content().to_string();
        let cursor = self.cursor();
        if content.is_empty() {
            return;
        }

        let chars: Vec<char> = content.chars().collect();
        let char_pos = content[..cursor].chars().count();

        // Find word boundaries
        let is_word_char = |c: char| c.is_alphanumeric();

        // Find start of current word
        let mut word_start = char_pos;
        while word_start > 0 && is_word_char(chars[word_start - 1]) {
            word_start -= 1;
        }

        // Find end of current word
        let mut word_end = char_pos;
        while word_end < chars.len() && is_word_char(chars[word_end]) {
            word_end += 1;
        }

        // Find start of previous word
        let mut prev_start = word_start;
        while prev_start > 0 && !is_word_char(chars[prev_start - 1]) {
            prev_start -= 1;
        }
        while prev_start > 0 && is_word_char(chars[prev_start - 1]) {
            prev_start -= 1;
        }

        // Find end of previous word
        let mut prev_end = prev_start;
        while prev_end < chars.len() && is_word_char(chars[prev_end]) {
            prev_end += 1;
        }

        if prev_start >= word_start || prev_end >= word_start {
            return;
        }

        // Extract words
        let prev_word: String = chars[prev_start..prev_end].iter().collect();
        let curr_word: String = chars[word_start..word_end].iter().collect();
        let between: String = chars[prev_end..word_start].iter().collect();

        // Reconstruct purely from char slices; slicing the raw string with
        // char indices would panic on non-ASCII content.
        let mut new_content = String::new();
        new_content.extend(chars[..prev_start].iter());
        new_content.push_str(&curr_word);
        new_content.push_str(&between);
        new_content.push_str(&prev_word);
        new_content.extend(chars[word_end..].iter());

        self.set_content(new_content);
        self.set_cursor(cursor);
    }

    pub fn uppercase_word(&mut self) {
        self.transform_word(|s| s.to_uppercase());
    }

    pub fn lowercase_word(&mut self) {
        self.transform_word(|s| s.to_lowercase());
    }

    pub fn capitalize_word(&mut self) {
        self.transform_word(|s| {
            let mut chars = s.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let mut result = c.to_uppercase().to_string();
                    result.extend(chars.flat_map(|c| c.to_lowercase()));
                    result
                }
            }
        });
    }

    fn transform_word<F: Fn(&str) -> String>(&mut self, transform: F) {
        let content = self.content().to_string();
        let cursor = self.cursor();
        if content.is_empty() {
            return;
        }

        let chars: Vec<char> = content.chars().collect();
        let char_pos = content[..cursor].chars().count();

        let is_word_char = |c: char| c.is_alphanumeric();

        // Find start of current word
        let mut word_start = char_pos;
        while word_start > 0 && is_word_char(chars[word_start - 1]) {
            word_start -= 1;
        }

        // Find end of current word
        let mut word_end = word_start;
        while word_end < chars.len() && is_word_char(chars[word_end]) {
            word_end += 1;
        }

        if word_start == word_end {
            return;
        }

        let word: String = chars[word_start..word_end].iter().collect();
        let transformed = transform(&word);

        // char indices must be translated to byte offsets before slicing the
        // UTF-8 string; slicing with char indices panics on non-ASCII content.
        let byte_start = chars[..word_start].iter().map(|c| c.len_utf8()).sum::<usize>();
        let byte_end = byte_start + chars[word_start..word_end].iter().map(|c| c.len_utf8()).sum::<usize>();
        let new_content = format!("{}{}{}", &content[..byte_start], transformed, &content[byte_end..]);

        self.set_content(new_content);
        self.set_cursor(cursor);
    }

    pub fn clear(&mut self) {
        self.textarea = TextArea::default();
        configure_textarea(&mut self.textarea);
        self.clear_selection();
        self.attachments.clear();
        self.attachment_placeholders.clear();
        self.compact_paste_range = None;
        self.reset_history_navigation();
        self.refresh_content_cache();
    }

    // ------------------------------------------------------------------
    // Undo / redo (new — powered by TextArea)
    // ------------------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        let changed = self.textarea.undo();
        if changed {
            self.compact_paste_range = None;
            self.refresh_content_cache();
        }
        changed
    }

    pub fn redo(&mut self) -> bool {
        let changed = self.textarea.redo();
        if changed {
            self.compact_paste_range = None;
            self.refresh_content_cache();
        }
        changed
    }

    // ------------------------------------------------------------------
    // History (command submission, NOT undo/redo)
    // ------------------------------------------------------------------

    pub fn add_to_history(&mut self, entry: InputHistoryEntry) {
        if !entry.is_empty() {
            if let Some(last) = self.history.last()
                && last.content == entry.content
                && last.elements == entry.elements
            {
                self.reset_history_navigation();
                return;
            }
            self.history.push(entry);
        }
        self.reset_history_navigation();
    }

    /// Prepend archived history entries from previous sessions.
    ///
    /// Entries are inserted at the front of the history vector so that Up-arrow
    /// navigation surfaces the current session's most recent prompts first,
    /// followed by archived prompts (newest archived first).  The caller should
    /// provide entries in reverse-chronological order (newest first).
    /// Duplicates (by content) against existing history or among the archived
    /// entries themselves are skipped.
    pub fn prepend_archived_history(&mut self, entries: Vec<InputHistoryEntry>) {
        let mut seen: HashSet<String> = self.history.iter().map(|e| e.content.clone()).collect();
        let mut to_prepend = Vec::new();
        for entry in entries {
            if entry.is_empty() || !seen.insert(entry.content.clone()) {
                continue;
            }
            to_prepend.push(entry);
        }
        if to_prepend.is_empty() {
            return;
        }
        // Caller provides newest-first; insert each at position 0 so the final
        // order is [oldest_archived, ..., newest_archived, session_entries...].
        // Up-arrow (which walks from the tail) therefore hits session entries
        // first, then the most recent archived entry, then older ones.
        let count = to_prepend.len();
        for entry in to_prepend {
            self.history.insert(0, entry);
        }
        self.archived_count += count;
        self.reset_history_navigation();
    }

    pub fn go_to_next_history(&mut self) -> Option<InputHistoryEntry> {
        match self.history_index {
            None => None,
            Some(i) if i < self.archived_count => {
                // In the archived range — restore the user's preserved draft
                // immediately instead of cycling through archived items.
                self.history_index = None;
                self.history_draft.take()
            }
            Some(0) => {
                self.history_index = None;
                self.history_draft.take()
            }
            Some(i) => {
                self.history_index = Some(i - 1);
                self.history.get(i - 1).cloned()
            }
        }
    }

    pub fn go_to_previous_history(&mut self) -> Option<InputHistoryEntry> {
        let current_index = match self.history_index {
            None => {
                self.history_draft = Some(self.current_history_entry());
                self.history.len().saturating_sub(1)
            }
            Some(i) => {
                if i == 0 {
                    return None;
                }
                i - 1
            }
        };

        if current_index < self.history.len() {
            self.history_index = Some(current_index);
            self.history.get(current_index).cloned()
        } else {
            None
        }
    }

    pub fn reset_history_navigation(&mut self) {
        self.history_index = None;
        self.history_draft = None;
    }

    pub fn history(&self) -> &[InputHistoryEntry] {
        &self.history
    }

    pub fn history_texts(&self) -> Vec<String> {
        self.history.iter().map(|entry| entry.content.clone()).collect()
    }

    pub fn history_index(&self) -> Option<usize> {
        self.history_index
    }

    pub fn attachments(&self) -> &[ContentPart] {
        &self.attachments
    }

    pub fn set_attachments(&mut self, attachments: Vec<ContentPart>) {
        self.attachments = attachments.into_iter().filter(ContentPart::is_image).collect();
        let content = self.content().to_owned();
        self.attachment_placeholders = attachment_placeholders_for_content(&content, self.attachments.len());
    }

    pub fn push_attachment(&mut self, attachment: ContentPart) -> Option<usize> {
        if attachment.is_image() {
            let placeholder_number = self.next_image_attachment_placeholder_number();
            let placeholder = image_attachment_placeholder(placeholder_number);
            self.attachments.push(attachment);
            self.attachment_placeholders.push(Some(placeholder));
            return Some(placeholder_number);
        }
        None
    }

    fn next_image_attachment_placeholder_number(&self) -> usize {
        let visible_max = image_attachment_placeholders(self.content())
            .into_iter()
            .map(|(number, _)| number)
            .max()
            .unwrap_or(0);
        let tracked_max = self
            .attachment_placeholders
            .iter()
            .filter_map(|placeholder| placeholder.as_deref())
            .flat_map(image_attachment_placeholders)
            .map(|(number, _)| number)
            .max()
            .unwrap_or(0);

        visible_max.max(tracked_max).max(self.attachments.len()).saturating_add(1)
    }

    pub fn current_history_entry(&self) -> InputHistoryEntry {
        let content = self.content().to_string();
        InputHistoryEntry::from_content_and_attachments(content.clone(), self.attachments_for_content(&content))
    }

    fn attachments_for_content(&self, content: &str) -> Vec<ContentPart> {
        self.attachments
            .iter()
            .enumerate()
            .filter_map(|(index, attachment)| {
                if !attachment.is_image() {
                    return Some(attachment.clone());
                }

                let Some(Some(placeholder)) = self.attachment_placeholders.get(index) else {
                    return Some(attachment.clone());
                };

                content.contains(placeholder).then(|| attachment.clone())
            })
            .collect()
    }

    pub fn apply_history_entry(&mut self, entry: InputHistoryEntry) {
        // Directly replace the TextArea without calling `set_content` which
        // would reset history navigation state.
        self.textarea = TextArea::from(entry.content.split('\n'));
        configure_textarea(&mut self.textarea);
        self.textarea.move_cursor(CursorMove::End);
        self.clear_selection();
        self.attachments = entry.attachment_elements();
        self.attachment_placeholders = attachment_placeholders_for_content(&entry.content, self.attachments.len());
        self.compact_paste_range = None;
        self.refresh_content_cache();
    }

    pub fn apply_history_index(&mut self, index: usize) -> bool {
        let Some(entry) = self.history.get(index).cloned() else {
            return false;
        };
        self.apply_history_entry(entry);
        true
    }
}

impl Default for InputManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_input_manager_is_empty() {
        let manager = InputManager::new();
        assert_eq!(manager.content(), "");
        assert_eq!(manager.cursor(), 0);
    }

    #[test]
    fn insert_text_updates_content_and_cursor() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        assert_eq!(manager.content(), "hello");
        assert_eq!(manager.cursor(), 5);
    }

    #[test]
    fn backspace_removes_character_before_cursor() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        manager.backspace();
        assert_eq!(manager.content(), "hell");
        assert_eq!(manager.cursor(), 4);
    }

    #[test]
    fn delete_removes_character_at_cursor() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        manager.set_cursor(1);
        manager.delete();
        assert_eq!(manager.content(), "hllo");
    }

    #[test]
    fn move_cursor_left_and_right() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        manager.move_cursor_left();
        assert_eq!(manager.cursor(), 4);
        manager.move_cursor_right();
        assert_eq!(manager.cursor(), 5);
    }

    #[test]
    fn clear_resets_state() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        manager.clear();
        assert_eq!(manager.content(), "");
        assert_eq!(manager.cursor(), 0);
    }

    #[test]
    fn history_navigation() {
        let mut manager = InputManager::new();
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("first".to_owned(), Vec::new()));
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("second".to_owned(), Vec::new()));

        assert_eq!(manager.go_to_previous_history().map(|entry| entry.content.clone()), Some("second".to_owned()));
        assert_eq!(manager.go_to_previous_history().map(|entry| entry.content.clone()), Some("first".to_owned()));
        assert_eq!(manager.go_to_previous_history().map(|entry| entry.content.clone()), None);
    }

    #[test]
    fn history_navigation_saves_draft() {
        let mut manager = InputManager::new();
        manager.set_content("current".to_owned());
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("previous".to_owned(), Vec::new()));

        manager.go_to_previous_history();
        assert_eq!(manager.go_to_next_history().map(|entry| entry.content.clone()), Some("current".to_owned()));
    }

    #[test]
    fn utf8_cursor_movement() {
        let mut manager = InputManager::new();
        manager.insert_text("你好");
        assert_eq!(manager.cursor(), 6); // 2 chars * 3 bytes

        manager.move_cursor_left();
        assert_eq!(manager.cursor(), 3);

        manager.move_cursor_right();
        assert_eq!(manager.cursor(), 6);
    }

    #[test]
    fn history_navigation_restores_attachments() {
        let mut manager = InputManager::new();
        manager.set_content("check this".to_owned());
        manager.set_attachments(vec![ContentPart::image("encoded".to_owned(), "image/png".to_owned())]);
        manager.add_to_history(manager.current_history_entry());
        manager.clear();

        let entry = manager.go_to_previous_history().expect("history entry");
        manager.apply_history_entry(entry);

        assert_eq!(manager.content(), "check this");
        assert_eq!(manager.attachments().len(), 1);
    }

    #[test]
    fn insert_text_replaces_selection() {
        let mut manager = InputManager::new();
        manager.insert_text("hello world");
        manager.set_cursor(5);
        manager.set_cursor_with_selection(11);

        manager.insert_text(" there");

        assert_eq!(manager.content(), "hello there");
        assert_eq!(manager.cursor(), "hello there".len());
        assert!(!manager.has_selection());
    }

    #[test]
    fn backspace_deletes_selected_range() {
        let mut manager = InputManager::new();
        manager.insert_text("hello world");
        manager.set_cursor(0);
        manager.set_cursor_with_selection(5);

        manager.backspace();

        assert_eq!(manager.content(), " world");
        assert_eq!(manager.cursor(), 0);
        assert!(!manager.has_selection());
    }

    #[test]
    fn move_cursor_left_collapses_selection_to_start() {
        let mut manager = InputManager::new();
        manager.insert_text("hello world");
        manager.set_cursor(0);
        manager.set_cursor_with_selection(5);

        manager.move_cursor_left();

        assert_eq!(manager.cursor(), 0);
        assert!(!manager.has_selection());
    }

    #[test]
    fn undo_redo() {
        let mut manager = InputManager::new();
        manager.insert_text("hello");
        assert_eq!(manager.content(), "hello");

        manager.undo();
        assert_eq!(manager.content(), "");
        assert_eq!(manager.cursor(), 0);

        manager.redo();
        assert_eq!(manager.content(), "hello");
    }

    #[test]
    fn prepend_archived_history_inserts_at_front() {
        let mut manager = InputManager::new();
        let now = Utc::now();
        manager
            .add_to_history(InputHistoryEntry::from_content_and_attachments("session-prompt".to_owned(), Vec::new()));

        // Input is newest-first; archived-1 is newer than archived-2.
        manager.prepend_archived_history(vec![
            InputHistoryEntry::from_content_and_timestamp("archived-1".to_owned(), now),
            InputHistoryEntry::from_content_and_timestamp("archived-2".to_owned(), now),
        ]);

        // After prepend: oldest archived at front, newest archived, then session.
        let texts = manager.history_texts();
        assert_eq!(texts, vec!["archived-2", "archived-1", "session-prompt"]);
    }

    #[test]
    fn prepend_archived_history_deduplicates() {
        let mut manager = InputManager::new();
        let now = Utc::now();
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("duplicate".to_owned(), Vec::new()));

        manager.prepend_archived_history(vec![
            InputHistoryEntry::from_content_and_timestamp("unique".to_owned(), now),
            InputHistoryEntry::from_content_and_timestamp("duplicate".to_owned(), now),
        ]);

        let texts = manager.history_texts();
        // "duplicate" was already in session history, so the archived copy is skipped.
        assert_eq!(texts, vec!["unique", "duplicate"]);
        assert_eq!(texts.iter().filter(|t| *t == "duplicate").count(), 1);
    }

    #[test]
    fn prepend_archived_history_skips_empty() {
        let mut manager = InputManager::new();
        let now = Utc::now();

        manager.prepend_archived_history(vec![
            InputHistoryEntry::from_content_and_timestamp("".to_owned(), now),
            InputHistoryEntry::from_content_and_timestamp("real".to_owned(), now),
        ]);

        assert_eq!(manager.history_texts(), vec!["real"]);
    }

    #[test]
    fn up_arrow_cycles_session_first_then_archived() {
        let mut manager = InputManager::new();
        let now = Utc::now();
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("session-2".to_owned(), Vec::new()));
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("session-1".to_owned(), Vec::new()));

        // archived-newer is the most recent archived entry.
        manager.prepend_archived_history(vec![
            InputHistoryEntry::from_content_and_timestamp("archived-newer".to_owned(), now),
            InputHistoryEntry::from_content_and_timestamp("archived-older".to_owned(), now),
        ]);

        // History: [archived-newer, archived-older, session-2, session-1]
        // Up arrow from fresh state: most recent session entry first, then
        // older session entries, then archived (newest archived first).
        assert_eq!(manager.go_to_previous_history().map(|e| e.content.clone()), Some("session-1".to_owned()));
        assert_eq!(manager.go_to_previous_history().map(|e| e.content.clone()), Some("session-2".to_owned()));
        assert_eq!(manager.go_to_previous_history().map(|e| e.content.clone()), Some("archived-newer".to_owned()));
        assert_eq!(manager.go_to_previous_history().map(|e| e.content.clone()), Some("archived-older".to_owned()));
        assert!(manager.go_to_previous_history().is_none());
    }

    #[test]
    fn down_restores_draft_from_archived_entries() {
        let mut manager = InputManager::new();
        let now = Utc::now();
        manager.set_content("my original prompt".to_owned());
        manager.add_to_history(InputHistoryEntry::from_content_and_attachments("session-1".to_owned(), Vec::new()));

        manager.prepend_archived_history(vec![
            InputHistoryEntry::from_content_and_timestamp("archived-1".to_owned(), now),
            InputHistoryEntry::from_content_and_timestamp("archived-2".to_owned(), now),
        ]);

        // Navigate backward into archived entries.
        manager.go_to_previous_history(); // session-1
        let entry = manager.go_to_previous_history().map(|e| e.content.clone());
        // archived-1 is the newest archived (adjacent to session entries).
        assert_eq!(entry, Some("archived-1".to_owned()));

        // Press Down from an archived entry — should restore the draft
        // instead of cycling to the next archived item.
        let restored = manager.go_to_next_history().map(|e| e.content.clone());
        assert_eq!(restored, Some("my original prompt".to_owned()));
        // History navigation should be fully exited.
        assert!(manager.history_index().is_none());
    }
}
