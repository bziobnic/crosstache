import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createStore,
  draftReducer,
  isDraftDirty,
  normalizeSecretDraft,
  operationEvent,
  operationResultStatus,
  createOwnerRegistry,
  bindOwnedRetry,
  MAX_OPERATION_TOMBSTONES,
  MAX_ROUTINE_OPERATION_HISTORY,
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

test('routine terminal operation history is bounded while active and durable failures survive', () => {
  let state = {};
  for (let index = 0; index < MAX_ROUTINE_OPERATION_HISTORY + 25; index++) {
    state = draftReducer(state, operationEvent(`request-${index}`, 'started'));
    state = draftReducer(state, operationEvent(`request-${index}`, 'succeeded'));
  }
  state = draftReducer(state, operationEvent('request-active', 'started'));
  state = draftReducer(state, {
    ...operationEvent('bulk-actionable', 'partially-succeeded', {
      failedNames: ['failed-name'],
    }),
    durable: true,
  });

  const operations = Object.values(state.operations);
  assert.equal(operations.filter(({ durable, status }) => !durable && status !== 'started').length,
    MAX_ROUTINE_OPERATION_HISTORY);
  assert.equal(state.operations['request-active'].status, 'started');
  assert.equal(state.operations['bulk-actionable'].durable, true);
  assert.equal(state.operations[`request-${MAX_ROUTINE_OPERATION_HISTORY + 24}`].status, 'succeeded');
  assert.ok(Object.keys(state.operationTerminals).length <= MAX_OPERATION_TOMBSTONES);
});

test('terminal operations are observable once, ignore double terminals, and dismiss durable state', () => {
  let state = draftReducer({}, operationEvent('one', 'started'));
  state = draftReducer(state, operationEvent('one', 'failed', { message: 'first' }));
  const terminal = state.operations.one;
  state = draftReducer(state, operationEvent('one', 'succeeded'));
  assert.deepEqual(state.operations.one, terminal);

  state = draftReducer(state, {
    ...operationEvent('bulk', 'failed', { failedNames: ['only-name'] }),
    durable: true,
  });
  state = draftReducer(state, { type: 'operation/dismiss', operationId: 'bulk' });
  assert.equal(state.operations.bulk, undefined);
  state = draftReducer(state, operationEvent('bulk', 'succeeded'));
  assert.equal(state.operations.bulk, undefined);
});

test('owner replacement clears handlers and invalidates late generations without retaining state', () => {
  const registry = createOwnerRegistry();
  const retry = { onclick: () => {} };
  const copy = { onclick: () => {} };
  const retained = { failedNames: ['private-name'], retry: () => {} };
  const first = registry.replace('secret-action', {
    retained,
    cleanup: () => {
      retry.onclick = null;
      copy.onclick = null;
    },
  });

  const second = registry.replace('secret-action', { retained: { failedNames: ['new-name'] } });

  assert.equal(retry.onclick, null);
  assert.equal(copy.onclick, null);
  assert.equal(registry.isCurrent('secret-action', first), false);
  assert.equal(registry.isCurrent('secret-action', second), true);
  registry.clear('secret-action', second);
  assert.equal(registry.has('secret-action'), false);
});

test('overlapping retry generations cannot inherit, unlock, duplicate, or publish over the owner', async () => {
  const registry = createOwnerRegistry();
  const button = { disabled: false, onclick: null };
  let releaseA;
  let releaseB;
  const gateA = new Promise((resolve) => { releaseA = resolve; });
  const gateB = new Promise((resolve) => { releaseB = resolve; });
  const published = [];
  let callsA = 0;
  let callsB = 0;

  const generationA = registry.replace('surface');
  bindOwnedRetry({
    registry,
    key: 'surface',
    generation: generationA,
    button,
    retry: async () => { callsA++; await gateA; return 'A'; },
    publish: (value) => published.push(value),
  });
  const pendingA = button.onclick();
  assert.equal(button.disabled, true);

  const generationB = registry.replace('surface');
  bindOwnedRetry({
    registry,
    key: 'surface',
    generation: generationB,
    button,
    retry: async () => { callsB++; await gateB; return 'B'; },
    publish: (value) => published.push(value),
  });
  assert.equal(button.disabled, false);
  const pendingB = button.onclick();
  await button.onclick();
  assert.equal(callsB, 1);
  assert.equal(button.disabled, true);

  releaseA();
  await pendingA;
  assert.equal(button.disabled, true);
  assert.deepEqual(published, []);

  releaseB();
  await pendingB;
  assert.equal(button.disabled, false);
  assert.deepEqual(published, ['B']);
  assert.equal(callsA, 1);
});
