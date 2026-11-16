'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const { compose, logger, trimPath } = require('../lib/middleware');

test('trimPath removes trailing slashes', () => {
  const ctx = { path: '/users/' };
  compose([trimPath])(ctx);
  assert.equal(ctx.path, '/users');
});

test('logger records entry and exit', () => {
  const lines = [];
  compose([logger(lines)])({ path: '/a' });
  assert.deepEqual(lines, ['> /a', '< /a']);
});
