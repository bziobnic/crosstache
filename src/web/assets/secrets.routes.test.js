import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createStore, draftReducer } from './store.js';
import { createDialogManager } from './dialogs.js';
import { ApiError } from './api-client.js';
import { PROTECTED_MASK } from './ui-model.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

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
  for (const selector of ['#secret-error', '#file-error', '#secret-form-error']) {
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
} = {}) {
  const { document, elements } = createDocument();
  const previous = new Map(['document', 'navigator', '__TAURI__', 'addEventListener', 'removeEventListener']
    .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const windowListeners = new Map();
  Object.defineProperty(globalThis, 'document', { configurable: true, value: document });
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard } });
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
  const store = createStore({ draft: null, savePending: false }, draftReducer);
  const dialogs = createDialogManager(document);
  dialogs.confirmDiscard = () => { confirmations.push(true); return true; };
  mountSecrets({ api, store, dialogs, preferences, token: 'test', exposureClock: clock });
  await new Promise((resolve) => setTimeout(resolve, 0));
  return {
    document,
    elements,
    store,
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
    const panel = ui.elements.get('#secret-error');
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
  let rejectStale;
  const stale = new Promise((_, reject) => { rejectStale = reject; });
  const ui = await mountRouteUi({
    apiImpl: async (_method, path) => {
      if (path === '/api/context') return { vault: 'one', backend: 'test', capabilities: { files: false } };
      if (path === '/api/types') return { types: [] };
      if (path === '/api/vaults') return { vaults: [{ name: 'one' }, { name: 'two' }] };
      if (path.startsWith('/api/secrets')) {
        listCalls++;
        if (listCalls === 2) return stale;
        return [];
      }
      return [];
    },
  });
  try {
    const picker = ui.elements.get('#vault-select');
    picker.value = 'two';
    await picker.onchange();
    picker.value = 'one';
    await picker.onchange();
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(listCalls, 3);
    rejectStale(new ApiError({ status: 503, code: 'xv-network', message: 'stale failure' }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(
      ui.elements.get('#secret-error').hidden,
      true,
      ui.elements.get('#secret-error').querySelector('.error-message').textContent,
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
      assert.equal(abortUi.elements.get('#secret-error').hidden, true);
    } finally {
      abortUi.restore();
    }
  } finally {
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
    await ui.elements.get('#reveal').onclick();
    assert.equal(value.value, 'top-secret');
    assert.equal(status.textContent, 'Value revealed. Hides in 2 seconds.');
    assert.doesNotMatch(status.textContent, /top-secret/);

    clock.advanceOneSecond();
    assert.equal(status.textContent, 'Value revealed. Hides in 1 second.');
    value.dispatch('pointerdown');
    clock.advanceOneSecond();
    assert.equal(value.value, 'top-secret');
    clock.advanceOneSecond();
    assert.equal(value.value, PROTECTED_MASK);

    await ui.elements.get('#reveal').onclick();
    ui.document.visibilityState = 'hidden';
    ui.document.dispatch('visibilitychange');
    assert.equal(value.value, PROTECTED_MASK);

    ui.document.visibilityState = 'visible';
    await ui.elements.get('#reveal').onclick();
    ui.dispatchWindow('blur');
    assert.equal(value.value, PROTECTED_MASK);
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

function recordSecretApi() {
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
      return { value: JSON.stringify({ 'a-b': 'first-record-value', 'a b': 'second-record-value' }) };
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
        `${input._protectionDescription.id} protected-value-status`,
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
    await ui.elements.get('#copy').onclick();
    assert.equal(ui.elements.get('#protected-value-status').textContent, 'Value copied. Clipboard clears in 2 seconds.');

    oldRead.resolve('identical-value');
    await Promise.resolve();
    await Promise.resolve();
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
