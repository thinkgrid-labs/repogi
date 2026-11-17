# Repogi 0.1.3 release notes (draft)

> **Draft, not published.** 0.1.2 is the latest release. 0.1.3 is the next public release and the only step from 0.1.2: it consolidates every change developed since, including work recorded in engineering reports under the internal milestone names "0.1.3" and "0.1.4". There are no 0.1.4 or 0.1.5 releases, and no intermediate binary needs to be installed.

## Upgrading from 0.1.2

- Install 0.1.3 directly over 0.1.2 (`curl -fsSL https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.sh | sh -s -- --version 0.1.3`, or rerun the installer once 0.1.3 is the latest release), then run `repogi index` once in each project.
- Versions: the syntax IR moves from 2 to 5. The index database schema (5), the durable memory schema (1), the memory export format (1), the document IR (2) and the relationship IR (1) are unchanged. These numbers are independent of the release number.
- Re-indexing reparses every source file (syntax IR 2 → 5); no SQLite schema migration is needed, and the result equals a fresh index.
- Until you re-index, read-only commands (`status`, `graph`, `context`, `investigate`, `doctor`, MCP tools, `memory list/show/verify`) work on the old index and never migrate it. `repogi doctor` reports `index was built by an older extractor`.
- `.repogi/memory.db` is not touched. Approved memories stay byte-identical and keep verifying against the re-indexed graph.
- Downgrading works the same way in reverse: an older binary reparses once.
- CLI commands, MCP tool IDs (`atlas_*`) and JSON schemas are unchanged. Graph results for JavaScript change as described below; in TypeScript files they change through the instance-member, constructed-receiver, computed-key, invalidation and workspace rules, which apply to TypeScript too.

These steps were checked from the published 0.1.2 binary and from both internal builds, with an approved memory: [index compatibility](STATIC_ANALYSIS_ACCURACY.md#index-compatibility).

## JavaScript and TypeScript analysis

Everything here is static: no indexed code runs, and a confirmed edge names a declaration, not an observed runtime call. Details and every rule: [static analysis accuracy](STATIC_ANALYSIS_ACCURACY.md).

**Instance members** (also TypeScript). `this.m()` and `super.m()` inside class methods resolve to the declaration the class hierarchy statically selects, with `evidence.dispatch = "static_declaration"`. Overridden methods are reported as ambiguous; own-property shadowing, prototype changes, dynamic superclasses and same-named writes elsewhere keep a site unconfirmed.

**Class instances** (also TypeScript). `const x = new C(); x.m()` resolves to the method `C`'s prototype chain declares, when `C` statically names an indexed class (local, imported, required, aliased, `new ns.C()`) and nothing can change what `x.m` is: `x` is not reassigned, written or passed to other code (`helper(x)` keeps `x.m()` unconfirmed), no constructor on the chain returns another object, no class on it has same-named or dynamic own members or a modified prototype, and no dynamic superclass is involved. `const y = x` aliases are followed. Rule `constructed_instance_static_declaration`.

**Computed property keys** (also TypeScript). `this[Symbol.iterator] = …`, `this[util.inspect.custom] = …` (for `require("node:util")`/`util`), `Symbol.for("literal")` and `const k = Symbol()` keys are recognized as symbols that cannot collide with method names, and integer keys (`x[0]`) as their string names. Unknown keys still make every name possible, and a shadowed `Symbol` or a `util` that is not the built-in module is unknown. This is what lets Koa's `Application` methods resolve.

**CommonJS.** Literal `require()` (whole, destructured, `.member`), unconditional top-level `module.exports`/`exports.name` tables, `exports = module.exports = f`, and ES module imports of CommonJS modules. Conditional, computed, escaping and shadowed exports stay unconfirmed. A write through another module's `require()` binding withholds that export.

**Narrower invalidation** (also TypeScript). Writing `x.m` invalidates only `x.m` uses; identifiers inside computed keys (`res[parseEdge(x)] = true`) are reads.

**Callable values.** Function-valued assignments (`exports.f = function () {}`, `F.prototype.m = () => {}`), members of `module.exports = { … }` and `const x = { … }`, and class expressions in top-level slots (`module.exports = class App {}`) are declarations with source locations, so search, navigation and impact see them and calls can resolve to them.

**Objects and prototypes.** `x.m()` resolves for a static object table, and for `const x = new F()` when `F.prototype.m` is assigned once at top level, unless the object or constructor escapes, is mutated elsewhere or defines the member twice.

**Alias origins.** `const v = obj.m` and `const w = v` resolve to the original implementation; the evidence chain keeps the local declaration.

## Packages and monorepos

**Local package entry points.** Directory imports use `package.json` `main` (including `require("..")`), and a package's own name resolves through exact `exports` subpaths with `require`/`import`/`default` conditions.

**Workspace packages.** In npm, Yarn and pnpm workspaces declared at the repository root (`package.json` `workspaces`, `pnpm-workspace.yaml`), an import of a member package by name resolves to its source when the importer's `package.json` declares the dependency with a version the member satisfies (`workspace:*`, `*`, or the member's exact version). Entry points follow `exports` or `main`; TypeScript importers resolve only when every `moduleResolution` mode agrees. Duplicate names, undeclared or unsatisfied dependencies, symlinked or malformed members, unsupported patterns and entries in `dist/` are reported with specific rules, never guessed. Cross-package references, impact and test candidates follow. Details: [workspace packages](WORKSPACE_INTELLIGENCE.md).

