import test from 'node:test';
import assert from 'node:assert/strict';

import {
  canPurgeSecret,
  clearClipboardIfUnchanged,
  createExposureTimer,
  deleteConfirmationModel,
  deletionNoticeModel,
} from './secrets.js';

function fakeClock() {
  let nextId = 0;
  const scheduled = new Map();
  return {
    setTimeout(callback, delay) {
      const id = ++nextId;
      scheduled.set(id, { callback, delay });
      return id;
    },
    clearTimeout(id) { scheduled.delete(id); },
    advanceOneSecond() {
      const ready = [...scheduled.entries()].filter(([, task]) => task.delay === 1000);
      for (const [id, task] of ready) {
        scheduled.delete(id);
        task.callback();
      }
    },
    get size() { return scheduled.size; },
  };
}

test('exposure timer ticks deterministically, resets, and expires once', () => {
  const clock = fakeClock();
  const ticks = [];
  let expirations = 0;
  const timer = createExposureTimer({
    seconds: 2,
    onTick: (remaining) => ticks.push(remaining),
    onExpire: () => { expirations++; },
    clock,
  });

  assert.deepEqual(ticks, [2]);
  clock.advanceOneSecond();
  assert.deepEqual(ticks, [2, 1]);
  timer.reset();
  assert.deepEqual(ticks, [2, 1, 2]);
  clock.advanceOneSecond();
  clock.advanceOneSecond();
  assert.deepEqual(ticks, [2, 1, 2, 1]);
  assert.equal(expirations, 1);
  assert.equal(clock.size, 0);
});

test('clipboard clearing never overwrites a newer value', async () => {
  let value = 'newer';
  const clipboard = {
    readText: async () => value,
    writeText: async (next) => { value = next; },
  };

  assert.equal(await clearClipboardIfUnchanged({ clipboard, expected: 'copied' }), false);
  assert.equal(value, 'newer');
  value = 'copied';
  assert.equal(await clearClipboardIfUnchanged({ clipboard, expected: 'copied' }), true);
  assert.equal(value, '');
});

test('clipboard clearing is unconfirmed when read or clear access fails', async () => {
  assert.equal(await clearClipboardIfUnchanged({
    clipboard: { readText: async () => { throw new Error('denied'); } },
    expected: 'copied',
  }), false);
  assert.equal(await clearClipboardIfUnchanged({
    clipboard: { readText: async () => 'copied', writeText: async () => { throw new Error('denied'); } },
    expected: 'copied',
  }), false);
});

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
