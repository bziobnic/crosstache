import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createPreferenceClient } from './preferences.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

function withStorage(run) {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
  const values = new Map();
  globalThis.localStorage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  try { return run(values); }
  finally {
    if (original) Object.defineProperty(globalThis, 'localStorage', original);
    else delete globalThis.localStorage;
  }
}

test('presentation column preferences persist when their schema is valid', () => withStorage((values) => {
  const preferences = createPreferenceClient(null);
  const widths = [28, 15, 14, 25, 18];
  assert.equal(preferences.set('xv.ui.columns.secrets.v1', widths), true);
  assert.equal(values.get('xv.ui.columns.secrets.v1'), JSON.stringify(widths));
  assert.deepEqual(preferences.get('xv.ui.columns.secrets.v1'), widths);
}));

test('secret-bearing and unknown preference keys never persist', () => withStorage((values) => {
  const preferences = createPreferenceClient(null);
  for (const key of ['secret.name', 'secret.value', 'secret.note', 'search.query', 'clipboard.contents', 'credentials.token', 'xv.ui.unknown']) {
    assert.equal(preferences.set(key, 'sensitive data'), false, key);
    assert.equal(preferences.get(key, 'fallback'), 'fallback', key);
  }
  assert.equal(values.size, 0);
}));

function manualClock() {
  let scheduled = null;
  return {
    setTimeoutImpl(callback, milliseconds) {
      assert.equal(milliseconds, 250);
      scheduled = callback;
      return 1;
    },
    clearTimeoutImpl() { scheduled = null; },
    async run() {
      assert.ok(scheduled, 'expected a debounced save');
      const callback = scheduled;
      scheduled = null;
      await callback();
    },
  };
}

test('preferences load once and expose deeply immutable whitelisted snapshots', async () => {
  const calls = [];
  const api = async (method, path) => {
    calls.push([method, path]);
    return {
      version: 1,
      theme: 'dark',
      exposure_timeout_seconds: 15,
      density: 'compact',
      folder_expansion: false,
      column_widths: { secrets: [30, 15, 14, 23, 18], files: [40, 14, 24, 22] },
      future_presentation: { accent: 'green' },
      secret_name: 'must-not-enter-client-state',
    };
  };
  const preferences = createPreferenceClient(api);

  await Promise.all([preferences.load(), preferences.load()]);
  const snapshot = preferences.snapshot();

  assert.deepEqual(calls, [['GET', '/api/preferences']]);
  assert.equal(snapshot.theme, 'dark');
  assert.equal(snapshot.future_presentation, undefined);
  assert.equal(snapshot.secret_name, undefined);
  assert.ok(Object.isFrozen(snapshot));
  assert.ok(Object.isFrozen(snapshot.column_widths));
  assert.ok(Object.isFrozen(snapshot.column_widths.secrets));
  assert.throws(() => { snapshot.theme = 'light'; }, TypeError);
  assert.throws(() => { snapshot.column_widths.secrets.push(1); }, TypeError);
});

test('preference saves are debounced and contain only whitelisted keys', async () => {
  const clock = manualClock();
  const calls = [];
  const api = async (method, path, body) => {
    calls.push({ method, path, body });
    if (method === 'GET') return { version: 1 };
    return body;
  };
  const preferences = createPreferenceClient(api, clock);
  await preferences.load();

  assert.equal(preferences.set('theme', 'dark'), true);
  assert.equal(preferences.set('density', 'compact'), true);
  assert.equal(preferences.set('secret_name', 'DB_URL'), false);
  assert.equal(preferences.set('search_query', 'payments'), false);
  await clock.run();

  assert.equal(calls.length, 2);
  assert.equal(calls[1].method, 'PUT');
  assert.equal(calls[1].path, '/api/preferences');
  assert.deepEqual(Object.keys(calls[1].body).sort(), [
    'column_widths', 'density', 'exposure_timeout_seconds',
    'folder_expansion', 'theme', 'version',
  ]);
  assert.equal(calls[1].body.theme, 'dark');
  assert.equal(calls[1].body.density, 'compact');
  assert.equal(calls[1].body.secret_name, undefined);
  assert.equal(calls[1].body.search_query, undefined);
});

test('failed preference saves report a non-blocking Settings error', async () => {
  const clock = manualClock();
  const errors = [];
  const api = async (method) => {
    if (method === 'GET') return { version: 1 };
    throw Object.assign(new Error('Disk unavailable'), { hint: 'Check config permissions.' });
  };
  const preferences = createPreferenceClient(api, {
    ...clock,
    onSettingsError: (error) => errors.push(error),
  });
  await preferences.load();
  assert.equal(preferences.set('theme', 'dark'), true);

  await assert.doesNotReject(clock.run());

  assert.equal(preferences.snapshot().theme, 'dark');
  assert.deepEqual(errors, [{
    message: 'Disk unavailable',
    hint: 'Check config permissions.',
  }]);
  assert.deepEqual(preferences.settingsError(), errors[0]);
});

test('production markup exposes a persistent accessible Settings error surface', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  assert.match(html, /id="settings-error"[^>]*class="error-panel"[^>]*role="alert"/);
  assert.match(html, /id="settings-error"[^>]*aria-live="assertive"/);
  assert.match(html, /Settings need attention/);
});

test('default Settings renderer shows failures and clears after a successful retry', async () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'document');
  const message = { textContent: '' };
  const hint = { textContent: '' };
  const surface = {
    hidden: true,
    querySelector(selector) {
      if (selector === '.error-message') return message;
      if (selector === '.error-hint') return hint;
      return null;
    },
  };
  globalThis.document = {
    getElementById(id) { return id === 'settings-error' ? surface : null; },
  };
  const clock = manualClock();
  let putAttempts = 0;
  const api = async (method, _path, body) => {
    if (method === 'GET') return { version: 1 };
    putAttempts += 1;
    if (putAttempts === 1) {
      throw Object.assign(new Error('Disk unavailable'), { hint: 'Check config permissions.' });
    }
    return body;
  };

  try {
    const preferences = createPreferenceClient(api, clock);
    await preferences.load();
    assert.equal(preferences.set('theme', 'dark'), true);
    await assert.doesNotReject(clock.run());
    assert.equal(surface.hidden, false);
    assert.equal(message.textContent, 'Disk unavailable');
    assert.equal(hint.textContent, 'Check config permissions.');

    assert.equal(preferences.set('theme', 'light'), true);
    await assert.doesNotReject(clock.run());
    assert.equal(surface.hidden, true);
    assert.equal(message.textContent, '');
    assert.equal(hint.textContent, '');
    assert.equal(preferences.settingsError(), null);
  } finally {
    if (original) Object.defineProperty(globalThis, 'document', original);
    else delete globalThis.document;
  }
});
