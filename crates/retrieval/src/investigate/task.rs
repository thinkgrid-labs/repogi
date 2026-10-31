//! Deterministic task interpretation: explicit author IDs, content terms and
//! keyword cues. Cues reorder whole plan phases; they are not semantic intent.
use super::{Category, MAX_SELECTORS, MAX_TERMS, Task};

#[derive(Debug, Default, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Cues {
    change: bool,
    bug: bool,
    knowledge: bool,
    impact: bool,
    tests: bool,
}

const STOP_WORDS: [&str; 65] = [
    "a",
    "about",
    "all",
    "an",
    "and",
    "any",
    "are",
    "at",
    "be",
    "by",
    "can",
    "code",
    "did",
    "do",
    "does",
    "explain",
    "find",
    "for",
    "from",
    "how",
    "identify",
    "implementation",
    "implemented",
    "implements",
    "in",
    "inspect",
    "into",
    "investigate",
    "is",
    "it",
    "its",
    "me",
    "my",
    "need",
    "needs",
    "of",
    "on",
    "or",
    "our",
    "project",
    "relevant",
    "related",
    "repo",
    "repository",
    "review",
    "should",
    "show",
    "that",
    "the",
    "these",
    "this",
    "those",
    "to",
    "understand",
    "use",
    "used",
    "uses",
    "using",
    "was",
    "we",
    "what",
    "when",
    "where",
    "which",
    "with",
];
const CHANGE_VERBS: [&str; 10] = [
    "add",
    "change",
    "extend",
    "implement",
    "modify",
    "refactor",
    "remove",
    "rename",
    "update",
    "changing",
];
const BUG_WORDS: [&str; 11] = [
    "bug", "broken", "crash", "error", "errors", "fail", "failing", "fails", "failure", "failures",
    "fix",
];
const KNOWLEDGE_WORDS: [&str; 13] = [
    "acceptance",
    "adr",
    "architectural",
    "architecture",
    "chosen",
    "decision",
    "decisions",
    "prd",
    "rationale",
    "requirement",
    "requirements",
    "selected",
    "why",
];
const IMPACT_WORDS: [&str; 10] = [
    "affect",
    "affected",
    "affects",
    "callers",
    "consumers",
    "depend",
    "dependents",
    "depends",
    "impact",
    "users",
];
const TEST_WORDS: [&str; 6] = ["coverage", "spec", "specs", "test", "testing", "tests"];
/// Verbs and cue words that steer phases but are not searched as content.
const CUE_ONLY: [&str; 8] = [
    "fix",
    "modify",
    "change",
    "changing",
    "update",
    "add",
    "implement",
    "refactor",
];

/// Explicit author IDs use the indexed `REQ-`/`ADR-` syntax; nothing else is a selector.
pub(super) fn author_id(token: &str) -> bool {
    let rest = token
        .strip_prefix("REQ-")
        .or_else(|| token.strip_prefix("ADR-"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && token.len() <= 128
            && rest
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
            && !rest.ends_with('-')
    })
}

/// A deterministic suffix trim so FTS5 prefix search matches inflections
/// ("expiration" → "expir*"). It is not a linguistic stemmer.
fn stem(word: &str) -> String {
    for suffix in [
        "ations", "ation", "ings", "ing", "ments", "ment", "ies", "es", "ed", "s",
    ] {
        if let Some(base) = word.strip_suffix(suffix)
            && base.chars().count() >= 4
        {
            return base.to_owned();
        }
    }
    word.to_owned()
}

