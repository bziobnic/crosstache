import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createPreferenceClient } from './preferences.js';
import { PALETTES } from './theme.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const FOREST_CUSTOM_THEME = { light: { ...PALETTES.forest.light }, dark: { ...PALETTES.forest.dark } };

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
    'column_widths', 'custom_theme', 'density', 'exposure_timeout_seconds',
    'folder_expansion', 'palette', 'theme', 'version',
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
  assert.match(html, /id="settings-status"[^>]*role="alert"[^>]*aria-live="assertive"/);
  assert.match(html, /id="settings-retry"/);
  assert.match(html, /id="settings-error-retry"/);
  assert.match(html, /Settings need attention/);
  const main = html.split(/<main[^>]*>/)[1].split('</main>')[0];
  assert.match(main, /id="settings-status"/, 'global status remains beside auth recovery');
});

test('explicit retry repeats a failed background load and clears only on success', async () => {
  const errors = [];
  let attempts = 0;
  const preferences = createPreferenceClient(async (method) => {
    assert.equal(method, 'GET');
    attempts++;
    if (attempts === 1) throw new Error('Preferences unavailable');
    return { version: 1, theme: 'dark' };
  }, {
    onSettingsError: (error) => errors.push(error),
  });

  await preferences.load();
  assert.equal(preferences.settingsError().message, 'Preferences unavailable');
  assert.equal(preferences.snapshot().theme, 'system');

  await preferences.retry();

  assert.equal(attempts, 2);
  assert.equal(preferences.snapshot().theme, 'dark');
  assert.equal(preferences.settingsError(), null);
  assert.deepEqual(errors.at(-1), null);
});

