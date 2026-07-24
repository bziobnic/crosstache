import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  boundTimeout,
  buildHelpDiagnostics,
  effectiveTheme,
  mountHelp,
  mountSettings,
} from './settings.js';

class FakeElement {
  constructor(value = '') {
    this.value = value;
    this.textContent = '';
    this.dataset = {};
    this.disabled = false;
    this.hidden = false;
    this.listeners = new Map();
    this.children = [];
  }

  addEventListener(type, listener) {
    this.listeners.set(type, listener);
  }

  removeEventListener(type) {
    this.listeners.delete(type);
  }

  dispatch(type) {
    this.listeners.get(type)?.({ currentTarget: this, preventDefault() {} });
  }

  append(child) {
    this.children.push(child);
  }

  querySelectorAll(selector) {
    return selector === 'option' ? this.children : [];
  }
}

function fakeDocument(ids, { createElements = false } = {}) {
  const elements = new Map(Object.entries(ids));
  return {
    documentElement: new FakeElement(),
    getElementById: (id) => elements.get(id) ?? null,
    ...(createElements ? { createElement: () => new FakeElement() } : {}),
  };
}

test('effectiveTheme follows the system query while explicit choices win', () => {
  assert.equal(effectiveTheme('system', { matches: true }), 'dark');
  assert.equal(effectiveTheme('system', { matches: false }), 'light');
  assert.equal(effectiveTheme('light', { matches: true }), 'light');
  assert.equal(effectiveTheme('dark', { matches: false }), 'dark');
  assert.equal(effectiveTheme('unknown', { matches: true }), 'dark');
});

test('boundTimeout constrains protected exposure to the configured policy', () => {
  assert.equal(boundTimeout(120, 30), 30);
  assert.equal(boundTimeout(15, 30), 15);
  assert.equal(boundTimeout(120, 0), 120);
  assert.equal(boundTimeout(-1, 30), 0);
});

async function mountedTimeoutOption({ requested, policy }) {
  const timeout = new FakeElement();
  const document = fakeDocument({ 'exposure-timeout-select': timeout }, { createElements: true });
  const values = {
    theme: 'system',
    density: 'comfortable',
    exposure_timeout_seconds: requested,
  };
  const mounted = mountSettings({
    preferences: {
      async load() { return values; },
      get(key, fallback) { return values[key] ?? fallback; },
      set() { return true; },
    },
    securityPolicy: policy,
    document,
    mediaQuery: { matches: false },
  });
  await mounted.ready;
  return timeout.children.find((option) => option.value === timeout.value);
}

test('nonstandard timeout labels distinguish current values from actual policy clamps', async () => {
  assert.equal((await mountedTimeoutOption({ requested: 17, policy: 0 })).textContent,
    '17 seconds (current)');
  assert.equal((await mountedTimeoutOption({ requested: 17, policy: 30 })).textContent,
    '17 seconds (current)');
  assert.equal((await mountedTimeoutOption({ requested: 120, policy: 17 })).textContent,
    '17 seconds (policy limit)');
});

test('zero policy permits the requested timeout while a zero timeout hides immediately', async () => {
  assert.equal(boundTimeout(120, 0), 120);
  assert.equal((await mountedTimeoutOption({ requested: 0, policy: 0 })).textContent,
    '0 seconds (current)');

  const timeout = new FakeElement('0');
  const status = new FakeElement();
  const values = { theme: 'system', density: 'comfortable', exposure_timeout_seconds: 30 };
  const mounted = mountSettings({
    preferences: {
      async load() { return values; },
      get(key, fallback) { return values[key] ?? fallback; },
      set(key, value) { values[key] = value; return true; },
    },
    securityPolicy: 0,
    document: fakeDocument({
      'exposure-timeout-select': timeout,
      'settings-live': status,
    }),
    mediaQuery: { matches: false },
  });
  await mounted.ready;
  timeout.value = '0';
  timeout.dispatch('change');
  assert.equal(status.textContent, 'Protected values hide immediately.');
});

test('mountSettings persists through the preference owner and resets layout only', async () => {
  const theme = new FakeElement();
  const timeout = new FakeElement();
  const density = new FakeElement();
  const reset = new FakeElement();
  const status = new FakeElement();
  const document = fakeDocument({
    'theme-select': theme,
    'exposure-timeout-select': timeout,
    'density-select': density,
    'layout-reset': reset,
    'settings-live': status,
  });
  const values = {
    theme: 'system',
    exposure_timeout_seconds: 30,
    density: 'compact',
    folder_expansion: false,
    column_widths: { secrets: [31, 15, 14, 22, 18], files: [40, 14, 24, 22] },
  };
  const changes = [];
  const preferences = {
    async load() { return { ...values }; },
    get(key, fallback) { return values[key] ?? fallback; },
    set(key, value) {
      changes.push([key, value]);
      values[key] = value;
      return true;
    },
  };
  const listeners = new Map();
  const mediaQuery = {
    matches: false,
    addEventListener(type, listener) { listeners.set(type, listener); },
    removeEventListener(type) { listeners.delete(type); },
  };

  const settings = mountSettings({
    preferences,
    securityPolicy: 30,
    document,
    mediaQuery,
  });
  await settings.ready;
  assert.equal(document.documentElement.dataset.theme, 'system');
  assert.equal(document.documentElement.dataset.effectiveTheme, 'light');
  assert.equal(document.documentElement.dataset.density, 'compact');

  theme.value = 'dark';
  theme.dispatch('change');
  density.value = 'comfortable';
  density.dispatch('change');
  timeout.value = '120';
  timeout.dispatch('change');
  reset.dispatch('click');

  assert.deepEqual(changes, [
    ['theme', 'dark'],
    ['density', 'comfortable'],
    ['exposure_timeout_seconds', 30],
    ['density', 'comfortable'],
    ['column_widths', {
      secrets: [28, 15, 14, 25, 18],
      files: [42, 12, 24, 22],
    }],
  ]);
  assert.equal(values.folder_expansion, false);

  values.theme = 'system';
  settings.refresh();
  mediaQuery.matches = true;
  listeners.get('change')?.();
  assert.equal(document.documentElement.dataset.effectiveTheme, 'dark');
  settings.destroy();
  assert.equal(listeners.has('change'), false);
});