`package.json` and `pnpm-workspace.yaml` are read as data only, never through symlinks or outside the repository; nothing is installed and no package manager runs. `browser`/`module` fields, `exports` patterns and other conditions, and `node_modules` stay unconfirmed.

## Measured

Against TypeScript 6.0.3 on four pinned repositories (estimates for these repositories, not guarantees), compared with 0.1.2: no resolved edge of 0.1.2 was lost; call recall, measured independently of which declarations a version indexes, rose from 0.24 to 0.88 on Express, 0.02 to 0.90 on Koa, 0.09 to 0.37 on RealWorld and 0.41 to 0.64 on Chart.js; oracle disagreements fell from 1,000 to 240 on Express and from 497 to 2 on Koa, all classified as naming a real declaration one hop from TypeScript's answer. Five hand-labeled fixtures (491 resolved labels, 170 unconfirmed sites) have no wrong confirmed edge. Cold indexing uses 11–35% more CPU than 0.1.2 on these repositories (median of 5 interleaved runs; the Chart.js figure is within its run-to-run spread), with 3–14% more peak memory and an index 2–10% larger; query latency is unchanged: [performance](STATIC_ANALYSIS_ACCURACY.md#performance).

## Evaluation

- Labeled correctness fixtures: `instance-dispatch`, `commonjs`, `javascript-intelligence`, `receiver-intelligence` and `workspace-packages`, with labels written from source text before indexing and every later correction recorded. `crates/cli/tests/static_accuracy.rs` fails on any wrong confirmed edge.
- `scripts/evaluation/javascript_graph.py` compares resolved edges with TypeScript's language service, and reports call recall on a fixed denominator that does not depend on which declarations a version indexes, so versions can be compared fairly.
- Agent outcome harness: suite `nodekit-v1` for CommonJS code, alongside `ledgerlite-v1`.

## Agent outcomes

No live agent runs have been authorized, so these notes make no claim about task success, tokens, time or cost. The harness ships with two offline-validated task suites (`ledgerlite-v1`, and `nodekit-v1` for CommonJS code): [agent outcomes](AGENT_OUTCOMES.md).

## Known limits

Receivers of unknown type (`ctx.throw()` on a middleware parameter), instances passed to other functions, `this` inside object-literal and prototype methods, objects extended through helpers (`mixin`, `delegate`), values built inside functions, packages in `node_modules`, nested workspaces and dynamic `import()` stay unconfirmed. Subclasses outside the repository, or of a workspace package Repogi does not follow, are not seen. Python, Rust and other languages are not indexed. Windows is not supported yet.
