function Session(id) { this.id = id; }
Session.prototype.validate = function () { return Boolean(this.id); };
Session.prototype.touch = function touch() { return this.validate(); };
Session.prototype.close = closeSession;
function closeSession() { return null; }
module.exports = Session;
