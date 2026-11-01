import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addItem, createCart } from '../src/cart/cart.js';
import { placeOrder } from '../src/api/orders.js';

test('rejects unknown tokens', () => {
  assert.deepEqual(placeOrder(new Map(), 'missing', createCart()), { status: 401 });
});

test('places an order for a live session', () => {
  const store = new Map([['t', { userId: 'u1', expiresAt: Date.now() + 60_000 }]]);
  const cart = addItem(createCart(), { priceCents: 1000 });
  const result = placeOrder(store, 't', cart, { taxRate: 0 });
  assert.equal(result.status, 200);
  assert.equal(result.receipt.totalCents, 1499);
});