test('explicit retry persists the latest overrides after a failed save', async () => {
  const clock = manualClock();
  let putAttempts = 0;
  const preferences = createPreferenceClient(async (method, _path, body) => {
    if (method === 'GET') return { version: 1 };
    putAttempts++;
    if (putAttempts === 1) throw new Error('Preferences read-only');
    return body;
  }, clock);
  await preferences.load();
  preferences.set('theme', 'dark');
  await clock.run();
  assert.equal(preferences.settingsError().message, 'Preferences read-only');

  await preferences.retry();

  assert.equal(putAttempts, 2);
  assert.equal(preferences.snapshot().theme, 'dark');
  assert.equal(preferences.settingsError(), null);
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

test('version-2 defaults include palette forest and matching Forest custom-theme defaults', () => {
  const preferences = createPreferenceClient(null);
  const snapshot = preferences.snapshot();
  assert.equal(snapshot.version, 2);
  assert.equal(snapshot.palette, 'forest');
  assert.deepEqual(snapshot.custom_theme, FOREST_CUSTOM_THEME);
});

test('v0/v1 data migrates to v2 by retaining prior fields and supplying palette+custom defaults', async () => {
  const preferences = createPreferenceClient(async (method) => {
    if (method === 'GET') return { theme: 'dark', density: 'compact' };
    throw new Error('unexpected write');
  });
  const loaded = await preferences.load();
  assert.equal(loaded.version, 2);
  assert.equal(loaded.theme, 'dark');
  assert.equal(loaded.density, 'compact');
  assert.equal(loaded.palette, 'forest');
  assert.deepEqual(loaded.custom_theme, FOREST_CUSTOM_THEME);
});

test('missing, v0, and v1 preference data ignores same-named v2 extension fields', async () => {
  for (const version of [undefined, 0, 1]) {
    const legacy = {
      theme: 'dark',
      density: 'compact',
      palette: 'custom',
      custom_theme: {
        light: { canvas: '#ffffff', surface: '#ffffff', text: '#000000', accent: '#000000', danger: '#000000' },
        dark: { canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#ffffff', danger: '#ffffff' },
      },
    };
    if (version !== undefined) legacy.version = version;
    const preferences = createPreferenceClient(async (method) => {
      if (method === 'GET') return legacy;
      throw new Error('unexpected write');
    });
    const loaded = await preferences.load();
    assert.equal(loaded.theme, 'dark', `version ${String(version)}`);
    assert.equal(loaded.density, 'compact', `version ${String(version)}`);
    assert.equal(loaded.palette, 'forest', `version ${String(version)}`);
    assert.deepEqual(loaded.custom_theme, FOREST_CUSTOM_THEME, `version ${String(version)}`);
  }
});

test('future preference versions fail safe to defaults without retaining future fields', async () => {
  const preferences = createPreferenceClient(async (method) => {
    if (method === 'GET') return {
      version: 3,
      theme: 'dark',
      density: 'compact',
      palette: 'custom',
      custom_theme: {
        light: { canvas: '#ffffff', surface: '#ffffff', text: '#000000', accent: '#000000', danger: '#000000' },
        dark: { canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#ffffff', danger: '#ffffff' },
      },
    };
    throw new Error('unexpected write');
  });
  const loaded = await preferences.load();
  assert.equal(loaded.theme, 'system');
  assert.equal(loaded.density, 'comfortable');
  assert.equal(loaded.palette, 'forest');
  assert.deepEqual(loaded.custom_theme, FOREST_CUSTOM_THEME);
});

test('unknown palette values fall back to the forest default', async () => {
  const preferences = createPreferenceClient(async (method) => {
    if (method === 'GET') return { version: 2, palette: 'not-a-real-palette' };
    throw new Error('unexpected write');
  });
  const loaded = await preferences.load();
  assert.equal(loaded.palette, 'forest');
});

test('set(palette, ...) only accepts the five known palette values', () => {
  const preferences = createPreferenceClient(null);
  for (const value of ['forest', 'nord', 'solarized', 'high-contrast', 'custom']) {
    assert.equal(preferences.set('palette', value), true, value);
  }
  for (const value of ['Forest', 'unknown', '', null, 42]) {
    assert.equal(preferences.set('palette', value), false, String(value));
  }
});

test('set(custom_theme, ...) requires exact shape, strict hex, and passing contrast in both variants', () => {
  const preferences = createPreferenceClient(null);
  const valid = {
    light: { canvas: '#ffffff', surface: '#fafafa', text: '#000000', accent: '#003399', danger: '#a30000' },
    dark: { canvas: '#000000', surface: '#0a0a0a', text: '#ffffff', accent: '#3399ff', danger: '#ff5555' },
  };
  assert.equal(preferences.set('custom_theme', valid), true);
  assert.deepEqual(preferences.get('custom_theme'), valid);

  const missingDark = { light: valid.light };
  assert.equal(preferences.set('custom_theme', missingDark), false);

  const extraKey = { ...valid, light: { ...valid.light, extra: '#000000' } };
  assert.equal(preferences.set('custom_theme', extraKey), false);

  const badHex = { ...valid, light: { ...valid.light, accent: 'not-a-hex' } };
  assert.equal(preferences.set('custom_theme', badHex), false);

  const lowContrast = {
    light: { canvas: '#808080', surface: '#888888', text: '#7a7a7a', accent: '#8a8a8a', danger: '#8f8f8f' },
    dark: valid.dark,
  };
  assert.equal(preferences.set('custom_theme', lowContrast), false);

  assert.deepEqual(preferences.get('custom_theme'), valid, 'last valid custom theme must remain after rejected writes');
});

test('valid custom_theme round-trips through save and reload unchanged', async () => {
  let stored = null;
  const api = async (method, _path, body) => {
    if (method === 'GET') return stored ?? { version: 2 };
    stored = body;
    return body;
  };
  const preferences = createPreferenceClient(api, {
    setTimeoutImpl: (callback) => { callback(); return 1; },
    clearTimeoutImpl() {},
  });
  await preferences.load();
  const custom = {
    light: { canvas: '#ffffff', surface: '#fafafa', text: '#000000', accent: '#003399', danger: '#a30000' },
    dark: { canvas: '#000000', surface: '#0a0a0a', text: '#ffffff', accent: '#3399ff', danger: '#ff5555' },
  };
  assert.equal(preferences.set('palette', 'custom'), true);
  assert.equal(preferences.set('custom_theme', custom), true);
  await new Promise((resolve) => setTimeout(resolve, 0));

  const reloaded = createPreferenceClient(async (method) => (method === 'GET' ? stored : stored));
  const snapshot = await reloaded.load();
  assert.equal(snapshot.palette, 'custom');
  assert.deepEqual(snapshot.custom_theme, custom);
});
