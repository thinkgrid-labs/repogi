# Workspace packages

0.1.3. In a monorepo, an import of a local workspace package by name (`import { createSession } from "@acme/session"`) resolves to the package's source inside the repository, with the same graph edges as a relative import: imports, references, calls, re-exports, change impact, test candidates and `investigate`. Nothing is installed, no package manager runs, and `node_modules` is never read: Repogi reads the declarations a package manager would act on and only claims what they determine.

## What declares the workspace

Only the repository root is read.

| Declaration | Read as |
| --- | --- |
| `package.json` `"workspaces": ["packages/*", "apps/web"]` (npm, Yarn) | The pattern list. |
| `package.json` `"workspaces": { "packages": [...] }` (Yarn) | The `packages` list; other keys (`nohoist`) are ignored. |
| `pnpm-workspace.yaml` `packages:` | A block list of plain or quoted scalars (`- 'packages/*'`, `- apps/web # comment`). Other top-level keys (`catalog:`) are ignored. Flow lists, anchors, aliases, tags, multiple documents and escapes are refused. |

Patterns are relative directories whose segments are literal names, `*` (one directory, not starting with `.`) or `**` (any depth). `!pattern` excludes. Braces, character classes, `?`, partial wildcards (`pkg-*`), absolute paths and `..` are not evaluated. Consecutive `**` collapse into one; a pattern may have at most 32 segments and two `**`, which keeps matching polynomial. At most 256 patterns and 10,000 members.

A **member** is a directory that a pattern selects and that holds a `package.json` the indexer can see. With `pnpm-workspace.yaml`, the root package is also a member, as pnpm always includes it; npm and Yarn classic do not link the root by name, so with `workspaces` alone it is not. Directories the indexer never enters (`node_modules`, `dist`, `build`, `.git` and the others in [module resolution](MODULE_RESOLUTION.md)) are not members. Nested workspace declarations in members are not read.

## Package identity

A member's identity is its `package.json` `name`, read as data (regular file, inside the root, size-capped, never through a symlink) with `version`, `types`/`typings` and `typesVersions`. The name must be a valid npm package name; a name that could traverse paths (`../x`) is refused. A member without a name cannot be imported by name. Directory names are never used as identities.

The workspace is **unsupported** as a whole (rule `invalid_workspace_configuration`, plus a diagnostic on `repogi index`) when any of these holds: an unsupported pattern or declaration shape, a symlinked directory that a pattern selects (its contents are never read, so its name and uniqueness are unknown), an unreadable or malformed member `package.json`, an invalid member name, or `package.json` and `pnpm-workspace.yaml` that select different members. Members that could be read keep their identities, so an import of their names reports the workspace problem instead of looking like a registry package; it never resolves.

## When an import names a workspace package

A bare specifier (`@acme/session`, `@acme/session/tokens`, `core/lib/x`) is tried after `paths`/`baseUrl` and the importer's own package ([self-reference](MODULE_RESOLUTION.md#package-entry-points)), as Node.js does. Its package name must match exactly one member:

| Situation | Result |
| --- | --- |
| No member has the name | Unchanged: `package_or_unmapped_specifier` (unsupported). |
| Several members have the name | Unsupported, `duplicate_workspace_package`, listing every directory. None is chosen. |
| The importer's nearest `package.json` does not list the name in `dependencies`, `optionalDependencies`, `devDependencies` or `peerDependencies` | Unsupported, `workspace_dependency_not_declared`. npm and Yarn would still find a hoisted link; pnpm would not. |
| The listed version may install a registry copy | Unsupported, `workspace_dependency_version_unverified`. |
| Otherwise | The member's entry point, below. |

A dependency specifier selects the local member when it uses the `workspace:` protocol (pnpm and Yarn refuse to install otherwise), is `*` or empty, is exactly the member's `version`, or is `^`, `~`, `>=` or `=` followed by exactly that version; `file:`/`link:` must name the member's directory. Other ranges are not evaluated (there is no semver solver), so `^2.0.0` for a member at `2.1.0` is reported, not guessed.

