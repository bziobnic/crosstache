import test from 'node:test';
import assert from 'node:assert/strict';

import { createStore } from './store.js';
import {
  contextDetails,
  contextQuery,
  formatContextLine,
  mountContextRail,
} from './context.js';

const primary = Object.freeze({
  backend: 'az-prod',
  backend_kind: 'azure',
  vault: 'payments',
  workspace: {
    alias: 'work',
    entries: [
      { alias: 'work', backend: 'az-prod', vault: 'payments', default: true },
      { alias: 'stage', backend: 'local-stage', vault: 'sandbox', default: false },
    ],
  },
  project: { name: 'checkout', path: '/work/checkout' },
  environment: { name: 'prod' },
  sources: {
    backend: 'project-environment',
    vault: 'workspace-entry',
    workspace: 'project-environment',
    project: 'project',
    environment: 'project',
  },
  connection: { state: 'connected', message: null },
  capabilities: {
    secrets: true,
    vaults: false,
    files: false,
    soft_delete: true,
    purge: false,
  },
  version: '1.2.3',
});

const stage = Object.freeze({
  ...primary,
  backend: 'local-stage',
  backend_kind: 'local',
  vault: 'sandbox',
  workspace: { ...primary.workspace, alias: 'stage' },
  environment: { name: 'stage' },
  connection: { state: 'unavailable', message: 'The selected backend is unavailable.' },
  capabilities: { files: true, soft_delete: false, purge: false },
});

test('context line keeps backend and vault unambiguous', () => {
  assert.equal(formatContextLine({
    backend: { name: 'az-prod' },
    vault: { name: 'payments' },
    project: { name: 'checkout' },
    environment: { name: 'prod' },
  }), 'az-prod / payments · checkout · prod');
  assert.equal(formatContextLine(primary), 'az-prod / payments · checkout · prod');
});

test('context query binds alias, backend, and vault as one immutable scope', () => {
  assert.equal(
    contextQuery(primary),
    '?alias=work&backend=az-prod&vault=payments',
  );
});

test('context details disclose provenance, capability limits, connection, and version', () => {
  const details = contextDetails(primary);
  assert.deepEqual(details.values, [
    { label: 'Backend', value: 'az-prod (azure)', source: 'Project environment' },
    { label: 'Vault', value: 'payments', source: 'Workspace entry' },
    { label: 'Workspace', value: 'work', source: 'Project environment' },
    { label: 'Project', value: 'checkout — /work/checkout', source: 'Project' },
    { label: 'Environment', value: 'prod', source: 'Project' },
  ]);
  assert.equal(details.connection, 'Connected');
  assert.deepEqual(details.limitations, [
    'Vault management unavailable',
    'File storage unavailable',
    'Permanent purge unavailable',
  ]);
  assert.equal(details.version, '1.2.3');
});

class FakeElement {
  constructor() {
    this.value = '';
    this.textContent = '';
    this.hidden = false;
    this.disabled = false;
    this.innerHTML = '';
    this.children = [];
    this.dataset = {};
    this.listeners = new Map();
    this.attributes = new Map();
  }

  appendChild(child) { this.children.push(child); }
  replaceChildren(...children) { this.children = children; }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  removeEventListener(type) { this.listeners.delete(type); }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  removeAttribute(name) { this.attributes.delete(name); }
  async change(value) {
    this.value = value;
    await this.listeners.get('change')?.({ currentTarget: this });
  }
}

function fakeDocument() {
  const scoped = [new FakeElement(), new FakeElement()];
  const elements = new Map([
    ['context-line', new FakeElement()],
    ['context-backend-kind', new FakeElement()],
    ['context-connection', new FakeElement()],
    ['context-capabilities', new FakeElement()],
    ['context-details-list', new FakeElement()],
    ['workspace-select', new FakeElement()],
    ['context-error', new FakeElement()],
    ['context-error-message', new FakeElement()],
    ['context-version', new FakeElement()],
  ]);
  return {
    elements,
    scoped,
    getElementById(id) { return elements.get(id) ?? null; },
    createElement() { return new FakeElement(); },
    querySelectorAll(selector) {
      return selector === '[data-context-scoped]' ? scoped : [];
    },
  };
}

