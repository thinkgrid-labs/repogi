import { Store } from './base.js';

function mixin(Base) { return class extends Base {}; }

export class Mixed extends mixin(Store) {
  go() { return this.read(); }
}

export class Fields {
  handler = () => 1;
  constructor() { this.cb = () => 2; this.ok = this.ok.bind(this); }
  run() { return this.handler() + this.cb() + this.ok(); }
  ok() { return 3; }
  bad() { const self = this; return self.ok(); }
  nested() { return [1].map(function () { return this.ok(); }); }
  arrow() { return [1].map(() => this.ok()); }
}

export class Patched {
  m() { return 1; }
  n() { return this.m(); }
}
Patched.prototype.m = function () { return 2; };

export class Dyn {
  constructor(k) { this[k] = () => 0; }
  m() { return 1; }
  n() { return this.m(); }
}
