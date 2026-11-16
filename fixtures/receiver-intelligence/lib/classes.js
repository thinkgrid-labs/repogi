const util = require('node:util');
const custom = Symbol('custom');
class Returns {
  constructor() { return makeOther(); }
  go() { return 1; }
}
class ReturnsThis {
  constructor() { if (!this) return; return this; }
  went() { return 1; }
}
class Fielded {
  halt = () => 0;
  constructor() { this.pause = () => 1; }
  pause() { return 1; }
  resume() { return 2; }
}
class Inspected {
  constructor() {
    this[util.inspect.custom] = this.show;
    this[Symbol.iterator] = this.show;
    this[custom] = this.show;
    this[0] = 1;
  }
  show() { return 'i'; }
}
class Keyed {
  constructor(key) { this[key] = 1; }
  turn() { return 1; }
}
class Patched { spin() { return 1; } }
Patched.prototype.spin = function () { return 2; };
class Mixed extends mixin(Object) { roll() { return 1; } }
function makeOther() { return {}; }
function mixin(B) { return B; }
const r1 = new Returns(); r1.go();
const r2 = new ReturnsThis(); r2.went();
const r3 = new Fielded(); r3.halt(); r3.pause(); r3.resume();
const r4 = new Inspected(); r4.show();
const r5 = new Keyed(1); r5.turn();
const r6 = new Patched(); r6.spin();
const r7 = new Mixed(); r7.roll();
module.exports = { Inspected };
