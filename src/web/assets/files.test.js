import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createUploadQueue,
  nextUploadState,
  uploadConflictDecision,
  uploadEvidenceState,
} from './files.js';

test('retry selects only failed cancelled and ambiguous entries', () => {
  const queue = createUploadQueue([{ id: 'a' }, { id: 'b' }, { id: 'c' }]);
  queue.transition('a', 'completed');
  queue.transition('b', 'failed', { error: 'network' });
  queue.transition('c', 'cancelled');
  assert.deepEqual(queue.retryable().map((item) => item.id), ['b', 'c']);
});

test('client bytes complete enters finishing before completed', () => {
  assert.equal(nextUploadState('uploading', { type: 'bytes-sent' }), 'finishing');
  assert.equal(nextUploadState('finishing', { type: 'server-confirmed' }), 'completed');
});

test('the exact queue state machine rejects impossible transitions', () => {
  assert.equal(nextUploadState('queued', { type: 'preflight-started' }), 'preflighting');
  assert.equal(nextUploadState('preflighting', { type: 'conflict' }), 'awaiting-conflict');
  assert.equal(nextUploadState('preflighting', { type: 'ready' }), 'uploading');
  assert.equal(nextUploadState('awaiting-conflict', { type: 'resolved' }), 'uploading');
  assert.equal(nextUploadState('uploading', { type: 'cancelled' }), 'cancelled');
  assert.equal(nextUploadState('uploading', { type: 'uncertain' }), 'ambiguous');
  assert.throws(() => nextUploadState('completed', { type: 'ready' }), /Invalid upload transition/);
});

test('scheduler claims no more than configured concurrency', () => {
  const queue = createUploadQueue(
    [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }],
    { maxConcurrent: 2 },
  );
  assert.deepEqual(queue.claimReady().map((item) => item.id), ['a', 'b']);
  assert.deepEqual(queue.claimReady().map((item) => item.id), []);
  queue.transition('a', 'completed');
  assert.deepEqual(queue.claimReady().map((item) => item.id), ['c']);
});

test('apply-to-all conflict policy never makes replace implicit', () => {
  assert.deepEqual(
    uploadConflictDecision({ policy: 'rename', suggestedName: 'report (2).pdf' }),
    { policy: 'rename', target: 'report (2).pdf' },
  );
  assert.deepEqual(uploadConflictDecision({ policy: 'skip' }), { policy: 'skip', target: null });
  assert.equal(uploadConflictDecision({ policy: null }), null);
  assert.throws(() => uploadConflictDecision({ policy: 'replace', allowReplace: false }), /unsupported/i);
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
