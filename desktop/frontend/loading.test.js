import test from 'node:test';
import assert from 'node:assert/strict';

import {
  renderRecovery,
  renderSetupForm,
  renderStartupState,
  validateSetup,
} from './loading.js';

const field = (view, name) => view.querySelector(`[name="${name}"]`);
const action = (view, name) => view.querySelector(`[data-action="${name}"]`);

test('loading and connecting name the current phase without transport details', () => {
  const loading = renderStartupState({ kind: 'loading-configuration' });
  const connecting = renderStartupState({
    kind: 'connecting',
    backend: 'azure',
    vault: 'team-vault',
  });

  assert.match(loading.textContent, /Loading configuration/);
  assert.match(connecting.textContent, /Connecting to team-vault/);
  assert.match(connecting.textContent, /Azure/);
  assert.doesNotMatch(`${loading.html}${connecting.html}`, /token=|127\.0\.0\.1/i);
});

test('setup required offers all supported backends without credential fields', () => {
  const view = renderStartupState({
    kind: 'setup-required',
    config_path: '/Users/example/.config/xv/xv.conf',
  });

  assert.match(view.textContent, /Create local vault/);
  assert.match(view.textContent, /Connect Azure/);
  assert.match(view.textContent, /Connect AWS/);
  assert.match(view.textContent, /Advanced configuration/);
  assert.equal(field(view, 'client_secret'), null);
  assert.equal(field(view, 'access_key'), null);
  assert.equal(field(view, 'password'), null);
});

test('local setup collects only store path, key file, and vault', () => {
  const view = renderSetupForm('local');

  assert.ok(field(view, 'store_path'));
  assert.ok(field(view, 'key_file'));
  assert.ok(field(view, 'vault'));
  assert.match(view.html, /aria-describedby="store_path-hint store_path-error"/);
  assert.equal(view.querySelector('input[type="password"]'), null);
});

test('azure setup collects the complete non-secret scope', () => {
  const view = renderSetupForm('azure');

  for (const name of [
    'subscription_id',
    'tenant_id',
    'vault',
    'resource_group',
    'location',
  ]) {
    assert.ok(field(view, name), `missing Azure field ${name}`);
  }
  assert.equal(field(view, 'client_secret'), null);
  assert.equal(field(view, 'token'), null);
});

test('aws setup collects region, optional profile, and vault prefix', () => {
  const view = renderSetupForm('aws');

  assert.ok(field(view, 'region'));
  assert.ok(field(view, 'profile'));
  assert.ok(field(view, 'vault_prefix'));
  assert.match(view.textContent, /Profile \(optional\)/);
  assert.equal(field(view, 'access_key'), null);
  assert.equal(field(view, 'secret_key'), null);
  assert.equal(field(view, 'session_token'), null);
});

test('setup validation is persistent, inline, and leaves optional provider scope alone', () => {
  const errors = validateSetup('aws', {
    region: ' ',
    profile: '',
    vault_prefix: '',
  });
  const view = renderSetupForm('aws');

  assert.deepEqual(errors, {
    region: 'Region is required.',
    vault_prefix: 'Vault prefix is required.',
  });
  assert.match(view.html, /data-form-status role="alert"/);
  assert.match(view.html, /data-preview hidden/);
  assert.match(view.html, /data-action="apply" hidden disabled/);
});

test('advanced setup shows the exact config path and CLI equivalents', () => {
  const configPath = '/Users/example/.config/xv/xv.conf';
  const view = renderSetupForm('advanced', { configPath });

  assert.match(view.textContent, new RegExp(configPath.replaceAll('/', '\\/')));
  assert.match(view.textContent, /xv init/);
  assert.match(view.textContent, /az login/);
  assert.match(view.textContent, /aws sso login/);
  assert.ok(action(view, 'open-config'));
});

test('recovery keeps safe evidence and every remediation action visible', () => {
  const view = renderRecovery({
    code: 'xv-auth-failed',
    operation: 'list-secrets',
    backend: 'azure',
    vault: 'team-vault',
    message: 'Authentication with the selected backend failed.',
    hint: "Run 'az login', then try again.",
    diagnostics: 'Azure CLI session is unavailable.',
  });

  for (const text of [
    'xv-auth-failed',
    'list-secrets',
    'azure',
    'team-vault',
    'Authentication with the selected backend failed.',
    "Run 'az login', then try again.",
    'Azure CLI session is unavailable.',
  ]) {
    assert.match(view.textContent, new RegExp(text.replaceAll(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  }
  for (const name of [
    'retry',
    'choose-backend',
    'open-config',
    'copy-diagnostics',
    'show-cli',
  ]) {
    assert.ok(action(view, name), `missing recovery action ${name}`);
  }
  assert.match(view.html, /<details[^>]*>/);
  assert.match(view.html, /role="alert"/);
});

test('renderer escapes untrusted diagnostics instead of creating markup', () => {
  const view = renderRecovery({
    code: 'xv-config-invalid',
    operation: 'load-config',
    backend: 'unknown',
    vault: '',
    message: '<script>steal()</script>',
    hint: 'Review <strong>fields</strong>.',
    diagnostics: 'token=<img src=x onerror=steal()>',
  });

  assert.doesNotMatch(view.html, /<script>|<img|<strong>/);
  assert.match(view.html, /&lt;script&gt;|&lt;img|&lt;strong&gt;/);
});
