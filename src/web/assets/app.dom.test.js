import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath, pathToFileURL } from 'node:url';
import * as model from './ui-model.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function loadProtectedRenderer() {
  const appPath = path.join(__dirname, 'secrets.js');
  const appSource = fs.readFileSync(appPath, 'utf8');
  const start = appSource.indexOf('function setRevealLabel');
  const end = appSource.indexOf('// Same rule as the TUI', start);
  assert.notEqual(start, -1, 'protected renderer start is present');
  assert.notEqual(end, -1, 'protected renderer end is present');

  const context = {
    XvUiModel: model,
    icon: (name) => ({ name }),
    updateProtectionDescription() {},
  };
  vm.runInNewContext(
    `'use strict';\n${appSource.slice(start, end)}\nglobalThis.renderProtectedControl = renderProtectedControl;`,
    context,
    { filename: appPath },
  );
  return context.renderProtectedControl;
}

// The asset test suite has no browser DOM dependency. These small controls
// preserve the relevant Web IDL distinction: textarea.type is read-only,
// while input.type is writable.
class MainSecretTextarea {
  constructor() {
    this.readOnly = false;
    this.value = '';
  }

  get type() { return 'textarea'; }
}

class RecordFieldInput {
  constructor() {
    this.readOnly = false;
    this.value = '';
    this._type = 'text';
  }

  get type() { return this._type; }
  set type(value) { this._type = value; }
}

class RevealButton {
  constructor(id) {
    this.id = id;
    this.textContent = '';
    this.children = [];
    this.dataset = {};
  }

  setAttribute() {}
  replaceChildren(...children) { this.children = children; }
}

test('protected renderer supports the main textarea and record-field input', () => {
  const renderProtectedControl = loadProtectedRenderer();
  const state = model.createProtectedState('stored secret', true);

  const textarea = new MainSecretTextarea();
  const mainButton = new RevealButton('reveal');
  assert.doesNotThrow(() => renderProtectedControl(textarea, mainButton, state));
  assert.equal(textarea.type, 'textarea');
  assert.equal(textarea.readOnly, true);
  assert.equal(textarea.value, model.PROTECTED_MASK);
  assert.equal(mainButton.children[1], 'Reveal');

  const input = new RecordFieldInput();
  const fieldButton = new RevealButton('field-reveal');
  assert.doesNotThrow(() => renderProtectedControl(input, fieldButton, state));
  assert.equal(input.type, 'text');
  assert.equal(input.readOnly, true);
  assert.equal(input.value, model.PROTECTED_MASK);
  assert.equal(fieldButton.textContent, 'Reveal');
});

function bootstrapDocument() {
  const elements = new Map();
  const element = () => ({
    hidden: false,
    className: '',
    innerHTML: '',
    dataset: {},
    classList: { add() {}, remove() {}, toggle() {} },
    setAttribute() {},
    removeAttribute() {},
    replaceChildren() {},
    appendChild() {},
    addEventListener() {},
    removeEventListener() {},
    querySelector() { return { textContent: '', hidden: false, setAttribute() {} }; },
  });
  const get = (selector) => {
    if (!elements.has(selector)) elements.set(selector, element());
    return elements.get(selector);
  };
  return {
    getElementById(id) { return get(`#${id}`); },
    querySelector(selector) {
      if (selector.endsWith('-table')) return { clientWidth: 100, querySelectorAll: () => [] };
      if (selector === '#secret-form') {
        const form = get(selector);
        form.elements = { value: { addEventListener() {} } };
        return form;
      }
      return get(selector);
    },
    querySelectorAll() { return []; },
    createElementNS() { return { classList: { add() {} }, setAttribute() {}, appendChild() {} }; },
    createTextNode(value) { return value; },
  };
}

test('app bootstrap supplies its persisted token to every initial API request', async () => {
  const original = new Map(['document', 'location', 'sessionStorage', 'history', 'fetch']
    .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const session = new Map();
  const calls = [];
  Object.assign(globalThis, {
    document: bootstrapDocument(),
    location: { search: '?token=bootstrap-token', pathname: '/' },
    sessionStorage: { getItem: (key) => session.get(key) || null, setItem: (key, value) => session.set(key, value) },
    history: { replaceState() {} },
    fetch: async (requestPath, options) => {
      calls.push({ requestPath, options });
      return { ok: false, status: 401, statusText: 'Unauthorized', json: async () => ({ error: 'Unauthorized' }) };
    },
  });

  try {
    const appUrl = pathToFileURL(path.join(__dirname, 'app.js')).href;
    await import(`${appUrl}?bootstrap-test=${Date.now()}`);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(
      calls.map(({ requestPath }) => requestPath).sort(),
      ['/api/context', '/api/preferences'],
    );
    assert.ok(calls.every(({ options }) => (
      options.headers.Authorization === 'Bearer bootstrap-token'
    )));
  } finally {
    for (const [key, descriptor] of original) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});

test('missing-token bootstrap never starts context loading or leaks a rejection', async () => {
  const original = new Map(['document', 'location', 'sessionStorage', 'history', 'fetch']
    .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const calls = [];
  const unhandled = [];
  const onUnhandled = (error) => unhandled.push(error);
  process.on('unhandledRejection', onUnhandled);
  Object.assign(globalThis, {
    document: bootstrapDocument(),
    location: { search: '', pathname: '/' },
    sessionStorage: { getItem: () => null, setItem() {} },
    history: { replaceState() {} },
    fetch: async (requestPath) => {
      calls.push(requestPath);
      if (requestPath === '/api/context') throw new Error('context must not start');
      throw new Error('safe preferences fixture failure');
    },
  });

  try {
    const appUrl = pathToFileURL(path.join(__dirname, 'app.js')).href;
    await import(`${appUrl}?missing-token-test=${Date.now()}`);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.deepEqual(calls, ['/api/preferences']);
    assert.deepEqual(unhandled, []);
  } finally {
    process.off('unhandledRejection', onUnhandled);
    for (const [key, descriptor] of original) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});
