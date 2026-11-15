# Static analysis accuracy (0.1.3 source builds)

What the binder confirms beyond 0.1.2, where it deliberately stops, and how that was measured. Everything here is static: no indexed code is executed, and a confirmed edge names a declaration, not an observed runtime call.

## Instance members: `this.m` and `super.m`

A `this.m` or `super.m` use (call or reference) resolves only when all of these hold:

1. **Bound `this`.** The nearest enclosing function that is not an arrow function is a method, field initializer or static block of a top-level class declaration Repogi indexes. A `function` (expression or declaration), an object-literal method, a class nested in a function, an anonymous class or class expression, and an aliased `this` (`const self = this`) do not qualify (`receiver_not_bound_to_indexed_class`).
2. **Known hierarchy.** Every `extends` on the way resolves to an indexed class, a package import or a global (`Error`). A dynamic superclass (`extends mixin(Base)`) or a binding that is not a class stops resolution (`dynamic_superclass`). Chains are walked at most 32 deep and cycles never resolve (`class_hierarchy_cyclic_or_too_deep`).
3. **A declared method.** The first class on the lookup path that declares `m` (instance or static, matching the context) wins: the context class for `this`, its superclass for `super`. Nothing declared up to the root or an unindexed superclass is `unresolved`. Private `#m` is looked up in the context class only. Body-less members (abstract methods, overload signatures) are never targets; getters and setters are references, not callable.
4. **No override.** For `this`, every indexed subclass of the context class (transitive, at most 512) is checked: if any declares `m`, the site is `ambiguous` (`overridden_in_subclass`) and lists no single target. `super.m` is not virtual and needs no override check.
5. **No shadowing own property.** For `this`, no class above or below the context class declares a field `m`, a string/number-named member `m`, or writes `this.m = …` (`own_property_may_shadow_method`). `this.m = this.m.bind(this)` is the one exception: it keeps the same method. A class with `this[k] = …`, a computed member name, or `Object.assign(this, …)`/`Object.defineProperty(this, …)`/`Reflect.*(this, …)` is `dynamic_own_members`.
6. **Untouched prototypes.** A `.prototype` that is written, deleted or passed on (`C.prototype.m = f`, `Object.assign(C.prototype, …)`, `const p = C.prototype`) marks that class; `__proto__`, `setPrototypeOf` or `.prototype` on a non-identifier marks every class in the module (`prototype_may_be_modified`). Reading a prototype member (`Base.prototype.m.call(this)`, `a === C.prototype.m`) does not.
7. **No untracked member write.** For `this`, a named member write anywhere in the repository whose receiver might be an instance (`obj.m = …`, `this.x.m = …`, `this.m = …` outside a class) withholds every `this.m` (`member_assigned_elsewhere`). So does a declaration of `m` in any class whose superclass is dynamic (`possible_override_in_dynamic_subclass`).

Resolved instance-member edges use the rules `this_member_static_declaration` and `super_member_static_declaration`, and carry `evidence.dispatch = "static_declaration"`. In retrieval output their confidence is `static_declaration` instead of `verified_static`. Treat them as "the declaration this code statically selects in this repository", not as proof of the runtime callee.

**Open-world limits (documented, not modeled):** subclasses outside the repository (an exported class extended by a consumer), `fn.call(other)` / `apply` / detached method calls, computed writes on arbitrary objects (`obj[k] = …`), properties set by an unindexed superclass's constructor, and mutation through helper functions that receive the instance (`merge(this, opts)`) are invisible to static analysis. Static `C.m()` keeps its 0.1.2 behavior plus one new check: a write `C.m = …` through any binding of `C` in any module now withholds `C.m()`.

## CommonJS

`require` is the CommonJS loader only in a `.cjs` file, or a `.js`/`.jsx` file without `import`/`export` statements that is not under a `package.json` with `"type": "module"` (nearest `package.json` inside the repository; a missing `type` means CommonJS). `.mjs` and TypeScript files never treat `require` as the loader, and neither does a file that declares, imports, takes as a parameter or assigns a name `require`.

