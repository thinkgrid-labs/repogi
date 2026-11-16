# receiver-intelligence fixture

Hand-labeled inputs for `new C()` receivers, computed own-property keys and their negative cases (labels in `EXPECTED.json`). Nothing here is executed.

- `lib/basic.js`: CommonJS class instances, constructor and instance aliases, missing and static-only members, a getter.
- `lib/esm.mjs`, `lib/shapes.mjs`: ES module constructors, namespace constructors (`new shapes.Circle()`), an override that does not affect an exact `Shape` instance.
- `lib/negative.js`: reassigned, factory-made, escaped (argument, alias argument, export), member-written and conditionally constructed receivers.
- `lib/classes.js`, `lib/shadow.js`: constructors that return objects, own fields, symbol keys (`Symbol.iterator`, `util.inspect.custom`, `const k = Symbol()`), integer keys, unknown keys, shadowed `Symbol` and `util`, prototype writes, dynamic superclasses.
- `lib/koa-like.js`, `lib/koa-user.js`: the Koa `this[util.inspect.custom]` constructor pattern.
- `lib/control.ts`: TypeScript constructor receivers.
