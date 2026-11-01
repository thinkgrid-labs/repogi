import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatPrice } from '../src/money/format.js';

test('formats dollars', () => {
  assert.equal(formatPrice(1234), '$12.34');
  assert.equal(formatPrice(5, 'EUR'), '€0.05');
});
