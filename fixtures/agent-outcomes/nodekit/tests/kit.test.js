'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const createKit = require('..');

test('middleware runs in order', () => {
  const seen = [];
  const kit = createKit()
    .use((ctx, next) => { seen.push('a'); return next(); })
    .use((ctx, next) => { seen.push('b'); return next(); });
  kit.handle({ path: '/x' });
  assert.deepEqual(seen, ['a', 'b']);
});
