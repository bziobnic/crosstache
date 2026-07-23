import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createStore, draftReducer } from './store.js';
import { createDialogManager } from './dialogs.js';
import { ApiError } from './api-client.js';
import * as XvUiModel from './ui-model.js';
import { mountContextRail } from './context.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const { PROTECTED_MASK } = XvUiModel;

class Element {
  constructor(id, document) {
    this.key = id;
    this.id = id.startsWith('#') ? id.slice(1) : id;
    this.document = document;
    this.hidden = false;
    this.disabled = false;
    this.value = '';
    this.textContent = '';
    this._innerHTML = '';
    this.dataset = {};
    this.children = [];
    this.attributes = new Map();
    const styles = new Map();
    this.style = {
      setProperty: (name, value) => styles.set(name, String(value)),
      getPropertyValue: (name) => styles.get(name) || '',
    };
    this.classes = new Set();
    this.classList = {
      add: (name) => this.classes.add(name),
      remove: (name) => this.classes.delete(name),
      toggle: (name, enabled) => (enabled ? this.classes.add(name) : this.classes.delete(name)),
      contains: (name) => this.classes.has(name),
    };
    this.listeners = new Map();
  }

  get innerHTML() { return this._innerHTML; }
  set innerHTML(value) { this._innerHTML = value; if (value === '') this.children = []; }
  get childNodes() { return this.children; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  removeAttribute(name) { this.attributes.delete(name); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  appendChild(child) { this.children.push(child); return child; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  querySelectorAll(selector) {
    const matches = [];
    const visit = (element) => {
      for (const child of element.children || []) {
        if (typeof child !== 'object') continue;
        if (selector === 'input[data-field-kind="secret"]' && child.dataset?.fieldKind === 'secret') {
          matches.push(child);
        }
        if (selector === '[role="treeitem"]' && child.getAttribute?.('role') === 'treeitem') {
          matches.push(child);
        }
        visit(child);
      }
    };
    visit(this);
    return matches;
  }
  querySelector(selector) { return this.document.element(`${this.key} ${selector}`); }
  addEventListener(type, listener) { this.listeners.set(type, listener); }
  dispatch(type, event = {}) { return this.listeners.get(type)?.({ preventDefault() {}, target: this, ...event }); }
  focus() { this.document.activeElement = this; }
  contains(target) {
    if (this === target) return true;
    return this.children.some((child) => child?.contains?.(target));
  }
}

function createDocument() {
  const elements = new Map();
  const document = {
    activeElement: null,
    listeners: new Map(),
    element(id) {
      if (!elements.has(id)) elements.set(id, new Element(id, document));
      return elements.get(id);
    },
    getElementById(id) { return document.element(`#${id}`); },
    querySelector(selector) {
      if (selector.endsWith('-table')) {
        const table = document.element(selector);
        table.clientWidth = 100;
        return table;
      }
      return document.element(selector);
    },
    querySelectorAll() { return []; },
    createElement(name) { return new Element(name, document); },
    createElementNS(_namespace, name) { return new Element(name, document); },
    createTextNode(value) { return value; },
    addEventListener(type, listener) { document.listeners.set(type, listener); },
    dispatch(type, event = {}) { return document.listeners.get(type)?.({ preventDefault() {}, ...event }); },
  };
  const form = document.element('#secret-form');
  document.element('#drawer').hidden = true;
  document.element('#drawer-backdrop').hidden = true;
  form.elements = Object.fromEntries(['name', 'value', 'folder', 'groups', 'note', 'expires_on']
    .map((name) => [name, document.element(`#field-${name}`)]));
  form.reset = () => {
    for (const field of Object.values(form.elements)) field.value = '';
  };
  document.element('#drawer').querySelectorAll = () => [
    form.elements.name,
    form.elements.value,
    document.element('#close-drawer'),
    document.element('#save'),
  ];
  for (const selector of [
    '#secret-error',
    '#secret-refresh-error',
    '#file-error',
    '#file-refresh-error',
    '#secret-form-error',
  ]) {
    document.element(selector).hidden = true;
  }
  document.element('#protected-value-status').hidden = true;
  return { document, elements };
}

function exposureClock() {
  let nextId = 0;
  const scheduled = new Map();
  return {
    setTimeout(callback, delay) {
      const id = ++nextId;
      scheduled.set(id, { callback, delay });
      return id;
    },
    clearTimeout(id) { scheduled.delete(id); },
    advanceOneSecond() {
      for (const [id, task] of [...scheduled]) {
        if (task.delay !== 1000) continue;
        scheduled.delete(id);
        task.callback();
      }
    },
  };
}

function findElement(root, predicate) {
  if (predicate(root)) return root;
  for (const child of root.children || []) {
    if (typeof child !== 'object') continue;
    const match = findElement(child, predicate);
    if (match) return match;
  }
  return null;
}

function findElements(root, predicate) {
  const matches = [];
  const visit = (element) => {
    if (predicate(element)) matches.push(element);
    for (const child of element.children || []) {
      if (typeof child === 'object') visit(child);
    }
  };
  visit(root);
  return matches;
}

async function mountRouteUi({
  failSave = false,
  apiImpl = null,
  tauriEvents = null,
  clipboard = { readText: async () => '', writeText: async () => {} },
  clock = globalThis,
  preferences = null,
  withContextRail = false,
  guardNavigation = async () => true,
  storageValues = new Map(),
} = {}) {
  const { document, elements } = createDocument();
  const previous = new Map(['document', 'navigator', 'localStorage', '__TAURI__', 'addEventListener', 'removeEventListener']
    .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const windowListeners = new Map();
  Object.defineProperty(globalThis, 'document', { configurable: true, value: document });
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard } });
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (key) => storageValues.get(key) ?? null,
      setItem: (key, value) => storageValues.set(key, value),
      removeItem: (key) => storageValues.delete(key),
      get length() { return storageValues.size; },
      key: (index) => [...storageValues.keys()][index] ?? null,
    },
  });
  Object.defineProperty(globalThis, 'addEventListener', {
    configurable: true,
    value: (type, listener) => windowListeners.set(type, listener),
  });
  Object.defineProperty(globalThis, 'removeEventListener', {
    configurable: true,
    value: (type) => windowListeners.delete(type),
  });
  if (tauriEvents) Object.defineProperty(globalThis, '__TAURI__', { configurable: true, value: { event: tauriEvents } });
  const api = apiImpl || (async (_method, path) => {
    if (failSave && _method === 'PUT') throw new Error('save failed');
    if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
    if (path === '/api/types') return { types: [] };
    if (path === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
    if (path.startsWith('/api/secrets')) return [];
    return [];
  });
  const confirmations = [];
  const { mountSecrets } = await import(`${pathToFileURL(path.join(__dirname, 'secrets.js')).href}?routes=${Date.now()}`);
  const store = createStore({
    context: null,
    initialSecrets: null,
    contextSwitchPending: false,
    scopedMutationPending: false,
    contextError: null,
    draft: null,
    savePending: false,
  }, draftReducer);
  const dialogs = createDialogManager(document);
  dialogs.confirmDiscard = () => { confirmations.push(true); return true; };
  const contextRail = withContextRail
    ? mountContextRail({ store, api, guardNavigation, document })
    : null;
  mountSecrets({
    api,
    store,
    dialogs,
    preferences,
    token: 'test',
    exposureClock: clock,
    contextRail,
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  return {
    document,
    elements,
    store,
    contextRail,
    confirmations,
    dispatchWindow(type, event = {}) { return windowListeners.get(type)?.(event); },
    find(selector, predicate) { return findElement(document.element(selector), predicate); },
    findAll(selector, predicate) { return findElements(document.element(selector), predicate); },
    async openDirty() {
      const invoker = elements.get('#new-secret');
      document.activeElement = invoker;
      await invoker.onclick({ currentTarget: invoker });
      elements.get('#field-name').value = 'changed';
      elements.get('#secret-form').dispatch('input');
    },
    restore() {
      for (const [key, descriptor] of previous) {
        if (descriptor) Object.defineProperty(globalThis, key, descriptor);
        else delete globalThis[key];
      }
    },
  };
}

function routedContext(alias, vault) {
  return {
    backend: 'local',
    backend_kind: 'local',
    vault,
    workspace: {
      alias,
      entries: [
        { alias: 'primary', backend: 'local', vault: 'one', default: true },
        { alias: 'stage', backend: 'local', vault: 'two', default: false },
      ],
    },
    project: null,
    environment: null,
    sources: {},
    connection: { state: 'connected', message: null },
    capabilities: {
      secrets: true,
      files: false,
      soft_delete: true,
      restore: true,
      purge: true,
    },
    version: 'test',
  };
}

async function settleUntil(predicate) {
  for (let attempt = 0; attempt < 20 && !predicate(); attempt++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  assert.ok(predicate(), 'expected asynchronous action to start');
}

test('workspace selector has one pending owner across every switch outcome and operation lock', { timeout: 10_000 }, async () => {
  const primary = routedContext('primary', 'one');
  const stage = routedContext('stage', 'two');
  const activations = [];
  const api = async (method, requestPath) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (method === 'POST' && requestPath === '/api/workspaces/activate') {
      const activation = {};
      activation.promise = new Promise((resolve, reject) => {
        activation.resolve = resolve;
        activation.reject = reject;
      });
      activations.push(activation);
      return activation.promise;
    }
    if (requestPath === '/api/types') return { types: [] };
    if (requestPath.startsWith('/api/secrets')) return [];
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, withContextRail: true });
  try {
    const selector = ui.elements.get('#workspace-select');
    assert.equal(selector.disabled, false);

    const succeeded = ui.contextRail.switchTo('stage');
    await settleUntil(() => activations[0]);
    assert.equal(selector.disabled, true);
    activations[0].resolve({ context: stage, secrets: [] });
    assert.equal(await succeeded, true);
    assert.equal(selector.disabled, false, 'activation success unlocks the selector');

    const failed = ui.contextRail.switchTo('primary');
    await settleUntil(() => activations[1]);
    assert.equal(selector.disabled, true);
    activations[1].reject(new Error('activation failed'));
    assert.equal(await failed, false);
    assert.equal(selector.disabled, false, 'activation failure unlocks the selector');

    const cancelled = ui.contextRail.switchTo('primary');
    await settleUntil(() => activations[2]);
    ui.store.dispatch({ type: 'mutation/pending', value: true });
    ui.store.dispatch({ type: 'mutation/pending', value: false });
    activations[2].resolve({ context: primary, secrets: [] });
    assert.equal(await cancelled, false);
    assert.equal(selector.disabled, false, 'activity-revision cancellation unlocks the selector');

    for (const [event, value] of [
      ['draft/save-pending', true],
      ['draft/save-pending', false],
      ['mutation/pending', true],
      ['mutation/pending', false],
    ]) {
      ui.store.dispatch({ type: event, value });
      assert.equal(selector.disabled, value, `${event}=${value}`);
    }
  } finally {
    ui.restore();
  }

  const guarded = await mountRouteUi({
    apiImpl: api,
    withContextRail: true,
    guardNavigation: async () => false,
  });
  try {
    const selector = guarded.elements.get('#workspace-select');
    assert.equal(await guarded.contextRail.switchTo('stage'), false);
    assert.equal(selector.disabled, false, 'guard cancellation never locks the selector');
  } finally {
    guarded.restore();
  }
});

test('failed and cancelled workspace activation preserve both current-scope stale refresh owners', async () => {
  const primary = {
    ...routedContext('primary', 'one'),
    capabilities: { ...routedContext('primary', 'one').capabilities, files: true },
  };
  const stage = {
    ...routedContext('stage', 'two'),
    capabilities: { ...routedContext('stage', 'two').capabilities, files: true },
  };
  const activations = [];
  let failSecretRefresh = false;
  let failFileRefresh = false;
  const api = async (method, requestPath) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (method === 'POST' && requestPath === '/api/workspaces/activate') {
      const activation = {};
      activation.promise = new Promise((resolve, reject) => {
        activation.resolve = resolve;
        activation.reject = reject;
      });
      activations.push(activation);
      return activation.promise;
    }
    if (requestPath === '/api/types') return { types: [] };
    if (method === 'GET' && requestPath.startsWith('/api/secrets?')) {
      if (failSecretRefresh) {
        failSecretRefresh = false;
        throw new ApiError({
          status: 503,
          code: 'xv-offline',
          message: 'Current secrets are stale',
          hint: 'Retry.',
        });
      }
      return [];
    }
    if (method === 'GET' && requestPath.startsWith('/api/files?')) {
      if (failFileRefresh) {
        failFileRefresh = false;
        throw new ApiError({
          status: 503,
          code: 'xv-offline',
          message: 'Current files are stale',
          hint: 'Retry.',
        });
      }
      return [];
    }
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, withContextRail: true });
  const pendingSwitches = [];
  try {
    failSecretRefresh = true;
    await ui.elements.get('#refresh-secrets').onclick();
    failFileRefresh = true;
    await ui.elements.get('#refresh-files').onclick();
    const secretPanel = ui.elements.get('#secret-refresh-error');
    const filePanel = ui.elements.get('#file-refresh-error');
    const secretRetry = secretPanel.querySelector('.error-retry');
    const fileRetry = filePanel.querySelector('.error-retry');
    const originalSecretRetry = secretRetry.onclick;
    const originalFileRetry = fileRetry.onclick;

    const failed = ui.contextRail.switchTo('stage');
    pendingSwitches.push(failed);
    await settleUntil(() => activations[0]);
    assert.equal(secretPanel.hidden, false, 'pending activation retains the current secret warning');
    assert.equal(filePanel.hidden, false, 'pending activation retains the current file warning');
    assert.equal(secretRetry.onclick, originalSecretRetry);
    assert.equal(fileRetry.onclick, originalFileRetry);
    activations[0].reject(new Error('activation failed'));
    assert.equal(await failed, false);
    assert.equal(secretPanel.hidden, false);
    assert.equal(filePanel.hidden, false);
    assert.equal(secretRetry.onclick, originalSecretRetry);
    assert.equal(fileRetry.onclick, originalFileRetry);

    const cancelled = ui.contextRail.switchTo('stage');
    pendingSwitches.push(cancelled);
    await settleUntil(() => activations[1]);
    ui.store.dispatch({ type: 'mutation/pending', value: true });
    ui.store.dispatch({ type: 'mutation/pending', value: false });
    activations[1].resolve({ context: stage, secrets: [] });
    assert.equal(await cancelled, false);
    assert.equal(secretPanel.hidden, false);
    assert.equal(filePanel.hidden, false);
    assert.equal(secretRetry.onclick, originalSecretRetry);
    assert.equal(fileRetry.onclick, originalFileRetry);

    assert.equal(await originalSecretRetry(), false);
    assert.equal(await originalFileRetry(), false);
    assert.equal(secretPanel.hidden, true);
    assert.equal(filePanel.hidden, true);
  } finally {
    for (const activation of activations) activation.resolve?.({ context: stage, secrets: [] });
    await Promise.allSettled(pendingSwitches);
    ui.restore();
  }
});

