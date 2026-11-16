'use strict';
const kit = require('./kit');

exports = module.exports = kit.createKit;
exports.middleware = require('./middleware');
exports.presets = require('./presets');
exports.Session = require('./session');
exports.validator = require('./validator');
