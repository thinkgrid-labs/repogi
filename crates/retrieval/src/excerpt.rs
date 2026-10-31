//! Deterministic contiguous evidence windows over untouched UTF-8 source bytes.
use std::{collections::BTreeMap, ops::Range};

/// Prepared once per section; subsequent budget probes only scan line metadata.
pub(crate) struct Windows<'a> {
    source: &'a str,
    lines: Vec<usize>,
    scores: Vec<f64>,
    anchor: usize,
    anchor_score: f64,
}

impl<'a> Windows<'a> {
    pub fn new(source: &'a str, matches: &[Range<usize>]) -> Self {
        let mut lines = code_intel_core::knowledge::line_starts(source);
        if !source.is_empty() && lines.last() == Some(&source.len()) {
            lines.pop();
        }
        lines.push(source.len());
        let mut counts = BTreeMap::new();
        for range in matches {
            *counts
                .entry(source[range.clone()].to_lowercase())
                .or_insert(0u32) += 1;
        }
        let mut scores = vec![0.0; lines.len()];
        let (mut anchor, mut anchor_score) = (0, 0.0);
        for range in matches {
            let weight = 1.0 / f64::from(counts[&source[range.clone()].to_lowercase()]);
            let line = lines
                .partition_point(|at| *at <= range.start)
                .saturating_sub(1);
            scores[line + 1] += weight;
            if weight > anchor_score {
                anchor = range.start;
                anchor_score = weight;
            }
        }
        for index in 1..scores.len() {
            scores[index] += scores[index - 1];
        }
        Self {
            source,
            lines,
            scores,
            anchor,
            anchor_score,
        }
    }

    /// Prefer complete lines maximizing rare matched-token coverage. A single
    /// overlong line falls back to a character-aligned window around its match.
    pub fn range(&self, bytes: usize) -> Range<usize> {
        if self.source.len() <= bytes {
            return 0..self.source.len();
        }
        if bytes == 0 {
            return 0..0;
        }
        let (mut right, mut best, mut score) = (0, 0..0, 0.0);
        for left in 0..self.lines.len() - 1 {
            right = right.max(left);
            while right + 1 < self.lines.len() && self.lines[right + 1] - self.lines[left] <= bytes
            {
                right += 1;
            }
            let value = self.scores[right] - self.scores[left];
            if value > score || (best.is_empty() && right > left) {
                best = self.lines[left]..self.lines[right];
                score = value;
            }
        }
        if score >= self.anchor_score && !best.is_empty() {
            return best;
        }
        let line = self
            .lines
            .partition_point(|at| *at <= self.anchor)
            .saturating_sub(1);
        // Anchor near the start of a character window, leaving room for the
        // matching word even when the match is near the end of a very long line.
        let mut start = self.anchor.saturating_sub(bytes / 4).max(self.lines[line]);
        while !self.source.is_char_boundary(start) {
            start += 1;
        }
        let mut end = (start + bytes).min(self.source.len());
        while !self.source.is_char_boundary(end) {
            end -= 1;
        }
        if self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r')
            && self.source.as_bytes().get(end) == Some(&b'\n')
        {
            end -= 1;
        }
        start..end
    }
}

/// End-exclusive bytes; inclusive lines. Preserve CRLF and all interior bytes.
pub(crate) fn positions(source: &str, range: &Range<usize>, first_line: usize) -> (usize, usize) {
    let starts = code_intel_core::knowledge::line_starts(source);
    let start = first_line
        + starts
            .partition_point(|at| *at <= range.start)
            .saturating_sub(1);
    let end = first_line
        + starts
            .partition_point(|at| *at < range.end)
            .saturating_sub(1);
    (start, end.max(start))
}