test('current-context refresh settles after activation failure and remains retryable without loading limbo', async () => {
  const primary = {
    ...routedContext('primary', 'one'),
    capabilities: { ...routedContext('primary', 'one').capabilities, files: true },
  };
  const activation = {};
  activation.promise = new Promise((resolve, reject) => {
    activation.resolve = resolve;
    activation.reject = reject;
  });
  const refresh = {};
  refresh.promise = new Promise((resolve, reject) => {
    refresh.resolve = resolve;
    refresh.reject = reject;
  });
  let secretListCalls = 0;
  let activationStarted = false;
  const api = async (method, requestPath) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (method === 'POST' && requestPath === '/api/workspaces/activate') {
      activationStarted = true;
      return activation.promise;
    }
    if (requestPath === '/api/types') return { types: [] };
    if (method === 'GET' && requestPath.startsWith('/api/secrets?')) {
      secretListCalls++;
      if (secretListCalls === 2) return refresh.promise;
      return [{ name: 'existing' }];
    }
    if (method === 'GET' && requestPath.startsWith('/api/files?')) return [];
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, withContextRail: true });
  let switching;
  try {
    const refreshing = ui.elements.get('#refresh-secrets').onclick();
    switching = ui.contextRail.switchTo('stage');
    await settleUntil(() => activationStarted);
    activation.reject(new Error('activation failed'));
    assert.equal(await switching, false);

    refresh.reject(new ApiError({
      status: 503,
      code: 'xv-offline',
      message: 'Refresh settled after failed activation',
      hint: 'Retry.',
    }));
    assert.equal(await refreshing, false);

    const panel = ui.elements.get('#secret-refresh-error');
    assert.equal(panel.hidden, false);
    assert.match(panel.querySelector('.error-message').textContent, /Refresh settled after failed activation/);
    assert.match(ui.elements.get('#secret-list-summary').textContent, /^1 secret/);
    assert.ok(ui.find('#secrets-table tbody', (element) => (
      element.getAttribute?.('aria-label') === 'Edit secret existing'
    )));

    await panel.querySelector('.error-retry').onclick();
    assert.equal(panel.hidden, true);
    assert.equal(secretListCalls, 3);
    assert.match(ui.elements.get('#secret-list-summary').textContent, /^1 secret/);
  } finally {
    refresh.resolve([]);
    activation.resolve({ context: primary, secrets: [] });
    ui.restore();
  }
});

