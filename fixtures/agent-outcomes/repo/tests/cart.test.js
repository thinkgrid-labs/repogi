import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addItem, createCart, subtotal } from '../src/cart/cart.js';

test('subtotal multiplies quantities', () => {
  const cart = addItem(addItem(createCart(), { priceCents: 250, quantity: 2 }), { priceCents: 100 });
  assert.equal(subtotal(cart), 600);
});
