# JSX component references

Repogi binds JSX component tags as reference sites in the static graph.

## Which tags are sites

The rule is TypeScript's own distinction between intrinsic and value-based elements:

| Tag | Site? | Why |
|---|---|---|
| `<Panel>`, `<Panel/>` | yes | Identifier that does not start with a lowercase ASCII letter |
| `<Icons.Star/>`, `<this.Item/>` | yes | Member expression: always value-based |
| `<div>`, `<svg>`, `<my-element>` | no | Lowercase-first or contains `-`: intrinsic |
| `<svg:path>` | no | Namespaced name: intrinsic |
| `</Panel>` | no | Closing tags are never sites, so `<Panel>…</Panel>` counts once |
| `<>…</>` | no | Fragment: no name |

For a member tag, the receiver is still a use, exactly as outside JSX. `<ThemeContext.Provider>` records a JSX site for `ThemeContext.Provider` and a plain REFERENCES to `ThemeContext` (when it names a non-namespace binding), so the provider keeps its dependency on the context. A namespace import receiver (`Icons` in `<Icons.Star/>`) is not a use of its own.

Attribute names record nothing. Attribute values (`data-x={Button}`) and children are ordinary code, so `{Button}` there is a plain REFERENCES edge and `{render()}` a CALLS edge. Before 3.7, element names were skipped (they sit in the element's `name` field, which the extractor treated like a declaration name). The exception was member tags, which leaked through as plain member references, once per opening and once per closing tag.

## Binding

A JSX site binds exactly like an identifier or member reference at that location:
- lexical scopes and shadowing;
- named, default, renamed and namespace imports;
- relative and `paths`/`baseUrl` modules;
- re-export chains and star barrels.

The scope-and-mutation rules of [SEMANTICS.md](SEMANTICS.md) apply unchanged. Two rules are specific to JSX:
- A resolved JSX site carries rule `jsx_component_reference`.
- A site that resolves only to an interface or type alias becomes `unsupported` with rule `jsx_target_is_a_type`, because a type cannot be rendered.

A type-only import (`import type { X }`) cannot establish a runtime use and stays `unsupported`. Every other outcome keeps the binder's status and rule:
- `package_or_unmapped_specifier` for package components;
- `ambiguous_module_candidates`;
- `module_not_found`;
- `unsupported_or_mutable_binding_or_receiver` for locals, mutated bindings and receivers.

Evidence on a JSX edge:
- `evidence.syntax` is `<Name> JSX element`.
- `target_text` is the tag name (`PrimaryButton`, `Icons.Star`), so unconfirmed same-name sites still group by name.
- The binding chain is the same as for a non-JSX reference, for example `App.tsx: Icons => ./components/Icons *` → `Icons.tsx: export Star`.

### Fixture: `fixtures/jsx`

[`fixtures/jsx/EXPECTED.json`](../fixtures/jsx/EXPECTED.json) holds hand-written labels. [`crates/cli/tests/jsx.rs`](../crates/cli/tests/jsx.rs) compares them as sets of (enclosing declaration, tag, line) → (file, declared name). Declared names are read from source text, not from Repogi.

| # | Scenario | Example | Expected |
|---|---|---|---|
| 1 | Direct relative import | `<Button />` | `components/Button.tsx` `Button` |
| 2 | `@/` alias | `<Panel>…</Panel>` | `widgets/Panel.tsx` `Panel` (once) |
| 3 | Named import alias | `<PrimaryButton></PrimaryButton>` | `Button` (once) |
| 4 | Default export | `<Card>` | `components/Card.tsx` `Card` |
| 5 | Barrel re-export of a default, renamed | `<BarrelCard />` via `@/components` | `Card` |
| 6 | Namespace-qualified | `<Icons.Star />` | `components/Icons.tsx` `Star` |
| 7 | Local shadowing | `const Shadowed = …; <Shadowed />` | unsupported: never the imported `Shadowed` |
| 8 | Nested JSX and closures | `<Card><span/></Card>`, `() => <Button />` in a hook | confirmed, inner tags separate |
| 9 | Intrinsic elements | `div`, `span`, `svg`, `my-element` | no edge |
| 10 | Unresolved package component | `<Toaster />` from `sonner` | unsupported, `package_or_unmapped_specifier` |
| 11 | Ambiguous binding | `./Ambig` with `Ambig.tsx` and `Ambig/index.tsx` | ambiguous, `ambiguous_module_candidates` |
| 12 | Type-only import | `import type { TypedThing }` | unsupported |
| 13 | A type as a tag | `<PanelProps />` (interface) | unsupported, `jsx_target_is_a_type` |
| 14 | JSX in a parameter default | `({ renderEmpty = () => <Card /> })` | `Card` |
| 15 | Context provider | `<ThemeContext.Provider>…</ThemeContext.Provider>` | JSX site unsupported (property of a `createContext` value); `ThemeContext` itself resolved as a plain reference, once |

The tests also check:
- **Count-independent scoring.** A name-matching binder that resolves more sites (3 → wrong targets) fails, even though its resolved count rises.
- **No stray edges.** Intrinsics, closing tags and attribute names add no edges, and no JSX site becomes CALLS.
- **IR upgrade.** An index whose cached IR predates JSX reparses once; MCP reports it as stale meanwhile.
- **Disable control.** `CODE_INTEL_DISABLE_JSX=1` changes only JSX sites. Every non-JSX edge is unchanged. The pre-3.7 graph differs by exactly the member-tag edges: opening tags reappear as plain REFERENCES with the same location, status, target and chain, and closing tags as duplicates.
- **MCP visibility.** `atlas_references` and upstream `atlas_dependencies` expose the sites.

Scenario 14 exposed a pre-existing binder bug. In `{ x = value }` (an `object_assignment_pattern`), every identifier in `value` was recorded as a local binding. The binder therefore treated `Empty` in `({ renderEmpty = () => <Empty /> })` as a parameter and reported it unsupported. Plain `x = value` patterns were already handled. The fix is in the binder, not JSX-specific. On Umami it changes 24 edges:
- adds 12 references inside default values that were previously skipped as bindings;
- removes 2 spurious `process` references;
- changes the status of 10, 2 of them JSX.

The bug never produced a wrong target, only false `unsupported` results.

## Querying

- `repogi graph`: JSX edges are REFERENCES with the syntax and rules above.
- `atlas_references`: JSX sites are listed as `REFERENCES(JSX) from <enclosing declaration>` in agent text and as `"jsx": true` in JSON, next to IMPORTS and plain REFERENCES. Unconfirmed same-name sites (`<Panel>` from `react-resizable-panels` when asking about the app's `Panel`) stay in their own section.
  - For change impact, request `kinds: ["REFERENCES"]` with `limit` up to 500. The default work limit of 50 sites truncates heavily used components, and says so (`truncated: N omitted, work limit reached`).
- `atlas_dependencies` upstream: the enclosing declarations of confirmed JSX sites appear as dependents (`REFERENCES` links), alongside importing modules.

## Cached IR and upgrades

Metadata key `semantic_ir` holds `SEMANTIC_IR_VERSION`, now **2**. Version 2 means two things:
- JSX sites are recorded in the cached per-file IR;
- object-pattern defaults are read as expressions.

An index written by an older binary has no key or version 1. The next `index` reparses every file once; otherwise unchanged files would keep IR without JSX sites. `atlas_status` reports `freshness.semantic_ir_outdated: true` until then.

Upgrading the syntax IR requires explicit re-indexing.

## Limits

These are conservative: none yields a wrong target.
- **Nested local components.** Components declared inside a function, such as `const Icon = …` inside a render function, are not addressable graph targets. Their sites are `unsupported`.
- **Mutated bindings.** `export const BarChart = memo(Inner); BarChart.displayName = 'BarChart';` counts as a write to `BarChart`. Member writes conservatively invalidate the binding (they could change `BarChart.x` targets), so `<BarChart>` is `unsupported`. This is the largest remaining gap on Umami.
- **Components as values.** A component passed as a prop, stored in a registry object or chosen at runtime (`const C = map[type]; <C/>`) has no static declaration target. `XContext.Provider` from React's `createContext` is a property of a package value; the receiver `XContext` is still referenced.
- **Files with syntax errors** supply no semantic edges at all; this is unchanged. On Umami, tree-sitter-typescript rejects a generic call with no arguments whose type argument is `typeof import('…')` (`importOriginal<typeof import('x')>()`). Three test files and their 7 JSX sites are lost to it.
- **No runtime claims.** JSX through `React.createElement(X)` is a CALLS of `createElement` with `X` as a plain reference. Lazy and dynamic imports, conditional rendering, render props and higher-order components are not traced. Package components are never resolved.
