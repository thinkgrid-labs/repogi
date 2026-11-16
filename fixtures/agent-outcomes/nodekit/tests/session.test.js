'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const Session = require('../lib/session');

test('a fresh session is valid', () => {
  const session = new Session('abc', 1000, 0);
  assert.equal(session.valid(10), true);
  assert.equal(session.expired(2000), true);
});
