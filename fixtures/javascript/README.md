# JavaScript correctness fixture

Mixed JavaScript/TypeScript sources with hand-reviewed labels in
`EXPECTED.json`, checked by `crates/cli/tests/javascript.rs`. Nothing here is
executed. Labels name every resolved IMPORTS, CALLS, REFERENCES, EXTENDS and
RE_EXPORTS edge (target = file and declared name), plus sites that must stay
unconfirmed with their expected status.

Covered: declarations, ESM named/default/namespace imports, aliases, barrel
re-exports (`export { default as x }`, `export *`), `jsconfig.json` paths,
extensionless and explicit-extension specifiers (including `./a.js` naming
the TypeScript source `a.ts` when `a.js` does not exist), `.mjs`/`.cjs` and
`.mts`/`.cts`, JSX components (local, imported, default, from `.tsx`),
JavaScript importing TypeScript and the reverse, `extends`, static members,
duplicate names, ambiguous candidates, packages, missing modules, imports that
leave the repository, shadowing parameters (plain, destructured, default, rest),
a mutable local alias, dynamic `import()`, CommonJS `require`, dynamic JSX
components, Unicode identifiers and a file with syntax errors.