| Form | Result |
| --- | --- |
| `const x = require("./lit")` | `x` is that module's `module.exports`. The specifier follows the JavaScript importer rules ([module resolution](MODULE_RESOLUTION.md#javascript-importers)). |
| `const { a, b: c } = require("./lit")`, `const y = require("./lit").b` | Each name is that export. Defaults (`{ a = 1 }`), rest elements, computed keys and nested patterns stay plain locals. |
| `require(expr)`, template literals, escaped strings | Not followed: the binding is a plain local. |
| A package, an unindexed file, an `.mjs`/TypeScript/ES module target | The `IMPORTS` edge is `unsupported` with the reason. A plain use of `x` still names its local declaration, as in 0.1.2. |

An export table is built only from unconditional top-level statements, in source order:

- `module.exports = { a, b: c, "d": e, m() {} }` replaces the table. Identifier values resolve; other values exist but have no declaration (`commonjs_export_value_not_a_declaration`).
- `module.exports = f` / `= require("./x")` sets the exported value (members of a class value resolve as static members; a re-exported module's names are followed, at most 64 deep).
- `exports.a = …`, `exports["a"] = …` and `module.exports.a = …` add names while `exports` still aliases `module.exports`. After `module.exports = …` or `exports = …`, writes to `exports` are not exports (Node.js semantics).
- `Object.defineProperty(exports, "__esModule", …)` is ignored.
- Anything else that could change exports at run time makes every lookup `unresolved` (`dynamic_commonjs_exports`): writes inside functions, conditionals or loops, computed keys, spreads, chained `module.exports = exports = …`, `exports`/`module.exports` passed to any call (`Object.assign(module.exports, …)`, `factory(exports)`), and a declaration that shadows `module` or `exports`.

ES module imports of a CommonJS module from a JavaScript file resolve as Node.js and bundlers expose them (`esm_import_of_commonjs`): `default` is `module.exports`, named imports use the same table. TypeScript importers are unchanged (their interop depends on compiler options). `require()` of an ES module stays `unsupported`: whether it works depends on the Node.js version. CommonJS modules get `EXPORTS` edges (`commonjs_export`). Package entry points (`main`, `exports`) remain unmodeled.

## Invalidation

0.1.2 treated every identifier on the left side of an assignment as reassigned, so `res[parseEdge(x)] = true` made every call to `parseEdge` unsupported. Now:

- Only rebinding a name (`x = …`, `x++`, destructuring targets, `for (x in …)`) marks it reassigned.
- A member write (`x.m = …`, `x[k] = …`, `x.m++`) invalidates only member uses of `x` for that property (`member_reassigned`), or all of them for a computed key. `x()`, `new x()` and plain references to `x` stay confirmed. `x.a.b = …` does not invalidate `x.a`.
- Identifiers inside a computed key, a member's object and a destructuring default are reads.
- Unknown assignment-target shapes keep the old whole-subtree rule.

## Index compatibility

The cached syntax IR is version 3. The first `repogi index` after upgrading reparses every source file once, and the graph equals a fresh index (checked byte for byte on the fixtures). Read-only commands (`status`, `graph`, `context`, `doctor`, MCP tools, `memory list/show/verify`) never migrate; `doctor` reports `index was built by an older extractor` until re-indexed. `.repogi/memory.db` is never touched by indexing: an approved memory backed by a 0.1.2 static edge stayed byte-identical and `evidence current` after the upgrade. Downgrading is the same in reverse: 0.1.2 sees IR 3, reparses every file and produces its own graph exactly. Interrupted impact-snapshot builds of any IR version are now cleaned up (previously only the current version's were).

## Evaluation

**Labeled fixtures** (`fixtures/instance-dispatch`, `fixtures/commonjs`; `crates/cli/tests/static_accuracy.rs`): every resolved edge must match a hand-written label including the target declaration's line, every label must resolve (133 resolved labels), and 65 unconfirmed sites must keep their status and rule. Labels were written from the source before comparing with Repogi output. The first comparison found two Repogi bugs, fixed in code with the labels unchanged (a mixin superclass classed as external; writes to a dead `exports` alias treated as dynamic), and one rule-order refinement (a field reports `own_property_may_shadow_method` rather than `member_not_declared_in_class_hierarchy`). Label additions made afterwards (omitted sites where Repogi was correct, and two new fixture lines) are listed under `corrections` in `fixtures/commonjs/EXPECTED.json`; no label was changed to match an output. A third bug (the receiver `x` of `x.f()` was no longer recorded as a use when `x` holds a `require()` result) was missed by the first labels, which omitted those sites; the edge-by-edge comparison with 0.1.2 on real repositories found it, and the receiver labels were added after the fix. The same test checks that incremental re-indexing after editing a superclass, an export table and a `package.json` type equals a fresh index, and bounds adversarial inputs (a 200-class chain, a self-extending class, comments and strings that look like code).

**Real repositories against TypeScript 6.0.3** (`scripts/evaluation/javascript_graph.py`, unchanged method plus one check): TypeScript's language service is asked for the definition at every resolved JavaScript edge and every unresolved call, and, new in this release, for the *implementations* at every resolved instance-member edge (any in-corpus implementation other than the claimed one would be an override that dispatch could select). A positive control on the fixture confirms the implementation query reports overrides. Same pinned commits, same oracle, both binaries, Linux x86_64, CPU governor `performance`, 2026-11-15:

| Repository (commit) | Version | Resolved JS edges checked | Disagree | Instance edges with another implementation | Call recall estimate |
| --- | --- | ---: | ---: | --- | ---: |
| [react-redux-realworld-example-app](https://github.com/gothinkster/react-redux-realworld-example-app) (`ee72eba`) | 0.1.2 | 361 | 0 | – | 0.64 |
| | 0.1.3 | 361 | 0 | 0 of 0 | 0.64 |
| [Chart.js](https://github.com/chartjs/Chart.js) (`71d4421`) | 0.1.2 | 2,646 | 1 | – | 0.54 |
| | 0.1.3 | 3,763 | 0 | 0 of 1,015 | 0.84 |
| [Express](https://github.com/expressjs/express) (`43fca52`) | 0.1.2 | 3,666 | 1,000 | – | 0.13 |
| | 0.1.3 | 3,706 | 790 | 0 of 0 | 0.15 |
| [Koa](https://github.com/koajs/koa) (`7c6bfe7`) | 0.1.2 | 1,528 | 497 | – | 0.07 |
| | 0.1.3 | 1,544 | 485 | 0 of 0 | 0.07 |

Every disagreement in both versions was classified: Repogi names the local declaration that holds a `require()` result (mostly `require("../..")`, a package root this release does not follow) or an alias (`const request = context.request`), where TypeScript follows the value into another file. These name a real declaration and are not wrong edges, but they are not counted as agreement either. No other disagreement class occurred.

Compared edge by edge, 0.1.3 lost **no** resolved edge that 0.1.2 confirmed on any of the four repositories. Gains: Chart.js 979 `this.` and 36 `super.` edges plus 133 recovered by narrower invalidation; Express 164 CommonJS and 124 invalidation edges; Koa 47. 253 edges changed target, all from a local `const x = require(…)` to the declaration the `require` loads.

Recall barely moves on Express and Koa: they are written as `exports.name = function () {}` and `app.method = function () {}` on prototype objects, which have no declaration facts for a binder to target. RealWorld's one `this.renderTabs()` miss is correctly ambiguous: a subclass overrides it.

**Performance** (median of 5 interleaved runs per binary; cold index / warm re-index / one-file edit / median query / peak RSS / index size):

| Repository | 0.1.2 | 0.1.3 |
| --- | --- | --- |
| RealWorld | 2.73 s / 0.32 s / 0.65 s / 4 ms / 25 MB / 3.7 MB | 2.68 s / 0.31 s / 0.61 s / 4 ms / 26 MB / 3.7 MB |
| Chart.js | 5.14 s / 0.43 s / 6.63 s / 42 ms / 135 MB / 83.4 MB | 6.66 s / 0.49 s / 6.89 s / 41 ms / 148 MB / 85.0 MB |
| Express | 3.60 s / 0.28 s / 3.22 s / 4 ms / 71 MB / 31.0 MB | 3.85 s / 0.23 s / 3.46 s / 4 ms / 74 MB / 31.7 MB |
| Koa | 5.02 s / 0.22 s / 2.15 s / 3 ms / 34 MB / 14.8 MB | 3.26 s / 0.34 s / 2.41 s / 3 ms / 37 MB / 15.4 MB |

Chart.js cold indexing is 29% slower at the median. The binder's own phase timing (`CODE_INTEL_PROFILE=1`) attributes about 15 ms to the new rules (rebind 166 → 181 ms); most of the variance is the SQLite commit, which ranged from 1.6 s to 3.6 s between two runs of the same 0.1.2 binary on this disk. Peak memory is up to 10% higher.

Static-graph accuracy and agent task outcomes are separate questions: see [agent outcomes](AGENT_OUTCOMES.md) for the latter, which has no live results yet.
