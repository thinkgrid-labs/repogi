function validate() { return true; }
function refresh() { return 1; }
class Token { static issue() { return 't'; } }
module.exports = { validate, renew: refresh, Token };
