# Project-aware module resolution

Repogi resolves TypeScript and JavaScript module specifiers natively in Rust, using the repository's own `tsconfig.json`/`jsconfig.json`:
- `baseUrl`;
- `paths`;
- local `extends` chains.

There is no Node.js, TypeScript compiler, language server or package lookup at runtime, and nothing is executed. Resolved modules feed the existing typed semantic graph, so aliased imports bind exactly like relative ones. That covers named, default and namespace imports, barrels and re-export chains, and therefore the CALLS, REFERENCES, EXTENDS and EXPORTS/RE_EXPORTS edges built on them.

`resolved` still means a unique static declaration binding under Repogi's profile. It is not a claim that the program type-checks or that a call happens at runtime.

## TypeScript's rules, and what Repogi implements

The rules come from the TypeScript handbook:
- [module resolution reference](https://www.typescriptlang.org/docs/handbook/modules/reference);
- [`paths`](https://www.typescriptlang.org/tsconfig/#paths), [`baseUrl`](https://www.typescriptlang.org/tsconfig/#baseUrl) and [`extends`](https://www.typescriptlang.org/tsconfig/#extends).

| Rule | TypeScript | Repogi |
|---|---|---|
| Which config governs a file | Editors use the nearest `tsconfig.json`, else `jsconfig.json`. `tsc -p` uses the project's `include`/`files`. | The nearest config directory at or above the file; `tsconfig.json` wins over `jsconfig.json` in one directory. `include`, `exclude`, `files` and `references` are **not** modelled. |
| JSONC | Comments and trailing commas are accepted. | Same (`//`, `/* */`, trailing commas, BOM). Anything else must be strict JSON; otherwise the project is invalid. |
| `extends` | String or array. Relative paths resolve from the extending file, with `.json` appended if missing. Later bases override earlier ones; the file's own options override all bases. | Same for relative files inside the root. Package `extends` (`@tsconfig/next`) and absolute paths are unsupported. |
| `baseUrl` | Resolved against the config that declares it. | Same. Outside the root, bare specifiers are unsupported. |
| `paths` base | `baseUrl` when set, else the directory of the config that declared `paths`. `${configDir}` means the final config's directory. | Same. |
| Pattern choice | An exact pattern wins. Otherwise the matching wildcard pattern with the **longest prefix**; on a tie, the first declared. At most one `*`. | Same. A pattern or substitution with two `*` makes the project invalid. |
| Substitutions | Tried **in order**; the first that yields a module wins. | Same. Skipped substitutions are recorded as evidence. |
| After `paths` | `baseUrl` lookup for non-relative names, then `node_modules`. | `baseUrl` lookup. Package lookup is out of profile (`package_or_unmapped_specifier`). |
| File candidates | `.ts`, `.tsx`, `.d.ts`, then `.js`/`.jsx` (with `allowJs`), then directory `package.json`, then `index.*`. `.js` specifiers try `.ts`/`.tsx`/`.d.ts` first. | The same order for TypeScript importers. The first existing candidate must be a file Repogi indexes; JavaScript files are indexed in source builds, so a `.js` hit now resolves. A first hit on `package.json` or a declaration-only `.d.mts`/`.d.cts` is **unsupported**, never skipped. Several indexed `x.ts`/`x.tsx`/`x/index.*` forms are **ambiguous**: TypeScript would pick `x.ts`, but Repogi keeps its conservative relative-import profile. |
| Directories Repogi never reads | Visible to TypeScript. | A mapping into `node_modules`, `dist`, `build`, `coverage`, `.next`, `generated`, `.git` or `.code-intel` is unsupported. |

**Not supported, so reported explicitly rather than guessed:**
- package resolution (`node_modules`), and for TypeScript importers package.json `main`/`exports`/`imports`/`typesVersions` (JavaScript importers: [package entry points](#package-entry-points));
- `moduleSuffixes`, `moduleResolution: classic`, `rootDirs`, `customConditions`;
- project references, and `include`/`exclude` membership;
- files hidden by `.gitignore`. These are treated as absent, so a mapping that depends on ignored generated files can fall through to a later substitution.

## JavaScript importers

TypeScript's candidate order is one tool's choice. For a file in `.js`, `.jsx`, `.mjs` or `.cjs`, Repogi uses only what Node.js, bundlers and TypeScript agree on, and reports the rest instead of picking a winner:

| Specifier | Repogi |
|---|---|
| With an extension (`./a.js`, `./b.mjs`, `./c.jsx`, `./d.ts`) | Exactly that file. If it does not exist, the TypeScript source compiled to that name: `a.js` → `a.ts`/`a.tsx`, `b.mjs` → `b.mts`, `c.cjs` → `c.cts` (several candidates: **ambiguous**). An existing `a.js` always wins over `a.ts`. |
| Without an extension (`./a`, and `.`/`..`) | Files first: `a.js`, `.jsx`, `.mjs`, `.cjs`, `.ts`, `.tsx`, `.mts`, `.cts`. Exactly one must exist; two or more (for example `a.js` beside `a.ts` or `a.mjs`) are **ambiguous**. Only when no file exists, the directory: its `package.json` `main` when it has one ([package entry points](#package-entry-points)), otherwise exactly one `a/index.*`. Bare `.` and `..` are relative, as in Node.js (0.1.4; `require("..")` previously fell through to package lookup). |
| JSON (`./data.json`, or `./data` with only `data.json`) | Not tracked; **unresolved**. |
| Bare (`react`, `@scope/pkg`) | `jsconfig`/`tsconfig` `paths` and `baseUrl` as above, then the importing file's own package through `exports` ([self-reference](#package-entry-points)), then **unsupported** (`package_or_unmapped_specifier`). |
| `require('./a')` | In a CommonJS file (`.cjs`, or `.js`/`.jsx` without ES module syntax and not under `"type": "module"`), a literal specifier binds like an import, with the same lookup ([CommonJS](STATIC_ANALYSIS_ACCURACY.md#commonjs)). The `require(...)` call itself stays **unresolved**. |
| `import('./a')` | Not an import binding. The call stays **unresolved** (no binding named `import`). |

These are static claims about which file a specifier names. They say nothing about whether the module loads at runtime under a particular tool.

## Package entry points

0.1.4, JavaScript importers only. `package.json` files inside the repository are read as data, with the same rules as configs: regular files only (never through symlinks), inside the root, size-capped; nothing is installed, downloaded or executed. Their entry fields are part of the resolution fingerprint, so editing them rebinds without reparsing.

**Directory imports** (`require("./pkg")`, `require("..")`) follow Node.js `LOAD_AS_DIRECTORY`:

| `package.json` | Result |
| --- | --- |
| No `main`, `exports`, `browser` or `module` field | The directory's `index` file, as if there were no `package.json` (previously **unsupported**). |
| `main` | The file `main` names, or that path as a directory (its own `package.json`, then `index`). Evidence: `pkg/package.json: main ./lib/x.js => pkg/lib/x.js`. A target that does not exist is **unresolved**; one outside the root is **unsupported** (`module_outside_repository`). |
| `browser` or `module` present | **Unsupported** (`package_entry_tool_dependent`): bundlers may pick another entry than Node.js. |
| Unreadable, malformed, a symlink, or a non-string `main` | **Unsupported** (`invalid_package_entry`). |
| `main` redirects nested more than 4 deep (including cycles) | **Unsupported** (`package_entry_tool_dependent`). |

`exports` does not apply to relative directory imports (Node.js uses it only for package names), so it is not consulted there.

**Self-reference.** A bare specifier that names the importing file's own package (the nearest `package.json` above it has that `name` and an `exports` field) resolves through `exports`, as Node.js does:

- Only exact subpaths: `"."` (or a top-level string or condition object) for `name`, `"./utils"` for `name/utils`. A subpath matched only by a `*` pattern is **unsupported** (`package_exports_pattern`); one that is absent or `null` is **unresolved** (`package_subpath_not_exported`).
- Conditions are walked in declaration order with the conditions every tool agrees on for the importer: `require` in a CommonJS file, `import` otherwise, and `default`. `types` is skipped; the other of `require`/`import` is skipped. Any other condition before the match (`node`, `browser`, `module`, `development`, custom names) is **unsupported** (`conditional_exports_tool_dependent`).
- The target must be `./`-relative without `..` or `node_modules` (`invalid_package_target`) and names exactly that file: no extension or `index` search. A target in `dist/`, `build/` and other directories Repogi never reads is unsupported like any other excluded path.
- Fallback arrays, objects mixing subpaths and conditions, and nesting beyond 8 levels or 1,024 entries are **unsupported** (`package_exports_unsupported`).

Packages in `node_modules`, workspace packages imported by name from another package, and `imports` (`#internal`) are not followed.

## Statuses and rules

Non-resolved module outcomes carry a specific `evidence.rule`. Edges that bind through such a module (CALLS, REFERENCES and others) inherit the rule.

| Rule | Status | Meaning |
|---|---|---|
| `module_not_found` | unresolved | No candidate exists: a relative path, every substitution of a matched pattern, and `baseUrl`. |
| `ambiguous_module_candidates` | ambiguous | Several indexed `x.ts`/`x.tsx`/`x/index.*` forms; the candidates are listed in evidence. |
| `package_or_unmapped_specifier` | unsupported | A bare specifier that no pattern matched and `baseUrl` did not find: usually a package. |
| `module_resolves_to_unindexed_file` | unsupported | TypeScript would pick a non-indexed file (JS, `.mts`, `package.json` redirect) first. |
| `module_outside_repository` | unsupported | A relative or `baseUrl` path, or a `package.json` `main`, leaves the root. |
| `package_entry_tool_dependent` | unsupported | A directory's `package.json` has `browser`/`module`, or `main` redirects too deep. |
| `invalid_package_entry` | unsupported | A directory's `package.json` cannot be read as data. |
| `package_exports_pattern`, `conditional_exports_tool_dependent`, `invalid_package_target`, `package_exports_unsupported` | unsupported | A self-reference through `exports` that Repogi does not choose for the tools. |
| `package_subpath_not_exported` | unresolved | The package's `exports` has no such subpath (or it is `null`). |
| `path_mapping_outside_repository` | unsupported | A substitution (or `*` capture such as `@/../x`) leaves the root. |
| `path_mapping_into_excluded_directory`, `module_in_excluded_directory` | unsupported | The candidate lies in a directory the indexer never reads. |
| `base_url_outside_repository` | unsupported | `baseUrl` is outside the root. |
| `invalid_project_configuration` | unsupported | The governing config is invalid: JSONC, missing/cyclic/package `extends`, a bad `paths` shape, or unsupported options. A diagnostic names the reason. |
| `unsupported_module_specifier` | unsupported | Backslashes and other unsupported specifier syntax. |

There is no basename fallback. An unresolved alias never becomes a confirmed symbol.

**Evidence** for a mapped import adds steps such as:
- `tsconfig.json: paths @/* -> ./src/* => src/lib/util.ts` (the substitution is shown repository-relative);
- `config/base.json: baseUrl . => src/lib/util.ts`;
- `src/app/page.ts: module @/lib/util => src/lib/util.ts`.

Skipped substitutions appear as `-> ./missing/* (no module)`. Evidence for relative imports identifies the selected source module.

## Configuration-aware indexing

1. `index` reads every `tsconfig.json`/`jsconfig.json` the walker can see. Configs are size-capped, must be regular files inside the root, and are never followed through symlinks.
2. It builds a resolution context: per-directory effective `baseUrl`/`paths`, or an error, plus the set of module files a resolver could select.
3. The context's BLAKE3 fingerprint is stored in the index metadata.
4. When the fingerprint differs from the indexed one, Repogi **rebinds every cached module from its stored syntax IR without reparsing any source**. That happens when a config is edited, added, deleted or becomes invalid, or when a JavaScript or `package.json` file appears. The rebind happens in the same transaction as the revision bump, and the report sets `resolution_changed: true`.
5. Warm runs with an unchanged fingerprint rebuild nothing. A failed update rolls back.
6. MCP `atlas_status` recomputes the fingerprint, so a config-only edit shows as `fresh: false` with `freshness.resolution_changed: true`.
7. Re-index explicitly after changing source or resolution settings.

The invalidation is coarse (a whole rebind), consistent with source changes.
