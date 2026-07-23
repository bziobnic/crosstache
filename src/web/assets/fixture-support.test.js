import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';

import { buildLocalConfig, waitForUiUrl } from '../../../tests/web/fixture-support.js';

test('browser fixture emits every required persisted Config field', () => {
  const config = buildLocalConfig({ store: '/tmp/store', keyFile: '/tmp/key', vault: 'playwright' });
  for (const field of [
    'debug = false',
    'subscription_id = ""',
    'default_vault = "playwright"',
    'default_resource_group = ""',
    'default_location = ""',
    'tenant_id = ""',
    'output_json = false',
    'no_color = true',
    'cache_enabled = false',
    'cache_ttl_secs = 0',
    'clipboard_timeout = 0',
  ]) {
    assert.match(config, new RegExp(field.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  }
  assert.match(config, /\[local\][\s\S]*default_vault = "playwright"/);
});

test('browser fixture startup timeout includes captured server output', async () => {
  const child = new EventEmitter();
  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  let timeout;
  const pending = waitForUiUrl(child, {
    timeoutMs: 25,
    setTimer(callback) { timeout = callback; return 1; },
    clearTimer() {},
  });
  child.stderr.emit('data', Buffer.from('boot marker'));
  timeout();
  await assert.rejects(pending, /timed out.*boot marker/);
});

test('browser fixture startup exit fails immediately with captured server output', async () => {
  const child = new EventEmitter();
  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  const pending = waitForUiUrl(child);
  child.stderr.emit('data', Buffer.from('invalid config marker'));
  child.emit('exit', 2);
  await assert.rejects(pending, /exited with 2.*invalid config marker/);
});