const diagnosticContext = {
  version: '0.26.2',
  config_path: '/Users/example/.config/xv/xv.conf',
  backend: 'local',
  vault: 'work',
  workspace: { alias: 'personal' },
  project: { name: 'crosstache' },
  environment: { name: 'prod' },
  connection: { state: 'connected', url: 'http://127.0.0.1:1234/?token=leak' },
  security: { clipboard_timeout_seconds: 30 },
  preferences: { exposure_timeout_seconds: 15 },
  capabilities: { files: true, trash: false, restore: false, purge: true },
  token: 'secret-token',
};

test('buildHelpDiagnostics is useful and allowlist-redacted', () => {
  const diagnostics = buildHelpDiagnostics(diagnosticContext);
  for (const expected of [
    'Crosstache 0.26.2',
    'Config: /Users/example/.config/xv/xv.conf',
    'Backend: local',
    'Vault: work',
    'Workspace: personal',
    'Connection: connected',
  ]) assert.match(diagnostics, new RegExp(expected.replaceAll('.', '\\.')));
  for (const forbidden of ['secret-token', '127.0.0.1', 'token=', 'http://']) {
    assert.ok(!diagnostics.includes(forbidden));
  }
  assert.match(diagnostics, /Capabilities: files, purge/);
  assert.ok(!diagnostics.includes('trash='));
  assert.match(diagnostics, /Security policy limit \(seconds\): 30/);
  assert.match(diagnostics, /Effective protected-value timeout \(seconds\): 15/);
  assert.ok(!diagnostics.includes('Protected value timeout: 30'));
});

test('zero security policy is reported as no limit without changing effective timeout semantics', () => {
  const diagnostics = buildHelpDiagnostics({
    ...diagnosticContext,
    security: { clipboard_timeout_seconds: 0 },
    preferences: { exposure_timeout_seconds: 0 },
  });
  assert.match(diagnostics, /Security policy limit \(seconds\): none/);
  assert.match(diagnostics, /Effective protected-value timeout \(seconds\): 0/);
});

test('diagnostics apply the shared timeout boundary to every available policy combination', () => {
  const cases = [
    { requested: 30, policy: 17, effective: 17 },
    { requested: 23, policy: 0, effective: 23 },
    { requested: 0, policy: 17, effective: 0 },
  ];
  for (const { requested, policy, effective } of cases) {
    const diagnostics = buildHelpDiagnostics({
      ...diagnosticContext,
      security: { clipboard_timeout_seconds: policy },
      preferences: { exposure_timeout_seconds: requested },
    });
    assert.match(
      diagnostics,
      new RegExp(`Effective protected-value timeout \\(seconds\\): ${effective}`),
    );
  }
});

test('diagnostics do not invent unavailable security or preference values', () => {
  const diagnostics = buildHelpDiagnostics({ version: '0.26.2' });
  assert.ok(!diagnostics.includes('Security policy limit'));
  assert.ok(!diagnostics.includes('Effective protected-value timeout'));
});

test('Help states the exact local bearer-session boundary in plain language', async () => {
  const markup = await readFile(new URL('./index.html', import.meta.url), 'utf8');
  assert.match(markup, /accepts connections only from this computer/i);
  assert.match(markup, /removed from the address bar and kept in this browser tab/i);
  assert.match(markup, /Any app or browser on this computer with that link can access this session while Crosstache is running\./);
  assert.match(markup, /Do not share it\./);
  assert.match(markup, /Copied diagnostics omit the link and token\./);
});

test('mountHelp copies only the redacted diagnostic contract', async () => {
  const copy = new FakeElement();
  const status = new FakeElement();
  const document = fakeDocument({
    'help-copy-diagnostics': copy,
    'help-copy-status': status,
  });
  const writes = [];
  mountHelp({
    context: () => diagnosticContext,
    document,
    clipboard: { async writeText(value) { writes.push(value); } },
  });
  copy.dispatch('click');
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(writes.length, 1);
  assert.equal(writes[0], buildHelpDiagnostics(diagnosticContext));
  assert.equal(status.textContent, 'Diagnostics copied.');
});

test('mountHelp loads server preferences before copying the effective timeout', async () => {
  const copy = new FakeElement();
  const writes = [];
  let loaded = false;
  mountHelp({
    context: () => ({ ...diagnosticContext, preferences: undefined }),
    preferences: {
      async load() { loaded = true; },
      snapshot() {
        return loaded ? { exposure_timeout_seconds: 17 } : { exposure_timeout_seconds: 30 };
      },
    },
    document: fakeDocument({
      'help-copy-diagnostics': copy,
      'help-copy-status': new FakeElement(),
    }),
    clipboard: { async writeText(value) { writes.push(value); } },
  });
  copy.dispatch('click');
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.match(writes[0], /Effective protected-value timeout \(seconds\): 17/);
});
