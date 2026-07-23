import test from 'node:test';
import assert from 'node:assert/strict';

import {
  canPurgeSecret,
  deleteConfirmationModel,
  deletionNoticeModel,
} from './secrets.js';

test('delete confirmation identifies backend, vault, five targets, overflow, and recovery', () => {
  const model = deleteConfirmationModel({
    backend: 'azure',
    vault: 'production',
    names: ['one', 'two', 'three', 'four', 'five', 'six', 'seven'],
    recoverable: true,
  });

  assert.match(model.message, /azure/);
  assert.match(model.message, /production/);
  assert.deepEqual(model.visibleNames, ['one', 'two', 'three', 'four', 'five']);
  assert.equal(model.overflow, 2);
  assert.match(model.recovery, /Trash/);
});

test('hard-delete confirmation and notice explicitly say recovery is unavailable', () => {
  const confirmation = deleteConfirmationModel({
    backend: 'legacy',
    vault: 'default',
    names: ['only'],
    recoverable: false,
  });
  const notice = deletionNoticeModel(['only'], false);

  assert.match(confirmation.recovery, /Recovery is unavailable/);
  assert.match(notice.message, /Recovery is unavailable/);
  assert.equal(notice.canUndo, false);
});

test('file-delete confirmation names context and targets and warns that files cannot be recovered', () => {
  const names = ['one.txt', 'two.txt', 'three.txt', 'four.txt', 'five.txt', 'six.txt', 'seven.txt'];
  const confirmation = deleteConfirmationModel({
    backend: 'local',
    vault: 'playwright',
    names,
    recoverable: false,
    kind: 'file',
  });

  assert.equal(confirmation.message, 'Delete 7 files from local vault playwright?');
  assert.deepEqual(confirmation.visibleNames, names.slice(0, 5));
  assert.equal(confirmation.overflow, 2);
  assert.equal(confirmation.recovery, 'Recovery is unavailable for files on local.');
});

test('recoverable deletion notice remains actionable with Undo', () => {
  const notice = deletionNoticeModel(['one', 'two'], true);
  assert.equal(notice.canUndo, true);
  assert.match(notice.message, /2 secrets moved to Trash/);
});

test('purge unlocks only for an exact secret-name match', () => {
  assert.equal(canPurgeSecret('prod/key', 'prod/key'), true);
  assert.equal(canPurgeSecret('prod/key', ' prod/key'), false);
  assert.equal(canPurgeSecret('prod/key', 'prod/KEY'), false);
  assert.equal(canPurgeSecret('prod/key', ''), false);
});
