# Independent affected-test Git-history fixtures

`repo/` is committed into a fresh temporary Git repository. Each case in
`cases.json` applies literal source edits, optional renames/additions/removals and
a second commit. Atlas reads it; no fixture test or project script executes.
`package.json` and `vitest.config.ts` contain execution traps for that boundary.

Labels were written from the fixture imports/calls before querying discovery.
They are maintained separately from Atlas output. They assert exact file sets,
terminal target file/name, shortest depth and, where relevant, baseline-only
evidence. Possible/unknown labels are explicit required examples; they are not
an exhaustive heuristic precision oracle. The reviewer is the implementing agent,
not an independent human reviewer or an execution/coverage oracle.

| Cases | Independent source facts |
|---|---|
| Function/unit; transitive integration | Named/namespace aliases call `calc`; `process` and `runTest` call `calc`, then tests call those enclosing functions |
| TSX | `component.test.tsx` renders the imported `Card` alias |
| Deleted export | Original consumers still refer to baseline `calc` |
| Renamed declaration | Old-name consumers remain in base; an added test calls `total` in head |
| Barrel retarget | The unchanged test's `compute` binding moves from `src/math.ts` to `src/other/math.ts` |
| Alias retarget | The alias test's `calc` resolves to the new configured module |
| File rename | Renamed `math.ts` also changes its function body; baseline/head target paths differ |
| Helper barrel | A test calls `runTest` through a re-export without confusing the export site with a test call |
| Name collision; unrelated colocation | Same-name calls to `src/other/math.ts` cannot be confirmed as consumers of `src/math.ts` |
| Dynamic/unsupported; syntax error | JavaScript, syntax-error, generated and ignored tests have unknown semantic coverage |
| No known test consumer | `untested` has no usage site; filename/proximity remains possible |
| Test addition/removal | Baseline-only and head-only tests both remain discoverable |
| Large fan-out | 240 explicitly generated test imports/calls are independently expected, beyond legacy per-site caps |
| Uncertain barrel loss | An unresolved export-star source creates an unverified binding loss |
| Negative control | Module trivia changes no declaration, statement, export or binding |

Both module-scope test callbacks and an enclosing `verify` declaration are covered.
The latter checks that evidence names the actual called `process` target rather
than mistakenly using the test's own enclosing declaration as its call target.
Deliberately wrong-target scoring checks reject an incorrect path with identical
test-file and resolved-relationship counts.

Additional integration checks cover small budgets/omission counts, directory
configuration, selected-file evidence, ignored tests, minimal complete static
profiles, mutable cache manifest tampering and changes during analysis.
