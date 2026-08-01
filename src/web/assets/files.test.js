import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createUploadQueue,
  nextUploadState,
  uploadConflictDecision,
  uploadEvidenceState,
  validateUploadConfirmation,
  validatePreflightResults,
} from './files.js';

test('retry selects only failed cancelled and ambiguous entries', () => {
  const queue = createUploadQueue([{ id: 'a' }, { id: 'b' }, { id: 'c' }]);
  queue.event('a', { type: 'cancel' });
  queue.event('a', { type: 'retry' });
  queue.event('a', { type: 'preflight-started' });
  queue.event('a', { type: 'preflight-ready' });
  queue.event('a', { type: 'transfer-started' });
  queue.event('a', { type: 'server-confirmed' });
  queue.event('b', { type: 'preflight-started' });
  queue.event('b', { type: 'failed' }, { error: 'network' });
  queue.event('c', { type: 'cancel' });
  assert.deepEqual(queue.retryable().map((item) => item.id), ['b', 'c']);
});

test('client bytes complete enters finishing before completed', () => {
  assert.equal(nextUploadState('uploading', { type: 'bytes-sent' }), 'finishing');
  assert.equal(nextUploadState('finishing', { type: 'server-confirmed' }), 'completed');
});

test('the exact queue state machine rejects impossible transitions', () => {
  assert.equal(nextUploadState('queued', { type: 'preflight-started' }), 'preflighting');
  assert.equal(nextUploadState('preflighting', { type: 'conflict' }), 'awaiting-conflict');
  assert.equal(nextUploadState('preflighting', { type: 'preflight-ready' }), 'queued');
  assert.equal(nextUploadState('queued', { type: 'transfer-started' }), 'uploading');
  assert.equal(nextUploadState('awaiting-conflict', { type: 'decision-upload' }), 'queued');
  assert.equal(nextUploadState('uploading', { type: 'cancelled' }), 'cancelled');
  assert.equal(nextUploadState('uploading', { type: 'uncertain' }), 'reconciling');
  assert.equal(nextUploadState('reconciling', { type: 'evidence-missing' }), 'cancelled');
  assert.equal(nextUploadState('reconciling', { type: 'evidence-present' }), 'ambiguous');
  assert.throws(() => nextUploadState('reconciling', { type: 'retry' }), /Invalid upload transition/);
  assert.throws(() => nextUploadState('completed', { type: 'preflight-ready' }), /Invalid upload transition/);
});

test('scheduler claims no more than configured concurrency', () => {
  const queue = createUploadQueue(
    [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }],
    { maxConcurrent: 2 },
  );
  for (const id of ['a', 'b', 'c', 'd']) {
    queue.event(id, { type: 'preflight-started' });
    queue.event(id, { type: 'preflight-ready' });
  }
  assert.deepEqual(queue.claimReady().map((item) => item.id), ['a', 'b']);
  assert.deepEqual(queue.claimReady().map((item) => item.id), []);
  queue.event('a', { type: 'server-confirmed' });
  assert.deepEqual(queue.claimReady().map((item) => item.id), ['c']);
});

test('per-item cancel and retry never alter a sibling', () => {
  const queue = createUploadQueue([{ id: 'a' }, { id: 'b' }]);
  queue.event('a', { type: 'preflight-started' });
  queue.event('b', { type: 'preflight-started' });
  queue.event('a', { type: 'cancel' });
  assert.equal(queue.get('a').state, 'cancelled');
  assert.equal(queue.get('b').state, 'preflighting');
  queue.event('a', { type: 'retry' });
  assert.equal(queue.get('a').state, 'queued');
  assert.equal(queue.get('b').state, 'preflighting');
  assert.equal('transition' in queue, false);
});

test('conflict upload decision clears stale terminal and evidence copy', () => {
  for (const [event, state, result] of [
    [{ type: 'decision-upload' }, 'queued', null],
    [{ type: 'skipped' }, 'completed', 'Skipped safely.'],
  ]) {
    const queue = createUploadQueue([{ id: 'a' }]);
    queue.event('a', { type: 'preflight-started' });
    queue.event('a', { type: 'conflict' }, {
      error: 'stale conflict',
      evidence: 'stale evidence',
      result: 'stale result',
    });
    queue.event('a', event, { policy: 'skip', result });
    assert.deepEqual(
      {
        state: queue.get('a').state,
        error: queue.get('a').error,
        evidence: queue.get('a').evidence,
        result: queue.get('a').result,
      },
      { state, error: null, evidence: '', result },
    );
  }
});

