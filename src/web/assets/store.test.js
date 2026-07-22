import test from 'node:test';
import assert from 'node:assert/strict';
import { createStore, draftReducer, isDraftDirty, normalizeSecretDraft } from './store.js';

test('draft normalization preserves secret whitespace and absent-versus-clear', () => {
  const baseline = normalizeSecretDraft({ name: ' db ', value: '  keep  ', note: undefined, folder: '' });
  assert.deepEqual(baseline, { name: 'db', value: '  keep  ', note: null, folder: '' });
  assert.equal(isDraftDirty({ baseline, working: structuredClone(baseline) }), false);
  const working = { ...baseline, note: '' };
  assert.equal(isDraftDirty({ baseline, working }), true);
});

test('draft reducer stores independent normalized baseline and working copies', () => {
  const state = draftReducer({}, {
    type: 'draft/open',
    draft: { name: ' db ', value: '  keep  ', note: undefined, folder: '' },
  });
  assert.deepEqual(state.draft.baseline, { name: 'db', value: '  keep  ', note: null, folder: '' });
  assert.deepEqual(state.draft.working, state.draft.baseline);
  assert.notEqual(state.draft.working, state.draft.baseline);
});

test('store dispatches immutable state snapshots and events', () => {
  const store = createStore({ count: 0 }, (state, event) => ({ count: state.count + event.by }));
  let notification;
  store.subscribe((snapshot, event) => { notification = { snapshot: structuredClone(snapshot), event }; });
  const result = store.dispatch({ by: 2 });
  result.count = 7;
  assert.deepEqual(store.snapshot(), { count: 2 });
  assert.deepEqual(notification, { snapshot: { count: 2 }, event: { by: 2 } });
});
