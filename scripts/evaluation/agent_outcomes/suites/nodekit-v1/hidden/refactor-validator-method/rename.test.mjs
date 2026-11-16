import test from 'node:test';
import assert from 'node:assert/strict';
import validator from '../../lib/validator.js';
import Session from '../../lib/session.js';

test('isNonEmpty replaces check', () => {
  assert.equal(validator.isNonEmpty('x'), true);
  assert.equal(validator.isNonEmpty(''), false);
  assert.equal(validator.isNonEmpty(3), false);
  assert.equal(validator.check, undefined);
});
test('checkAll and sessions keep working', () => {
  assert.equal(validator.checkAll(['a', 'b']), true);
  assert.equal(validator.checkAll(['a', '']), false);
  assert.equal(new Session('id', 10, 0).valid(5), true);
  assert.equal(new Session('', 10, 0).valid(5), false);
});