test('successful workspace commit clears both refresh owners before new-scope files settle', async () => {
  const primary = {
    ...routedContext('primary', 'one'),
    capabilities: { ...routedContext('primary', 'one').capabilities, files: true },
  };
  const stage = {
    ...routedContext('stage', 'two'),
    capabilities: { ...routedContext('stage', 'two').capabilities, files: true },
  };
  const activation = deferred();
  const stageFiles = deferred();
  let failSecretRefresh = false;
  let failFileRefresh = false;
  let stageFileStarted = false;
  const api = async (method, requestPath) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (method === 'POST' && requestPath === '/api/workspaces/activate') return activation.promise;
    if (requestPath === '/api/types') return { types: [] };
    if (method === 'GET' && requestPath.startsWith('/api/secrets?')) {
      if (failSecretRefresh) {
        failSecretRefresh = false;
        throw new ApiError({ status: 503, code: 'xv-offline', message: 'Stale secrets' });
      }
      return [];
    }
    if (method === 'GET' && requestPath.startsWith('/api/files?')) {
      const vault = new URLSearchParams(requestPath.split('?')[1] || '').get('vault');
      if (failFileRefresh) {
        failFileRefresh = false;
        throw new ApiError({ status: 503, code: 'xv-offline', message: 'Stale files' });
      }
      if (vault === 'two') {
        stageFileStarted = true;
        return stageFiles.promise;
      }
      return [];
    }
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, withContextRail: true });
  let switching;
  try {
    failSecretRefresh = true;
    await ui.elements.get('#refresh-secrets').onclick();
    failFileRefresh = true;
    await ui.elements.get('#refresh-files').onclick();
    const secretPanel = ui.elements.get('#secret-refresh-error');
    const filePanel = ui.elements.get('#file-refresh-error');
    const secretRetry = secretPanel.querySelector('.error-retry');
    const fileRetry = filePanel.querySelector('.error-retry');

    switching = ui.contextRail.switchTo('stage');
    await Promise.resolve();
    assert.equal(secretPanel.hidden, false, 'pending activation keeps current warning visible');
    assert.equal(filePanel.hidden, false, 'pending activation keeps current warning visible');
    activation.resolve({ context: stage, secrets: [] });
    assert.equal(await switching, true);
    await settleUntil(() => stageFileStarted);

    assert.equal(secretPanel.hidden, true);
    assert.equal(filePanel.hidden, true);
    assert.equal(secretRetry.onclick, null);
    assert.equal(fileRetry.onclick, null);
    assert.equal(secretRetry.disabled, false);
    assert.equal(fileRetry.disabled, false);
  } finally {
    stageFiles.resolve([]);
    activation.resolve({ context: stage, secrets: [] });
    await Promise.allSettled(switching ? [switching] : []);
    await new Promise((resolve) => setTimeout(resolve, 0));
    ui.restore();
  }
});

test('mounted folder navigation filters rows and restores expansion without leaking selection across workspaces', async () => {
  const primary = routedContext('primary', 'one');
  const stage = routedContext('stage', 'two');
  const primarySecrets = [
    { name: 'prod-secret', folder: 'apps/prod' },
    ...Array.from({ length: 50 }, (_, index) => ({ name: `loose-${index}`, folder: null })),
  ];
  const stageSecrets = [{ name: 'stage-secret', folder: 'other/nested' }];
  const api = async (method, requestPath, body) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (method === 'POST' && requestPath === '/api/workspaces/activate') {
      return body.alias === 'stage'
        ? { context: stage, secrets: stageSecrets }
        : { context: primary, secrets: primarySecrets };
    }
    if (requestPath === '/api/types') return { types: [] };
    if (method === 'POST' && requestPath.startsWith('/api/folder-tokens')) {
      const scopeCharacter = requestPath.includes('vault=two') ? 'T' : 'S';
      return {
        version: 1,
        scope_token: scopeCharacter.repeat(43),
        folders: body.folders.map((folder, index) => ({
          path: folder,
          token: String.fromCharCode(65 + index).repeat(43),
        })),
      };
    }
    if (method === 'GET' && requestPath.startsWith('/api/secrets')) return primarySecrets;
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, withContextRail: true });
  try {
    const treeitems = () => ui.findAll('#secrets-folder-tree', (element) => (
      element.getAttribute?.('role') === 'treeitem'
    ));
    const item = (label) => treeitems().find((element) => (
      element.getAttribute('aria-label')?.startsWith(`${label},`)
    ));

    assert.equal(item('apps').getAttribute('aria-expanded'), 'false', '51 items start collapsed');
    item('apps').focus();
    item('apps').onkeydown({ key: 'ArrowRight', preventDefault() {} });
    assert.equal(item('apps').getAttribute('aria-expanded'), 'true');
    assert.ok(item('prod'), 'nested child becomes visible');

    item('apps').onclick();
    const visiblePrimaryRows = ui.findAll('#secrets-table tbody', (element) => (
      element.getAttribute?.('aria-label')?.startsWith('Edit secret ')
    ));
    assert.deepEqual(
      visiblePrimaryRows.map((element) => element.getAttribute('aria-label')),
      ['Edit secret prod-secret'],
    );
    assert.equal(item('apps').getAttribute('aria-selected'), 'true');
    assert.match(ui.elements.get('#secret-list-summary').textContent, /^1 of 51 secrets/);

    ui.store.dispatch({
      type: 'context/switch-succeeded',
      context: {
        ...primary,
        workspace: { ...primary.workspace, alias: 'same-vault-alias' },
      },
      secrets: primarySecrets,
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(
      item('All items').getAttribute('aria-selected'),
      'true',
      'selection resets even when a different workspace alias targets the same backend and vault',
    );

    assert.equal(await ui.contextRail.switchTo('stage'), true);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(item('All items').getAttribute('aria-selected'), 'true');
    assert.equal(item('other').getAttribute('aria-expanded'), 'true', 'small workspace expands');
    assert.equal(item('apps'), undefined, 'prior workspace folders are absent');

    assert.equal(await ui.contextRail.switchTo('primary'), true);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(item('All items').getAttribute('aria-selected'), 'true', 'folder selection resets');
    assert.equal(item('apps').getAttribute('aria-expanded'), 'true', 'scoped expansion restores');
    assert.ok(item('prod'));
  } finally {
    ui.restore();
  }
});

