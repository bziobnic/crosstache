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
import { PALETTES, resolveTokens } from './theme.js';

class FakeStyle {
  constructor() {
    this.properties = new Map();
  }

  setProperty(name, value) {
    this.properties.set(name, value);
  }

  getPropertyValue(name) {
    return this.properties.get(name) ?? '';
  }
}

class FakeElement {
  constructor(value = '') {
    this.value = value;
    this.textContent = '';
    this.dataset = {};
    this.disabled = false;
    this.hidden = false;
    this.ariaInvalid = null;
    this.style = new FakeStyle();
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

const FOREST_CUSTOM_THEME = { light: { ...PALETTES.forest.light }, dark: { ...PALETTES.forest.dark } };

function fakeThemeSettingsDocument() {
  const paletteSelect = new FakeElement('forest');
  const customFieldset = new FakeElement();
  const variantSelect = new FakeElement('light');
  const colorInputs = {
    canvas: new FakeElement(),
    surface: new FakeElement(),
    text: new FakeElement(),
    accent: new FakeElement(),
    danger: new FakeElement(),
  };
  const resetCustom = new FakeElement();
  const settingsStatus = new FakeElement();
  const customStatus = new FakeElement();
  const layoutReset = new FakeElement();
  const document = fakeDocument({
    'palette-select': paletteSelect,
    'custom-theme-fieldset': customFieldset,
    'custom-variant-select': variantSelect,
    'custom-color-canvas': colorInputs.canvas,
    'custom-color-surface': colorInputs.surface,
    'custom-color-text': colorInputs.text,
    'custom-color-accent': colorInputs.accent,
    'custom-color-danger': colorInputs.danger,
    'custom-theme-reset': resetCustom,
    'settings-live': settingsStatus,
    'custom-theme-status': customStatus,
    'layout-reset': layoutReset,
  });
  return {
    document,
    paletteSelect,
    customFieldset,
    variantSelect,
    colorInputs,
    resetCustom,
    status: customStatus,
    customStatus,
    settingsStatus,
    layoutReset,
  };
}

function fakePreferences(initial = {}) {
  const values = {
    theme: 'system',
    density: 'comfortable',
    palette: 'forest',
    custom_theme: { light: { ...FOREST_CUSTOM_THEME.light }, dark: { ...FOREST_CUSTOM_THEME.dark } },
    ...initial,
  };
  const changes = [];
  return {
    values,
    changes,
    async load() { return { ...values }; },
    get(key, fallback) { return values[key] ?? fallback; },
    set(key, value) {
      changes.push([key, value]);
      values[key] = value;
      return true;
    },
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

test('a built-in palette applies its resolved tokens as inline custom properties and sets data-palette', async () => {
  const { document } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'nord' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;

  const root = document.documentElement;
  assert.equal(root.dataset.palette, 'nord');
  assert.equal(root.style.getPropertyValue('color-scheme'), 'light');
  const tokens = resolveTokens('nord', 'light', preferences.get('custom_theme'));
  assert.equal(root.style.getPropertyValue('--color-canvas'), tokens.canvas);
  assert.equal(root.style.getPropertyValue('--color-accent'), tokens.accent);
  assert.equal(root.style.getPropertyValue('--rail-bg'), tokens.railBg);
});

test('mode and palette stay independent: System mode follows prefers-color-scheme for a non-Forest palette', async () => {
  const { document } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ theme: 'system', palette: 'solarized' });
  const listeners = new Map();
  const mediaQuery = {
    matches: false,
    addEventListener(type, listener) { listeners.set(type, listener); },
    removeEventListener(type) { listeners.delete(type); },
  };
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery });
  await mounted.ready;

  const root = document.documentElement;
  assert.equal(root.dataset.effectiveTheme, 'light');
  assert.equal(root.style.getPropertyValue('--color-canvas'), resolveTokens('solarized', 'light', preferences.get('custom_theme')).canvas);

  mediaQuery.matches = true;
  listeners.get('change')?.();
  assert.equal(root.dataset.effectiveTheme, 'dark');
  assert.equal(root.style.getPropertyValue('color-scheme'), 'dark');
  assert.equal(root.style.getPropertyValue('--color-canvas'), resolveTokens('solarized', 'dark', preferences.get('custom_theme')).canvas);
  assert.equal(preferences.get('palette'), 'solarized', 'palette is untouched by mode changes');
});

