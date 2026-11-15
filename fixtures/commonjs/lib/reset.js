function x() { return 1; }
function y() { return 2; }
exports.x = x;
module.exports = { y };
exports.z = x;
