# CommonJS fixture

Hand-labeled static CommonJS cases (`EXPECTED.json`), checked by `crates/cli/tests/static_accuracy.rs`. Labels were written from the source text and Node.js semantics before comparing with Repogi output. Nothing here is executed.

Positive: `require("literal")` whole-module, destructured, aliased and `.member` bindings; `module.exports = { … }`, `exports.name = …`, `exports["name"] = …`, `module.exports = fn`, `module.exports = require(…)`, `Object.defineProperty(exports, "__esModule", …)`; ES module imports of CommonJS.
Negative: dynamic specifiers, packages, `.mjs` targets, `exports.x` before `module.exports = …` and after the alias breaks, conditional exports, `Object.assign(module.exports, …)`, function-expression values, destructuring defaults, reassigned bindings, a local `require`, `.js` under `"type": "module"`, TypeScript importers.
