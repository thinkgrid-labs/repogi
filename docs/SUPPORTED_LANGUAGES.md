# Supported languages

| Language | Extensions | Grammar | Since |
| --- | --- | --- | --- |
| TypeScript | `.ts` (including `.d.ts`), `.mts`, `.cts` | Tree-sitter TypeScript 0.23.2 | `.ts` 0.1.0; `.mts`/`.cts` source builds (0.1.2) |
| TSX | `.tsx` | Tree-sitter TSX 0.23.2 | 0.1.0 |
| JavaScript | `.js`, `.mjs`, `.cjs` (JSX syntax accepted) | Tree-sitter JavaScript 0.23.1 | source builds (0.1.2) |
| JSX | `.jsx` | Tree-sitter JavaScript 0.23.1 | source builds (0.1.2) |

Extensions match exactly and case-sensitively (`a.JS` is not indexed). Markdown (`.md` and `.markdown`, case-insensitive) is indexed separately as project knowledge: it supplies document intent and source locations, never resolved code relationships. See [Knowledge](KNOWLEDGE.md).

Python, Rust, Go and every other language are outside indexing scope. Use native search for them; a query with no matches says that other languages were not searched. Indexing never executes source in any language.

## What JavaScript support covers

JavaScript and TypeScript share one evidence model, one graph and one binder. Each language keeps its own grammar and its own module-resolution rules ([module resolution](MODULE_RESOLUTION.md#javascript-importers)). Supported:

- Declarations: functions, generators, classes and their methods, top-level `const`/`let`/`var`, with source paths, byte offsets and line ranges.
- ES modules: named, default and namespace imports, import aliases, `export` declarations, `export default`, export lists, re-exports (`export { a as b } from`, `export { default as x } from`, `export * from`).
- Static uses: direct calls, namespace members (`ns.f()`), static class members (`C.m()`), `extends`, identifier references.
- JSX in `.jsx` and in `.js` files: component references (local, imported, default, from `.tsx`), never calls. Lower-case tags are intrinsic elements.
- Mixed repositories: JavaScript importing TypeScript (including TypeScript output names such as `./a.js` for `a.ts`) and TypeScript importing JavaScript, `.mjs`→`.mts`, `.cjs`→`.cts`.
- `jsconfig.json`/`tsconfig.json` `paths` and `baseUrl`, as for TypeScript.
- Incremental indexing, change impact and advisory test discovery for JavaScript sources and tests.

Never confirmed (reported as `unresolved`, `unsupported` or `ambiguous`):

- CommonJS: `require()` calls, destructured `require` bindings, `module.exports` and `exports.name`. Declarations in CommonJS files are still indexed and searchable.
- Dynamic `import()`, computed members, values reached through objects or registries, dynamic JSX components (`const C = map[kind]; <C />`).
- Instance dispatch (`this.method()`, `super.method()`), as for TypeScript.
- Package imports and `package.json` `main`/`exports`; Node.js runtime resolution is not modeled.
- Bindings that are reassigned anywhere in their scope, and any name shadowed by a parameter or local declaration.
- Files with syntax errors (for example Flow type annotations): their intact declarations are kept, but they produce no use edges.

Minified and generated JavaScript (`*.min.js`, `*.generated.mjs` and the same for the other new extensions, plus files starting with `// @generated`) is skipped like generated TypeScript.

## Evaluation

**Labeled fixture.** `fixtures/javascript/EXPECTED.json` holds hand-reviewed labels for every resolved edge in a mixed JavaScript/TypeScript fixture (69 resolved edges, 14 sites that must stay unconfirmed). `crates/cli/tests/javascript.rs` requires an exact match: no false confirmed edge, no missed label.

**Real repositories against an independent oracle.** `scripts/evaluation/javascript_graph.py` indexes a copy of a repository and asks the TypeScript 6.0.3 language service (with `allowJs`; it only reads files) for the definition at every resolved edge in a JavaScript file, and at every unresolved call whose callee is a plain name. Single runs, one Linux desktop with a power-saving CPU governor, 2026-11-01:

| Repository (commit) | Files | Cold / warm index | Index size | Resolved JS edges agreeing with TypeScript | Call recall estimate |
| --- | ---: | --- | ---: | --- | ---: |
| [react-redux-realworld-example-app](https://github.com/gothinkster/react-redux-realworld-example-app) (`ee72eba`) | 38 | 0.08 s / 0.02 s | 3.7 MB | 361 / 361 | 0.64 |
| [Chart.js](https://github.com/chartjs/Chart.js) (`71d4421`) | 745 | 1.74 s / 0.06 s | 83 MB | 2,645 / 2,646 | 0.54 |

The one Chart.js disagreement was reviewed: Repogi resolves a use in `karma.conf.cjs` to the local `const` that holds a `require()` result, while TypeScript follows the `require` into the other file. Both describe the code; Repogi's edge is not wrong.

Recall counts only call sites where TypeScript names a declaration Repogi indexes. In Chart.js, 586 of the 793 such misses are `this.`/`super.` instance calls, 53 are TypeScript overloads (several declarations, reported as ambiguous), and most of the rest are calls through bindings the binder treats as possibly reassigned. The oracle is not ground truth, and these two repositories do not represent every JavaScript code base.

## Known gaps

- Instance and prototype dispatch, object-literal members and CommonJS are the largest sources of unconfirmed JavaScript calls.
- A write to `obj[name(...)]` marks every identifier on its left side as reassigned, so `name` loses its confirmed calls. This over-invalidation is deliberate (it never adds a wrong edge) and also applies to TypeScript.
- See [grammar limitations](TYPESCRIPT_GRAMMAR_LIMITATIONS.md) and [semantic scope](SEMANTICS.md).