test('the custom fieldset is only shown when palette is custom', async () => {
  const forest = fakeThemeSettingsDocument();
  const forestPreferences = fakePreferences({ palette: 'forest' });
  await (await mountSettings({ preferences: forestPreferences, securityPolicy: 0, document: forest.document, mediaQuery: { matches: false } })).ready;
  assert.equal(forest.customFieldset.hidden, true);

  const custom = fakeThemeSettingsDocument();
  const customPreferences = fakePreferences({ palette: 'custom' });
  await (await mountSettings({ preferences: customPreferences, securityPolicy: 0, document: custom.document, mediaQuery: { matches: false } })).ready;
  assert.equal(custom.customFieldset.hidden, false);
});

test('selecting a palette persists it, applies its tokens, and toggles the custom fieldset', async () => {
  const { document, paletteSelect, customFieldset } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'forest' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;

  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');
  assert.equal(preferences.get('palette'), 'custom');
  assert.equal(customFieldset.hidden, false);
  assert.equal(document.documentElement.dataset.palette, 'custom');
});

test('valid custom color edits apply a live preview and persist through preferences', async () => {
  const { document, paletteSelect, colorInputs } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');

  colorInputs.accent.value = '#003399';
  colorInputs.accent.dispatch('input');

  assert.equal(preferences.get('custom_theme').light.accent, '#003399');
  assert.equal(document.documentElement.style.getPropertyValue('--color-accent'), '#003399');
  assert.equal(colorInputs.accent.ariaInvalid, 'false');
});

test('invalid custom colors show an actionable status and never replace the last valid applied theme', async () => {
  const {
    document, paletteSelect, colorInputs, customStatus, settingsStatus,
  } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');

  const beforeAccent = document.documentElement.style.getPropertyValue('--color-accent');
  const setCallsBefore = preferences.changes.length;

  colorInputs.accent.value = '#cccccc';
  colorInputs.accent.dispatch('input');

  assert.equal(document.documentElement.style.getPropertyValue('--color-accent'), beforeAccent);
  assert.equal(preferences.get('custom_theme').light.accent, PALETTES.forest.light.accent);
  assert.equal(preferences.changes.length, setCallsBefore, 'an invalid edit must not be persisted');
  assert.equal(colorInputs.accent.ariaInvalid, 'true');
  assert.match(customStatus.textContent, /accent vs surface is [0-9.]+:1, needs 4.5:1/);
  assert.equal(settingsStatus.textContent, '', 'custom validation must not duplicate into the general Settings live region');

  colorInputs.accent.value = 'not-a-hex';
  colorInputs.accent.dispatch('input');
  assert.equal(colorInputs.accent.ariaInvalid, 'true');
});

test('incomplete hex typing is retained without live-region spam or persistence', async () => {
  const {
    document, colorInputs, customStatus, settingsStatus,
  } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  const before = preferences.changes.length;

  colorInputs.accent.value = '#12';
  colorInputs.accent.dispatch('input');

  assert.equal(colorInputs.accent.value, '#12');
  assert.equal(colorInputs.accent.ariaInvalid, 'true');
  assert.equal(customStatus.textContent, '');
  assert.equal(settingsStatus.textContent, '');
  assert.equal(preferences.changes.length, before);
});

