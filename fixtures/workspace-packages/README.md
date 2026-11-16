# workspace-packages fixture

An npm-style monorepo (`workspaces: ["packages/*", "apps/*"]`) with hand-labeled cross-package imports (`EXPECTED.json`). Nothing here is installed or executed.

- `apps/web` (ES modules): `exports` subpaths, an unexported subpath, an ES import of a CommonJS package through `main`, a duplicate package name, a dependency range the local version does not satisfy, a registry package, and a cross-package `new Session()` receiver.
- `apps/api` (CommonJS): `main`, a subpath without `exports`, and `require()` of an ES-module package.
- `apps/admin` (TypeScript): `types` conditions, a `types` entry in an excluded `dist/` directory, and a JavaScript package from TypeScript.
- `tools/check.js`: an importer whose `package.json` (the root) does not declare the dependency.