test('mounted local search and composable filters stay metadata-only and expose removable controls', async () => {
  const context = {
    ...routedContext('primary', 'one'),
    capabilities: { ...routedContext('primary', 'one').capabilities, files: true },
  };
  const secretRows = [
    {
      name: 'prod-login',
      folder: 'prod',
      groups: ['ops'],
      note: 'needle-private-note',
      tags: { 'xv-type': 'login' },
      enabled: true,
      expires_on: '2035-01-01T00:00:00Z',
    },
    {
      name: 'disabled-database',
      folder: 'prod',
      groups: ['ops'],
      tags: { 'xv-type': 'database' },
      enabled: false,
    },
  ];
  const fileRows = [
    { name: 'prod/report.pdf', size: 4, content_type: 'application/pdf' },
    { name: 'dev/readme.txt', size: 3, content_type: 'text/plain' },
  ];
  const api = async (method, requestPath) => {
    if (method === 'GET' && requestPath === '/api/context') return context;
    if (requestPath === '/api/types') return { types: [] };
    if (requestPath === '/api/vaults') return { vaults: [{ name: 'one' }] };
    if (method === 'GET' && requestPath.startsWith('/api/secrets')) return secretRows;
    if (method === 'GET' && requestPath.startsWith('/api/files')) return fileRows;
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api });
  try {
    const visibleNames = (surface, prefix) => ui.findAll(
      `#${surface}-table tbody`,
      (element) => element.getAttribute?.('aria-label')?.startsWith(prefix),
    ).map((element) => element.getAttribute('aria-label'));

    const search = ui.elements.get('#search');
    search.value = 'needle-private-note';
    search.oninput();
    assert.deepEqual(visibleNames('secrets', 'Edit secret '), [], 'notes are never searchable');
    assert.equal(ui.elements.get('#secret-search-clear').hidden, false);
    ui.elements.get('#secret-search-clear').onclick();
    assert.equal(search.value, '');

    const enabled = ui.elements.get('#secret-filter-enabled');
    enabled.value = 'false';
    enabled.onchange();
    assert.equal(ui.elements.get('#secret-item-count').textContent, '1 / 2 secrets');
    assert.deepEqual(visibleNames('secrets', 'Edit secret '), ['Edit secret disabled-database']);
    const statusChip = ui.find('#secret-filter-chips', (element) => (
      element.getAttribute?.('aria-label') === 'Remove Status: disabled filter'
    ));
    assert.ok(statusChip);
    statusChip.onclick();
    assert.deepEqual(visibleNames('secrets', 'Edit secret '), [
      'Edit secret disabled-database',
      'Edit secret prod-login',
    ]);

    await ui.elements.get('#tab-files').onclick();
    const fileSearch = ui.elements.get('#file-search');
    fileSearch.value = 'APPLICATION/PDF';
    fileSearch.oninput();
    assert.deepEqual(ui.findAll('#files-table tbody', (element) => (
      element.key === 'strong' && element.textContent
    )).map((element) => element.textContent), ['prod/report.pdf']);
    assert.equal(ui.elements.get('#file-search-clear').hidden, false);
  } finally {
    ui.restore();
  }
});

test('empty list refresh persists pruning and cleans legacy folder state before a later re-add', async () => {
  const context = routedContext('primary', 'one');
  const apps = XvUiModel.folderIdentity('apps');
  const prod = XvUiModel.folderIdentity('apps/prod');
  const populated = [
    { name: 'prod-secret', folder: prod.path },
    ...Array.from({ length: 50 }, (_, index) => ({ name: `loose-${index}`, folder: null })),
  ];
  let listed = populated;
  const tokenBodies = [];
  const storageValues = new Map();
  const api = async (method, requestPath, body) => {
    if (method === 'GET' && requestPath === '/api/context') return context;
    if (requestPath === '/api/types') return { types: [] };
    if (requestPath === '/api/vaults') return { vaults: [{ name: 'one' }] };
    if (method === 'GET' && requestPath.startsWith('/api/secrets')) return listed;
    if (method === 'POST' && requestPath.startsWith('/api/folder-tokens')) {
      tokenBodies.push(body);
      return {
        version: 1,
        scope_token: 'S'.repeat(43),
        folders: body.folders.map((folder, index) => ({
          path: folder,
          token: String.fromCharCode(65 + index).repeat(43),
        })),
      };
    }
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api, storageValues });
  try {
    const item = (label) => ui.findAll('#secrets-folder-tree', (element) => (
      element.getAttribute?.('role') === 'treeitem'
      && element.getAttribute('aria-label')?.startsWith(`${label},`)
    ))[0];
    item('apps').onkeydown({ key: 'ArrowRight', preventDefault() {} });
    const v4Key = [...storageValues.keys()].find((key) => (
      key.startsWith('xv.ui.folder-expansion.v4:')
    ));
    assert.ok(v4Key);
    assert.match(storageValues.get(v4Key), /A{43}/);
    storageValues.set('xv.ui.folder-expansion.v1', 'true');
    storageValues.set('xv.ui.folder-expansion.v2:raw:scope:secrets', '["apps"]');
    storageValues.set('xv.ui.folder-expansion.v3:oldhash', '{"version":3}');

    listed = [{ name: 'unfiled-only', folder: null }];
    ui.elements.get('#vault-select').value = 'one';
    await ui.elements.get('#vault-select').onchange();
    await settleUntil(() => tokenBodies.length === 2 && !item('apps'));

    assert.deepEqual(tokenBodies[1], { surface: 'secrets', folders: [] });
    assert.deepEqual(JSON.parse(storageValues.get(v4Key)), { version: 4, expanded: [] });
    assert.equal(
      [...storageValues.keys()].some((key) => /^xv\.ui\.folder-expansion\.v[1-3]/.test(key)),
      false,
    );

    const tokenIndex = XvUiModel.createFolderTokenIndex({
      version: 1,
      scope_token: 'S'.repeat(43),
      folders: [
        { path: apps.path, token: 'A'.repeat(43) },
        { path: prod.path, token: 'B'.repeat(43) },
      ],
    });
    const fresh = XvUiModel.createFolderNavigationState(globalThis.localStorage);
    fresh.sync({ backend: 'local', vault: 'one', surface: 'secrets' }, {
      total: 51,
      folderIds: [apps, prod],
      expandableIds: [apps],
      tokenIndex,
    });
    assert.deepEqual(fresh.snapshot().expanded, []);
  } finally {
    ui.restore();
  }
});

