'use strict';

function compose(stack) {
  return function run(ctx) {
    let index = -1;
    function dispatch(i) {
      if (i <= index) throw new Error('next() called twice');
      index = i;
      const fn = stack[i];
      if (!fn) return ctx;
      return fn(ctx, () => dispatch(i + 1));
    }
    return dispatch(0);
  };
}

function logger(lines, prefix = '') {
  return (ctx, next) => {
    lines.push(`${prefix}> ${ctx.path}`);
    const result = next();
    lines.push(`${prefix}< ${ctx.path}`);
    return result;
  };
}

exports.compose = compose;
exports.logger = logger;
exports.trimPath = function trimPath(ctx, next) {
  ctx.path = ctx.path.replace(/\/+$/, '');
  return next();
};
