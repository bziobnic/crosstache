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
