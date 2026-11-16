import test from 'node:test';
import assert from 'node:assert/strict';
import middleware from '../../lib/middleware.js';
import presets from '../../lib/presets.js';

test('logger without a prefix is unchanged', () => {
  const lines = [];
  middleware.compose([middleware.logger(lines)])({ path: '/a' });
  assert.deepEqual(lines, ['> /a', '< /a']);
});
test('logger with a prefix', () => {
  const lines = [];
  middleware.compose([middleware.logger(lines, 'p:')])({ path: '/a' });
  assert.deepEqual(lines, ['p:> /a', 'p:< /a']);
});
test('verbose preset uses the [kit] prefix', () => {
  const lines = [];
  middleware.compose(presets.verbose(lines))({ path: '/a/' });
  assert.deepEqual(lines, ['[kit] > /a', '[kit] < /a']);
});
