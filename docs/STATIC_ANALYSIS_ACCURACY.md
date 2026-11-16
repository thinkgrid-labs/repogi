# Static analysis accuracy (0.1.4)

What the binder confirms beyond 0.1.2, where it deliberately stops, and how that was measured. Everything here is static: no indexed code is executed, and a confirmed edge names a declaration, not an observed runtime call.

0.1.4 is the first release after 0.1.2. It contains the instance-member, CommonJS and invalidation work that was developed and merged as an internal milestone (0.1.3, never published), plus deeper JavaScript analysis: callable values, object and prototype tables, alias origins and `package.json` entry points. Sections below mark which part each rule comes from only where that matters for comparing measurements.

## Instance members: `this.m` and `super.m`

A `this.m` or `super.m` use (call or reference) resolves only when all of these hold:

1. **Bound `this`.** The nearest enclosing function that is not an arrow function is a method, field initializer or static block of a class Repogi indexes: a top-level class declaration or, in JavaScript, a class expression in a supported top-level slot (`module.exports = class App {}`, `exports.X = class {}`, `const X = class {}`; see [callable values](#callable-values)). A `function` (expression or declaration), an object-literal method, a prototype method (`F.prototype.m = function () {}`), a class nested in a function, and an aliased `this` (`const self = this`) do not qualify (`receiver_not_bound_to_indexed_class`).
2. **Known hierarchy.** Every `extends` on the way resolves to an indexed class, a package import or `require()` of a package, or a global (`Error`). A dynamic superclass (`extends mixin(Base)`) or a binding that is not a class stops resolution (`dynamic_superclass`). Chains are walked at most 32 deep and cycles never resolve (`class_hierarchy_cyclic_or_too_deep`).
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
- `exports = module.exports = f` (0.1.4) sets the value and keeps `exports` aliased to it, so later `exports.a = …` statements are named exports. `const p = module.exports = { … }` and `var p = exports = module.exports = { … }` (0.1.4) export the object declared as `p`, whose members come from its [object table](#object-tables).
- `exports.a = …`, `exports["a"] = …` and `module.exports.a = …` add names while `exports` still aliases `module.exports`. After `module.exports = …` or `exports = …`, writes to `exports` are not exports (Node.js semantics).
- Values (0.1.4): an identifier names its declaration; an inline function, arrow function or class expression names its own [callable fact](#callable-values) (`exports.f = function () {}`, `module.exports = { m() {} }`); `x.member` names that member of `x`'s value; other values exist but have no declaration (`commonjs_export_value_not_a_declaration`).
- `Object.defineProperty(exports, "__esModule", …)` is ignored.
- Anything else that could change exports at run time makes every member lookup `unresolved` (`dynamic_commonjs_exports`): writes inside functions, conditionals or loops, computed keys, spreads, chained `module.exports = exports = …`, `exports`/`module.exports` passed to any call (`Object.assign(module.exports, …)`, `factory(exports)`), `module` passed or assigned as a value (`factory(module)`, 0.1.4), and a declaration that shadows `module` or `exports`. When every reason only affects members (computed keys, spreads, escapes of the exports object, nested member writes), the exported value itself is still known, so a whole-module `require()` resolves to it (0.1.4); reasons that may replace `module.exports` (a nested `module.exports = …`, rebinding or passing `module`, chained assignments, shadowing) leave it unresolved too.
- A write through another module's `require()` binding (`const s = require("./svc"); s.validate = other;`, or a computed `s[k] = …`) withholds that export everywhere (`member_reassigned`, 0.1.4). 0.1.3 builds ignored such writes, as for read-only ES namespaces, and could confirm `validate` for other importers.

ES module imports of a CommonJS module from a JavaScript file resolve as Node.js and bundlers expose them (`esm_import_of_commonjs`): `default` is `module.exports`, named imports use the same table. TypeScript importers are unchanged (their interop depends on compiler options). `require()` of an ES module stays `unsupported`: whether it works depends on the Node.js version. CommonJS modules get `EXPORTS` edges (`commonjs_export`). Package entry points: see [module resolution](MODULE_RESOLUTION.md#package-entry-points).

## Callable values

0.1.4, JavaScript files only. Function-valued slots get a source-located declaration fact, so the binder has an implementation to name:

| Source | Fact (kind, name, location) |
| --- | --- |
| `a.b = function () {}`, `= function f() {}`, `= () => {}`, `= function* () {}` at top level (the last assignment of a chain) | `function` `a.b`, the assignment `a.b = …` |
| `module.exports = { m() {}, k: function () {}, a: () => {} }` | `method` `module.exports.m` (the method), `function` `module.exports.k` (the `key: value` pair) |
| `const x = { m() {}, k: () => {} }` at top level (also `x = module.exports = { … }`) | `method` `x.m`, `function` `x.k` |
| `module.exports = class App {}`, `exports.X = class {}`, `const X = class {}`, a class value in such an object literal | `class` named after the class, else the slot; its methods as `method` facts |

Getters and setters get no fact (they are accessors, not callable values). A function fact's signature is the text before the function body. Nested and conditional code is not scanned: its values have no stable declaration identity. The symbol records the expression form (`function_expression`, `arrow_function`, `object_method`, `class_expression`). TypeScript files are unchanged.

A fact locates the implementation; it does not make the slot immutable. In `let h = function original() {}; h = other; h()`, the declarator still locates `original`, and the call stays unconfirmed because `h` is reassigned. Whether a use resolves to a fact is decided by the binding, object, prototype and export rules below.

## Object tables

0.1.4, JavaScript only. A top-level `const`/`let`/`var x = { … }` (also `x = module.exports = { … }` and `x = exports = module.exports = { … }`) gets a table of statically known properties: literal members, then unconditional top-level `x.name = value` statements in source order. `x.m` resolves to the property's value (its fact, or the declaration an identifier, `y.member` or `require()` value names) with the rule `object_property_static_declaration` when all of these hold:

1. **The binding is stable.** `x` is never rebound (`unsupported_or_mutable_binding_or_receiver`) and `x.m` is not written outside the table statements (`member_reassigned`).
2. **The table is static.** No spread, computed key or `__proto__` key in the literal, and `x` is used only as a member receiver, a table statement target or an exported value (`module.exports = x`, `exports.n = x`, `module.exports = { x }`). Passing or assigning `x` anywhere, a computed or nested member write, `delete x.m`, and `x[k] = …` make every property `unresolved` (`dynamic_object_members`).
3. **One definition.** A property defined twice (literal and statement, or two statements) is `object_property_redefined`: which value a call sees depends on execution order.
4. **No untracked writes.** No write through any other binding that resolves to `x` (including importers of the module that exports it; `member_reassigned`), and no write of a member named `m` on a receiver Repogi cannot identify (`member_assigned_elsewhere`, the instance-member rule).
5. **A value.** A getter or setter is `object_property_is_accessor`; a non-declaration value (`name: 'svc'`) is `object_property_value_not_a_declaration`; a missing property is `missing_object_property` (it may come from `Object.prototype`).

`this.m` inside object-literal methods stays unresolved: the receiver depends on how the method is called.

## Prototype tables

0.1.4, JavaScript only. A top-level function declaration `F` gets a prototype table from unconditional top-level `F.prototype.name = value` statements. A use `x.m` resolves to that value with `prototype_method_static_declaration` and `evidence.dispatch = "static_declaration"` only when `x` is a binding initialized with `new F(…)` (`const x = new F()`), `x` is not rebound and `x.m` is not written, `F` resolves to that function declaration, and:

- the prototype is not replaced or changed elsewhere (`F.prototype = …`, a nested or conditional `F.prototype.m = …`, a prototype use as a value, a prototype change in another module, opaque `__proto__`/`setPrototypeOf` in the module; `prototype_may_be_modified`);
- `F` does not escape (`util.inherits(F, …)`, `mixin(F)`, `const G = F`; `prototype_may_be_modified`); calls, `new F()`, member reads, comparisons and exports are fine;
- the module gives `this` no computed own members (`this[k] = …`, `Object.assign(this, …)`; `dynamic_own_members`);
- no write of a member named `m` on an unidentified receiver, including `this.m = …` in the constructor (`member_assigned_elsewhere`);
- `m` is defined exactly once (`object_property_redefined`) and declared at all (`member_not_declared_on_prototype`).

Classes are unchanged: `const x = new C(); x.m()` still does not resolve. Open-world limits are those of instance members: instances passed to helpers that mutate them, `fn.call(other)`, and consumers outside the repository.

## Alias origins

0.1.4, JavaScript only. `const v = w` and `const v = w.p` record where the value came from. When that origin resolves to an implementation (a function, method or class fact, a callable variable such as `const f = () => {}`), a module or an object table, uses of `v` resolve to it; the evidence chain keeps the local declaration (`v = w.p (local declaration variable:…)`). The origin is read once, at the declaration: later writes to `w.p` do not change `v`, and later writes to `v` make it reassigned. If the origin is anything else (a parameter, a call result, an accessor, a redefined property), `v` keeps naming its local declaration as before, and member uses (`v.m()`) report the origin's reason. Alias walks stop at 64 hops and cut cycles (`var x = y; var y = x`).

## Invalidation

0.1.2 treated every identifier on the left side of an assignment as reassigned, so `res[parseEdge(x)] = true` made every call to `parseEdge` unsupported. Now:

- Only rebinding a name (`x = …`, `x++`, destructuring targets, `for (x in …)`) marks it reassigned.
- A member write (`x.m = …`, `x[k] = …`, `x.m++`) invalidates only member uses of `x` for that property (`member_reassigned`), or all of them for a computed key. `x()`, `new x()` and plain references to `x` stay confirmed. `x.a.b = …` does not invalidate `x.a`.
- Identifiers inside a computed key, a member's object and a destructuring default are reads.
- Unknown assignment-target shapes keep the old whole-subtree rule.

## Index compatibility

The cached syntax IR is version 4 (0.1.2 wrote 2; the internal 0.1.3 builds wrote 3). There is one upgrade path: the first `repogi index` after installing 0.1.4 reparses every source file once, whichever of those versions built the index, and the graph equals a fresh 0.1.4 index. No SQLite schema migration is involved; new facts are ordinary rows. Read-only commands (`status`, `graph`, `context`, `investigate`, `doctor`, MCP tools, `memory list/show/verify`) never migrate; `doctor` reports `index was built by an older extractor` until re-indexed. `.repogi/memory.db` is never touched by indexing. Downgrading is the same in reverse: an older binary sees IR 4, reparses every file and produces its own graph exactly. Interrupted impact-snapshot builds of any IR version are cleaned up.

Checked on `fixtures/javascript-intelligence` with an approved memory backed by a static edge, from both a 0.1.2 index (IR 2) and an internal 0.1.3 index (IR 3): read-only commands left `index.db` and `memory.db` byte-identical; `index` moved IR to 4 and reparsed all 30 files; `memory.db` stayed byte-identical through the upgrade and a downgrade; the memory verified `approved` and `current`; the upgraded graph equalled a fresh index; `impact` reported the changed declarations of an edit made afterwards; the downgraded graph equalled the old binary's fresh index.

## Evaluation

### Labeled fixtures

`crates/cli/tests/static_accuracy.rs` checks three hand-labeled fixtures exactly: every resolved edge must match a label including the target declaration's line, every label must resolve, and every unconfirmed site must keep its status and rule.

| Fixture | Resolved labels | Unconfirmed sites | Covers |
| --- | ---: | ---: | --- |
| `fixtures/instance-dispatch` | 53 | 31 | `this`/`super`, overrides, shadowing, prototype changes, dynamic superclasses |
| `fixtures/commonjs` | 83 | 31 | `require`, export tables, CommonJS/ESM interop, dynamic exports |
| `fixtures/javascript-intelligence` (0.1.4) | 182 | 75 | callable values, object and prototype tables, class expressions, alias origins, `main`, self-reference `exports` and conditions, consumer writes, cycles, shadowing, reassignment, a TypeScript control |

Labels were written from the source text and Node.js semantics before Repogi indexed a fixture, and are never changed to match an output; `corrections` in each `EXPECTED.json` lists every later label change. In 0.1.4 three `commonjs` labels moved from unconfirmed to resolved (`exports.b = function () {}` now has a callable fact), recorded as an intended semantic change.

The first comparison of the new fixture found three Repogi defects, fixed in code with the labels unchanged: `require("..")` (bare `..`) was treated as a package name; a class extending `require("events")` was classed as a dynamic superclass instead of an external one (an ES `import` of the same package was already external); and a conditional `F.prototype.run = …` withheld every unrelated `obj.run()` through the global "assigned elsewhere" rule, although it can only change `F`'s prototype. Four more were found while writing the labels, before indexing: alias origins ignored callable variables, an object's own `x.m = …` statements invalidated `x.m`, `browser`/`module` entries reported a misleading rule, and a replaced prototype had no table to report it. A probe found the 0.1.3 hole described under [CommonJS](#commonjs) (consumer writes), now a negative label.

The same test file checks that incremental re-indexing equals a fresh index after edits that cross modules (a superclass, an export table, a package type, a consumer write removed, an `exports` subpath moved, an object no longer escaping), and bounds adversarial inputs: a 200-class chain, a self-extending class, 400 chained aliases, `main` redirect cycles, a symlinked `package.json` pointing outside the root (refused, and nothing outside the root appears in evidence), a malformed `package.json`, `exports` nested 60 deep, a 3,000-member literal and a `__proto__` key.

### Real repositories against TypeScript 6.0.3

`scripts/evaluation/javascript_graph.py` asks TypeScript's language service (with `allowJs`; it only reads files) for the definition at every resolved JavaScript edge, at every unresolved call with a static name, and for the implementations at every edge that carries `evidence.dispatch`. Alias-origin edges are also checked at each alias initializer in their evidence chain, because TypeScript names the local alias at the use. Same pinned commits and oracle as before; the three binaries are a local build of the 0.1.2 source (its resolved edges equal the published 0.1.2 asset's on all four repositories), the internal 0.1.3 build of `main` at `83a07c0`, and the 0.1.4 candidate. Linux x86_64, Node.js 24.18.0, 2026-11-16.

Recall is shown two ways. *0.1.3 method* counts a miss only when TypeScript names a declaration the evaluated binary indexes, so its denominator grows when a binary indexes more declarations: RealWorld's falls although confirmed calls rose from 7 to 29. *Fixed model* counts a miss whenever TypeScript names any in-corpus definition that is not an import alias (an absent package such as `supertest`) or a parameter, which does not depend on the binary.

| Repository (commit) | Version | Resolved JS edges checked | Disagree | Confirmed calls | Call recall, 0.1.3 method | Call recall, fixed model |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| [react-redux-realworld-example-app](https://github.com/gothinkster/react-redux-realworld-example-app) (`ee72eba`) | 0.1.2 | 361 | 0 | 7 | 0.64 | 0.09 |
| | 0.1.3 internal | 361 | 0 | 7 | 0.64 | 0.09 |
| | 0.1.4 | 405 | 0 | 29 | 0.47 | 0.37 |
| [Chart.js](https://github.com/chartjs/Chart.js) (`71d4421`) | 0.1.2 | 2,646 | 1 | 930 | 0.54 | 0.41 |
| | 0.1.3 internal | 3,763 | 0 | 1,442 | 0.84 | 0.64 |
| | 0.1.4 | 3,763 | 0 | 1,442 | 0.84 | 0.64 |
| [Express](https://github.com/expressjs/express) (`43fca52`) | 0.1.2 | 3,666 | 1,000 | 277 | 0.13 | 0.24 |
| | 0.1.3 internal | 3,706 | 790 | 330 | 0.15 | 0.28 |
| | 0.1.4 | 4,706 | 240 | 1,028 | 0.46 | 0.88 |
| [Koa](https://github.com/koajs/koa) (`7c6bfe7`) | 0.1.2 | 1,528 | 497 | 14 | 0.07 | 0.02 |
| | 0.1.3 internal | 1,544 | 485 | 14 | 0.07 | 0.02 |
| | 0.1.4 | 2,005 | 2 | 355 | 0.42 | 0.52 |

These are estimates for four repositories against one oracle, not guarantees for other code bases.

Every disagreement was classified; none names a wrong declaration:

| Repository | Count | Repogi names | TypeScript names |
| --- | ---: | --- | --- |
| Express | 192 | `createApplication`, following `module.exports = require("./lib/express")` from the package root | the first hop, `module.exports` in `index.js` |
| Express | 32 | `Router = require("router")`, the local holding a package | the export `exports.Router = Router` |
| Express | 10 | `methods = require("./utils").methods` (the export's value is not a declaration) | `exports.methods = …` |
| Express | 6 | `app = exports = module.exports = {}`, the declared exported object | the module |
| Koa | 2 | `createHttpError = require("http-errors")`, the local holding a package | `module.exports.createHttpError = …` |

The 0.1.3 disagreement class (Repogi names the local declaration holding a `require()` result where TypeScript follows it) shrank from 790 to 42 on Express and from 485 to 2 on Koa, because package roots, chained exports and class expressions are now followed. The instance-dispatch check found no other in-corpus implementation for any of Chart.js's 1,015 `this.`/`super.` edges, as in 0.1.3. No prototype-dispatch edge occurs in these repositories (Express extends prototypes through `mixin`, which this release does not follow). Koa's six alias-origin edges (`const request = context.request` in tests) agree with TypeScript at the use.

Compared edge by edge, 0.1.4 lost **no** resolved edge of 0.1.2 or of the internal 0.1.3 build on any of the four repositories, and every edge in a TypeScript file is identical to 0.1.3 (8,728 on Chart.js). Gains over 0.1.3: Express 922, Koa 462, RealWorld 44 (object-literal members), Chart.js 1. Retargeted: Express 912 (780 from a local `require()` holder to the implementing function, 127 from a module to its declared object, 5 to a function) and Koa 492 (328 to functions, 157 to the class expression `module.exports = class Application`, 7 to a declared object).

What still misses: on Koa, almost all remaining misses are methods called on receivers whose type is not statically known (`ctx.throw()`, `app.use()` after `new Koa()`), and `this.m` inside `Application` stays withheld because its constructor writes `this[util.inspect.custom]` (`dynamic_own_members`). On Express, remaining misses are local functions and properties inside function bodies. On Chart.js, they are the 0.1.3 classes (overrides, overloads, possibly reassigned bindings).

### Performance

Wall-clock indexing on the evaluation machine varied by up to a factor of eight between identical runs (the SQLite commit on its disk dominates), so it cannot show differences of this size and is not reported. CPU time (user + system) of a cold `repogi index` is stable to a few percent and measures the extraction and binding work; peak memory, index size and query latency come from a separate interleaved run (5 repetitions, rotating order). All three binaries are local builds with the same toolchain; medians, 2026-11-16:

| Repository | Cold-index CPU s (0.1.2 / 0.1.3 internal / 0.1.4) | Peak RSS MB | Index MB | Median query |
| --- | --- | --- | --- | --- |
| RealWorld | 0.096 / 0.101 / 0.108 | 25.4 / 25.6 / 26.6 | 3.70 / 3.74 / 3.81 | 4 / 4 / 6 ms |
| Koa | 0.277 / 0.325 / 0.362 | 33.9 / 37.2 / 38.7 | 14.8 / 15.4 / 15.9 | 3 / 3 / 3 ms |
| Express | 0.589 / 0.681 / 0.777 | 71.1 / 73.5 / 76.0 | 31.0 / 31.7 / 32.5 | 4 / 4 / 4 ms |
| Chart.js | 1.715 / 2.072 / 2.171 | 134.6 / 147.4 / 150.3 | 83.4 / 85.0 / 85.1 | 57 / 52 / 52 ms |

0.1.4 costs 5–14% more CPU than the internal 0.1.3 build and 13–32% more than 0.1.2 (7 interleaved repetitions; spreads under 10% except one 0.1.2 Koa outlier). The binder's resolve phase (`CODE_INTEL_PROFILE=1`) grew from 33 to 43 ms on Koa and 87 to 105 ms on Express; the rest is extraction (object, prototype and escape scans over each JavaScript file). Peak memory is up to 4% above 0.1.3 and 14% above 0.1.2; the index is up to 3% larger than 0.1.3. Query latency is unchanged except RealWorld (4 to 6 ms), which has more declarations to rank.

Static-graph accuracy and agent task outcomes are separate questions: see [agent outcomes](AGENT_OUTCOMES.md) for the latter, which has no live results yet.
