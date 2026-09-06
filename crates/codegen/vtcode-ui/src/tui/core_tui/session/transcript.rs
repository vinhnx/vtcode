//! Optimized transcript reflow cache for efficient line wrapping and rendering
//!
//! This module provides improved caching mechanisms for reflowing transcript content,
//! with performance optimizations for large transcripts.

use std::sync::Arc;

use super::{Session, message::TranscriptLine};

pub(super) struct TranscriptScrollAnchor {
    message_index: usize,
    row_in_message: usize,
}

#[derive(Default, Clone)]
pub struct CachedMessage {
    pub revision: u64,
    valid: bool,
    pub lines: Vec<TranscriptLine>,
}

pub struct TranscriptReflowCache {
    pub width: u16,
    pub total_rows: usize,
    pub row_offsets: Vec<usize>, // Precomputed row offsets for faster access
    pub messages: Vec<CachedMessage>,
}

impl TranscriptReflowCache {
    pub fn new(width: u16) -> Self {
        Self {
            width,
            total_rows: 0,
            row_offsets: Vec::new(),
            messages: Vec::new(),
        }
    }

    /// Updates the cache width and invalidates relevant data
    pub fn set_width(&mut self, new_width: u16) {
        if self.width != new_width {
            self.width = new_width;
            self.invalidate_content();
        }
    }

    /// Invalidates the content cache when width changes
    pub fn invalidate_content(&mut self) {
        for message in &mut self.messages {
            message.lines.clear(); // Clear cached lines
            message.valid = false;
        }
    }

    /// Checks if a specific message needs reflow based on revision and content hash
    pub fn needs_reflow(&self, index: usize, current_revision: u64) -> bool {
        if index >= self.messages.len() {
            return true;
        }

        let cached = &self.messages[index];
        !cached.valid || cached.revision != current_revision
    }

    /// Invalidates one cached message without dropping unrelated transcript
    /// entries. This is used when grouped message reflow depends on a later
    /// line that has not changed the group's head revision.
    pub(crate) fn invalidate_message(&mut self, index: usize) {
        if let Some(message) = self.messages.get_mut(index) {
            message.valid = false;
        }
    }

    /// Updates a cached message with new reflowed content
    pub fn update_message(&mut self, index: usize, revision: u64, lines: Vec<TranscriptLine>) {
        // Ensure we have enough space in the messages vector
        while self.messages.len() <= index {
            self.messages.push(CachedMessage::default());
        }

        let message = &mut self.messages[index];
        message.revision = revision;
        message.valid = true;
        message.lines = lines;
    }

    /// Precomputes row offsets starting from a specific index
    pub fn update_row_offsets_from(&mut self, start_index: usize) {
        if start_index == 0 {
            self.row_offsets.clear();
            self.row_offsets.reserve(self.messages.len());
        } else {
            self.row_offsets.truncate(start_index);
        }

        let mut current_offset = if start_index > 0 && start_index <= self.row_offsets.len() {
            // This branch is defensive if callers request a partially updated prefix.
            self.row_offsets[start_index - 1] + self.messages[start_index - 1].lines.len()
        } else if start_index > 0 && !self.row_offsets.is_empty() {
            // After truncate(start_index), the last element is at start_index - 1
            let last_idx = self.row_offsets.len() - 1;
            self.row_offsets[last_idx] + self.messages[last_idx].lines.len()
        } else {
            0
        };

        let start = self.row_offsets.len();
        for message in self.messages.iter().skip(start) {
            self.row_offsets.push(current_offset);
            current_offset += message.lines.len();
        }

        self.total_rows = current_offset;
    }

