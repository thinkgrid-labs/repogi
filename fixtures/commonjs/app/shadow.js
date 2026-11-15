function require(path) { return { validate() { return path; } }; }
const session = require('../lib/session');
session.validate();
