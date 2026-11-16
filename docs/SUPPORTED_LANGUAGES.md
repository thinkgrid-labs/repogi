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

- Declarations: functions, generators, classes and their methods, top-level `const`/`let`/`var`, with source paths, byte offsets and line ranges. In JavaScript (0.1.4) also callable values in top-level slots: `a.b = function () {}`/`() => {}`, `F.prototype.m = …`, members of `module.exports = { … }` and `const x = { … }`, and class expressions such as `module.exports = class App {}` ([callable values](STATIC_ANALYSIS_ACCURACY.md#callable-values)).
- ES modules: named, default and namespace imports, import aliases, `export` declarations, `export default`, export lists, re-exports (`export { a as b } from`, `export { default as x } from`, `export * from`).
- Static uses: direct calls, namespace members (`ns.f()`), static class members (`C.m()`), `extends`, identifier references.
- Instance members (0.1.4): `this.m()` and `super.m()` inside methods of indexed classes, to the declaration the class hierarchy statically selects, withheld when a subclass overrides it or anything may shadow it ([static analysis accuracy](STATIC_ANALYSIS_ACCURACY.md#instance-members-thism-and-superm)).
- CommonJS (0.1.4): literal `require()` bindings (whole, destructured, `.member`), unconditional top-level `module.exports`/`exports.name` tables including `exports = module.exports = f`, and ES module imports of them ([CommonJS](STATIC_ANALYSIS_ACCURACY.md#commonjs)).
- Objects, prototypes and aliases (0.1.4): `x.m()` for static object tables, `x.m()` for `const x = new F()` with a static `F.prototype` table, and `const v = obj.m` alias origins ([object tables](STATIC_ANALYSIS_ACCURACY.md#object-tables), [prototype tables](STATIC_ANALYSIS_ACCURACY.md#prototype-tables), [alias origins](STATIC_ANALYSIS_ACCURACY.md#alias-origins)).
- Local package entry points (0.1.4): directory imports through `package.json` `main` (including `require("..")`) and self-reference through exact `exports` subpaths ([package entry points](MODULE_RESOLUTION.md#package-entry-points)).
- JSX in `.jsx` and in `.js` files: component references (local, imported, default, from `.tsx`), never calls. Lower-case tags are intrinsic elements.
- Mixed repositories: JavaScript importing TypeScript (including TypeScript output names such as `./a.js` for `a.ts`) and TypeScript importing JavaScript, `.mjs`→`.mts`, `.cjs`→`.cts`.
- `jsconfig.json`/`tsconfig.json` `paths` and `baseUrl`, as for TypeScript.
- Incremental indexing, change impact and advisory test discovery for JavaScript sources and tests.

Never confirmed (reported as `unresolved`, `unsupported` or `ambiguous`):

- Dynamic CommonJS: `require(expr)`, packages in `node_modules`, `require()` of ES modules, and exports written conditionally, inside functions, through computed keys or passed to calls.
- Dynamic `import()`, computed members, objects that escape or are mutated outside their table statements, registries, dynamic JSX components (`const C = map[kind]; <C />`).
- Instance dispatch that static analysis cannot pin down: overridden methods (reported ambiguous), `this` in plain functions, object-literal or prototype methods, or aliased; dynamic superclasses, own-property shadowing, prototype changes and same-named member writes elsewhere; `new C()` receivers of classes.
- `package.json` `browser`/`module` entries, `*` export patterns, conditions other than `require`/`import`/`default`, `imports`, and packages imported by name from another package; Node.js runtime resolution is not modeled.
- Bindings that are reassigned anywhere in their scope, and any name shadowed by a parameter or local declaration. A member write (`x.m = …`) invalidates only `x.m` uses.
- Files with syntax errors (for example Flow type annotations): their intact declarations are kept, but they produce no use edges.

Minified and generated JavaScript (`*.min.js`, `*.generated.mjs` and the same for the other new extensions, plus files starting with `// @generated`) is skipped like generated TypeScript.

## Evaluation

**Labeled fixture.** `fixtures/javascript/EXPECTED.json` holds hand-reviewed labels for every resolved edge in a mixed JavaScript/TypeScript fixture (69 resolved edges, 14 sites that must stay unconfirmed). `crates/cli/tests/javascript.rs` requires an exact match: no false confirmed edge, no missed label.

**Instance dispatch, CommonJS and 0.1.4 JavaScript analysis.** `fixtures/instance-dispatch`, `fixtures/commonjs` and `fixtures/javascript-intelligence` hold hand-reviewed labels (318 resolved edges, 137 sites that must stay unconfirmed, with expected rules), checked exactly by `crates/cli/tests/static_accuracy.rs`.

**Real repositories against an independent oracle.** `scripts/evaluation/javascript_graph.py` indexes a copy of a repository and asks the TypeScript 6.0.3 language service (with `allowJs`; it only reads files) for the definition at every resolved edge in a JavaScript file, at every unresolved call whose callee is a plain name, and for the implementations at every resolved `this.`/`super.` edge. 0.1.4 results on four repositories, compared with 0.1.2 and the internal 0.1.3 build on the same commits: [static analysis accuracy](STATIC_ANALYSIS_ACCURACY.md#evaluation). The original 0.1.2 measurements (single runs, one Linux desktop with a power-saving CPU governor, 2026-11-01):

| Repository (commit) | Files | Cold / warm index | Index size | Resolved JS edges agreeing with TypeScript | Call recall estimate |
| --- | ---: | --- | ---: | --- | ---: |
| [react-redux-realworld-example-app](https://github.com/gothinkster/react-redux-realworld-example-app) (`ee72eba`) | 38 | 0.08 s / 0.02 s | 3.7 MB | 361 / 361 | 0.64 |
| [Chart.js](https://github.com/chartjs/Chart.js) (`71d4421`) | 745 | 1.74 s / 0.06 s | 83 MB | 2,645 / 2,646 | 0.54 |

The one Chart.js disagreement was reviewed: Repogi resolves a use in `karma.conf.cjs` to the local `const` that holds a `require()` result, while TypeScript follows the `require` into the other file. Both describe the code; Repogi's edge is not wrong.

Recall counts only call sites where TypeScript names a declaration Repogi indexes. In Chart.js, 586 of the 793 such misses are `this.`/`super.` instance calls, 53 are TypeScript overloads (several declarations, reported as ambiguous), and most of the rest are calls through bindings the binder treats as possibly reassigned. The oracle is not ground truth, and these two repositories do not represent every JavaScript code base.

In 0.1.4 the Chart.js estimate is 0.84 (1,015 instance-member edges, none with another in-corpus implementation) with no oracle disagreement; Express and Koa rise from 0.13 and 0.07 to 0.46 and 0.42, and a recall measure that does not depend on which declarations a binary indexes rises from 0.24 to 0.88 (Express) and from 0.02 to 0.52 (Koa) ([details](STATIC_ANALYSIS_ACCURACY.md#real-repositories-against-typescript-603)).

## Known gaps

- Methods called on receivers whose type is not statically known (`ctx.throw()`, `app.use()` after `const app = new Koa()` for a class) stay unconfirmed; so do `this.m` calls inside object-literal and prototype methods.
- Objects extended by helpers (`mixin(app, proto)`, `delegate(proto, …)`) are treated as escaped, so their members stay unconfirmed.
- Only functions, classes and objects in top-level slots have facts; values built inside functions do not.
- See [grammar limitations](TYPESCRIPT_GRAMMAR_LIMITATIONS.md) and [semantic scope](SEMANTICS.md).
