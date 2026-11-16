const Emitter = require('events');
module.exports = class Application extends Emitter {
  listen() { return this.callback(); }
  callback() { return this.handle(); }
  handle() { return 1; }
};
