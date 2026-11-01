import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addItem, createCart } from '../src/cart/cart.js';
import { checkout } from '../src/checkout/checkout.js';

test('adds tax and shipping', () => {
  const cart = addItem(createCart(), { priceCents: 1000 });
  assert.deepEqual(checkout(cart, { taxRate: 0.1 }), {
    itemsCents: 1000,
    taxCents: 100,
    shippingCents: 499,
    totalCents: 1599,
  });
});
