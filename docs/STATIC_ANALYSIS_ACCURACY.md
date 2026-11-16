# Static analysis accuracy (0.1.3)

What the binder confirms beyond 0.1.2, where it deliberately stops, and how that was measured. Everything here is static: no indexed code is executed, and a confirmed edge names a declaration, not an observed runtime call.

0.1.3 is the first release after 0.1.2 and consolidates three internal development milestones that were never published: instance members, CommonJS and narrower invalidation (internal "0.1.3", merged as PR #9); callable values, object and prototype tables, alias origins and `package.json` entry points (internal "0.1.4", PR #10); and constructed class instances, computed property keys and workspace packages (this release's final milestone). Section labels and measurement tables keep the internal milestone names under which results were recorded (for example "0.1.4 internal"), so every number stays traceable to the binary that produced it.

## Instance members: `this.m` and `super.m`

A `this.m` or `super.m` use (call or reference) resolves only when all of these hold:

1. **Bound `this`.** The nearest enclosing function that is not an arrow function is a method, field initializer or static block of a class Repogi indexes: a top-level class declaration or, in JavaScript, a class expression in a supported top-level slot (`module.exports = class App {}`, `exports.X = class {}`, `const X = class {}`; see [callable values](#callable-values)). A `function` (expression or declaration), an object-literal method, a prototype method (`F.prototype.m = function () {}`), a class nested in a function, and an aliased `this` (`const self = this`) do not qualify (`receiver_not_bound_to_indexed_class`).
2. **Known hierarchy.** Every `extends` on the way resolves to an indexed class, a package import or `require()` of a package, or a global (`Error`). A dynamic superclass (`extends mixin(Base)`) or a binding that is not a class stops resolution (`dynamic_superclass`). Chains are walked at most 32 deep and cycles never resolve (`class_hierarchy_cyclic_or_too_deep`).
3. **A declared method.** The first class on the lookup path that declares `m` (instance or static, matching the context) wins: the context class for `this`, its superclass for `super`. Nothing declared up to the root or an unindexed superclass is `unresolved`. Private `#m` is looked up in the context class only. Body-less members (abstract methods, overload signatures) are never targets; getters and setters are references, not callable.
4. **No override.** For `this`, every indexed subclass of the context class (transitive, at most 512) is checked: if any declares `m`, the site is `ambiguous` (`overridden_in_subclass`) and lists no single target. `super.m` is not virtual and needs no override check.
5. **No shadowing own property.** For `this`, no class above or below the context class declares a field `m`, a string/number-named member `m`, or writes `this.m = …` (`own_property_may_shadow_method`). `this.m = this.m.bind(this)` is the one exception: it keeps the same method. A class with `this[k] = …` or a computed member name whose key is not proven ([computed keys](#computed-property-keys), 0.1.3), or with `Object.assign(this, …)`/`Object.defineProperty(this, …)`/`Reflect.*(this, …)`, is `dynamic_own_members`.
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
- `exports = module.exports = f` (internal 0.1.4) sets the value and keeps `exports` aliased to it, so later `exports.a = …` statements are named exports. `const p = module.exports = { … }` and `var p = exports = module.exports = { … }` (0.1.4) export the object declared as `p`, whose members come from its [object table](#object-tables).
- `exports.a = …`, `exports["a"] = …` and `module.exports.a = …` add names while `exports` still aliases `module.exports`. After `module.exports = …` or `exports = …`, writes to `exports` are not exports (Node.js semantics).
- Values (internal 0.1.4): an identifier names its declaration; an inline function, arrow function or class expression names its own [callable fact](#callable-values) (`exports.f = function () {}`, `module.exports = { m() {} }`); `x.member` names that member of `x`'s value; other values exist but have no declaration (`commonjs_export_value_not_a_declaration`).
- `Object.defineProperty(exports, "__esModule", …)` is ignored.
- Anything else that could change exports at run time makes every member lookup `unresolved` (`dynamic_commonjs_exports`): writes inside functions, conditionals or loops, computed keys, spreads, chained `module.exports = exports = …`, `exports`/`module.exports` passed to any call (`Object.assign(module.exports, …)`, `factory(exports)`), `module` passed or assigned as a value (`factory(module)`, internal 0.1.4), and a declaration that shadows `module` or `exports`. When every reason only affects members (computed keys, spreads, escapes of the exports object, nested member writes), the exported value itself is still known, so a whole-module `require()` resolves to it (internal 0.1.4); reasons that may replace `module.exports` (a nested `module.exports = …`, rebinding or passing `module`, chained assignments, shadowing) leave it unresolved too.
- A write through another module's `require()` binding (`const s = require("./svc"); s.validate = other;`, or a computed `s[k] = …`) withholds that export everywhere (`member_reassigned`, internal 0.1.4). Internal 0.1.3 builds ignored such writes, as for read-only ES namespaces, and could confirm `validate` for other importers.

ES module imports of a CommonJS module from a JavaScript file resolve as Node.js and bundlers expose them (`esm_import_of_commonjs`): `default` is `module.exports`, named imports use the same table. TypeScript importers are unchanged (their interop depends on compiler options). `require()` of an ES module stays `unsupported`: whether it works depends on the Node.js version. CommonJS modules get `EXPORTS` edges (`commonjs_export`). Package entry points: see [module resolution](MODULE_RESOLUTION.md#package-entry-points).

## Callable values

Internal 0.1.4, JavaScript files only. Function-valued slots get a source-located declaration fact, so the binder has an implementation to name:

| Source | Fact (kind, name, location) |
| --- | --- |
| `a.b = function () {}`, `= function f() {}`, `= () => {}`, `= function* () {}` at top level (the last assignment of a chain) | `function` `a.b`, the assignment `a.b = …` |
| `module.exports = { m() {}, k: function () {}, a: () => {} }` | `method` `module.exports.m` (the method), `function` `module.exports.k` (the `key: value` pair) |
| `const x = { m() {}, k: () => {} }` at top level (also `x = module.exports = { … }`) | `method` `x.m`, `function` `x.k` |
| `module.exports = class App {}`, `exports.X = class {}`, `const X = class {}`, a class value in such an object literal | `class` named after the class, else the slot; its methods as `method` facts |

Getters and setters get no fact (they are accessors, not callable values). A function fact's signature is the text before the function body. Nested and conditional code is not scanned: its values have no stable declaration identity. The symbol records the expression form (`function_expression`, `arrow_function`, `object_method`, `class_expression`). TypeScript files are unchanged.

A fact locates the implementation; it does not make the slot immutable. In `let h = function original() {}; h = other; h()`, the declarator still locates `original`, and the call stays unconfirmed because `h` is reassigned. Whether a use resolves to a fact is decided by the binding, object, prototype and export rules below.

## Object tables

Internal 0.1.4, JavaScript only. A top-level `const`/`let`/`var x = { … }` (also `x = module.exports = { … }` and `x = exports = module.exports = { … }`) gets a table of statically known properties: literal members, then unconditional top-level `x.name = value` statements in source order. `x.m` resolves to the property's value (its fact, or the declaration an identifier, `y.member` or `require()` value names) with the rule `object_property_static_declaration` when all of these hold:

1. **The binding is stable.** `x` is never rebound (`unsupported_or_mutable_binding_or_receiver`) and `x.m` is not written outside the table statements (`member_reassigned`).
2. **The table is static.** No spread, computed key or `__proto__` key in the literal, and `x` is used only as a member receiver, a table statement target or an exported value (`module.exports = x`, `exports.n = x`, `module.exports = { x }`). Passing or assigning `x` anywhere, a computed or nested member write, `delete x.m`, and `x[k] = …` make every property `unresolved` (`dynamic_object_members`).
3. **One definition.** A property defined twice (literal and statement, or two statements) is `object_property_redefined`: which value a call sees depends on execution order.
4. **No untracked writes.** No write through any other binding that resolves to `x` (including importers of the module that exports it; `member_reassigned`), and no write of a member named `m` on a receiver Repogi cannot identify (`member_assigned_elsewhere`, the instance-member rule).
5. **A value.** A getter or setter is `object_property_is_accessor`; a non-declaration value (`name: 'svc'`) is `object_property_value_not_a_declaration`; a missing property is `missing_object_property` (it may come from `Object.prototype`).

`this.m` inside object-literal methods stays unresolved: the receiver depends on how the method is called.

## Prototype tables

Internal 0.1.4, JavaScript only. A top-level function declaration `F` gets a prototype table from unconditional top-level `F.prototype.name = value` statements. A use `x.m` resolves to that value with `prototype_method_static_declaration` and `evidence.dispatch = "static_declaration"` only when `x` is a binding initialized with `new F(…)` (`const x = new F()`), `x` is not rebound and `x.m` is not written, `F` resolves to that function declaration, and:

- the prototype is not replaced or changed elsewhere (`F.prototype = …`, a nested or conditional `F.prototype.m = …`, a prototype use as a value, a prototype change in another module, opaque `__proto__`/`setPrototypeOf` in the module; `prototype_may_be_modified`);
- `F` does not escape (`util.inherits(F, …)`, `mixin(F)`, `const G = F`; `prototype_may_be_modified`); calls, `new F()`, member reads, comparisons and exports are fine;
- the module gives `this` no computed own members (`this[k] = …`, `Object.assign(this, …)`; `dynamic_own_members`);
- no write of a member named `m` on an unidentified receiver, including `this.m = …` in the constructor (`member_assigned_elsewhere`);
- `m` is defined exactly once (`object_property_redefined`) and declared at all (`member_not_declared_on_prototype`);
- `x` does not escape (0.1.3, see [constructed instances](#constructed-instances); `receiver_escapes`).

Open-world limits are those of instance members: `fn.call(other)` and consumers outside the repository.

## Constructed instances

0.1.3, JavaScript and TypeScript. `x.m` where `x` is initialized with `new C(…)` resolves with the rule `constructed_instance_static_declaration` and `evidence.dispatch = "static_declaration"` when all of these hold:

1. **A known class.** `C` (or `ns.C`) statically resolves to an indexed class through the ordinary binding rules: a local class, an ES import, a `require()`, a namespace member or an [alias](#alias-origins) (`const S = Session; new S()`). Anything else keeps the earlier behavior; a binding that may hold several constructors (`const C = cond ? A : B`) reports `constructor_not_a_known_class`.
2. **A stable receiver.** `x` is not rebound (`let x = new C(); x = other` is unsupported) and `x.m` is not written (`member_reassigned`).
3. **No escape** (`receiver_escapes`). Every use of `x` is a member access, a call of `x`, a comparison, `typeof`, or `const y = x`, whose uses then count too (a write through `y` is a write through `x`). Passing `x` to any function, returning, storing, assigning, spreading or exporting it lets code Repogi does not follow add or replace own properties (`Object.assign(x, …)`, `mixin(x)`), so the site stays unconfirmed. `const x = new C(); helper(x); x.m()` is unconfirmed even when `helper` is harmless.
4. **The whole chain is static.** For `C` and every indexed class above it: the prototype is not modified (`prototype_may_be_modified`), the constructor does not return a value other than `this` (`constructor_may_return_object`: `return createReplacement()` or `return new Proxy(this, …)` may hand out another object), there are no dynamic own members (`dynamic_own_members`, see [computed keys](#computed-property-keys)), and no field, string-named member or `this.m = …` writes `m` as an own property (`own_property_may_shadow_method`). A dynamic superclass anywhere on the chain (`extends mixin(Base)`) is `dynamic_superclass`.
5. **A declared instance method.** The first class on the chain that declares an instance method `m` wins. Static methods are not instance members: `x.create()` for a `static create()` is `member_not_declared_in_class_hierarchy`, as is a name nothing declares; reaching an unindexed superclass is `inherited_from_unindexed_class`. Getters and setters are references, not callable; abstract and overload signatures are never targets.
6. **No untracked write** of a member named `m` anywhere in the repository (`member_assigned_elsewhere`, the instance-member rule).

Unlike `this.m`, the receiver is exactly a `C`: `new C()` never creates a subclass instance, so overrides in subclasses do not matter (`const s = new Shape(); s.area()` names `Shape.area` even when `Circle` overrides it). TypeScript type annotations play no part: `const c: Base = new Derived()` uses `Derived`, and `let c: C = new C(); c = make()` stays unconfirmed. Open-world limits are those of instance members: own properties set by an unindexed superclass's constructor, prototype changes made by code outside the repository, `x.m.call(other)`, and mutation through `this` inside the class's own methods (`helper(this)`), which `this.m` resolution does not model either.

## Computed property keys

0.1.3. A computed write `this[k] = …` or a computed class member `[k]() {}` previously made every member name possible (`dynamic_own_members`). The key is now classified, and only keys whose identity the source proves are narrowed:

| Key | Classified as |
| --- | --- |
| `"name"` (no escapes) | the string key `name` |
| `0`, `42` | the key `"0"`, `"42"` (canonical decimal integers below 2^53 only; `01`, `1.5`, `1e3`, `0x10`, `1_000` stay unknown) |
| `Symbol.iterator` and the other well-known symbols | a symbol, while `Symbol` is the global: not shadowed or assigned in the module |
| `Symbol.for("literal")` | a registered symbol, under the same `Symbol` rule |
| `k` with `const k = Symbol(…)` (or an unreassigned `let`) | a unique symbol |
| `util.inspect.custom`, `inspect.custom` | a symbol only when the binding is an unwritten `require()`/`import` of `util` or `node:util`, the specifier resolves to no repository file (no `paths`, `baseUrl`, self-reference or workspace package claims it), and no untracked receiver writes `custom` |
| anything else (`key`, `getKey()`, `Symbol.for(name)`, a shadowed `Symbol`, `getUtil().inspect.custom`) | unknown: `dynamic_own_members` as before |

A symbol key cannot equal any string-named member, so it does not invalidate `this.m` or `x.m`; a string or integer key is an own property of that name. Member writes on other receivers use the same string and integer rule (`x[0] = …` invalidates `x.0`, not every member of `x`); a string with escapes (`x["\u0061"]`) is now unknown everywhere, where earlier builds took its raw text as the name. The bare `util` specifier is taken to be the Node.js core module, which Node.js loads before any `node_modules` package of that name; a bundler that substitutes a browser polyfill is outside this profile. Prototype tables keep the earlier rule (any computed `this[k]` write in the module).

**Koa.** `Application`'s constructor runs `this[util.inspect.custom] = this.inspect` with `const util = require('node:util')`. That one write had withheld every `this.m` inside the class. It is now a proven symbol key, so `this.m` in `Application` and `app.m()` after `const app = new Koa()` follow the ordinary rules; all other safeguards still apply, and receivers of unknown type (`ctx.throw()` in middleware) stay unresolved.

## Composition helpers

0.1.3 evaluated helper-based composition and does not model it. In the pinned repositories it appears as `mixin(app, EventEmitter.prototype, false)` in Express (`merge-descriptors`) and `delegate(proto, 'response').method('attachment')` in Koa (`delegates`). Both helpers are packages, so their behavior is not in indexed source and cannot be verified; their names prove nothing. A local helper would need its exact identity, a body built only from statically named property copies, known target and source objects, and no conditional or computed behavior; no such call exists in the four repositories, and no top-level `Object.assign(x, { … })` onto an object table occurs there either. So every helper call keeps its earlier effect: the object or prototype escapes, and the affected members stay unconfirmed. Generalized `mixin`/`delegate`/`extend`/`assign` recognition remains future work.

## Alias origins

Internal 0.1.4, JavaScript only. `const v = w` and `const v = w.p` record where the value came from. When that origin resolves to an implementation (a function, method or class fact, a callable variable such as `const f = () => {}`), a module or an object table, uses of `v` resolve to it; the evidence chain keeps the local declaration (`v = w.p (local declaration variable:…)`). The origin is read once, at the declaration: later writes to `w.p` do not change `v`, and later writes to `v` make it reassigned. If the origin is anything else (a parameter, a call result, an accessor, a redefined property), `v` keeps naming its local declaration as before, and member uses (`v.m()`) report the origin's reason. Alias walks stop at 64 hops and cut cycles (`var x = y; var y = x`). In 0.1.3, `const y = x` where `x = new C()` makes `y.m()` a member use of the same instance ([constructed instances](#constructed-instances)); the chain records `y = x (alias of the instance)`.

## Invalidation

0.1.2 treated every identifier on the left side of an assignment as reassigned, so `res[parseEdge(x)] = true` made every call to `parseEdge` unsupported. Now:

- Only rebinding a name (`x = …`, `x++`, destructuring targets, `for (x in …)`) marks it reassigned.
- A member write (`x.m = …`, `x[k] = …`, `x.m++`) invalidates only member uses of `x` for that property (`member_reassigned`), or all of them for a computed key. `x()`, `new x()` and plain references to `x` stay confirmed. `x.a.b = …` does not invalidate `x.a`.
- Identifiers inside a computed key, a member's object and a destructuring default are reads.
- Unknown assignment-target shapes keep the old whole-subtree rule.

## Index compatibility

The cached syntax IR is version 5 (published 0.1.2 wrote 2; the internal 0.1.3 and 0.1.4 builds wrote 3 and 4). The IR number is independent of the release number. There is one upgrade path: the first `repogi index` after installing 0.1.3 reparses every source file once, whichever of those versions built the index, and the graph equals a fresh 0.1.3 index. No SQLite schema migration is involved; new facts are ordinary rows. Read-only commands (`status`, `graph`, `context`, `investigate`, `doctor`, MCP tools, `memory list/show/verify`) never migrate; `doctor` reports `index was built by an older extractor` until re-indexed. `.repogi/memory.db` is never touched by indexing. Downgrading is the same in reverse: an older binary sees IR 5, reparses every file and produces its own graph exactly. Interrupted impact-snapshot builds of any IR version are cleaned up. Workspace declarations are part of the resolution fingerprint, not the IR: editing them rebinds without reparsing.

Checked on `fixtures/javascript-intelligence` with an approved memory backed by a static edge, from the published 0.1.2 binary (IR 2, the release asset checked against its `SHA256SUMS`), the internal 0.1.3 build (IR 3) and the internal 0.1.4 build (IR 4): read-only commands of the candidate left `index.db` and `memory.db` byte-identical; `index` moved IR to 5 and reparsed all 30 files; `memory.db` stayed byte-identical through the upgrade and a downgrade; the memory verified `approved` and `current`; the upgraded graph equalled a fresh index; `impact` reported the changed declarations of an edit made afterwards; each downgraded graph equalled that binary's fresh index.

## Evaluation

### Labeled fixtures

`crates/cli/tests/static_accuracy.rs` checks five hand-labeled fixtures exactly: every resolved edge must match a label including the target declaration's line, every label must resolve, and every unconfirmed site must keep its status and rule.

| Fixture | Resolved labels | Unconfirmed sites | Covers |
| --- | ---: | ---: | --- |
| `fixtures/instance-dispatch` | 53 | 31 | `this`/`super`, overrides, shadowing, prototype changes, dynamic superclasses |
| `fixtures/commonjs` | 83 | 31 | `require`, export tables, CommonJS/ESM interop, dynamic exports |
| `fixtures/javascript-intelligence` (internal 0.1.4) | 182 | 75 | callable values, object and prototype tables, class expressions, alias origins, `main`, self-reference `exports` and conditions, consumer writes, cycles, shadowing, reassignment, a TypeScript control |
| `fixtures/receiver-intelligence` (0.1.3) | 136 | 25 | `new C()` from local, imported, required, aliased and namespace constructors; instance aliases; overrides; static-only, missing and accessor members; reassigned, factory-made, escaped (argument, alias argument, export) and member-written receivers; conditional constructors; constructors returning objects; own fields; prototype writes; dynamic superclasses; well-known, registered, `const` and `util.inspect.custom` symbol keys; integer keys; unknown keys; shadowed `Symbol` and `util`; the Koa constructor pattern; a TypeScript control |
| `fixtures/workspace-packages` (0.1.3) | 37 | 8 | [workspace packages](WORKSPACE_INTELLIGENCE.md) from ES module, CommonJS and TypeScript importers: `exports` subpaths, an unexported subpath, `main`, ES import of CommonJS, `require()` of ES modules, a duplicate name, undeclared and unsatisfied dependencies, a registry package, `types` conditions, a `dist/` entry, a cross-package `new Session()` |

Labels were written from the source text and Node.js semantics before Repogi indexed a fixture, and are never changed to match an output; `corrections` in each `EXPECTED.json` lists every later label change. In internal 0.1.4 three `commonjs` labels moved from unconfirmed to resolved (`exports.b = function () {}` now has a callable fact), recorded as an intended semantic change.

The first comparison of the new fixture found three Repogi defects, fixed in code with the labels unchanged: `require("..")` (bare `..`) was treated as a package name; a class extending `require("events")` was classed as a dynamic superclass instead of an external one (an ES `import` of the same package was already external); and a conditional `F.prototype.run = …` withheld every unrelated `obj.run()` through the global "assigned elsewhere" rule, although it can only change `F`'s prototype. Four more were found while writing the labels, before indexing: alias origins ignored callable variables, an object's own `x.m = …` statements invalidated `x.m`, `browser`/`module` entries reported a misleading rule, and a replaced prototype had no table to report it. A probe found the internal 0.1.3 hole described under [CommonJS](#commonjs) (consumer writes), now a negative label.

For the 0.1.3 release both new fixtures matched their labels on the first comparison, with no label changes (`corrections` is empty). The adversarial workspace test found one Repogi defect: an unsupported workspace declaration dropped every member identity, so imports of member names looked like registry packages (`package_or_unmapped_specifier`) instead of reporting the workspace problem; members that can be read now keep their identities (the import still never resolves). The first real-repository run found a second, described under [workspace packages on Chart.js](#workspace-packages-on-chartjs).

The same test file checks that incremental re-indexing equals a fresh index after edits that cross modules (a superclass, an export table, a package type, a consumer write removed, an `exports` subpath moved, an object no longer escaping; in 0.1.3 also an unknown computed key becoming a symbol, a receiver that stops escaping, and manifest-only edits: a renamed workspace package, a `!` exclusion removing a duplicate, a dependency range that now selects the member), and bounds adversarial inputs: a 200-class chain, a self-extending class, 400 chained aliases, `main` redirect cycles, a symlinked `package.json` pointing outside the root (refused, and nothing outside the root appears in evidence), a malformed `package.json`, `exports` nested 60 deep, a 3,000-member literal and a `__proto__` key; in 0.1.3 also symlinked and malformed workspace members, unsupported glob and YAML syntax, out-of-root patterns, 300 patterns, conflicting npm and pnpm declarations, traversal in package names and `exports` targets, the pnpm root project, and a re-export cycle across two workspace packages.

### Real repositories against TypeScript 6.0.3

`scripts/evaluation/javascript_graph.py` asks TypeScript's language service (with `allowJs`; it only reads files) for the definition at every resolved JavaScript edge, at every unresolved call with a static name, and for the implementations at every edge that carries `evidence.dispatch`. Alias-origin edges are also checked at each alias initializer in their evidence chain, because TypeScript names the local alias at the use. Same pinned commits and oracle as before; the binaries are a local build of the 0.1.2 source (its resolved edges equal the published 0.1.2 asset's on all four repositories), the internal 0.1.3 build of `main` at `83a07c0`, the internal 0.1.4 build (rc2, sha256 `c93aa559…`, the source merged as `37b3fcd`) and the 0.1.3 release candidate (rc2, sha256 `fde802c8…`). Linux x86_64, Node.js 24.18.0, 2026-11-16. For the 0.1.3 release the 0.1.2 and internal 0.1.4 rows were re-measured with the same script on the same day and reproduced the earlier numbers exactly; the script gained one separate section for constructed receivers in TypeScript files (below) and is otherwise unchanged.

Recall is shown two ways. *Internal 0.1.3 method* counts a miss only when TypeScript names a declaration the evaluated binary indexes, so its denominator grows when a binary indexes more declarations: RealWorld's falls although confirmed calls rose from 7 to 29. *Fixed model* counts a miss whenever TypeScript names any in-corpus definition that is not an import alias (an absent package such as `supertest`) or a parameter, which does not depend on the binary.

| Repository (commit) | Version | Resolved JS edges checked | Disagree | Confirmed calls | Call recall, internal 0.1.3 method | Call recall, fixed model |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| [react-redux-realworld-example-app](https://github.com/gothinkster/react-redux-realworld-example-app) (`ee72eba`) | 0.1.2 | 361 | 0 | 7 | 0.64 | 0.09 |
| | 0.1.3 internal | 361 | 0 | 7 | 0.64 | 0.09 |
| | 0.1.4 internal | 405 | 0 | 29 | 0.47 | 0.37 |
| | **0.1.3 release** | 405 | 0 | 29 | 0.47 | 0.37 |
| [Chart.js](https://github.com/chartjs/Chart.js) (`71d4421`) | 0.1.2 | 2,646 | 1 | 930 | 0.54 | 0.41 |
| | 0.1.3 internal | 3,763 | 0 | 1,442 | 0.84 | 0.64 |
| | 0.1.4 internal | 3,763 | 0 | 1,442 | 0.84 | 0.64 |
| | **0.1.3 release** | 3,763 | 0 | 1,442 | 0.84 | 0.64 |
| [Express](https://github.com/expressjs/express) (`43fca52`) | 0.1.2 | 3,666 | 1,000 | 277 | 0.13 | 0.24 |
| | 0.1.3 internal | 3,706 | 790 | 330 | 0.15 | 0.28 |
| | 0.1.4 internal | 4,706 | 240 | 1,028 | 0.46 | 0.88 |
| | **0.1.3 release** | 4,706 | 240 | 1,028 | 0.46 | 0.88 |
| [Koa](https://github.com/koajs/koa) (`7c6bfe7`) | 0.1.2 | 1,528 | 497 | 14 | 0.07 | 0.02 |
| | 0.1.3 internal | 1,544 | 485 | 14 | 0.07 | 0.02 |
| | 0.1.4 internal | 2,005 | 2 | 355 | 0.42 | 0.52 |
| | **0.1.3 release** | 2,537 | 2 | 612 | 0.72 | 0.90 |

These are estimates for four repositories against one oracle, not guarantees for other code bases.

Every disagreement was classified; none names a wrong declaration (the 0.1.3 release has exactly the internal 0.1.4 disagreements):

| Repository | Count | Repogi names | TypeScript names |
| --- | ---: | --- | --- |
| Express | 192 | `createApplication`, following `module.exports = require("./lib/express")` from the package root | the first hop, `module.exports` in `index.js` |
| Express | 32 | `Router = require("router")`, the local holding a package | the export `exports.Router = Router` |
| Express | 10 | `methods = require("./utils").methods` (the export's value is not a declaration) | `exports.methods = …` |
| Express | 6 | `app = exports = module.exports = {}`, the declared exported object | the module |
| Koa | 2 | `createHttpError = require("http-errors")`, the local holding a package | `module.exports.createHttpError = …` |

The 0.1.3 disagreement class (Repogi names the local declaration holding a `require()` result where TypeScript follows it) shrank from 790 to 42 on Express and from 485 to 2 on Koa, because package roots, chained exports and class expressions are now followed. The instance-dispatch check found no other in-corpus implementation for any of Chart.js's 1,015 `this.`/`super.` edges, as in 0.1.3. No prototype-dispatch edge occurs in these repositories (Express extends prototypes through `mixin`, which this release does not follow). Koa's six alias-origin edges (`const request = context.request` in tests) agree with TypeScript at the use.

Compared edge by edge, internal 0.1.4 lost **no** resolved edge of 0.1.2 or of the internal 0.1.3 build on any of the four repositories, and every edge in a TypeScript file is identical to 0.1.3 (8,728 on Chart.js). Gains over 0.1.3: Express 922, Koa 462, RealWorld 44 (object-literal members), Chart.js 1. Retargeted: Express 912 (780 from a local `require()` holder to the implementing function, 127 from a module to its declared object, 5 to a function) and Koa 492 (328 to functions, 157 to the class expression `module.exports = class Application`, 7 to a declared object).

What still missed in internal 0.1.4: on Koa, methods called on receivers whose type is not statically known (`ctx.throw()`, `app.use()` after `new Koa()`), and `this.m` inside `Application`, withheld because its constructor writes `this[util.inspect.custom]` (`dynamic_own_members`). On Express, local functions and properties inside function bodies. On Chart.js, the internal 0.1.3 classes (overrides, overloads, possibly reassigned bindings).

**0.1.3 release.** Edge by edge, the release candidate loses and retargets **no** resolved edge of internal 0.1.4 (or of 0.1.2) on any of the four repositories. Express and RealWorld graphs are identical to internal 0.1.4, and Chart.js's JavaScript graph is too. All gains are on Koa: 532 new resolved edges, 520 with `constructed_instance_static_declaration` (252 calls, all in `__tests__` after `const app = new Koa()`: `use` 126, `callback` 114, `onerror` 5, `listen` 5, `createContext` and `toJSON`) and 12 more `this.m` edges inside `Application`, both made possible by the [`util.inspect.custom` key](#computed-property-keys). Every one agrees with TypeScript at the use (oracle disagreements stay at the two classified above), and TypeScript's implementation search finds no other in-corpus implementation for any of Koa's 532 dispatch edges. Koa's fixed-model call recall rises from 0.52 to 0.90; its 66 remaining misses are, by TypeScript's declaration kind, 41 methods, 18 functions, 3 constants, 3 local functions and 1 property. By Repogi's rule, the 242 misses of the internal-0.1.3 method are 200 receivers Repogi cannot type (mostly `ctx`, `request` and `response` parameters in middleware: `unsupported_or_mutable_binding_or_receiver`), 35 `this` uses in object-literal methods (`receiver_not_bound_to_indexed_class`), 3 instances passed to a helper (`receiver_escapes`), 2 dynamic CommonJS exports and 2 missing bindings. No TypeScript file in the four repositories has a `new C()` receiver that resolves, so the separate TypeScript check had nothing to verify there; the labeled TypeScript controls cover that path.

#### Workspace packages on Chart.js

Chart.js is the only pinned repository with a workspace (`pnpm-workspace.yaml`: `docs`, `test/integration/*`, plus the root package `chart.js`). Its integration packages declare `"chart.js": "workspace:*"`, and the root `exports` points every condition into `dist/`, which is never indexed. So imports of `chart.js` there still do not resolve, but 39 TypeScript-file edges now say why (`module_in_excluded_directory`, `module_resolves_to_unindexed_file`) instead of `package_or_unmapped_specifier`; nothing resolved changed. The first candidate (rc1) did lose 13 resolved `this.m` edges in Chart.js: `docs/scripts/log2.js` extends `Scale` from `'chart.js'` without declaring the dependency, the new rule `workspace_dependency_not_declared` was no longer recognized as a package superclass, and the class counted as a dynamic subclass that could override `configure`, `draw`, `parse` and `getPixelForValue` anywhere. An unfollowed workspace import now keeps the earlier package-superclass classification (rc2), with a regression test; as for any package superclass, overrides in such subclasses are an open-world limit. rc1's results are kept with the evaluation records.

### Performance

Wall-clock indexing on the evaluation machine varied by up to a factor of eight between identical runs (the SQLite commit on its disk dominates), so it cannot show differences of this size and is not reported. CPU time (user + system) of a cold `repogi index` is stable to a few percent and measures the extraction and binding work; peak memory, index size and query latency come from a separate interleaved run (5 repetitions, rotating order). All three binaries are local builds with the same toolchain; medians, 2026-11-16:

Internal 0.1.4 measurement (2026-11-16, kept as recorded):

| Repository | Cold-index CPU s (0.1.2 / 0.1.3 internal / 0.1.4 internal) | Peak RSS MB | Index MB | Median query |
| --- | --- | --- | --- | --- |
| RealWorld | 0.096 / 0.101 / 0.108 | 25.4 / 25.6 / 26.6 | 3.70 / 3.74 / 3.81 | 4 / 4 / 6 ms |
| Koa | 0.277 / 0.325 / 0.362 | 33.9 / 37.2 / 38.7 | 14.8 / 15.4 / 15.9 | 3 / 3 / 3 ms |
| Express | 0.589 / 0.681 / 0.777 | 71.1 / 73.5 / 76.0 | 31.0 / 31.7 / 32.5 | 4 / 4 / 4 ms |
| Chart.js | 1.715 / 2.072 / 2.171 | 134.6 / 147.4 / 150.3 | 83.4 / 85.0 / 85.1 | 57 / 52 / 52 ms |

Internal 0.1.4 cost 5–14% more CPU than the internal 0.1.3 build and 13–32% more than 0.1.2 (7 interleaved repetitions; spreads under 10% except one 0.1.2 Koa outlier). The binder's resolve phase (`CODE_INTEL_PROFILE=1`) grew from 33 to 43 ms on Koa and 87 to 105 ms on Express; the rest is extraction (object, prototype and escape scans over each JavaScript file). Peak memory is up to 4% above 0.1.3 and 14% above 0.1.2; the index is up to 3% larger than 0.1.3. Query latency is unchanged except RealWorld (4 to 6 ms), which has more declarations to rank.

**0.1.3 release.** A separate interleaved run (5 repetitions, rotating order, same toolchain, nothing else running; `results-rel013/cpu.json` and `perf.json` in the evaluation records) compares 0.1.2, internal 0.1.4 and the final candidate. Compare numbers only within one run: the same internal 0.1.4 binary measured 0.896 s on Express here and 0.777 s in the run above.

| Repository | Cold-index CPU s (0.1.2 / 0.1.4 internal / 0.1.3) | Spread of 0.1.3 runs | Peak RSS MB | Index MB | Median query |
| --- | --- | ---: | --- | --- | --- |
| RealWorld | 0.101 / 0.113 / 0.112 | 11% | 25.4 / 25.7 / 26.2 | 3.70 / 3.81 / 3.81 | 5 / 7 / 7 ms |
| Koa | 0.295 / 0.373 / 0.381 | 4% | 34.0 / 38.1 / 38.9 | 14.8 / 15.9 / 16.3 | 3 / 4 / 3 ms |
| Express | 0.686 / 0.896 / 0.903 | 6% | 71.0 / 75.6 / 76.3 | 31.0 / 32.5 / 32.6 | 4 / 4 / 4 ms |
| Chart.js | 1.821 / 2.204 / 2.452 | 22% | 134.7 / 149.9 / 151.1 | 83.4 / 85.1 / 85.2 | 41 / 41 / 41 ms |

Against internal 0.1.4, cold-index CPU is within 1–2% on RealWorld, Koa and Express. Chart.js's median is 11% higher, but its runs spread by 22–24% for both binaries and the fastest runs differ by 1.5% (2.159 s and 2.192 s), so this run cannot show a Chart.js difference either way; the binder's resolve phase there went from 190 to 181 ms. New work is bounded: escape analysis compares only same-named bindings, workspace patterns are capped at 32 segments and two `**`, and the class chain walk is capped as before. Peak memory is 0.5–2% above internal 0.1.4, the index up to 2.6% larger (Koa, which has 532 more edges), and query latency unchanged. Wall-clock indexing times (warm, one-file edit) varied more than these differences on this disk and are not reported.

Static-graph accuracy and agent task outcomes are separate questions: see [agent outcomes](AGENT_OUTCOMES.md) for the latter, which has no live results yet.