test('stale empty-folder token response cannot hydrate a switched workspace', async () => {
  const primary = routedContext('primary', 'one');
  const stage = routedContext('stage', 'two');
  let listed = [
    { name: 'prod-secret', folder: 'apps/prod' },
    ...Array.from({ length: 50 }, (_, index) => ({ name: `loose-${index}`, folder: null })),
  ];
  let emptyTokenRequest;
  const api = async (method, requestPath, body) => {
    if (method === 'GET' && requestPath === '/api/context') return primary;
    if (requestPath === '/api/types') return { types: [] };
    if (requestPath === '/api/vaults') {
      return { vaults: [{ name: 'one' }, { name: 'two' }] };
    }
    if (method === 'GET' && requestPath.startsWith('/api/secrets')) return listed;
    if (method === 'POST' && requestPath.startsWith('/api/folder-tokens')) {
      if (body.folders.length === 0) {
        return new Promise((resolve) => { emptyTokenRequest = { resolve }; });
      }
      const stageScope = requestPath.includes('vault=two');
      return {
        version: 1,
        scope_token: (stageScope ? 'T' : 'S').repeat(43),
        folders: body.folders.map((folder, index) => ({
          path: folder,
          token: String.fromCharCode(65 + index).repeat(43),
        })),
      };
    }
    return [];
  };
  const ui = await mountRouteUi({ apiImpl: api });
  try {
    listed = [];
    ui.elements.get('#vault-select').value = 'one';
    ui.elements.get('#vault-select').onchange();
    await settleUntil(() => emptyTokenRequest);

    ui.store.dispatch({
      type: 'context/switch-succeeded',
      context: stage,
      secrets: [{ name: 'nested', folder: 'other/nested' }],
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    emptyTokenRequest.resolve({
      version: 1,
      scope_token: 'S'.repeat(43),
      folders: [],
    });
    await new Promise((resolve) => setTimeout(resolve, 0));

    const labels = ui.findAll('#secrets-folder-tree', (element) => (
      element.getAttribute?.('role') === 'treeitem'
    )).map((element) => element.getAttribute('aria-label'));
    assert.ok(labels.some((label) => label.startsWith('other,')));
    assert.equal(labels.some((label) => label.startsWith('apps,')), false);
    assert.equal(ui.elements.get('#secret-list-summary').textContent.includes('1 secret'), true);
  } finally {
    ui.restore();
  }
});

async function openExistingSecret(ui, name) {
  const button = ui.find('#secrets-table tbody', (element) => (
    element.getAttribute?.('aria-label') === `Edit secret ${name}`
  ));
  assert.ok(button, `edit control for ${name} is rendered`);
  button.onclick({ stopPropagation() {} });
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test('drawer routes guard cancel, Escape, backdrop, tabs, vault changes, and competing edits', async () => {
  const ui = await mountRouteUi();
  try {
    await ui.openDirty();
    await ui.elements.get('#close-drawer').onclick();
    assert.equal(ui.elements.get('#drawer').hidden, true);
    assert.equal(ui.document.activeElement, ui.elements.get('#new-secret'));

    await ui.openDirty();
    await ui.document.dispatch('keydown', { key: 'Escape' });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(ui.elements.get('#drawer').hidden, true);

    await ui.openDirty();
    await ui.elements.get('#drawer-backdrop').onclick();
    assert.equal(ui.elements.get('#drawer').hidden, true);

    await ui.openDirty();
    await ui.elements.get('#tab-files').onclick();
    assert.equal(ui.elements.get('#files-view').hidden, false);

    await ui.openDirty();
    ui.elements.get('#vault-select').value = 'two';
    await ui.elements.get('#vault-select').onchange();
    assert.equal(ui.elements.get('#vault-select').value, 'two');

    await ui.openDirty();
    await ui.elements.get('#new-secret').onclick({ currentTarget: ui.elements.get('#new-secret') });
    assert.equal(ui.elements.get('#drawer').hidden, false);
    assert.equal(ui.confirmations.length, 6);
  } finally {
    ui.restore();
  }
});

test('pending saves deactivate the backdrop and preserve save-control disabled states', async () => {
  const ui = await mountRouteUi();
  try {
    await ui.openDirty();
    const backdrop = ui.elements.get('#drawer-backdrop');
    const save = ui.elements.get('#save');
    const remove = ui.elements.get('#delete');
    save.disabled = true;
    remove.disabled = true;

    ui.store.dispatch({ type: 'draft/save-pending', value: true });
    assert.equal(backdrop.dataset.pending, 'true');
    assert.equal(backdrop.classList.contains('pending-disabled'), true);
    const consumed = [];
    assert.equal(backdrop.onclick({ preventDefault: () => consumed.push('prevented'), stopPropagation: () => consumed.push('stopped') }), false);
    assert.deepEqual(consumed, ['prevented', 'stopped']);
    assert.equal(ui.confirmations.length, 0);
    assert.equal(save.disabled, true);
    assert.equal(remove.disabled, true);

    ui.store.dispatch({ type: 'draft/save-pending', value: false });
    assert.equal(backdrop.dataset.pending, 'false');
    assert.equal(backdrop.classList.contains('pending-disabled'), false);
    assert.equal(save.disabled, true);
    assert.equal(remove.disabled, true);
    await backdrop.onclick();
    assert.equal(ui.confirmations.length, 1);
    assert.equal(ui.elements.get('#drawer').hidden, true);
    assert.doesNotMatch(
      fs.readFileSync(path.join(__dirname, 'style.css'), 'utf8'),
      /\.drawer-backdrop\.pending-disabled\s*\{[^}]*pointer-events/,
    );
  } finally {
    ui.restore();
  }
});

test('drawer receives focus and keeps Tab navigation within the modal', async () => {
  const ui = await mountRouteUi();
  try {
    const invoker = ui.elements.get('#new-secret');
    ui.document.activeElement = invoker;
    await invoker.onclick({ currentTarget: invoker });
    assert.equal(ui.document.activeElement, ui.elements.get('#field-name'));
    ui.document.activeElement = ui.elements.get('#save');
    await ui.document.dispatch('keydown', { key: 'Tab' });
    assert.equal(ui.document.activeElement, ui.elements.get('#field-name'));
  } finally {
    ui.restore();
  }
});

test('page clears native save-pending state after save completion and failure', async () => {
  for (const failSave of [false, true]) {
    const payloads = [];
    const ui = await mountRouteUi({
      failSave,
      tauriEvents: { listen() {}, emit: async (_name, payload) => { payloads.push(payload); } },
    });
    try {
      const invoker = ui.elements.get('#new-secret');
      await invoker.onclick({ currentTarget: invoker });
      const form = ui.elements.get('#secret-form');
      form.elements.name.value = 'created';
      form.elements.value.value = 'secret';
      form.elements.value.oninput();
      await form.onsubmit({ preventDefault() {}, target: form });
      assert.deepEqual(payloads, [true, false]);
    } finally {
      ui.restore();
    }
  }
});

test('list failures persist with Retry and retry replaces the failed list state', async () => {
  let listCalls = 0;
  const ui = await mountRouteUi({
    apiImpl: async (_method, path) => {
      if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
      if (path === '/api/types') return { types: [] };
      if (path === '/api/vaults') return { vaults: [{ name: 'one' }] };
      if (path.startsWith('/api/secrets')) {
        listCalls++;
        if (listCalls === 1) throw new ApiError({ status: 503, code: 'xv-network', message: 'Backend unavailable', hint: 'Retry now' });
        return [];
      }
      return [];
    },
  });
  try {
    const panel = ui.elements.get('#secret-refresh-error');
    assert.equal(panel.hidden, false);
    assert.equal(panel.querySelector('.error-message').textContent, 'Backend unavailable');
    await panel.querySelector('.error-retry').onclick();
    assert.equal(panel.hidden, true);
    assert.equal(listCalls, 2);
  } finally {
    ui.restore();
  }
});

test('form failures preserve the dirty draft and focus the named field', async () => {
  const error = new ApiError({ status: 400, code: 'xv-invalid-request', message: 'Name is invalid', field: 'name' });
  const ui = await mountRouteUi({
    apiImpl: async (method, path) => {
      if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
      if (path === '/api/types') return { types: [] };
      if (path === '/api/vaults') return { vaults: [{ name: 'one' }] };
      if (method === 'PUT') throw error;
      if (path.startsWith('/api/secrets')) return [];
      return [];
    },
  });
  try {
    const invoker = ui.elements.get('#new-secret');
    await invoker.onclick({ currentTarget: invoker });
    const form = ui.elements.get('#secret-form');
    form.elements.name.value = 'bad name';
    form.elements.value.value = 'draft secret';
    form.elements.value.oninput();
    await form.onsubmit({ preventDefault() {}, target: form });
    assert.equal(form.elements.value.value, 'draft secret');
    assert.equal(ui.elements.get('#secret-form-error').hidden, false);
    assert.equal(ui.document.activeElement, form.elements.name);
  } finally {
    ui.restore();
  }
});

test('aborted and stale list failures leave the current list surface unchanged', async () => {
  let listCalls = 0;
  const listSignals = [];
  let rejectStale;
  const stale = new Promise((_, reject) => { rejectStale = reject; });
  const ui = await mountRouteUi({
    apiImpl: async (_method, path, _body, _raw, options) => {
      if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
      if (path === '/api/types') return { types: [] };
      if (path === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
      if (path.startsWith('/api/secrets')) {
        listCalls++;
        listSignals.push(options?.signal);
        if (listCalls === 2) return stale;
        return [];
      }
      return [];
    },
  });
  try {
    const picker = ui.elements.get('#vault-select');
    picker.value = 'two';
    const staleLoad = picker.onchange();
    picker.value = 'one';
    await picker.onchange();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(listCalls, 3);
    assert.equal(listSignals[1].aborted, true);
    rejectStale(new ApiError({ status: 503, code: 'xv-network', message: 'stale failure' }));
    await staleLoad;
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(
      ui.elements.get('#secret-refresh-error').hidden,
      true,
      ui.elements.get('#secret-refresh-error').querySelector('.error-message').textContent,
    );

    const abortUi = await mountRouteUi({
      apiImpl: async (_method, path) => {
        if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
        if (path === '/api/types') return { types: [] };
        if (path === '/api/vaults') return { vaults: [{ name: 'one' }] };
        if (path.startsWith('/api/secrets')) throw Object.assign(new Error('aborted'), { name: 'AbortError' });
        return [];
      },
    });
    try {
      assert.equal(abortUi.elements.get('#secret-refresh-error').hidden, true);
    } finally {
      abortUi.restore();
    }
  } finally {
    ui.restore();
  }
});

test('legacy vault transition clears both refresh owners before delayed new-scope files settle', async () => {
  const delayedFiles = deferred();
  const secretVaultCalls = [];
  const fileVaultCalls = [];
  let failSecretRefresh = false;
  let failFileRefresh = false;
  let delayNextVaultFiles = false;
  const vaultFrom = (requestPath) => new URLSearchParams(requestPath.split('?')[1] || '').get('vault');
  const ui = await mountRouteUi({
    apiImpl: async (method, requestPath) => {
      if (requestPath === '/api/context') {
        return {
          vault: 'one',
          backend: 'test',
          capabilities: { files: true, soft_delete: false },
        };
      }
      if (requestPath === '/api/types') return { types: [] };
      if (requestPath === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
      if (method === 'GET' && requestPath.startsWith('/api/secrets?')) {
        const vault = vaultFrom(requestPath);
        secretVaultCalls.push(vault);
        if (vault === 'one' && failSecretRefresh) {
          failSecretRefresh = false;
          throw new ApiError({
            status: 503,
            code: 'xv-network',
            message: 'Old secret refresh failed',
            hint: 'Retry.',
          });
        }
        return [];
      }
      if (method === 'GET' && requestPath.startsWith('/api/files?')) {
        const vault = vaultFrom(requestPath);
        fileVaultCalls.push(vault);
        if (vault === 'one' && failFileRefresh) {
          failFileRefresh = false;
          throw new ApiError({
            status: 503,
            code: 'xv-network',
            message: 'Old file refresh failed',
            hint: 'Retry.',
          });
        }
        if (vault === 'two' && delayNextVaultFiles) return delayedFiles.promise;
        return [];
      }
      return [];
    },
  });
  try {
    failSecretRefresh = true;
    await ui.elements.get('#refresh-secrets').onclick();
    failFileRefresh = true;
    await ui.elements.get('#refresh-files').onclick();

    const secretPanel = ui.elements.get('#secret-refresh-error');
    const filePanel = ui.elements.get('#file-refresh-error');
    const secretRetry = secretPanel.querySelector('.error-retry');
    const fileRetry = filePanel.querySelector('.error-retry');
    const staleSecretHandler = secretRetry.onclick;
    const staleFileHandler = fileRetry.onclick;
    assert.equal(secretPanel.hidden, false);
    assert.equal(filePanel.hidden, false);

    secretRetry.disabled = true;
    fileRetry.disabled = true;
    delayNextVaultFiles = true;
    const picker = ui.elements.get('#vault-select');
    picker.value = 'two';
    const switching = picker.onchange();
    await settleUntil(() => fileVaultCalls.includes('two'));

    assert.equal(secretPanel.hidden, true);
    assert.equal(filePanel.hidden, true);
    assert.equal(secretRetry.onclick, null);
    assert.equal(fileRetry.onclick, null);
    assert.equal(secretRetry.disabled, false);
    assert.equal(fileRetry.disabled, false);

    const secretCallsBeforeStaleRetry = secretVaultCalls.length;
    const fileCallsBeforeStaleRetry = fileVaultCalls.length;
    assert.equal(await staleSecretHandler(), false);
    assert.equal(await staleFileHandler(), false);
    assert.equal(secretVaultCalls.length, secretCallsBeforeStaleRetry);
    assert.equal(fileVaultCalls.length, fileCallsBeforeStaleRetry);

    delayedFiles.resolve([]);
    await switching;
    assert.equal(secretVaultCalls.at(-1), 'two');
    assert.equal(fileVaultCalls.at(-1), 'two');
  } finally {
    delayedFiles.resolve([]);
    ui.restore();
  }
});

function existingSecretApi(value = 'top-secret') {
  return async (method, path) => {
    if (path === '/api/context') {
      return { vault: 'one', backend: 'test', capabilities: { files: false, soft_delete: false } };
    }
    if (path === '/api/types') return { types: [] };
    if (path === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
    if (method === 'GET' && path.startsWith('/api/secrets/existing?')) {
      return { tags: {}, content_type: '', enabled: true, not_before: null };
    }
    if (method === 'POST' && path.startsWith('/api/secrets/existing/value?')) return { value };
    if (method === 'GET' && path.startsWith('/api/secrets?')) return [{ name: 'existing' }];
    if (method === 'PATCH') return {};
    return [];
  };
}

function twoSecondPreferences() {
  const state = { exposure_timeout_seconds: 2 };
  return {
    load: async () => state,
    get: (key, fallback) => state[key] ?? fallback,
    snapshot: () => ({ ...state }),
  };
}

test('mounted protected fields reset inactivity and hide on timeout, visibility, and blur', async () => {
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: existingSecretApi(),
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    const value = ui.elements.get('#field-value');
    const status = ui.elements.get('#protected-value-status');
    const reveal = ui.elements.get('#reveal');
    assert.equal(reveal.getAttribute('aria-label'), 'Reveal value');
    await reveal.onclick();
    assert.equal(value.value, 'top-secret');
    assert.equal(reveal.getAttribute('aria-label'), 'Hide value');
    assert.equal(status.textContent, 'Value revealed. Hides in 2 seconds.');
    assert.doesNotMatch(status.textContent, /top-secret/);

    clock.advanceOneSecond();
    assert.equal(status.textContent, 'Value revealed. Hides in 1 second.');
    value.dispatch('pointerdown');
    clock.advanceOneSecond();
    assert.equal(value.value, 'top-secret');
    clock.advanceOneSecond();
    assert.equal(value.value, PROTECTED_MASK);
    assert.equal(reveal.getAttribute('aria-label'), 'Reveal value');

    await reveal.onclick();
    assert.equal(reveal.getAttribute('aria-label'), 'Hide value');
    await reveal.onclick();
    assert.equal(value.value, PROTECTED_MASK);
    assert.equal(reveal.getAttribute('aria-label'), 'Reveal value');

    await reveal.onclick();
    ui.document.visibilityState = 'hidden';
    ui.document.dispatch('visibilitychange');
    assert.equal(value.value, PROTECTED_MASK);
    assert.equal(reveal.getAttribute('aria-label'), 'Reveal value');

    ui.document.visibilityState = 'visible';
    await reveal.onclick();
    ui.dispatchWindow('blur');
    assert.equal(value.value, PROTECTED_MASK);
    assert.equal(reveal.getAttribute('aria-label'), 'Reveal value');
  } finally {
    ui.restore();
  }
});

test('mounted drawer close, save, and context switch forget protected values and store drafts', async () => {
  const ui = await mountRouteUi({
    apiImpl: existingSecretApi(),
    clock: exposureClock(),
    preferences: twoSecondPreferences(),
  });
  try {
    const value = ui.elements.get('#field-value');
    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#reveal').onclick();
    await ui.elements.get('#close-drawer').onclick();
    assert.equal(value.value, '');
    assert.equal(ui.store.snapshot().draft, null);

    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#reveal').onclick();
    const form = ui.elements.get('#secret-form');
    await form.onsubmit({ preventDefault() {}, target: form });
    assert.equal(value.value, '');
    assert.equal(ui.store.snapshot().draft, null);

    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#reveal').onclick();
    ui.elements.get('#vault-select').value = 'two';
    await ui.elements.get('#vault-select').onchange();
    assert.equal(value.value, '');
    assert.equal(ui.store.snapshot().draft, null);
  } finally {
    ui.restore();
  }
});

test('mounted copy countdown names the field and preserves newer clipboard content', async () => {
  let clipboardValue = '';
  const clipboard = {
    readText: async () => clipboardValue,
    writeText: async (value) => { clipboardValue = value; },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: existingSecretApi(),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#copy').onclick();
    const status = ui.elements.get('#protected-value-status');
    assert.equal(clipboardValue, 'top-secret');
    assert.equal(status.textContent, 'Value copied. Clipboard clears in 2 seconds.');
    assert.doesNotMatch(status.textContent, /top-secret/);

    clipboardValue = 'newer-content';
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, 'newer-content');
    assert.equal(status.textContent, 'Value clipboard clearing could not be confirmed.');
  } finally {
    ui.restore();
  }
});

function deferred() {
  let resolve;
  const promise = new Promise((finish) => { resolve = finish; });
  return { promise, resolve };
}

function deferredPreferences() {
  const gate = deferred();
  return {
    gate,
    client: {
      load: () => gate.promise,
      get: (_key, fallback) => gate.value ?? fallback,
      snapshot: () => ({ exposure_timeout_seconds: gate.value ?? 2 }),
    },
    resolve(value = 2) {
      gate.value = value;
      gate.resolve({ exposure_timeout_seconds: value });
    },
  };
}

function recordSecretApi(values = { 'a-b': 'first-record-value', 'a b': 'second-record-value' }) {
  return async (method, path) => {
    if (path === '/api/context') {
      return { vault: 'one', backend: 'test', capabilities: { files: true, soft_delete: false } };
    }
    if (path === '/api/types') return { types: [] };
    if (path === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
    if (method === 'GET' && path.startsWith('/api/secrets/existing?')) {
      return {
        tags: { 'xv-type': 'collision' },
        content_type: 'application/vnd.xv.record',
        enabled: true,
        not_before: null,
      };
    }
    if (method === 'POST' && path.startsWith('/api/secrets/existing/value?')) {
      return { value: JSON.stringify(values) };
    }
    if (method === 'GET' && path.startsWith('/api/secrets?')) return [{ name: 'existing' }];
    if (path.startsWith('/api/files')) return [];
    return [];
  };
}

test('pending plain and record reveals cannot resume after blur or document hiding', async () => {
  const plainValue = deferred();
  const plainApi = existingSecretApi();
  const plain = await mountRouteUi({
    apiImpl: async (method, path) => (
      method === 'POST' && path.startsWith('/api/secrets/existing/value?')
        ? plainValue.promise
        : plainApi(method, path)
    ),
    preferences: twoSecondPreferences(),
    clock: exposureClock(),
  });
  try {
    await openExistingSecret(plain, 'existing');
    const revealing = plain.elements.get('#reveal').onclick();
    await Promise.resolve();
    plain.dispatchWindow('blur');
    plainValue.resolve({ value: 'top-secret' });
    await revealing;
    assert.equal(plain.elements.get('#field-value').value, PROTECTED_MASK);
  } finally {
    plain.restore();
  }

  const recordPreferences = deferredPreferences();
  const record = await mountRouteUi({
    apiImpl: recordSecretApi(),
    preferences: recordPreferences.client,
    clock: exposureClock(),
  });
  try {
    await openExistingSecret(record, 'existing');
    const reveal = record.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Reveal a-b');
    assert.ok(reveal, 'record reveal control is field-specific');
    const revealing = reveal.onclick();
    record.document.visibilityState = 'hidden';
    record.document.dispatch('visibilitychange');
    recordPreferences.resolve();
    await revealing;
    const input = record.find('#record-fields', (element) => element.dataset?.fieldName === 'a-b');
    assert.equal(input.value, PROTECTED_MASK);
  } finally {
    record.restore();
  }
});

test('deferred record reveal and copy never resume after close, vault switch, or tab switch', async () => {
  for (const operation of ['reveal', 'copy']) {
    for (const contextChange of ['close', 'vault', 'tab']) {
      const preferences = deferredPreferences();
      const writes = [];
      const ui = await mountRouteUi({
        apiImpl: recordSecretApi(),
        preferences: preferences.client,
        clipboard: { readText: async () => '', writeText: async (value) => { writes.push(value); } },
        clock: exposureClock(),
      });
      try {
        await openExistingSecret(ui, 'existing');
        const controlName = `${operation === 'reveal' ? 'Reveal' : 'Copy'} a-b`;
        const control = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === controlName);
        assert.ok(control, `record ${operation} control is field-specific`);
        const pending = control.onclick();
        if (contextChange === 'close') await ui.elements.get('#close-drawer').onclick();
        if (contextChange === 'vault') {
          ui.elements.get('#vault-select').value = 'two';
          await ui.elements.get('#vault-select').onchange();
        }
        if (contextChange === 'tab') await ui.elements.get('#tab-files').onclick();
        preferences.resolve();
        await pending;
        assert.deepEqual(writes, [], `${operation} did not copy after ${contextChange}`);
        const input = ui.find('#record-fields', (element) => element.dataset?.fieldName === 'a-b');
        if (input) assert.equal(input.value, PROTECTED_MASK, `${operation} stayed masked after ${contextChange}`);
      } finally {
        ui.restore();
      }
    }
  }
});

test('record fields have collision-free descriptions and field-specific timer status', async () => {
  const ui = await mountRouteUi({
    apiImpl: recordSecretApi(),
    preferences: twoSecondPreferences(),
    clock: exposureClock(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    const inputs = ui.findAll('#record-fields', (element) => element.dataset?.fieldKind === 'secret');
    assert.equal(inputs.length, 2);
    const descriptionIds = inputs.map((input) => input._protectionDescription.id);
    assert.equal(new Set(descriptionIds).size, 2);
    for (const input of inputs) {
      assert.equal(
        input.getAttribute('aria-describedby'),
        `${input.id}-field-help ${input._protectionDescription.id} protected-value-status`,
      );
    }

    const reveal = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Reveal a-b');
    await reveal.onclick();
    const status = ui.elements.get('#protected-value-status');
    assert.equal(status.textContent, 'a-b revealed. Hides in 2 seconds.');
    assert.doesNotMatch(status.textContent, /first-record-value/);
  } finally {
    ui.restore();
  }
});

test('an older clipboard expiry cannot clear or announce over a newer same-field copy', async () => {
  let clipboardValue = '';
  let reads = 0;
  const oldRead = deferred();
  const clipboard = {
    readText: async () => (++reads === 1 ? oldRead.promise : clipboardValue),
    writeText: async (value) => { clipboardValue = value; },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: existingSecretApi('identical-value'),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#copy').onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    const newerCopy = ui.elements.get('#copy').onclick();
    await Promise.resolve();
    oldRead.resolve('identical-value');
    await newerCopy;
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, 'identical-value');
    assert.equal(ui.elements.get('#protected-value-status').textContent, 'Value copied. Clipboard clears in 2 seconds.');

    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, '');
    assert.equal(ui.elements.get('#protected-value-status').textContent, 'Value clipboard cleared.');
  } finally {
    ui.restore();
  }
});

test('stale clipboard expiry never overwrites reopened or overlapping field status', async () => {
  let clipboardValue = '';
  let pendingRead = deferred();
  let delayNextRead = true;
  const clipboard = {
    readText: async () => {
      if (delayNextRead) {
        delayNextRead = false;
        return pendingRead.promise;
      }
      return clipboardValue;
    },
    writeText: async (value) => { clipboardValue = value; },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: recordSecretApi(),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    const copyA = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a-b');
    const revealB = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Reveal a b');
    await copyA.onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await revealB.onclick();
    const status = ui.elements.get('#protected-value-status');
    assert.equal(status.textContent, 'a b revealed. Hides in 2 seconds.');
    pendingRead.resolve('first-record-value');
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(status.textContent, 'a b revealed. Hides in 2 seconds.');

    await ui.elements.get('#close-drawer').onclick();
    await openExistingSecret(ui, 'existing');
    pendingRead = deferred();
    delayNextRead = true;
    const staleCopy = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a-b');
    await staleCopy.onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await ui.elements.get('#close-drawer').onclick();
    await openExistingSecret(ui, 'existing');
    const reopenedReveal = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Reveal a b');
    await reopenedReveal.onclick();
    const reopenedStatus = status.textContent;
    pendingRead.resolve('first-record-value');
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(status.textContent, reopenedStatus);
  } finally {
    ui.restore();
  }
});

test('global clipboard ownership protects an identical newer copy from another field', async () => {
  let clipboardValue = '';
  const staleRead = deferred();
  let firstRead = true;
  const clipboard = {
    readText: async () => (firstRead ? (firstRead = false, staleRead.promise) : clipboardValue),
    writeText: async (value) => { clipboardValue = value; },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: recordSecretApi({ 'a-b': 'identical-value', 'a b': 'identical-value' }),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    const copyA = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a-b');
    const copyB = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a b');
    await copyA.onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();

    const newerCopy = copyB.onclick();
    await Promise.resolve();
    staleRead.resolve('identical-value');
    await newerCopy;
    await new Promise((resolve) => setTimeout(resolve, 0));

    const status = ui.elements.get('#protected-value-status');
    assert.equal(clipboardValue, 'identical-value');
    assert.equal(status.textContent, 'a b copied. Clipboard clears in 2 seconds.');
    clock.advanceOneSecond();
    assert.equal(clipboardValue, 'identical-value');
    clock.advanceOneSecond();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, '');
    assert.equal(status.textContent, 'a b clipboard cleared.');
  } finally {
    ui.restore();
  }
});

test('global clipboard ownership protects an identical copy after close and reopen', async () => {
  let clipboardValue = '';
  const staleRead = deferred();
  let firstRead = true;
  const clipboard = {
    readText: async () => (firstRead ? (firstRead = false, staleRead.promise) : clipboardValue),
    writeText: async (value) => { clipboardValue = value; },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: existingSecretApi('identical-value'),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    await ui.elements.get('#copy').onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await ui.elements.get('#close-drawer').onclick();
    await openExistingSecret(ui, 'existing');

    const newerCopy = ui.elements.get('#copy').onclick();
    await new Promise((resolve) => setTimeout(resolve, 0));
    staleRead.resolve('identical-value');
    await newerCopy;
    await new Promise((resolve) => setTimeout(resolve, 0));

    const status = ui.elements.get('#protected-value-status');
    assert.equal(clipboardValue, 'identical-value');
    assert.equal(status.textContent, 'Value copied. Clipboard clears in 2 seconds.');
    clock.advanceOneSecond();
    assert.equal(clipboardValue, 'identical-value');
    clock.advanceOneSecond();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, '');
    assert.equal(status.textContent, 'Value clipboard cleared.');
  } finally {
    ui.restore();
  }
});

test('global clipboard handoff serializes a newer copy behind an in-flight clear write', async () => {
  let clipboardValue = '';
  const clearWrite = deferred();
  let delayClear = true;
  const clipboard = {
    readText: async () => clipboardValue,
    writeText: async (value) => {
      if (value === '' && delayClear) {
        delayClear = false;
        await clearWrite.promise;
      }
      clipboardValue = value;
    },
  };
  const clock = exposureClock();
  const ui = await mountRouteUi({
    apiImpl: recordSecretApi({ 'a-b': 'identical-value', 'a b': 'identical-value' }),
    clipboard,
    clock,
    preferences: twoSecondPreferences(),
  });
  try {
    await openExistingSecret(ui, 'existing');
    const copyA = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a-b');
    const copyB = ui.find('#record-fields', (element) => element.getAttribute?.('aria-label') === 'Copy a b');
    await copyA.onclick();
    clock.advanceOneSecond();
    clock.advanceOneSecond();
    await Promise.resolve();
    await Promise.resolve();

    const newerCopy = copyB.onclick();
    await Promise.resolve();
    clearWrite.resolve();
    await newerCopy;
    await new Promise((resolve) => setTimeout(resolve, 0));

    const status = ui.elements.get('#protected-value-status');
    assert.equal(clipboardValue, 'identical-value');
    assert.equal(status.textContent, 'a b copied. Clipboard clears in 2 seconds.');
    clock.advanceOneSecond();
    assert.equal(clipboardValue, 'identical-value');
    clock.advanceOneSecond();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(clipboardValue, '');
    assert.equal(status.textContent, 'a b clipboard cleared.');
  } finally {
    ui.restore();
  }
});