    /// Gets the total number of rows in the transcript
    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    /// Gets a range of visible lines for a given window
    pub fn get_visible_range(&self, start_row: usize, max_rows: usize) -> Vec<TranscriptLine> {
        if max_rows == 0 || start_row >= self.total_rows {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(max_rows.min(self.total_rows - start_row));
        let current_row = start_row;
        let remaining_rows = max_rows.min(self.total_rows - start_row);

        // Find the starting message index using binary search on row_offsets
        let start_message_idx = match self.row_offsets.binary_search(&current_row) {
            Ok(idx) => idx,
            Err(0) => 0,
            Err(pos) => pos - 1,
        };

        for msg_idx in start_message_idx..self.messages.len() {
            let msg_start_row = self.row_offsets[msg_idx];
            let msg = &self.messages[msg_idx];

            if msg_start_row >= current_row + remaining_rows {
                break;
            }

            let skip_lines = current_row.saturating_sub(msg_start_row);

            // Optimize: avoid enumerate(), just use skip()
            let target_count = remaining_rows - result.len();
            result.extend(msg.lines.iter().skip(skip_lines).take(target_count).cloned());

            if result.len() >= remaining_rows {
                break;
            }
        }

        result
    }

    #[expect(
        dead_code,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    pub fn message_start_row(&self, index: usize) -> Option<usize> {
        self.row_offsets.get(index).copied()
    }

    #[expect(
        dead_code,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    pub fn message_row_count(&self, index: usize) -> Option<usize> {
        self.messages.get(index).map(|m| m.lines.len())
    }
}

impl Session {
    pub(super) fn transcript_scroll_anchor(&mut self) -> Option<TranscriptScrollAnchor> {
        if self.transcript_width == 0 || self.scroll_manager.offset() == 0 {
            return None;
        }
        self.ensure_scroll_metrics();
        let top_row = self.scroll_manager.max_offset().saturating_sub(self.scroll_manager.offset());
        let cache = self.ensure_reflow_cache(self.transcript_width);
        let message_index = cache
            .messages
            .iter()
            .enumerate()
            .rposition(|(index, message)| !message.lines.is_empty() && cache.row_offsets[index] <= top_row)?;
        Some(TranscriptScrollAnchor {
            message_index,
            row_in_message: top_row.saturating_sub(cache.row_offsets[message_index]),
        })
    }

    pub(super) fn restore_transcript_scroll_anchor(&mut self, anchor: Option<TranscriptScrollAnchor>) {
        let Some(anchor) = anchor else {
            return;
        };
        self.ensure_scroll_metrics();
        let cache = self.ensure_reflow_cache(self.transcript_width);
        let Some(message) = cache.messages.get(anchor.message_index) else {
            return;
        };
        let top_row = cache.row_offsets[anchor.message_index]
            .saturating_add(anchor.row_in_message.min(message.lines.len().saturating_sub(1)));
        self.scroll_manager
            .set_offset(self.scroll_manager.max_offset().saturating_sub(top_row));
        self.invalidate_transcript_viewport();
    }

    /// Ensures the reflow cache is up to date for the given width
    pub(super) fn ensure_reflow_cache(&mut self, width: u16) -> &mut TranscriptReflowCache {
        let mut cache = self
            .transcript_cache
            .take()
            .unwrap_or_else(|| TranscriptReflowCache::new(width));

        // Update width if needed and handle width changes
        let mut width_changed = false;
        if cache.width != width {
            cache.set_width(width);
            width_changed = true;
        }

        // Resize message cache to match current line count
        cache.messages.truncate(self.lines.len());
        while cache.messages.len() < self.lines.len() {
            cache.messages.push(CachedMessage::default());
        }

        // Process any dirty messages (those that need reflow)
        // Use the hint from session if available to avoid O(N) scan
        let mut first_dirty = if width_changed {
            0
        } else {
            self.first_dirty_line.unwrap_or(self.lines.len())
        };

        // Verify and find the actual first dirty message
        // We scan from the hint downwards to be safe, but usually it's accurate
        first_dirty = (first_dirty..self.lines.len())
            .find(|&index| cache.needs_reflow(index, self.lines[index].revision))
            .unwrap_or(self.lines.len());

        // If no messages need reflow, just return existing cache
        if first_dirty == self.lines.len() {
            // Still need to ensure row offsets are correct (e.g. if messages were removed)
            cache.update_row_offsets_from(first_dirty);
            self.first_dirty_line = None;
            return self.transcript_cache.insert(cache);
        }

        // Update all messages from the first dirty one onwards
        for index in first_dirty..self.lines.len() {
            let line = &self.lines[index];
            if cache.needs_reflow(index, line.revision) {
                // Use Session method from reflow.rs to avoid duplication
                let new_lines = self.reflow_message_lines(index, width, true);
                cache.update_message(index, line.revision, new_lines);
            }
        }

        // Update row offsets and total row count incrementally
        cache.update_row_offsets_from(first_dirty);
        self.first_dirty_line = None;
        self.transcript_cache.insert(cache)
    }

