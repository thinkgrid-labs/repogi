'use strict';
const { compose } = require('./middleware');

function createKit() {
  const stack = [];
  return {
    use(fn) {
      stack.push(fn);
      return this;
    },
    handle(ctx) {
      return compose(stack)(ctx);
    },
  };
}

module.exports = { createKit };
