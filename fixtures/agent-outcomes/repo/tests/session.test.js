import { test } from 'node:test';
import assert from 'node:assert/strict';
import { getUserSession } from '../src/lib/session.js';

test('expired sessions are ignored', () => {
  const store = new Map([['t', { userId: 'u1', expiresAt: 0 }]]);
  assert.equal(getUserSession(store, 't'), null);
});
