//! Deterministic retrieval strategies with progressive, evidence-preserving output.
mod agent;
pub mod impact;
mod legacy;
pub mod manifest;
pub mod navigate;
mod optimized;
mod packing;
mod semantic;
pub mod signal;
pub mod tests;

use anyhow::{Result, ensure};
use code_intel_core::ContextResponse;
use code_intel_store::Store;

pub use legacy::render;

/// A response envelope cannot fit; adapters preserve a stable budget error code.
#[derive(Debug)]
pub struct BudgetTooSmall {
    pub minimum: usize,
}
impl std::fmt::Display for BudgetTooSmall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "output budget is too small for metadata (minimum {} UTF-8 bytes); increase the budget",
            self.minimum
        )
    }
}
impl std::error::Error for BudgetTooSmall {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Text,
    /// Compact text for agents: IDs for follow-up calls, no repeated provenance,
    /// query echo or JSON escaping of source. Budgeted like the other formats.
    Agent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Strategy {
    Lexical,
    Graph,
    #[default]
    Optimized,
    Semantic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Detail {
    Overview,
    Focused,
    #[default]
    Detailed,
}

impl Detail {
    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Focused => "focused",
            Self::Detailed => "detailed",
        }
    }
}

#[derive(Debug, Default)]
pub struct Options {
    pub strategy: Strategy,
    pub detail: Detail,
    pub select: Vec<String>,
    /// Return only the selected IDs, without dependency expansion at any detail.
    pub skip_dependencies: bool,
    /// The caller capped the selection list; reported as `candidate_limit_reached`.
    pub selection_truncated: bool,
}

pub fn context(
    store: &Store,
    query: &str,
    max_tokens: usize,
    format: Format,
) -> Result<ContextResponse> {
    context_with_options(store, query, max_tokens, format, &Options::default())
}

pub fn context_with_options(
    store: &Store,
    query: &str,
    max_tokens: usize,
    format: Format,
    options: &Options,
) -> Result<ContextResponse> {
    store.check_cancelled()?;
    if matches!(options.strategy, Strategy::Optimized | Strategy::Semantic) {
        optimized::context(store, query, max_tokens, format, options)
    } else {
        ensure!(
            options.detail == Detail::Detailed && options.select.is_empty(),
            "--detail and --select require --retrieval optimized"
        );
        legacy::context(
            store,
            query,
            max_tokens,
            format,
            options.strategy == Strategy::Graph,
        )
    }
}
