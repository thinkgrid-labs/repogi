import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as session from '../../src/lib/session.js';
import { placeOrder } from '../../src/api/orders.js';

test('loadSession replaces getUserSession without an alias', () => {
  assert.equal(typeof session.loadSession, 'function');
  assert.equal(session.getUserSession, undefined);
});

test('consumers still work', () => {
  const store = new Map([['t', { userId: 'u1', expiresAt: Date.now() + 60_000 }]]);
  assert.equal(session.loadSession(store, 't').userId, 'u1');
  assert.equal(placeOrder(store, 'x', { items: [] }).status, 401);
});
