const util = require('util');
function Base() {}
function Child() {}
util.inherits(Child, Base);
Child.prototype.run = function () { return 1; };
const child = new Child();
child.run();
function Replaced() {}
Replaced.prototype = { run() { return 1; } };
const replaced = new Replaced();
replaced.run();
function Later() {}
Later.prototype.run = function () { return 1; };
if (process.env.DEBUG) { Later.prototype.run = function () { return 2; }; }
const later = new Later();
later.run();
function Twice() {}
Twice.prototype.run = function () { return 1; };
Twice.prototype.run = function () { return 2; };
const twice = new Twice();
twice.run();
function Own() { this.go = function () { return 0; }; }
Own.prototype.go = function () { return 1; };
const own = new Own();
own.go();
module.exports = { Base };
