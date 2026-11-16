'use strict';
// Deprecated: kept for old integrations. Do not change.
exports.trimPath = function trimPath(ctx, next) {
  ctx.path = ctx.path.replace(/\/+$/, '');
  return next();
};
