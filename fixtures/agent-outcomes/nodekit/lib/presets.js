'use strict';
const middleware = require('./middleware');

const makeLogger = middleware.logger;
const trim = middleware.trimPath;

exports.verbose = function verbose(lines) {
  return [trim, makeLogger(lines)];
};
