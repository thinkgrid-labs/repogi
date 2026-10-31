# Repogi documentation

This directory contains public usage, API and behavior documentation. Keep internal plans, design proposals, evaluation notes and maintainer procedures under `internal/`, outside public source exports.

Start with [installation](DEVELOPER_PREVIEW_INSTALLATION.md) (prebuilt binaries, checksums and source builds; [release assets](RELEASES.md)), then configure a client through [MCP](MCP.md). Repogi indexes TypeScript/TSX and Markdown project documents; see [supported languages](SUPPORTED_LANGUAGES.md).

## Releases

The latest published release is **0.1.0**, with prebuilt Linux and macOS binaries for TypeScript/TSX code intelligence: context, references, dependencies, change impact and test candidates. Project knowledge, knowledge relationships, agent investigation and project memory are part of the next release, **0.1.1**. Until it is published, [build from source](DEVELOPER_PREVIEW_INSTALLATION.md#build-from-source) to use them.

The 0.1.0 binary cannot read indexes written by newer builds. It refuses them with `unsupported index schema` and changes nothing. A newer build upgrades a 0.1.0 index the next time you run `repogi index`.

## Using Repogi

- [Project memory](PROJECT_MEMORY.md): developer-reviewed claims with immutable evidence snapshots, live verification, history and opt-in agent access (0.1.1).
- [Memory storage, backup and recovery](MEMORY_STORAGE.md): the durable `.repogi/memory.db` store, migration from index-stored memory, index rebuilds, export/import and recovery.
- [Agent investigation](AGENT_INVESTIGATION.md): evidence-backed investigation plans for a task, with uncertainty, freshness and work limits (0.1.1).
- [Knowledge relationships](KNOWLEDGE_RELATIONSHIPS.md): explicit requirements, ADRs, bounded trace queries and source-backed links (0.1.1).
- [Project knowledge](KNOWLEDGE.md): Markdown indexing, query-centered excerpts, balanced code/docs context, extractor upgrades and evidence limits.
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

[GitHub Actions CI](../.github/workflows/ci.yml) checks public source membership, formatting, Clippy, workspace tests and a release CLI build on Linux, and runs the workspace tests on macOS, for pull requests and pushes to `main`. It uses Rust 1.98.0 with the committed lockfile. Tests run with one test thread so timing-sensitive concurrency checks do not compete with unrelated tests.

The separate [release workflow](../.github/workflows/release.yml) builds, tests and packages each platform on tags and manual dispatch only; publishing is an owner-approved draft. See [release assets and verification](RELEASES.md).

For local development commands and workspace layout, see the [project README](../README.md#documentation-and-contributing).
