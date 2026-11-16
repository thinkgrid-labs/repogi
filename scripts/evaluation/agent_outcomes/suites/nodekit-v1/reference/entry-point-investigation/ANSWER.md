`require('nodekit')` loads the package entry point named by `main` in `package.json`, `lib/index.js`. That module sets `exports = module.exports = kit.createKit`, so the package value is the `createKit` factory from `lib/kit.js` (with `middleware`, `presets`, `Session` and `validator` attached as properties). Calling it returns a kit object with `use` and `handle`.

CITATIONS: package.json:4, lib/index.js:4, lib/kit.js:4
