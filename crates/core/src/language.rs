//! Indexed source languages, chosen by exact (case-sensitive) file extension.

/// A source language Repogi parses. TypeScript and JavaScript share one
/// evidence model; each has its own grammar and module-resolution rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// `.ts`, `.mts`, `.cts` (including declaration files).
    TypeScript,
    /// `.tsx`.
    Tsx,
    /// `.js`, `.mjs`, `.cjs`. JSX syntax is accepted, as in the grammar.
    JavaScript,
    /// `.jsx`.
    Jsx,
}

/// Every indexed source extension, in reporting order.
pub const SOURCE_EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// The same list as shown to people and agents.
pub const SOURCE_EXTENSIONS_TEXT: &str = ".ts/.tsx/.mts/.cts/.js/.jsx/.mjs/.cjs";

impl Language {
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension {
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::Jsx,
            _ => return None,
        })
    }

    /// The language of a `/`-separated or native path, if it is indexed.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Self::from_extension(extension)
    }

    /// The value recorded in the index `files.language` column.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
        }
    }

    #[must_use]
    pub fn is_javascript(self) -> bool {
        matches!(self, Self::JavaScript | Self::Jsx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_exact_and_case_sensitive() {
        assert_eq!(Language::of("src/a.ts"), Some(Language::TypeScript));
        assert_eq!(Language::of("src/a.d.ts"), Some(Language::TypeScript));
        assert_eq!(Language::of("a.mts"), Some(Language::TypeScript));
        assert_eq!(Language::of("a.tsx"), Some(Language::Tsx));
        assert_eq!(Language::of("lib/a.cjs"), Some(Language::JavaScript));
        assert_eq!(Language::of("a.jsx"), Some(Language::Jsx));
        for path in [
            "a.TS", "a.Js", "a.json", "a.ts.bak", ".js", "src/.ts", "Makefile",
        ] {
            assert_eq!(Language::of(path), None, "{path}");
        }
        for extension in SOURCE_EXTENSIONS {
            assert!(Language::from_extension(extension).is_some());
        }
        assert_eq!(
            SOURCE_EXTENSIONS_TEXT,
            SOURCE_EXTENSIONS.map(|e| format!(".{e}")).join("/")
        );
    }
}
