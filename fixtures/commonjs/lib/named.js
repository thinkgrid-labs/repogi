function a() { return 1; }
exports.a = a;
exports.b = function () { return 2; };
exports['c'] = a;
