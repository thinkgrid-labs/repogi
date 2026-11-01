import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyTax } from '../src/cart/tax.js';

test('rounds half away from zero', () => {
  assert.equal(applyTax(250, 0.1), 25);
  assert.equal(applyTax(5, 0.1), 1);
});
