# Project-aware module resolution

Repogi resolves TypeScript/TSX module specifiers natively in Rust, using the repository's own `tsconfig.json`/`jsconfig.json`:
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
| File candidates | `.ts`, `.tsx`, `.d.ts`, then `.js`/`.jsx` (with `allowJs`), then directory `package.json`, then `index.*`. `.js` specifiers try `.ts`/`.tsx`/`.d.ts` first. | The same order. The first existing candidate must be a file Repogi indexes. A first hit on JavaScript, `.mts`/`.cts` or `package.json` is **unsupported**, never skipped. Several indexed `x.ts`/`x.tsx`/`x/index.*` forms are **ambiguous**: TypeScript would pick `x.ts`, but Repogi keeps its conservative relative-import profile. |
| Directories Repogi never reads | Visible to TypeScript. | A mapping into `node_modules`, `dist`, `build`, `coverage`, `.next`, `generated`, `.git` or `.code-intel` is unsupported. |

**Not supported, so reported explicitly rather than guessed:**
- package resolution and package.json `exports`/`imports`/`typesVersions`;
- `moduleSuffixes`, `moduleResolution: classic`, `rootDirs`, `customConditions`;
- project references, and `include`/`exclude` membership;
- `.mts`/`.cts` sources;
- files hidden by `.gitignore`. These are treated as absent, so a mapping that depends on ignored generated files can fall through to a later substitution.

## Statuses and rules

Non-resolved module outcomes carry a specific `evidence.rule`. Edges that bind through such a module (CALLS, REFERENCES and others) inherit the rule.

| Rule | Status | Meaning |
|---|---|---|
| `module_not_found` | unresolved | No candidate exists: a relative path, every substitution of a matched pattern, and `baseUrl`. |
| `ambiguous_module_candidates` | ambiguous | Several indexed `x.ts`/`x.tsx`/`x/index.*` forms; the candidates are listed in evidence. |
| `package_or_unmapped_specifier` | unsupported | A bare specifier that no pattern matched and `baseUrl` did not find: usually a package. |
| `module_resolves_to_unindexed_file` | unsupported | TypeScript would pick a non-indexed file (JS, `.mts`, `package.json` redirect) first. |
| `module_outside_repository` | unsupported | A relative or `baseUrl` path leaves the root. |
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
