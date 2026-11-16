const service = require('./service');
const { createServer } = require('./server');
exports = module.exports = createApp;
function createApp() { return service.validate(); }
exports.service = service;
exports.createServer = createServer;
exports.version = '1';
