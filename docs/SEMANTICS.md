# Static semantic scope

Repogi indexes TypeScript, TSX, JavaScript and JSX declarations, imports, exports and supported static use sites. Relative imports, selected path aliases, barrels and re-exports connect cross-file bindings. Direct identifier calls, supported namespace members and static class methods can resolve. JSX component uses are references, not calls.

Unknown object receivers, dynamic dispatch, runtime registration, unsupported syntax and ambiguous exports retain uncertainty. Text search matches never become confirmed semantic edges. The native resolver is not a TypeScript compiler or runtime oracle.

An index revision identifies a SQLite snapshot. Changed/deleted source and resolution settings invalidate graph state during explicit indexing. Queries do not rebuild it. Current-source checks cover selected files and do not prove whole-graph freshness. See [resolution](MODULE_RESOLUTION.md), [grammar limitations](TYPESCRIPT_GRAMMAR_LIMITATIONS.md) and [navigation limits](REFERENCE_NAVIGATION_LIMITS.md).

## JavaScript

JavaScript uses the same binder and evidence rules as TypeScript; only parsing and module lookup differ ([supported languages](SUPPORTED_LANGUAGES.md), [JavaScript importers](MODULE_RESOLUTION.md#javascript-importers)). Two grammar differences are handled explicitly so they cannot create confirmed edges: JavaScript parameters (plain, destructured, default and rest) are bound as locals, so a parameter that shadows an import never resolves to it, and `class A extends B` records an EXTENDS use as in TypeScript. Edge provenance names the extractor: `tree_sitter_javascript` for facts and `javascript_static_binder_v1` for resolved edges in JavaScript files. CommonJS (`require`, `module.exports`), dynamic `import()` and instance dispatch stay unconfirmed.
