'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const validator = require('../lib/validator');

test('non-empty strings pass', () => {
  assert.equal(validator.check('x'), true);
  assert.equal(validator.check(''), false);
  assert.equal(validator.checkAll(['a', 'b']), true);
});