test('preflight requires exactly one recognized result per candidate', () => {
  const candidates = [{ id: 'a' }, { id: 'b' }];
  assert.deepEqual(validatePreflightResults(candidates, [
    { client_id: 'a', status: 'ready', max_bytes: 10 },
    { client_id: 'b', status: 'conflict', suggested_name: 'b (2)' },
  ]).map(({ client_id }) => client_id), ['a', 'b']);
  assert.throws(() => validatePreflightResults(candidates, [
    { client_id: 'a', status: 'ready' },
  ]), /exactly one/i);
  assert.throws(() => validatePreflightResults(candidates, [
    { client_id: 'a', status: 'ready' },
    { client_id: 'a', status: 'ready' },
    { client_id: 'b', status: 'ready' },
  ]), /exactly one/i);
  assert.throws(() => validatePreflightResults(candidates, [
    { client_id: 'a', status: 'ready' },
    { client_id: 'unknown', status: 'ready' },
  ]), /unknown/i);
  assert.throws(() => validatePreflightResults(candidates, [
    { client_id: 'a', status: 'mystery' },
    { client_id: 'b', status: 'ready' },
  ]), /status/i);
  for (const suggestedName of ['', '../outside.txt', '/absolute.txt', 'bad\\name.txt']) {
    assert.throws(() => validatePreflightResults(candidates, [
      { client_id: 'a', status: 'conflict', suggested_name: suggestedName },
      { client_id: 'b', status: 'ready' },
    ]), /suggestion/i);
  }
});

test('apply-to-all conflict policy never makes replace implicit', () => {
  assert.deepEqual(
    uploadConflictDecision({ policy: 'rename', suggestedName: 'report (2).pdf' }),
    { policy: 'rename', target: 'report (2).pdf' },
  );
  assert.deepEqual(uploadConflictDecision({ policy: 'skip' }), { policy: 'skip', target: null });
  assert.equal(uploadConflictDecision({ policy: null }), null);
  assert.throws(() => uploadConflictDecision({ policy: 'replace', allowReplace: false }), /unsupported/i);
  assert.throws(
    () => uploadConflictDecision({ policy: 'rename', suggestedName: '../outside.txt' }),
    /rename/i,
  );
});

test('metadata evidence labels uncertain completion without guessing', () => {
  assert.deepEqual(
    uploadEvidenceState({ before: null, after: { name: 'a.txt', size: 7 }, expectedSize: 7 }),
    { state: 'ambiguous', evidence: 'The destination now exists, but this upload could not be confirmed.' },
  );
  assert.deepEqual(
    uploadEvidenceState({ before: { name: 'a.txt', size: 7 }, after: { name: 'a.txt', size: 7 }, expectedSize: 7 }),
    { state: 'ambiguous', evidence: 'The file exists, but this upload could not be confirmed.' },
  );
  assert.deepEqual(
    uploadEvidenceState({ before: null, after: null, expectedSize: 7 }),
    { state: 'cancelled', evidence: 'Server metadata confirms no destination file.' },
  );
});

test('upload confirmation must match the immutable logical target and policy', () => {
  assert.deepEqual(
    validateUploadConfirmation(
      { name: 'docs/report.txt' },
      { expectedName: 'docs/report.txt', policy: null },
    ),
    { name: 'docs/report.txt' },
  );
  assert.deepEqual(
    validateUploadConfirmation(
      { name: 'docs/report.txt', status: 'skipped' },
      { expectedName: 'docs/report.txt', policy: 'skip' },
    ),
    { name: 'docs/report.txt', status: 'skipped' },
  );
  assert.deepEqual(
    validateUploadConfirmation(
      { name: 'docs/report.txt', size: 10 },
      { expectedName: 'docs/report.txt', policy: 'skip' },
    ),
    { name: 'docs/report.txt', size: 10 },
  );
  for (const [response, expected] of [
    [{ name: 'elsewhere/report.txt' }, { expectedName: 'docs/report.txt', policy: null }],
    [{ name: 'docs/report.txt', status: 'skipped' }, { expectedName: 'docs/report.txt', policy: 'replace' }],
    [{ name: 'elsewhere/report.txt', status: 'skipped' }, { expectedName: 'docs/report.txt', policy: 'skip' }],
  ]) {
    assert.throws(
      () => validateUploadConfirmation(response, expected),
      (error) => error?.name === 'AmbiguousUploadError' && error?.ambiguous === true,
    );
  }
});
