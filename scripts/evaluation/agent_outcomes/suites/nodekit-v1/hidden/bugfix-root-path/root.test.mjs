import test from 'node:test';
import assert from 'node:assert/strict';
import middleware from '../../lib/middleware.js';

const run = (path) => {
  const ctx = { path };
  middleware.compose([middleware.trimPath])(ctx);
  return ctx.path;
};

test('root path stays root', () => assert.equal(run('/'), '/'));
test('repeated root slashes collapse to root', () => assert.equal(run('//'), '/'));
test('trailing slashes are still removed', () => {
  assert.equal(run('/users/'), '/users');
  assert.equal(run('/a/b//'), '/a/b');
  assert.equal(run('/a'), '/a');
});