test('per-variant drafts permit an invalid multi-field transition and persist only the completed valid variant', async () => {
  const { document, variantSelect, colorInputs, customStatus } = fakeThemeSettingsDocument();
  const initial = {
    light: { canvas: '#ffffff', surface: '#ffffff', text: '#000000', accent: '#000000', danger: '#000000' },
    dark: { canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#ffffff', danger: '#ffffff' },
  };
  const preferences = fakePreferences({ palette: 'custom', custom_theme: structuredClone(initial) });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  const beforeCanvas = document.documentElement.style.getPropertyValue('--color-canvas');

  for (const [key, value] of [
    ['canvas', '#000000'],
    ['surface', '#000000'],
    ['text', '#ffffff'],
    ['accent', '#ffffff'],
  ]) {
    colorInputs[key].value = value;
    colorInputs[key].dispatch('input');
    assert.equal(colorInputs[key].value, value, `${key} draft remains visible`);
    assert.deepEqual(preferences.get('custom_theme').light, initial.light, `${key} invalid draft is not persisted`);
    assert.equal(document.documentElement.style.getPropertyValue('--color-canvas'), beforeCanvas);
  }

  colorInputs.danger.value = '#ffffff';
  colorInputs.danger.dispatch('input');
  assert.deepEqual(preferences.get('custom_theme').light, {
    canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#ffffff', danger: '#ffffff',
  });
  assert.deepEqual(preferences.get('custom_theme').dark, initial.dark);
  assert.equal(document.documentElement.style.getPropertyValue('--color-canvas'), '#000000');
  assert.match(customStatus.textContent, /preview updated/i);
  assert.doesNotMatch(customStatus.textContent, /saved/i);

  variantSelect.value = 'dark';
  variantSelect.dispatch('change');
  colorInputs.accent.value = '#12';
  colorInputs.accent.dispatch('input');
  variantSelect.value = 'light';
  variantSelect.dispatch('change');
  variantSelect.value = 'dark';
  variantSelect.dispatch('change');
  assert.equal(colorInputs.accent.value, '#12', 'invalid dark draft survives variant switches');
  assert.deepEqual(preferences.get('custom_theme').dark, initial.dark);
});

test('the custom variant selector edits light and dark independently', async () => {
  const { document, paletteSelect, variantSelect, colorInputs } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');

  variantSelect.value = 'dark';
  variantSelect.dispatch('change');
  assert.equal(colorInputs.accent.value, PALETTES.forest.dark.accent);

  colorInputs.accent.value = '#3399ff';
  colorInputs.accent.dispatch('input');
  assert.equal(preferences.get('custom_theme').dark.accent, '#3399ff');
  assert.equal(preferences.get('custom_theme').light.accent, PALETTES.forest.light.accent);
});

test('reset restores both custom variants to Forest defaults and re-applies when active', async () => {
  const { document, paletteSelect, colorInputs, resetCustom } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');

  colorInputs.accent.value = '#003399';
  colorInputs.accent.dispatch('input');
  assert.notEqual(preferences.get('custom_theme').light.accent, PALETTES.forest.light.accent);

  resetCustom.dispatch('click');
  assert.deepEqual(preferences.get('custom_theme'), FOREST_CUSTOM_THEME);
  assert.equal(document.documentElement.style.getPropertyValue('--color-accent'), PALETTES.forest.light.accent);
});

test('layout reset preserves theme, palette, and custom theme state', async () => {
  const { document, paletteSelect, colorInputs, layoutReset } = fakeThemeSettingsDocument();
  const preferences = fakePreferences({ palette: 'custom', theme: 'dark' });
  const mounted = mountSettings({ preferences, securityPolicy: 0, document, mediaQuery: { matches: false } });
  await mounted.ready;
  paletteSelect.value = 'custom';
  paletteSelect.dispatch('change');
  colorInputs.accent.value = '#003399';
  colorInputs.accent.dispatch('input');

  const customBefore = preferences.get('custom_theme');
  layoutReset.dispatch('click');

  assert.equal(preferences.get('theme'), 'dark');
  assert.equal(preferences.get('palette'), 'custom');
  assert.deepEqual(preferences.get('custom_theme'), customBefore);
});
