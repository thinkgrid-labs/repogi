# Repogi documentation

Start with [installation](DEVELOPER_PREVIEW_INSTALLATION.md), then configure a client through [MCP](MCP.md). Repogi indexes TypeScript and TSX; see [supported languages](SUPPORTED_LANGUAGES.md).

## Using Repogi

- [Reference-first workflows](REFERENCE_FIRST_AGENT_WORKFLOWS.md): discover declarations, inspect consumers and read current source.
- [Navigation limits](REFERENCE_NAVIGATION_LIMITS.md): response budgets, traversal limits, freshness and uncertainty.
- [Change impact](CHANGE_IMPACT.md) and [compact summaries](IMPACT_SUMMARIES.md): investigate Git changes and retrieve selected evidence.
- [Affected-test discovery](AFFECTED_TEST_DISCOVERY.md): identify advisory test candidates without executing them.
- [Claude Code](agents/claude-code.md) and [Codex](agents/codex.md): client setup.

## API and architecture

- [Architecture](ARCHITECTURE.md), [semantic scope](SEMANTICS.md), [module resolution](MODULE_RESOLUTION.md), [JSX references](JSX_SEMANTICS.md) and [grammar limitations](TYPESCRIPT_GRAMMAR_LIMITATIONS.md).
- [MCP impact](MCP_IMPACT.md) and [MCP affected tests](MCP_TEST_DISCOVERY.md).
- [Verification manifests](VERIFICATION_MANIFEST.md), [MCP artifact transport](MCP_VERIFICATION_MANIFEST.md), [reference consumer](MANIFEST_CONSUMER_INTEROPERABILITY.md) and [CI artifact integration](CI_ARTIFACT_INTEGRATION.md).

## Contributing

[GitHub Actions CI](../.github/workflows/ci.yml) checks public source membership, formatting, Clippy, workspace tests and a release CLI build on pull requests and pushes to `main`. It uses Rust 1.98.0 with the committed lockfile. Tests run with one test thread so timing-sensitive concurrency checks do not compete with unrelated tests.

For local development commands and workspace layout, see the [project README](../README.md#development-and-contributing).
