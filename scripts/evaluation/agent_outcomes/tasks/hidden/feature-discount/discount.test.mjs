import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addItem, createCart } from '../../src/cart/cart.js';
import { checkout } from '../../src/checkout/checkout.js';

const cart = addItem(createCart(), { priceCents: 3000 });

test('discount is applied before tax', () => {
  const receipt = checkout(cart, { taxRate: 0.1, discountCents: 1000 });
  assert.equal(receipt.discountCents, 1000);
  assert.equal(receipt.taxCents, 200);
  assert.equal(receipt.totalCents, 3000 - 1000 + 200 + receipt.shippingCents);
});

test('no discount keeps the previous totals', () => {
  const receipt = checkout(cart, { taxRate: 0.1 });
  assert.equal(receipt.discountCents, 0);
  assert.equal(receipt.taxCents, 300);
});

test('negative discounts are rejected', () => {
  assert.throws(() => checkout(cart, { discountCents: -1 }), RangeError);
});

test('a discount larger than the subtotal is capped at the subtotal', () => {
  const receipt = checkout(cart, { discountCents: 5000 });
  assert.equal(receipt.discountCents, 3000);
  assert.equal(receipt.taxCents, 0);
});
