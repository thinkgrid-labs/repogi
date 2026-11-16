function a() { return 1; }
module.exports = a;
if (process.env.X) { module.exports.extra = 1; }