    /// Gets the total number of rows in the transcript for a given width
    pub(crate) fn total_transcript_rows(&mut self, width: u16) -> usize {
        if width == 0 {
            return 0;
        }
        let cache = self.ensure_reflow_cache(width);
        cache.total_rows()
    }

    /// Collects a window of visible lines from the transcript
    fn collect_transcript_window(&mut self, width: u16, start_row: usize, max_rows: usize) -> Vec<TranscriptLine> {
        if max_rows == 0 {
            return Vec::new();
        }
        let cache = self.ensure_reflow_cache(width);
        cache.get_visible_range(start_row, max_rows)
    }

    /// Collects a window of visible lines with caching
    pub(crate) fn collect_transcript_window_cached(
        &mut self,
        width: u16,
        start_row: usize,
        max_rows: usize,
    ) -> Arc<Vec<TranscriptLine>> {
        // Check if we have cached visible lines for this exact position and width
        if let Some((cached_offset, cached_width, cached_rows, cached_lines)) = &self.visible_lines_cache
            && *cached_offset == start_row
            && *cached_width == width
            && *cached_rows == max_rows
        {
            // Return Arc clone (cheap pointer copy, no Vec allocation)
            return Arc::clone(cached_lines);
        }

        // Not in cache, fetch from transcript
        let visible_lines = self.collect_transcript_window(width, start_row, max_rows);

        // Cache for next render (wrapped in Arc for cheap sharing)
        let arc_lines = Arc::new(visible_lines);
        self.visible_lines_cache = Some((start_row, width, max_rows, Arc::clone(&arc_lines)));

        arc_lines
    }

    /// Return the reflowed row range occupied by one logical message.
    pub(crate) fn transcript_message_row_range(&mut self, width: u16, index: usize) -> Option<(usize, usize)> {
        let cache = self.ensure_reflow_cache(width);
        let start = cache.row_offsets.get(index).copied()?;
        let end = cache
            .row_offsets
            .get(index.saturating_add(1))
            .copied()
            .unwrap_or(cache.total_rows);
        Some((start, end.max(start.saturating_add(1))))
    }
}

impl Default for TranscriptReflowCache {
    fn default() -> Self {
        Self::new(80) // Default terminal width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_revision_zero_remains_invalid_after_resize_or_invalidation() {
        let mut cache = TranscriptReflowCache::new(80);
        cache.update_message(0, 0, Vec::new());
        assert!(!cache.needs_reflow(0, 0));
        cache.set_width(40);
        assert!(cache.needs_reflow(0, 0));
        cache.update_message(0, 0, Vec::new());
        cache.invalidate_message(0);
        assert!(cache.needs_reflow(0, 0));
        cache.update_message(0, u64::MAX, Vec::new());
        assert!(cache.needs_reflow(0, 0));
    }

    use ratatui::text::Line;
    use std::sync::Arc;

    use crate::tui::core_tui::types::{InlineMessageKind, InlineSegment, InlineTextStyle, InlineTheme};

    fn line(text: impl Into<Line<'static>>) -> TranscriptLine {
        TranscriptLine { line: text.into(), explicit_links: Vec::new() }
    }

    fn segment(text: &str) -> InlineSegment {
        InlineSegment {
            text: text.to_string(),
            style: Arc::new(InlineTextStyle::default()),
        }
    }

    fn history_session() -> Session {
        let mut session = Session::new(InlineTheme::default(), None, 20);
        session.apply_transcript_width(80);
        session.apply_transcript_rows(6);
        for index in 0..30 {
            session.handle_command(crate::tui::core_tui::types::InlineCommand::AppendLine {
                kind: InlineMessageKind::Agent,
                segments: vec![segment(&format!("message {index}: {}", "word ".repeat(20)))],
            });
        }
        session.ensure_scroll_metrics();
        let top_row = session.ensure_reflow_cache(80).row_offsets[10];
        session.scroll_manager.set_offset(session.scroll_manager.max_offset() - top_row);
        session
    }

