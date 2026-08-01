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

test('XHR upload reports progress, enters finishing, and resolves parsed 2xx response', async () => {
  const events = [];
  const xhr = {
    upload: {},
    open(method, path) { this.method = method; this.path = path; },
    setRequestHeader(name, value) { this.header = [name, value]; },
    send(body) {
      this.body = body;
      this.upload.onprogress({ lengthComputable: true, loaded: 4, total: 8 });
      this.upload.onload();
      this.status = 201;
      this.responseText = '{"name":"report.pdf"}';
      this.onload();
    },
  };
  const client = createApiClient({ token: 'session-token', xhrFactory: () => xhr });
  const formData = new FormData();

  const result = await client.upload({
    path: '/api/files?vault=one',
    formData,
    onProgress: (event) => events.push(event),
  });

  assert.deepEqual(result, { name: 'report.pdf' });
  assert.deepEqual(events, [
    { loaded: 4, total: 8 },
    { loaded: 8, total: 8, finishing: true },
  ]);
  assert.deepEqual(xhr.header, ['Authorization', 'Bearer session-token']);
});

test('XHR upload wires AbortSignal to xhr.abort and rejects with AbortError', async () => {
  const controller = new AbortController();
  let xhr;
  const client = createApiClient({
    token: 'session-token',
    xhrFactory: () => (xhr = {
      upload: {},
      open() {},
      setRequestHeader() {},
      send() {},
      abort() { this.onabort(); },
    }),
  });
  const pending = client.upload({
    path: '/api/files',
    formData: new FormData(),
    signal: controller.signal,
  });
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  assert.ok(xhr);
});

test('XHR upload with an already-aborted signal settles without sending bytes', async () => {
  const controller = new AbortController();
  controller.abort();
  let sent = false;
  const client = createApiClient({
    token: 'session-token',
    xhrFactory: () => ({
      upload: {},
      open() {},
      setRequestHeader() {},
      send() { sent = true; },
      abort() {},
    }),
  });

  await assert.rejects(client.upload({
    path: '/api/files',
    formData: new FormData(),
    signal: controller.signal,
  }), { name: 'AbortError' });
  assert.equal(sent, false);
});

for (const [label, responseText] of [
  ['empty', ''],
  ['malformed', '{'],
  ['incomplete', '{}'],
]) {
  test(`XHR upload treats ${label} 2xx confirmation as ambiguous`, async () => {
    const xhr = {
      upload: {},
      open() {},
      setRequestHeader() {},
      send() {
        this.status = 200;
        this.responseText = responseText;
        this.onload();
      },
    };
    const client = createApiClient({ token: 'session-token', xhrFactory: () => xhr });
    await assert.rejects(client.upload({
      path: '/api/files',
      formData: new FormData(),
    }), (error) => error?.ambiguous === true && error?.name === 'AmbiguousUploadError');
  });
}

for (const phase of ['factory', 'open', 'header', 'send']) {
  test(`XHR upload cleans up and emits one terminal event when ${phase} throws synchronously`, async () => {
    const inflight = [];
    const operations = [];
    let added = 0;
    let removed = 0;
    const signal = {
      aborted: false,
      addEventListener() { added++; },
      removeEventListener() { removed++; },
    };
    const xhr = {
      upload: {},
      open() {
        if (phase === 'open') throw new Error('open failed');
      },
      setRequestHeader() {
        if (phase === 'header') throw new Error('header failed');
      },
      send() {
        if (phase === 'send') throw new Error('send failed');
      },
      abort() {},
    };
    const client = createApiClient({
      token: 'session-token',
      onInflight: (count) => inflight.push(count),
      onOperation: (event) => operations.push(event),
      xhrFactory: () => {
        if (phase === 'factory') throw new Error('factory failed');
        return xhr;
      },
    });

    await assert.rejects(client.upload({
      path: '/api/files',
      formData: new FormData(),
      signal,
    }), new RegExp(`${phase} failed`));

    assert.deepEqual(inflight, [1, 0]);
    assert.deepEqual(operations.map(({ status }) => status), ['started', 'failed']);
    assert.equal(removed, added);
    if (phase !== 'factory') {
      assert.equal(xhr.onload, null);
      assert.equal(xhr.onerror, null);
      assert.equal(xhr.onabort, null);
      assert.equal(xhr.upload.onprogress, null);
      assert.equal(xhr.upload.onload, null);
    }
  });
}

test('XHR upload ignores a synchronous send throw after an already terminal callback', async () => {
  const operations = [];
  const inflight = [];
  const xhr = {
    upload: {},
    open() {},
    setRequestHeader() {},
    send() {
      this.status = 200;
      this.responseText = '{"name":"only-once.txt"}';
      this.onload();
      throw new Error('late send throw');
    },
    abort() {},
  };
  const client = createApiClient({
    token: 'session-token',
    xhrFactory: () => xhr,
    onOperation: (event) => operations.push(event),
    onInflight: (count) => inflight.push(count),
  });

  assert.deepEqual(await client.upload({
    path: '/api/files',
    formData: new FormData(),
  }), { name: 'only-once.txt' });
  assert.deepEqual(operations.map(({ status }) => status), ['started', 'succeeded']);
  assert.deepEqual(inflight, [1, 0]);
});
