#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SectionBoundaryMode {
    BracketOnly,
    BracketOrMarkdown,
}

pub(crate) fn find_prompt_section_bounds(
    prompt: &str,
    section_header: &str,
    boundary_mode: SectionBoundaryMode,
) -> Option<(usize, usize)> {
    fn is_section_header_line(line: &str, boundary_mode: SectionBoundaryMode) -> bool {
        let trimmed = line.trim();
        (trimmed.starts_with('[') && trimmed.ends_with(']'))
            || matches!(boundary_mode, SectionBoundaryMode::BracketOrMarkdown) && trimmed.starts_with("## ")
    }

    let mut offset = 0usize;
    let mut section_start = None;

    for line in prompt.split_inclusive('\n') {
        let trimmed = line.trim();
        if section_start.is_none() && trimmed == section_header {
            section_start = Some(offset);
            offset += line.len();
            continue;
        }

        if let Some(start) = section_start
            && is_section_header_line(line, boundary_mode)
        {
            return Some((start, offset));
        }

        offset += line.len();
    }

    section_start.map(|start| (start, prompt.len()))
}

#[cfg(test)]
mod tests {
    use super::{SectionBoundaryMode, find_prompt_section_bounds};

    #[test]
    fn markdown_boundary_mode_stops_before_next_markdown_heading() {
        let prompt = "Base\n## Active Tools\n- a\n## Environment\n- b\n";
        let bounds = find_prompt_section_bounds(prompt, "## Active Tools", SectionBoundaryMode::BracketOrMarkdown)
            .expect("section bounds");

        assert_eq!(&prompt[bounds.0..bounds.1], "## Active Tools\n- a\n");
    }

    #[test]
    fn bracket_only_mode_ignores_markdown_headings() {
        let prompt = "Base\n[Harness Limits]\n- a\n## Environment\n- b\n";
        let bounds = find_prompt_section_bounds(prompt, "[Harness Limits]", SectionBoundaryMode::BracketOnly)
            .expect("section bounds");

        assert_eq!(&prompt[bounds.0..bounds.1], "[Harness Limits]\n- a\n## Environment\n- b\n");
    }
}