## Entry points

**JavaScript importers** follow Node.js:

- With `exports`: exactly the [self-reference rules](MODULE_RESOLUTION.md#package-entry-points): exact subpaths only, conditions in declaration order with `require` (CommonJS importer) or `import`, plus `default`; `types` and the other of `require`/`import` are skipped; any other condition before the match is `conditional_exports_tool_dependent`. Patterns, fallback arrays and targets leaving the package are unsupported. A subpath that `exports` does not list is `package_subpath_not_exported` (unresolved): `exports` encapsulates the package.
- Without `exports`: the package directory as Node.js loads a directory (`main`, then `index`; `browser`/`module` fields are tool dependent), or a subpath as a file, then as a directory.
- The selected file is then bound like any module: `require()` of an ES module is still `require_of_es_module_or_typescript`, and an ES import of a CommonJS package uses the CommonJS export table.

**TypeScript importers.** TypeScript picks a different file depending on `moduleResolution`, which Repogi does not read. So every mode that finds a file must find the same indexed file:

- `exports` with `types`, `import` or `require`, and `default` conditions (`node16`, `nodenext`, `bundler`), each target with TypeScript's extension substitution (`./src/index.js` finds `src/index.ts`);
- `types`/`typings`, else `main`, else `index` (`node10`, and every mode when `exports` is absent).

A mode that finds nothing does not compete (when `exports` is present), but an `exports` target that does not exist fails. Different files are `workspace_entry_tool_dependent`; `typesVersions` is always tool dependent; a `types` entry in `dist/` is `module_in_excluded_directory`.

**Evidence** names every step, for example:

```text
package.json: workspace package @acme/session => packages/session/package.json
apps/web/package.json: depends on @acme/session workspace:*
packages/session/package.json: exports . -> ./src/index.js => packages/session/src/index.js
apps/web/src/app.js: module @acme/session => packages/session/src/index.js
```

## Boundaries

- One repository: the root `repogi index` was given. Workspace discovery uses the files the indexer already walked, never follows symlinks and never reads outside the root. MCP root allowlists and `.repogi/memory.db` are unchanged; there is no cross-repository memory.
- Not supported: `node_modules` packages, nested workspaces, Yarn PnP and `resolutions`, pnpm `overrides`, catalogs and `link-workspace-packages` settings, Bun/Lerna/Nx/Turborepo/Rush configuration, `imports` (`#internal`), conditional and pattern exports beyond the rules above, and TypeScript project references.

## Incremental indexing

The workspace (declaring files, member names, directories, versions, `types` fields and the dependency declarations naming members) is part of the resolution fingerprint. Renaming a package, editing `workspaces` or `pnpm-workspace.yaml`, changing a dependency range or moving an entry point rebinds every module from its cached syntax without reparsing (`resolution_changed: true`); the result equals a fresh index (`crates/cli/tests/static_accuracy.rs`).

## Evaluation

`fixtures/workspace-packages` is an npm-style monorepo with hand-written labels (written before indexing) for ES module, CommonJS and TypeScript importers: `exports` subpaths, an unexported subpath, `main`, an ES import of a CommonJS package, `require()` of an ES-module package, a duplicate name, an undeclared dependency, a range the local version does not satisfy, a registry package, `types` conditions, a `types` entry in `dist/`, and a cross-package `new Session()` receiver. An adversarial test covers symlinked and malformed members, unsupported glob and YAML syntax, out-of-root patterns, 300 patterns, conflicting npm/pnpm declarations, traversal in names and `exports` targets, and a re-export cycle across two packages.

Of the four pinned evaluation repositories, only Chart.js declares a workspace (`pnpm-workspace.yaml`: `docs`, `test/integration/*`); its integration packages depend on the root package with `"chart.js": "workspace:*"`. Results are under [static analysis accuracy](STATIC_ANALYSIS_ACCURACY.md#workspace-packages-on-chartjs).
