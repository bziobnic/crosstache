import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath, pathToFileURL } from 'node:url';
import * as model from './ui-model.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

test('the command-center shell owns one tab set inside the context rail', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const railStart = html.indexOf('<aside id="context-rail"');
  const railEnd = html.indexOf('</aside>', railStart);
  const tabsStart = html.indexOf('<nav id="vault-tabs"', railStart);
  assert.ok(tabsStart > railStart && tabsStart < railEnd);
  assert.equal((html.match(/id="vault-tabs"/g) || []).length, 1);
  assert.equal((html.match(/role="tab"/g) || []).length, 3);
  assert.match(html, /class="context-rail-top"/);
  assert.match(html, /class="context-rail-footer"/);
});

const VOID_ELEMENTS = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'source', 'track', 'wbr']);

function parseStaticHtml(html) {
  const root = { tag: '#document', attributes: new Map(), children: [] };
  const stack = [root];
  for (const token of html.match(/<\/?[^>]+>/g) || []) {
    if (token.startsWith('</')) {
      const name = token.slice(2, -1).trim().toLowerCase();
      const index = stack.map((node) => node.tag).lastIndexOf(name);
      if (index > 0) stack.length = index;
      continue;
    }
    if (token.startsWith('<!')) continue;
    const match = /^<\s*([\w-]+)([\s\S]*?)\/?\s*>$/.exec(token);
    if (!match) continue;
    const [, tag, source] = match;
    const attributes = new Map();
    for (const attribute of source.matchAll(/([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/g)) {
      attributes.set(attribute[1], attribute[2] ?? attribute[3] ?? attribute[4] ?? '');
    }
    const node = { tag: tag.toLowerCase(), attributes, children: [] };
    stack.at(-1).children.push(node);
    if (!VOID_ELEMENTS.has(node.tag) && !token.endsWith('/>')) stack.push(node);
  }
  return root;
}

function findAll(root, predicate) {
  const matches = [];
  for (const child of root.children) {
    if (predicate(child)) matches.push(child);
    matches.push(...findAll(child, predicate));
  }
  return matches;
}

function byId(root, id) {
  return findAll(root, (node) => node.attributes.get('id') === id)[0] || null;
}

function hasClass(node, name) {
  return (node.attributes.get('class') || '').split(/\s+/).includes(name);
}

function assertHeadingContract(view, { action, count, search }) {
  const headings = view.children.filter((node) => hasClass(node, 'view-heading'));
  assert.equal(headings.length, 1, `${view.attributes.get('id')} has one direct view heading`);
  const heading = headings[0];
  assert.equal(findAll(heading, (node) => node.attributes.get('id') === count).length, 1,
    `${count} stays in the view heading`);
  if (action) {
    assert.equal(findAll(view, (node) => node.attributes.get('id') === action).length, 1,
      `${action} appears once in its view`);
    assert.equal(findAll(heading, (node) => node.attributes.get('id') === action).length, 1,
      `${action} stays in the view heading`);
  } else {
    assert.equal(findAll(heading, (node) => node.tag === 'button').length, 0,
      'Trash does not offer a heading action');
  }
  if (!search) return;
  const fields = findAll(view, (node) => hasClass(node, 'search-field'));
  assert.equal(fields.length, 1, `${view.attributes.get('id')} has one dominant search field`);
  assert.equal(findAll(fields[0], (node) => (
    node.tag === 'input' && node.attributes.get('id') === search && node.attributes.get('type') === 'search'
  )).length, 1, `${search} is the dominant search control`);
}

test('content views keep heading actions, counts, and dominant searches in their semantic owners', () => {
  const document = parseStaticHtml(fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8'));
  const secrets = byId(document, 'secrets-view');
  const files = byId(document, 'files-view');
  const trash = byId(document, 'trash-view');
  assert.ok(secrets && files && trash, 'content views are present');
  assertHeadingContract(secrets, { action: 'new-secret', count: 'secret-item-count', search: 'search' });
  assertHeadingContract(files, { action: 'browse-files-header', count: 'file-item-count', search: 'file-search' });
  assertHeadingContract(trash, { count: 'trash-item-count' });
});

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
    click() { this.onclick?.(); },
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

test('top command trigger delegates to the existing commands control', async () => {
  const original = new Map(['document', 'location', 'sessionStorage', 'history', 'fetch']
    .map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const document = bootstrapDocument();
  let commandOpens = 0;
  Object.assign(globalThis, {
    document,
    location: { search: '', pathname: '/' },
    sessionStorage: { getItem: () => null, setItem() {} },
    history: { replaceState() {} },
    fetch: async () => ({ ok: false, status: 401, statusText: 'Unauthorized', json: async () => ({ error: 'Unauthorized' }) }),
  });

  try {
    const appUrl = pathToFileURL(path.join(__dirname, 'app.js')).href;
    await import(`${appUrl}?command-trigger-test=${Date.now()}`);
    document.getElementById('commands-open').onclick = () => { commandOpens++; };
    document.getElementById('top-command-open').click();
    assert.equal(commandOpens, 1);
  } finally {
    for (const [key, descriptor] of original) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete globalThis[key];
    }
  }
});