    fn assert_cached_matches_fresh(session: &mut Session, width: u16) {
        let cached = session.collect_transcript_window_cached(width, 0, usize::MAX);
        let fresh = session.reflow_transcript_lines(width);
        assert_eq!(cached.len(), fresh.len());
        for (cached_line, fresh_line) in cached.iter().zip(&fresh) {
            assert_eq!(&cached_line.line, fresh_line);
        }
    }

    #[test]
    fn history_anchor_survives_streaming_resize_and_overlay_height() {
        let mut session = history_session();
        let anchor = session.transcript_scroll_anchor().unwrap();
        assert_eq!(anchor.message_index, 10);
        for width in [32, 100, 48] {
            session.apply_transcript_width(width);
            session.apply_transcript_rows(3);
            session.handle_command(crate::tui::core_tui::types::InlineCommand::AppendLine {
                kind: InlineMessageKind::Agent,
                segments: vec![segment("streaming output")],
            });
            session.apply_transcript_rows(6);
            let restored = session.transcript_scroll_anchor().unwrap();
            assert_eq!(restored.message_index, anchor.message_index);
            assert_eq!(restored.row_in_message, anchor.row_in_message);
            assert_cached_matches_fresh(&mut session, width);
        }
    }

    #[test]
    fn view_row_recalculation_preserves_history_anchor() {
        let mut session = history_session();
        let anchor = session.transcript_scroll_anchor().unwrap();

        session.apply_view_rows(10);

        let restored = session.transcript_scroll_anchor().unwrap();
        assert_eq!(restored.message_index, anchor.message_index);
        assert_eq!(restored.row_in_message, anchor.row_in_message);
    }

    #[test]
    fn bottom_follow_survives_reflow() {
        let mut session = history_session();
        session.scroll_manager.set_offset(0);
        for width in [32, 100] {
            session.apply_transcript_width(width);
            session.apply_transcript_rows(3);
            assert_eq!(session.scroll_manager.offset(), 0);
        }
    }

    #[test]
    fn theme_change_refreshes_cached_transcript_styles() {
        let mut session = history_session();
        let before = session.collect_transcript_window_cached(80, 0, usize::MAX);
        session.handle_command(crate::tui::core_tui::types::InlineCommand::SetTheme {
            theme: InlineTheme {
                foreground: Some(anstyle::Color::Rgb(anstyle::RgbColor(12, 34, 56))),
                ..InlineTheme::default()
            },
        });
        let after = session.collect_transcript_window_cached(80, 0, usize::MAX);
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(before.iter().zip(after.iter()).any(|(before, after)| before.line != after.line));
        assert_cached_matches_fresh(&mut session, 80);
    }

    #[test]
    fn test_cache_initialization() {
        let cache = TranscriptReflowCache::new(100);
        assert_eq!(cache.width, 100);
        assert_eq!(cache.total_rows(), 0);
        assert!(cache.messages.is_empty());
    }

    #[test]
    fn test_update_message() {
        let mut cache = TranscriptReflowCache::new(80);
        let test_line = line("Test line");
        let lines = vec![test_line];

        cache.update_message(0, 1, lines);

        assert!(!cache.messages.is_empty());
        assert_eq!(cache.messages[0].revision, 1);
        assert_eq!(cache.messages[0].lines.len(), 1);
    }

    #[test]
    fn test_row_offsets() {
        let mut cache = TranscriptReflowCache::new(80);

        // Add three messages: 2 lines, 1 line, 3 lines
        cache.update_message(0, 1, vec![line(Line::default()), line(Line::default())]);
        cache.update_message(1, 2, vec![line(Line::default())]);
        cache.update_message(2, 3, vec![line(Line::default()), line(Line::default()), line(Line::default())]);

        cache.update_row_offsets_from(0);

        assert_eq!(cache.row_offsets, vec![0, 2, 3]); // [0, 0+2, 0+2+1]
        assert_eq!(cache.total_rows(), 6); // 2+1+3
    }

