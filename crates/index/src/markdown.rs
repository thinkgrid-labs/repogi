//! `CommonMark` heading extraction with original source bytes and disjoint sections.
use anyhow::{Result, ensure};
use code_intel_core::knowledge::{Document, DocumentSection};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Bound per-document section metadata as well as the shared 1 MiB file limit.
pub const MAX_SECTIONS: usize = 4096;

pub fn extract(path: &str, source: String, hash: String) -> Result<Document> {
    let id = format!("document:{path}");
    let starts = code_intel_core::knowledge::line_starts(&source);
    let mut headings = Vec::new();
    let mut heading = None;
    let mut containers = 0usize;
    for (event, range) in
        Parser::new_ext(&source, Options::ENABLE_YAML_STYLE_METADATA_BLOCKS).into_offset_iter()
    {
        match event {
            Event::Start(Tag::BlockQuote(_) | Tag::Item) => containers += 1,
            Event::End(TagEnd::BlockQuote(_) | TagEnd::Item) => {
                containers = containers.saturating_sub(1);
            }
            Event::Start(Tag::Heading { level, .. }) if containers == 0 => {
                let start_line = starts.partition_point(|offset| *offset <= range.start);
                heading = Some((starts[start_line - 1], level as usize, String::new()));
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some((_, _, name)) = &mut heading {
                    name.push_str(&text);
                    ensure!(
                        name.len() <= 1024,
                        "document heading exceeds 1024 UTF-8 bytes"
                    );
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, name)) = &mut heading {
                    name.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = heading.take() {
                    headings.push(heading);
                }
                ensure!(
                    headings.len() <= MAX_SECTIONS,
                    "document exceeds {MAX_SECTIONS} sections"
                );
            }
            _ => {}
        }
    }
    // Each section ends before the next heading, rather than repeating nested
    // bodies in every ancestor. Keep preamble/front matter at original lines.
    if headings.first().is_none_or(|(start, _, _)| *start > 0) && !source.is_empty() {
        headings.insert(0, (0, 0, String::new()));
    }
    ensure!(
        headings.len() <= MAX_SECTIONS,
        "document exceeds {MAX_SECTIONS} sections"
    );
    let mut hierarchy: Vec<(usize, String)> = Vec::new();
    let mut sections = Vec::new();
    let mut occurrences = std::collections::BTreeMap::<Vec<String>, usize>::new();
    for (position, (start, level, name)) in headings.iter().enumerate() {
        let end = headings
            .get(position + 1)
            .map_or(source.len(), |next| next.0);
        while hierarchy.last().is_some_and(|(parent, _)| parent >= level) {
            hierarchy.pop();
        }
        if *level > 0 {
            hierarchy.push((*level, name.clone()));
        }
        let hierarchy: Vec<_> = hierarchy.iter().map(|(_, name)| name.clone()).collect();
        let occurrence = occurrences.entry(hierarchy.clone()).or_default();
        *occurrence += 1;
        // Structured encoding prevents delimiter collisions in paths/headings.
        let identity = serde_json::to_vec(&(&id, &hierarchy, *occurrence))?;
        sections.push(DocumentSection {
            id: format!("section:{}", blake3::hash(&identity).to_hex()),
            heading: name.clone(),
            hierarchy,
            start_byte: *start,
            end_byte: end,
            start_line: starts.partition_point(|offset| *offset <= *start),
            end_line: starts.partition_point(|offset| *offset < end),
            content: source[*start..end].to_owned(),
        });
    }
    let mut document = Document {
        id,
        path: path.into(),
        hash,
        content: source,
        sections,
        knowledge: code_intel_core::relationships::KnowledgeExtraction::default(),
    };
    document.knowledge = crate::relationships::extract(&document)?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_has_disjoint_original_ranges_and_hierarchy() {
        let source = include_str!("../../../fixtures/knowledge/docs/PRD.md");
        let document = extract("docs/PRD.md", source.into(), crate::content_hash(source)).unwrap();
        assert_eq!(document.sections.len(), 5);
        let section = &document.sections[3];
        assert_eq!(section.heading, "Session expiration");
        assert_eq!(
            section.hierarchy,
            [
                "Product requirements",
                "Authentication requirements",
                "Session expiration"
            ]
        );
        assert_eq!((section.start_line, section.end_line), (12, 22));
        assert!(section.content.contains("# Example heading inside a fence"));
        let joined: String = document
            .sections
            .iter()
            .map(|section| section.content.as_str())
            .collect();
        assert_eq!(joined, source);
        for pair in document.sections.windows(2) {
            assert_eq!(pair[0].end_line + 1, pair[1].start_line);
        }
        let again = extract("docs/PRD.md", source.into(), crate::content_hash(source)).unwrap();
        assert_eq!(
            document.sections.iter().map(|s| &s.id).collect::<Vec<_>>(),
            again.sections.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn setext_crlf_duplicate_headings_and_containers() {
        let source = "Title\r\n=====\r\n\r\n- # list heading\r\n\r\n> # quote heading\r\n\r\n~~~md\r\n# fence\r\n~~~\r\n\r\n## Same\r\none\r\n## Same\r\ntwo\r\n";
        let document = extract("README.md", source.into(), crate::content_hash(source)).unwrap();
        assert_eq!(document.sections.len(), 3);
        assert_eq!(document.sections[0].heading, "Title");
        assert_eq!(document.sections[0].end_line, 11);
        assert_ne!(document.sections[1].id, document.sections[2].id);
        assert_eq!(document.sections[2].start_line, 14);
        let changed = source.replace("one", "longer body");
        assert_eq!(
            document.sections[2].id,
            extract("README.md", changed.clone(), crate::content_hash(&changed))
                .unwrap()
                .sections[2]
                .id
        );
    }

    #[test]
    fn standalone_carriage_returns_have_valid_section_ranges() {
        let source = "# First\rbody\r## Second\rrecorded intent\r";
        let document = extract("policy.md", source.into(), crate::content_hash(source)).unwrap();
        assert_eq!(document.sections.len(), 2);
        assert_eq!(
            (
                document.sections[0].start_line,
                document.sections[0].end_line
            ),
            (1, 2)
        );
        assert_eq!(
            (
                document.sections[1].start_line,
                document.sections[1].end_line
            ),
            (3, 4)
        );
        assert_eq!(document.sections[0].content, "# First\rbody\r");
    }

    #[test]
    fn html_indented_code_and_unicode_preserve_source_partitions() {
        let source = "---\ntitle: 雪\n---\n\n# Café [policy](local.md)\n\n    # indented code\n\n<script>\n# script content\n</script>\n\n<Widget prop={value}>\n# MDX-like HTML block content\n</Widget>\n\n> # quoted\n\n- # item\n\n## 雪 decisions\nIntent with `code` and a link.\n";
        let document = extract(
            "Unicode policy.md",
            source.into(),
            crate::content_hash(source),
        )
        .unwrap();
        assert_eq!(
            document
                .sections
                .iter()
                .map(|s| s.heading.as_str())
                .collect::<Vec<_>>(),
            ["", "Café policy", "雪 decisions"]
        );
        assert_eq!(
            document
                .sections
                .iter()
                .map(|s| s.content.as_str())
                .collect::<String>(),
            source
        );
        for section in &document.sections {
            assert_eq!(
                &source[section.start_byte..section.end_byte],
                section.content
            );
            assert!(section.start_line <= section.end_line);
            assert_eq!(
                section.start_line,
                code_intel_core::knowledge::line_starts(source)
                    .partition_point(|at| *at <= section.start_byte)
            );
        }
    }

    #[test]
    fn hierarchy_identity_survives_line_shifts_but_changes_with_parent_heading() {
        let source = "# Parent\ntext\n### Child\nintent\n### Child\nsecond\n";
        let original = extract("policy.md", source.into(), crate::content_hash(source)).unwrap();
        let shifted = format!("preamble\n\n{source}");
        let shifted = extract("policy.md", shifted.clone(), crate::content_hash(&shifted)).unwrap();
        assert_eq!(original.sections[1].id, shifted.sections[2].id);
        assert_ne!(original.sections[1].id, original.sections[2].id);
        let renamed = source.replace("Parent", "New parent");
        let renamed = extract("policy.md", renamed.clone(), crate::content_hash(&renamed)).unwrap();
        assert_ne!(original.sections[1].id, renamed.sections[1].id);
    }

    #[test]
    fn empty_plain_and_unclosed_fences_are_safe() {
        for source in [
            "",
            "plain text",
            "```md\n# not a heading\n",
            "---\n# unclosed frontmatter\n",
        ] {
            let document = extract("empty.md", source.into(), crate::content_hash(source)).unwrap();
            assert!(document.sections.len() <= 2);
            assert_eq!(
                document
                    .sections
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect::<String>(),
                source
            );
        }
    }
}
