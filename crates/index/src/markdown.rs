//! `CommonMark` heading extraction with original source bytes and disjoint sections.
use anyhow::{Result, ensure};
use code_intel_core::knowledge::{Document, DocumentSection};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Bound per-document section metadata as well as the shared 1 MiB file limit.
pub const MAX_SECTIONS: usize = 4096;

pub fn extract(path: &str, source: String, hash: String) -> Result<Document> {
    let id = format!("document:{path}");
    let mut starts = vec![0];
    starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
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
    Ok(Document {
        id,
        path: path.into(),
        hash,
        content: source,
        sections,
    })
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
