# JSX component references

Repogi records supported JSX component uses as REFERENCES edges, rather than function calls. Both opening/self-closing tags count once; closing tags and intrinsic elements add no JSX site.

| JSX | Treatment |
| --- | --- |
| `<Button />`, `<Panel>…</Panel>` | Value-based component reference. |
| `<Icons.Star />` | Member component reference, subject to static binding support. |
| `<div />`, `<my-element />`, `<svg:path />` | Intrinsic/namespaced element; no component reference. |
| `<>…</>` | Fragment; no named component reference. |

Attribute names add no references. Attribute expressions and children are ordinary code: `{Button}` can be a plain reference and `{render()}` a call. A namespace-import receiver is not counted as a separate value use.

## Binding and evidence

JSX uses the same lexical scopes, import aliases, namespace imports, relative/path-mapped modules and re-export chains as ordinary references. Local shadowing and writes to a binding can prevent confirmation. A resolved JSX site carries `jsx_component_reference`; a tag resolving only to an interface/type alias is unsupported with `jsx_target_is_a_type`.

Type-only imports cannot establish runtime component use. Unresolved package imports, ambiguous module candidates, unknown receivers and mutable bindings retain their status and evidence rule. A matching name is not a confirmed target.

`target_text` retains the written tag, such as `PrimaryButton` or `Icons.Star`. Evidence includes the source site, syntax description and available binding chain. See [semantic scope](SEMANTICS.md) and [module resolution](MODULE_RESOLUTION.md).

## Query component use sites

```text
atlas_references {"name":"Button","path":"src/Button.tsx","kinds":["REFERENCES"],"limit":500,"max_bytes":16000}
atlas_dependencies {"name":"Button","path":"src/Button.tsx","direction":"upstream","depth":2,"max_bytes":8000}
```

Reference JSON marks JSX sites with `jsx:true`. Agent text identifies them as `REFERENCES(JSX)`. Upstream dependencies can include enclosing declarations of those sites. Inspect truncation, omissions and count flags; neither response proves runtime rendering or complete coverage. Use [navigation limits](REFERENCE_NAVIGATION_LIMITS.md) when recovering omitted sites.

## Index upgrades

The cached syntax IR includes JSX sites and expressions inside object-pattern defaults. If an index's stored IR is outdated, `atlas_status` marks it stale. Run `repogi index .` explicitly to rebuild the necessary source facts before relying on the graph.

## Limits

- Nested local components are not separate addressable declaration targets.
- Member writes can conservatively invalidate a component binding, including display-name assignments.
- Components selected from registries, props or runtime values may have no static target. React context providers can retain a reference to the context value without a resolved provider-member target.
- Syntax-error or unsupported source retains diagnostics and cannot supply confirmed relationships for affected constructs. See [grammar limitations](TYPESCRIPT_GRAMMAR_LIMITATIONS.md).
- Lazy/dynamic imports, higher-order components, conditional rendering and runtime dispatch are not traced. Package components are not resolved.
- `React.createElement(X)` is treated as an ordinary call to `createElement` and a reference to `X`; it is not a JSX site.
