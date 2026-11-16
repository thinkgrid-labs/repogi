const patched = require('./patched');
patched.validate = function () { return false; };
patched.refresh();
