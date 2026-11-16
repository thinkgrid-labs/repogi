'use strict';
const validator = require('./validator');

function Session(id, ttlMs, startedAt) {
  this.id = id;
  this.ttlMs = ttlMs;
  this.startedAt = startedAt;
}

Session.prototype.expired = function expired(now) {
  return now - this.startedAt > this.ttlMs;
};

Session.prototype.valid = function valid(now) {
  return validator.isNonEmpty(this.id) && !this.expired(now);
};

module.exports = Session;