    #[test]
    fn test_get_visible_range() {
        let mut cache = TranscriptReflowCache::new(80);

        // Add two messages
        cache.update_message(0, 1, vec![line("Line 1"), line("Line 2")]);
        cache.update_message(1, 2, vec![line("Line 3")]);

        cache.update_row_offsets_from(0);

        // Get first 2 rows
        let range = cache.get_visible_range(0, 2);
        assert_eq!(range.len(), 2);

        // Get from row 1 (second line of first message) to row 2 (first line of second message)
        let range = cache.get_visible_range(1, 2);
        assert_eq!(range.len(), 2);
    }

    #[test]
    fn test_needs_reflow() {
        let cache = TranscriptReflowCache::new(80);

        // Initially needs reflow
        assert!(cache.needs_reflow(0, 1));

        // After adding message with same revision, doesn't need reflow
        let mut cache = TranscriptReflowCache::new(80);
        cache.update_message(0, 1, vec![line(Line::default())]);
        assert!(!cache.needs_reflow(0, 1));

        // But needs reflow with different revision
        assert!(cache.needs_reflow(0, 2));
    }

    #[test]
    fn test_width_changes() {
        let mut cache = TranscriptReflowCache::new(80);
        assert_eq!(cache.width, 80);

        cache.set_width(120);
        assert_eq!(cache.width, 120);
    }

    #[test]
    fn test_message_accessors() {
        let mut cache = TranscriptReflowCache::new(80);
        cache.update_message(0, 1, vec![line("Test"), line("Lines")]);

        cache.update_row_offsets_from(0);

        assert_eq!(cache.row_offsets.first().copied(), Some(0));
        assert_eq!(cache.messages.first().map(|m| m.lines.len()), Some(2));
        assert_eq!(cache.row_offsets.get(1).copied(), None); // Non-existent message
        assert_eq!(cache.messages.get(1).map(|m| m.lines.len()), None); // Non-existent message
    }

    #[test]
    fn test_empty_range() {
        let cache = TranscriptReflowCache::new(80);
        let range = cache.get_visible_range(0, 0);
        assert!(range.is_empty());
    }

    #[test]
    fn test_out_of_bounds_range() {
        let cache = TranscriptReflowCache::new(80);
        let range = cache.get_visible_range(100, 10); // Start beyond available rows
        assert!(range.is_empty());
    }

    #[test]
    fn test_incremental_row_offsets() {
        let mut cache = TranscriptReflowCache::new(80);

        // Add three messages
        cache.update_message(0, 1, vec![line("M1-L1"), line("M1-L2")]);
        cache.update_message(1, 2, vec![line("M2-L1")]);
        cache.update_message(2, 3, vec![line("M3-L1"), line("M3-L2")]);

        cache.update_row_offsets_from(0);
        assert_eq!(cache.row_offsets, vec![0, 2, 3]);
        assert_eq!(cache.total_rows(), 5);

        // Update second message (index 1)
        cache.update_message(1, 4, vec![line("M2-L1-New"), line("M2-L2-New")]);
        cache.update_row_offsets_from(1);

        assert_eq!(cache.row_offsets, vec![0, 2, 4]);
        assert_eq!(cache.total_rows(), 6);

        // Add fourth message
        cache.update_message(3, 5, vec![line("M4-L1")]);
        cache.update_row_offsets_from(3);

        assert_eq!(cache.row_offsets, vec![0, 2, 4, 6]);
        assert_eq!(cache.total_rows(), 7);
    }

    #[test]
    fn visible_window_cache_respects_viewport_rows() {
        let mut session = Session::new(InlineTheme::default(), None, 20);
        for index in 0..6 {
            session.push_line(InlineMessageKind::Agent, vec![segment(&format!("line {index}"))]);
        }

        let first = session.collect_transcript_window_cached(80, 0, 2);
        let cached = session.collect_transcript_window_cached(80, 0, 2);
        let resized = session.collect_transcript_window_cached(80, 0, 3);

        assert!(Arc::ptr_eq(&first, &cached));
        assert!(!Arc::ptr_eq(&first, &resized));
        assert_eq!(resized.len(), 3);
    }
}
