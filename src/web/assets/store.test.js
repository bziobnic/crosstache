import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createStore,
  draftReducer,
  isDraftDirty,
  normalizeSecretDraft,
  operationEvent,
  operationResultStatus,
  safeDiagnostic,
} from './store.js';

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

test('operation events use the exact durable status vocabulary', () => {
  for (const status of ['started', 'succeeded', 'partially-succeeded', 'cancelled', 'failed']) {
    assert.deepEqual(operationEvent('operation-1', status), {
      type: 'operation/status',
      operationId: 'operation-1',
      status,
    });
  }
  assert.throws(() => operationEvent('operation-1', 'complete'), /status/i);
});

test('aggregate result status distinguishes total and partial failures', () => {
  assert.equal(operationResultStatus([{ ok: true }, { ok: true }]), 'succeeded');
  assert.equal(operationResultStatus([{ ok: true }, { ok: false }]), 'partially-succeeded');
  assert.equal(operationResultStatus([{ ok: false }, { ok: false }]), 'failed');
});

test('safe diagnostics retain only actionable scope and failed names', () => {
  const diagnostic = safeDiagnostic({
    code: 'xv-conflict',
    message: 'Could not delete selected items.',
    hint: 'Retry the failed names.',
    backend: 'local',
    vault: 'team',
    failedNames: ['one', 'two'],
    value: 'secret-value',
    note: 'private note',
    auth: 'private auth',
    headers: { Authorization: 'Bearer private' },
    details: { token: 'private token' },
  });

  assert.deepEqual(diagnostic, {
    code: 'xv-conflict',
    message: 'Could not delete selected items.',
    hint: 'Retry the failed names.',
    backend: 'local',
    vault: 'team',
    failedNames: ['one', 'two'],
  });
  assert.doesNotMatch(JSON.stringify(diagnostic), /secret-value|private note|private auth|Bearer|private token/);
});
