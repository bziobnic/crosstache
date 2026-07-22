import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createStore, draftReducer } from './store.js';
import { createDialogManager } from './dialogs.js';
import { ApiError } from './api-client.js';

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
    this.innerHTML = '';
    this.dataset = {};
    this.children = [];
    this.classes = new Set();
    this.classList = {
      add: (name) => this.classes.add(name),
      remove: (name) => this.classes.delete(name),
      toggle: (name, enabled) => (enabled ? this.classes.add(name) : this.classes.delete(name)),
      contains: (name) => this.classes.has(name),
    };
    this.listeners = new Map();
  }

  setAttribute() {}
  removeAttribute() {}
  appendChild(child) { this.children.push(child); return child; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children = children; }
  querySelectorAll() { return []; }
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
  return { document, elements };
}

async function mountRouteUi({ failSave = false, apiImpl = null, tauriEvents = null } = {}) {
  const { document, elements } = createDocument();
  const previous = new Map(['document', 'navigator', '__TAURI__'].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  Object.defineProperty(globalThis, 'document', { configurable: true, value: document });
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { clipboard: { writeText: async () => {} } } });
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
  mountSecrets({ api, store, dialogs, token: 'test' });
  await new Promise((resolve) => setTimeout(resolve, 0));
  return {
    document,
    elements,
    store,
    confirmations,
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
