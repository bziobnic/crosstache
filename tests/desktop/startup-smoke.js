import test from 'node:test';
import assert from 'node:assert/strict';

import { createStartupWorkflow } from '../../desktop/frontend/loading.js';

const createHarness = (snapshot = {
  kind: 'setup-required',
  config_path: '/tmp/isolated-xv.conf',
}) => {
  const calls = [];
  const views = [];
  let stateListener;
  const invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === 'startup_status') return snapshot;
    if (command === 'preview_setup') {
      return {
        backend: args.request.backend,
        vault: args.request.vault || args.request.vault_prefix,
      };
    }
    if (command === 'retry_startup') return snapshot;
    return {};
  };
  const workflow = createStartupWorkflow({
    invoke,
    listen: async (event, listener) => {
      assert.equal(event, 'xv://startup-state');
      stateListener = listener;
      return () => {};
    },
    onRender: (view) => views.push(view),
  });
  return {
    calls,
    views,
    workflow,
    emit: (payload) => stateListener({ payload }),
  };
};

test('mocked startup reaches setup without any real filesystem command', async () => {
  const harness = createHarness();

  await harness.workflow.start();

  assert.deepEqual(harness.calls, [{ command: 'startup_status', args: undefined }]);
  assert.match(harness.views.at(-1).textContent, /Choose where Crosstache stores secrets/);
  assert.match(harness.views.at(-1).textContent, /\/tmp\/isolated-xv\.conf/);
});

test('all provider previews send only their documented non-secret fields', async () => {
  const harness = createHarness();
  const cases = [
    ['local', {
      store_path: '/tmp/store',
      key_file: '/tmp/key',
      vault: 'personal',
    }],
    ['azure', {
      subscription_id: 'sub',
      tenant_id: 'tenant',
      vault: 'team-vault',
      resource_group: 'team',
      location: 'eastus',
    }],
    ['aws', {
      region: 'us-east-1',
      profile: '',
      vault_prefix: 'team',
    }],
  ];

  for (const [kind, values] of cases) {
    const result = await harness.workflow.preview(kind, values);
    assert.equal(result.ok, true);
    assert.equal(result.request.backend, kind);
    const serialized = JSON.stringify(result.request);
    assert.doesNotMatch(serialized, /secret|password|token|access_key/i);
  }
  assert.equal(harness.calls.filter(({ command }) => command === 'preview_setup').length, 3);
});

test('apply is impossible before preview and reuses the exact previewed request', async () => {
  const harness = createHarness();

  const blocked = await harness.workflow.apply();
  assert.equal(blocked.ok, false);
  assert.equal(harness.calls.length, 0);

  const preview = await harness.workflow.preview('local', {
    store_path: ' /tmp/store ',
    key_file: ' /tmp/key ',
    vault: ' personal ',
  });
  const applied = await harness.workflow.apply();

  assert.equal(applied.ok, true);
  assert.deepEqual(
    harness.calls.filter(({ command }) => command === 'apply_setup')[0].args.request,
    preview.request,
  );
});

test('a safe pre-commit apply failure preserves the preview for an explicit retry', async () => {
  const calls = [];
  let attempts = 0;
  const workflow = createStartupWorkflow({
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === 'preview_setup') return { backend: 'local', vault: 'personal' };
      if (command === 'apply_setup' && attempts++ === 0) {
        throw {
          code: 'xv-config-invalid',
          operation: 'apply-setup',
          message: 'The candidate configuration is invalid.',
          diagnostics: 'No values were persisted.',
        };
      }
      return {};
    },
    listen: async () => () => {},
    onRender: () => {},
  });
  await workflow.preview('local', {
    store_path: '/tmp/store',
    key_file: '/tmp/key',
    vault: 'personal',
  });

  assert.equal((await workflow.apply()).ok, false);
  assert.equal((await workflow.apply()).ok, true);
  assert.equal(calls.filter(({ command }) => command === 'preview_setup').length, 1);
  assert.equal(calls.filter(({ command }) => command === 'apply_setup').length, 2);
});

test('recovery event stays visible and safe remediation commands take no caller data', async () => {
  const harness = createHarness();
  await harness.workflow.start();

  harness.emit({
    kind: 'recoverable-failure',
    config_path: '/tmp/isolated-xv.conf',
    error: {
      code: 'xv-auth-failed',
      operation: 'list-secrets',
      backend: 'azure',
      vault: 'team-vault',
      message: 'Authentication failed.',
      hint: "Run 'az login', then retry.",
      diagnostics: 'Azure CLI session unavailable.',
    },
  });

  assert.match(harness.views.at(-1).textContent, /xv-auth-failed/);
  await harness.workflow.openConfig();
  await harness.workflow.copyDiagnostics();
  assert.deepEqual(harness.calls.slice(-2), [
    { command: 'open_config', args: undefined },
    { command: 'copy_diagnostics', args: undefined },
  ]);
});

test('invalid Azure and AWS previews return safe persistent errors, and Retry reruns startup', async () => {
  const calls = [];
  const views = [];
  const workflow = createStartupWorkflow({
    invoke: async (command, args) => {
      calls.push({ command, args });
      if (command === 'preview_setup') {
        throw {
          code: 'xv-config-invalid',
          operation: 'preview-setup',
          backend: args.request.backend,
          vault: args.request.vault || args.request.vault_prefix,
          message: 'Provider scope is invalid.',
          hint: 'Review the highlighted connection details.',
          diagnostics: 'No configuration was changed.',
        };
      }
      if (command === 'retry_startup') {
        return { kind: 'setup-required', config_path: '/tmp/isolated-xv.conf' };
      }
      return { kind: 'setup-required', config_path: '/tmp/isolated-xv.conf' };
    },
    listen: async () => () => {},
    onRender: (view) => views.push(view),
  });

  const azure = await workflow.preview('azure', {
    subscription_id: 'invalid',
    tenant_id: 'invalid',
    vault: 'team',
    resource_group: 'missing',
    location: 'invalid',
  });
  const aws = await workflow.preview('aws', {
    region: 'invalid',
    profile: '',
    vault_prefix: 'team',
  });

  assert.equal(azure.ok, false);
  assert.equal(azure.error.code, 'xv-config-invalid');
  assert.equal(aws.ok, false);
  assert.equal(aws.error.code, 'xv-config-invalid');
  await workflow.retry();
  assert.equal(calls.at(-1).command, 'retry_startup');
  assert.match(views.at(-1).textContent, /Choose where Crosstache stores secrets/);
});

test('provider command errors are rendered only through the safe recovery model', async () => {
  const views = [];
  const workflow = createStartupWorkflow({
    invoke: async (command) => {
      if (command === 'startup_status') {
        throw {
          code: 'xv-config-invalid',
          operation: 'load-config',
          backend: 'unknown',
          vault: '',
          message: '<script>not markup</script>',
          hint: 'Open config.',
          diagnostics: 'redacted',
        };
      }
      return {};
    },
    listen: async () => () => {},
    onRender: (view) => views.push(view),
  });

  await workflow.start();
  assert.match(views.at(-1).textContent, /not markup/);
  assert.doesNotMatch(views.at(-1).html, /<script>/);
});
