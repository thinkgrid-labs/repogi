import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatPrice } from '../../src/money/format.js';

test('negative amounts put the sign before the symbol', () => {
  assert.equal(formatPrice(-500), '-$5.00');
  assert.equal(formatPrice(-5), '-$0.05');
  assert.equal(formatPrice(-1234, 'EUR'), '-€12.34');
});

test('positive amounts are unchanged', () => {
  assert.equal(formatPrice(1234), '$12.34');
  assert.equal(formatPrice(0), '$0.00');
  assert.equal(formatPrice(5, 'GBP'), '£0.05');
});
