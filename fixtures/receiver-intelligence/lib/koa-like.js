const util = require('node:util');
module.exports = class Application {
  constructor() {
    this.middleware = [];
    if (util.inspect.custom) {
      this[util.inspect.custom] = this.inspect;
    }
  }
  use(fn) { this.middleware.push(fn); return this; }
  callback() { return this.handle(); }
  handle() { return 1; }
  inspect() { return {}; }
};
