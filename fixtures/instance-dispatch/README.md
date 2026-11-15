# Instance dispatch fixture

Hand-labeled `this.m()` / `super.m()` cases for JavaScript and TypeScript (`EXPECTED.json`), checked by `crates/cli/tests/static_accuracy.rs`. Labels were written from the source text before comparing with Repogi output. Nothing here is executed.

Positive: own and inherited methods, `super` (instance and static), private `#m`, arrow-function `this`, `this.m = this.m.bind(this)`, TypeScript generics and access modifiers.
Negative: subclass overrides (ambiguous), abstract members, getters called as functions, fields and `this.m = …` shadowing, `this[k] = …`, prototype assignment, `function` `this`, aliased `this`, static/instance mismatch, dynamic (`mixin(…)`) and cyclic superclasses, classes nested in functions.