pub(super) fn normalize(original: &str, explicit: &[String]) -> (Task, Cues) {
    let mut selectors: Vec<String> = Vec::new();
    for selector in explicit {
        if !selectors.contains(selector) {
            selectors.push(selector.clone());
        }
    }
    let mut remainder = String::with_capacity(original.len());
    for token in original.split(|c: char| {
        c.is_whitespace() || matches!(c, ',' | ';' | '(' | ')' | '[' | ']' | '"' | '\'' | '`')
    }) {
        let token = token.trim_end_matches(['?', '.', ':', '!']);
        if author_id(token) {
            if !selectors.iter().any(|s| s == token) && selectors.len() < MAX_SELECTORS {
                selectors.push(token.to_owned());
            }
        } else {
            remainder.push_str(token);
            remainder.push(' ');
        }
    }
    let words: Vec<String> = remainder
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let first = words.first().map(String::as_str);
    let cues = Cues {
        change: first.is_some_and(|w| CHANGE_VERBS.contains(&w)),
        bug: words.iter().any(|w| BUG_WORDS.contains(&w.as_str())),
        knowledge: words.iter().any(|w| KNOWLEDGE_WORDS.contains(&w.as_str()))
            || selectors.iter().any(|s| author_id(s)),
        impact: words.iter().any(|w| IMPACT_WORDS.contains(&w.as_str())),
        tests: words.iter().any(|w| TEST_WORDS.contains(&w.as_str())),
    };
    let mut terms: Vec<String> = Vec::new();
    for word in words {
        let cue_only = CUE_ONLY.contains(&word.as_str())
            || KNOWLEDGE_WORDS.contains(&word.as_str())
            || IMPACT_WORDS.contains(&word.as_str())
            || TEST_WORDS.contains(&word.as_str());
        if word.chars().count() < 2
            || word.chars().all(|c| c.is_ascii_digit())
            || STOP_WORDS.contains(&word.as_str())
            || cue_only
            || terms.contains(&word)
        {
            continue;
        }
        terms.push(word);
        if terms.len() == MAX_TERMS {
            break;
        }
    }
    let mut search_terms: Vec<String> = Vec::new();
    for term in &terms {
        let stemmed = stem(term);
        if !search_terms.contains(&stemmed) {
            search_terms.push(stemmed);
        }
    }
    let mut labels = Vec::new();
    for (on, label) in [
        (cues.change, "change"),
        (cues.bug, "bug"),
        (cues.knowledge, "knowledge"),
        (cues.impact, "impact"),
        (cues.tests, "tests"),
    ] {
        if on {
            labels.push(label);
        }
    }
    (
        Task {
            original: original.to_owned(),
            classification: "supported",
            cues: labels,
            terms,
            search_terms,
            selectors,
        },
        cues,
    )
}

/// Phase order of categories; cues only move whole categories forward.
pub(super) fn category_order(cues: Cues) -> Vec<Category> {
    let mut order = vec![
        Category::Selected,
        Category::Implementation,
        Category::Knowledge,
        Category::Memory,
        Category::Consumers,
        Category::Dependencies,
        Category::Tests,
        Category::Uncertainty,
        Category::Lexical,
    ];
    let mut promote = |category: Category, after: Category| {
        order.retain(|c| *c != category);
        let at = order.iter().position(|c| *c == after).map_or(0, |p| p + 1);
        order.insert(at, category);
    };
    if cues.tests {
        promote(Category::Tests, Category::Implementation);
    }
    if cues.impact {
        promote(Category::Consumers, Category::Implementation);
    }
    if cues.knowledge {
        promote(Category::Knowledge, Category::Selected);
    }
    // Opt-in memory steps always follow the recorded knowledge they relate to.
    promote(Category::Memory, Category::Knowledge);
    order
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn normalization_extracts_author_ids_cues_and_stems_without_inventing_terms() {
        let (task, cues) = normalize("What implements REQ-AUTH-001?", &[]);
        assert_eq!(task.selectors, ["REQ-AUTH-001"]);
        assert!(task.terms.is_empty() && cues.knowledge);
        let (task, cues) = normalize("Modify session expiration", &[]);
        assert_eq!(task.terms, ["session", "expiration"]);
        assert_eq!(task.search_terms, ["session", "expir"]);
        assert!(cues.change && !cues.bug);
        let (task, cues) = normalize("Why does this project use SQLite?", &[]);
        assert_eq!(task.terms, ["sqlite"]);
        assert!(cues.knowledge);
        let (task, cues) = normalize("Identify relevant tests for session timeout", &[]);
        assert_eq!(task.terms, ["session", "timeout"]);
        assert!(cues.tests);
        let (task, _) = normalize("req-auth-001 REQ- ADR-0001-", &[]);
        assert!(
            task.selectors.is_empty(),
            "lowercase or malformed IDs are not selectors"
        );
    }

    #[test]
    fn cues_reorder_whole_categories_only() {
        let order = category_order(Cues {
            knowledge: true,
            ..Cues::default()
        });
        // Pre-memory categories keep their v0.4 relative order.
        let existing: Vec<Category> = order
            .iter()
            .copied()
            .filter(|c| *c != Category::Memory)
            .collect();
        assert_eq!(
            &existing[..3],
            [
                Category::Selected,
                Category::Knowledge,
                Category::Implementation
            ]
        );
        let order = category_order(Cues {
            impact: true,
            ..Cues::default()
        });
        assert_eq!(
            &order[1..3],
            [Category::Implementation, Category::Consumers]
        );
        assert_eq!(order.len(), 9);
        let order = category_order(Cues {
            knowledge: true,
            ..Cues::default()
        });
        assert_eq!(&order[1..3], [Category::Knowledge, Category::Memory]);
    }
}
