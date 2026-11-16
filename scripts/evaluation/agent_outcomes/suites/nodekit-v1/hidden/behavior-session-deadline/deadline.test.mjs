import test from 'node:test';
import assert from 'node:assert/strict';
import Session from '../../lib/session.js';

test('expired exactly at the deadline', () => {
  const session = new Session('id', 1000, 500);
  assert.equal(session.expired(1499), false);
  assert.equal(session.expired(1500), true);
  assert.equal(session.expired(1501), true);
  assert.equal(session.valid(1500), false);
  assert.equal(session.valid(1499), true);
});
