import test from 'node:test';
import assert from 'node:assert/strict';
import { createPreferenceClient } from './preferences.js';

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
