import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addItem, createCart } from '../../src/cart/cart.js';
import { checkout } from '../../src/checkout/checkout.js';

const cartOf = (cents) => addItem(createCart(), { priceCents: cents });

test('exactly $50.00 ships free', () => {
  assert.equal(checkout(cartOf(5000)).shippingCents, 0);
});

test('$49.99 pays the flat rate', () => {
  assert.equal(checkout(cartOf(4999)).shippingCents, 499);
});

test('tax is never charged on shipping', () => {
  const receipt = checkout(cartOf(4000), { taxRate: 0.1 });
  assert.equal(receipt.taxCents, 400);
  assert.equal(receipt.totalCents, 4000 + 400 + 499);
});