function reducer(state, event) {
  switch (event.type) {
    case 'context/loaded':
      return { ...state, context: event.context };
    case 'context/switch-started':
      return { ...state, contextSwitchPending: true };
    case 'context/switch-succeeded':
      return {
        ...state,
        context: event.context,
        initialSecrets: event.secrets,
        contextSwitchPending: false,
        contextError: null,
      };
    case 'context/switch-failed':
      return { ...state, contextSwitchPending: false, contextError: event.error };
    case 'context/switch-cancelled':
      return { ...state, contextSwitchPending: false };
    case 'mutation/pending':
      return { ...state, scopedMutationPending: Boolean(event.value) };
    case 'draft/save-pending':
      return { ...state, savePending: Boolean(event.value) };
    default:
      return state;
  }
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

async function mounted({ api, guardNavigation = async () => true, initial = primary } = {}) {
  const document = fakeDocument();
  const store = createStore({
    context: null,
    initialSecrets: null,
    contextSwitchPending: false,
    contextError: null,
    draft: null,
    savePending: false,
    scopedMutationPending: false,
  }, reducer);
  const calls = [];
  const request = api ?? (async (method, path, body) => {
    calls.push({ method, path, body });
    if (method === 'GET') return initial;
    return { context: stage, secrets: [{ name: 'stage-only' }] };
  });
  const rail = mountContextRail({ store, api: request, guardNavigation, document });
  await rail.ready;
  return { document, store, calls, rail };
}

test('mounted switch commits context and list together after the guard', async () => {
  const order = [];
  const api = async (method) => {
    order.push(method);
    if (method === 'GET') return primary;
    return { context: stage, secrets: [{ name: 'stage-only' }] };
  };
  const fixture = await mounted({
    api,
    guardNavigation: async () => { order.push('guard'); return true; },
  });

  await fixture.rail.switchTo('stage');

  assert.deepEqual(order, ['GET', 'guard', 'POST']);
  assert.equal(fixture.store.snapshot().context.backend, 'local-stage');
  assert.deepEqual(fixture.store.snapshot().initialSecrets, [{ name: 'stage-only' }]);
  assert.equal(fixture.document.getElementById('context-line').textContent,
    'local-stage / sandbox · checkout · stage');
});

test('an already guarded palette activation does not run the navigation guard twice', async () => {
  let guardCalls = 0;
  const fixture = await mounted({
    guardNavigation: async () => { guardCalls++; return false; },
  });

  assert.equal(await fixture.rail.switchTo('stage', { skipGuard: true }), true);
  assert.equal(guardCalls, 0);
  assert.equal(fixture.store.snapshot().context.backend, 'local-stage');
});

test('exact workspace targets cannot be remapped by alias while a guard is pending', async () => {
  const guard = deferred();
  let activationCalls = 0;
  const fixture = await mounted({
    guardNavigation: () => guard.promise,
    api: async (method) => {
      if (method === 'GET') return primary;
      activationCalls++;
      return { context: stage, secrets: [] };
    },
  });
  const switching = fixture.rail.switchTo({
    alias: 'stage',
    backend: 'local-stage',
    vault: 'sandbox',
  });
  fixture.store.dispatch({
    type: 'context/loaded',
    context: {
      ...primary,
      version: 'remapped',
      workspace: {
        ...primary.workspace,
        entries: primary.workspace.entries.map((entry) => (
          entry.alias === 'stage' ? { ...entry, backend: 'evil-remap', vault: 'other' } : entry
        )),
      },
    },
  });
  guard.resolve(true);

  assert.equal(await switching, false);
  assert.equal(activationCalls, 0);
});

test('exact workspace activation posts the complete approved tuple', async () => {
  const fixture = await mounted();
  await fixture.rail.switchTo({
    alias: 'stage',
    backend: 'local-stage',
    vault: 'sandbox',
  }, { skipGuard: true });
  assert.deepEqual(fixture.calls.at(-1), {
    method: 'POST',
    path: '/api/workspaces/activate',
    body: { alias: 'stage', backend: 'local-stage', vault: 'sandbox' },
  });
});

test('workspace activation emits exact operation lifecycle statuses with one operation ID', async () => {
  const fixture = await mounted();
  const events = [];
  fixture.store.subscribe((_snapshot, event) => {
    if (event.type === 'operation/status') events.push(event);
  });

  await fixture.rail.switchTo('stage');

  assert.deepEqual(events.map(({ status }) => status), ['started', 'succeeded']);
  assert.equal(events[0].operationId, events[1].operationId);
  assert.match(events[0].operationId, /^context-switch-/);
});

test('dirty draft rejection and save lock preserve the current context', async () => {
  let guardCalls = 0;
  let activationCalls = 0;
  const fixture = await mounted({
    guardNavigation: async () => { guardCalls++; return false; },
    api: async (method) => {
      if (method === 'GET') return primary;
      activationCalls++;
      return { context: stage, secrets: [] };
    },
  });
  const select = fixture.document.getElementById('workspace-select');

  select.value = 'stage';
  await fixture.rail.switchTo('stage');
  assert.equal(fixture.store.snapshot().context.backend, 'az-prod');
  assert.equal(select.value, 'work');
  assert.equal(activationCalls, 0);

  fixture.store.dispatch({ type: 'draft/save-pending', value: true });
  await fixture.rail.switchTo('stage');
  assert.equal(guardCalls, 1);
  assert.equal(activationCalls, 0);
  assert.equal(fixture.store.snapshot().context.backend, 'az-prod');

  fixture.store.dispatch({ type: 'draft/save-pending', value: false });
  fixture.store.dispatch({ type: 'mutation/pending', value: true });
  await fixture.rail.switchTo('stage');
  assert.equal(guardCalls, 1);
  assert.equal(activationCalls, 0);
  assert.equal(fixture.store.snapshot().context.backend, 'az-prod');
});

test('switch pending makes every scoped application surface inert immediately', async () => {
  const activation = deferred();
  const fixture = await mounted({
    api: async (method) => method === 'GET'
      ? primary
      : activation.promise,
  });

  const switching = fixture.rail.switchTo('stage');
  await Promise.resolve();

  assert.equal(fixture.store.snapshot().contextSwitchPending, true);
  assert.ok(fixture.document.scoped.every((surface) => surface.attributes.has('inert')));

  activation.resolve({ context: stage, secrets: [] });
  await switching;
  assert.ok(fixture.document.scoped.every((surface) => !surface.attributes.has('inert')));
});

test('activation cannot publish after scoped activity starts during its response window', async () => {
  const activation = deferred();
  const fixture = await mounted({
    api: async (method) => method === 'GET'
      ? primary
      : activation.promise,
  });

  const switching = fixture.rail.switchTo('stage');
  await Promise.resolve();
  fixture.store.dispatch({ type: 'mutation/pending', value: true });
  fixture.store.dispatch({ type: 'mutation/pending', value: false });
  activation.resolve({ context: stage, secrets: [{ name: 'wrong-scope' }] });

  assert.equal(await switching, false);
  assert.equal(fixture.store.snapshot().context.backend, 'az-prod');
  assert.equal(fixture.store.snapshot().initialSecrets, null);
  assert.equal(fixture.store.snapshot().contextSwitchPending, false);
});

test('obsolete out-of-order switch cannot replace the latest context', async () => {
  const first = deferred();
  const second = deferred();
  let activation = 0;
  const fixture = await mounted({
    api: async (method, _path, body) => {
      if (method === 'GET') return primary;
      activation++;
      const response = activation === 1 ? first.promise : second.promise;
      return response.then((result) => ({
        context: body.alias === 'work' ? primary : stage,
        secrets: result.secrets,
      }));
    },
  });

  const firstSwitch = fixture.rail.switchTo('stage');
  while (activation < 1) await Promise.resolve();
  const secondSwitch = fixture.rail.switchTo('work');
  while (activation < 2) await Promise.resolve();
  second.resolve({ secrets: [{ name: 'current' }] });
  await secondSwitch;
  first.resolve({ secrets: [{ name: 'stale' }] });
  await firstSwitch;

  assert.equal(fixture.store.snapshot().context.backend, 'az-prod');
  assert.deepEqual(fixture.store.snapshot().initialSecrets, [{ name: 'current' }]);
});

for (const missing of ['context', 'secrets']) {
  test(`a partial activation missing ${missing} rolls back to the prior snapshot`, async () => {
    const fixture = await mounted({
      api: async (method, path) => {
        if (method === 'GET') return primary;
        return {
          ...(missing === 'context' ? {} : { context: stage }),
          ...(missing === 'secrets' ? {} : { secrets: [] }),
        };
      },
    });

    await fixture.rail.switchTo('stage');

    assert.equal(fixture.store.snapshot().context.backend, 'az-prod');
    assert.equal(fixture.store.snapshot().initialSecrets, null);
    assert.equal(fixture.store.snapshot().contextSwitchPending, false);
    assert.match(fixture.store.snapshot().contextError.message, /activation/i);
  });
}
