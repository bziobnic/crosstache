import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

test('frontend modules expose the approved boundaries', () => {
  const expected = {
    'api-client.js': 'export function createApiClient',
    'store.js': 'export function createStore',
    'dialogs.js': 'export function createDialogManager',
    'preferences.js': 'export function createPreferenceClient',
    'secrets.js': 'export function mountSecrets',
  };
  for (const [name, marker] of Object.entries(expected)) {
    const source = fs.readFileSync(path.join(__dirname, name), 'utf8');
    assert.match(source, new RegExp(marker));
  }
  const apiClient = fs.readFileSync(path.join(__dirname, 'api-client.js'), 'utf8');
  assert.match(apiClient, /createApiClient\(\{ token, onInflight, fetchImpl = globalThis\.fetch \}\)/);
  assert.doesNotMatch(apiClient, /xhrFactory|createXhr/);
});
