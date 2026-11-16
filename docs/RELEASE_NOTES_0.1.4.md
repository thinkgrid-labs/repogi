# Repogi 0.1.4 release notes (draft)

> **Draft, not published.** 0.1.2 is the latest release. These notes describe the upgrade from 0.1.2 to 0.1.4 as one step. There is no 0.1.3 release: that work was an internal milestone merged into source and ships here.

## Upgrading from 0.1.2

- Install 0.1.4 and run `repogi index` once. Every source file is reparsed (syntax IR 2 → 4); no SQLite schema migration is needed, and the result equals a fresh index.
- Until you re-index, read-only commands (`status`, `graph`, `context`, `investigate`, `doctor`, MCP tools, `memory list/show/verify`) work on the old index and never migrate it. `repogi doctor` reports `index was built by an older extractor`.
- `.repogi/memory.db` is not touched. Approved memories stay byte-identical and keep verifying against the re-indexed graph.
- Downgrading works the same way in reverse: an older binary reparses once.
- CLI commands, MCP tool IDs (`atlas_*`) and JSON schemas are unchanged. Graph results for JavaScript change as described below; in TypeScript files they change only through the instance-member and invalidation rules (both apply to TypeScript too).

## JavaScript analysis

Everything here is static: no indexed code runs, and a confirmed edge names a declaration, not an observed runtime call. Details and every rule: [static analysis accuracy](STATIC_ANALYSIS_ACCURACY.md).

**Instance members** (also TypeScript). `this.m()` and `super.m()` inside class methods resolve to the declaration the class hierarchy statically selects, with `evidence.dispatch = "static_declaration"`. Overridden methods are reported as ambiguous; own-property shadowing, prototype changes, dynamic superclasses and same-named writes elsewhere keep a site unconfirmed.

**CommonJS.** Literal `require()` (whole, destructured, `.member`), unconditional top-level `module.exports`/`exports.name` tables, `exports = module.exports = f`, and ES module imports of CommonJS modules. Conditional, computed, escaping and shadowed exports stay unconfirmed. A write through another module's `require()` binding now withholds that export.

**Narrower invalidation** (also TypeScript). Writing `x.m` invalidates only `x.m` uses; identifiers inside computed keys (`res[parseEdge(x)] = true`) are reads.

**Callable values.** Function-valued assignments (`exports.f = function () {}`, `F.prototype.m = () => {}`), members of `module.exports = { … }` and `const x = { … }`, and class expressions in top-level slots (`module.exports = class App {}`) are now declarations with source locations, so search, navigation and impact see them and calls can resolve to them.

**Objects and prototypes.** `x.m()` resolves for a static object table, and for `const x = new F()` when `F.prototype.m` is assigned once at top level, unless the object or constructor escapes, is mutated elsewhere or defines the member twice.

**Alias origins.** `const v = obj.m` and `const w = v` resolve to the original implementation; the evidence chain keeps the local declaration.

**Local package entry points.** Directory imports use `package.json` `main` (including `require("..")`), and a package's own name resolves through exact `exports` subpaths with `require`/`import`/`default` conditions. `browser`/`module` fields, patterns, other conditions and `node_modules` stay unconfirmed. `package.json` is read as data only, never through symlinks or outside the repository.

## Measured on real repositories

Against TypeScript 6.0.3 on four pinned repositories (estimates for these repositories, not guarantees): no resolved edge of 0.1.2 was lost; Express oracle disagreements fell from 1,000 to 240 and Koa's from 497 to 2, all classified as naming a real declaration one hop from TypeScript's answer; call recall, measured independently of which declarations a version indexes, rose from 0.24 to 0.88 on Express, 0.02 to 0.52 on Koa and 0.09 to 0.37 on RealWorld, and from 0.41 to 0.64 on Chart.js. Indexing CPU time is 5–14% above the internal 0.1.3 build and 13–32% above 0.1.2: [performance](STATIC_ANALYSIS_ACCURACY.md#performance).

## Agent outcomes

No live agent runs have been authorized, so these notes make no claim about task success, tokens, time or cost. A second offline-validated task suite for CommonJS code (`nodekit-v1`) is included ([agent outcomes](AGENT_OUTCOMES.md)).

## Known limits

Receivers of unknown type (`ctx.throw()`, `app.use()` on a class instance), `this` inside object-literal and prototype methods, objects extended through helpers (`mixin`, `delegate`), values built inside functions, packages in `node_modules` and dynamic `import()` stay unconfirmed. Python, Rust and other languages are not indexed.
