import test from 'node:test';
import assert from 'node:assert/strict';
import { guardNavigation } from './dialogs.js';

test('navigation guard keeps a dirty draft unless discard is confirmed', async () => {
  const draft = { baseline: { name: 'a' }, working: { name: 'b' } };
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => false }), false);
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => true }), true);
  assert.equal(await guardNavigation({ draft, savePending: true, confirmDiscard: async () => true }), false);
});

test('navigation guard proceeds without a draft or confirmation', async () => {
  assert.equal(await guardNavigation({ draft: null, savePending: false, confirmDiscard: async () => {
    throw new Error('confirmation should not be requested');
  } }), true);
});
