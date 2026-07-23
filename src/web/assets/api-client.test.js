import test from 'node:test';
import assert from 'node:assert/strict';
import { ApiError, createApiClient } from './api-client.js';

test('api client retains structured error fields', async () => {
  const fetch = async () => new Response(JSON.stringify({ error: {
    code: 'xv-conflict', message: 'Name exists', hint: 'Choose another name', field: 'name', details: { name: 'a' },
  }}), { status: 409, headers: { 'content-type': 'application/json' } });
  const api = createApiClient({ token: 't', fetchImpl: fetch });

  await assert.rejects(
    api('GET', '/x'),
    (error) => error instanceof ApiError
      && error.code === 'xv-conflict'
      && error.field === 'name'
      && error.hint === 'Choose another name'
      && error.details.name === 'a',
  );
});

test('API client creates XHRs through its injected factory', () => {
  const sentinel = { xhr: true };
  let calls = 0;
  const client = createApiClient({
    token: 'test-token',
    fetchImpl: async () => ({ ok: true, text: async () => '' }),
    xhrFactory: () => { calls++; return sentinel; },
  });

  assert.equal(client.createXhr(), sentinel);
  assert.equal(calls, 1);
});

test('API client forwards an abort signal without changing authentication', async () => {
  const controller = new AbortController();
  let options;
  const client = createApiClient({
    token: 'session-token',
    fetchImpl: async (_path, requestOptions) => {
      options = requestOptions;
      return { ok: true, text: async () => '' };
    },
  });

  await client('GET', '/api/context', undefined, false, { signal: controller.signal });

  assert.equal(options.signal, controller.signal);
  assert.equal(options.headers.Authorization, 'Bearer session-token');
});

test('API client emits exact lifecycle statuses with stable operation IDs', async () => {
  const events = [];
  const client = createApiClient({
    token: 'session-token',
    onOperation: (event) => events.push(event),
    fetchImpl: async () => ({ ok: true, text: async () => '[]' }),
  });

  await client('GET', '/api/secrets');

  assert.deepEqual(events.map(({ status }) => status), ['started', 'succeeded']);
  assert.equal(events[0].operationId, events[1].operationId);
  assert.match(events[0].operationId, /^request-/);
});

test('API client emits cancelled for aborts without leaking the request', async () => {
  const events = [];
  const secretMarker = 'must-not-leak';
  const controller = new AbortController();
  const client = createApiClient({
    token: 'session-token',
    onOperation: (event) => events.push(event),
    fetchImpl: async () => {
      controller.abort();
      throw Object.assign(new Error(`network failure ${secretMarker}`), { name: 'AbortError' });
    },
  });

  await assert.rejects(
    client('POST', '/api/secrets/private', {
      value: secretMarker,
      note: secretMarker,
      headers: { Authorization: secretMarker },
    }, false, { signal: controller.signal }),
    { name: 'AbortError' },
  );

  assert.deepEqual(events.map(({ status }) => status), ['started', 'cancelled']);
  assert.doesNotMatch(JSON.stringify(events), new RegExp(secretMarker));
});
