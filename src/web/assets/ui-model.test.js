import test from 'node:test';
import assert from 'node:assert/strict';
import * as model from './ui-model.js';

test('dates are date-only and absent expiration is blank', () => {
  assert.equal(model.formatDate('2026-07-15T23:45:00Z'), '2026-07-15');
  assert.equal(model.formatDate('Unknown'), 'Unknown');
  assert.equal(model.expirationDate(null), '');
  assert.equal(model.expirationDate('2027-02-03T00:00:00Z'), '2027-02-03');
});

test('Azure timestamps stay date-only when the runtime cannot parse their suffix', () => {
  const NativeDate = globalThis.Date;
  globalThis.Date = class WebKitDate extends NativeDate {
    constructor(value) {
      super(value === '2023-05-13 13:03:15 UTC' ? Number.NaN : value);
    }
  };
  try {
    assert.equal(model.formatDate('2023-05-13 13:03:15 UTC'), '2023-05-13');
  } finally {
    globalThis.Date = NativeDate;
  }
});

test('all stored protected values use the same mask', () => {
  const short = model.createProtectedState('a', true);
  const long = model.createProtectedState('a much longer secret', true);
  assert.equal(model.protectedDisplay(short), '***************');
  assert.equal(model.protectedDisplay(long), '***************');
  model.revealProtected(short);
  assert.equal(model.protectedDisplay(short), 'a');
  model.editProtected(short, 'changed');
  model.hideProtected(short);
  assert.equal(model.protectedDisplay(short), '***************');
  assert.equal(short.value, 'changed');
  assert.equal(short.dirty, true);
});

test('overlapping protected loads cannot overwrite a newer edit and hide', async () => {
  const state = model.createProtectedState(null, true);
  let resolveLoad;
  let loadCount = 0;
  const storedValue = new Promise((resolve) => { resolveLoad = resolve; });
  const loader = () => { loadCount++; return storedValue; };

  const revealLoad = model.loadProtected(state, loader);
  const copyLoad = model.loadProtected(state, loader);
  assert.strictEqual(revealLoad, copyLoad);
  assert.equal(loadCount, 1);

  model.revealProtected(state, 'draft');
  model.editProtected(state, 'edited');
  model.hideProtected(state);
  resolveLoad('stored value');

  assert.equal(await revealLoad, 'edited');
  assert.equal(await copyLoad, 'edited');
  assert.equal(state.value, 'edited');
  assert.equal(state.masked, true);
  assert.equal(loadCount, 1);
});

test('numeric and date sorts use name tie breaking and empty-last order', () => {
  const items = [
    { name: 'beta', size: 5, updated: '2025-01-02T00:00:00Z' },
    { name: 'Alpha', size: 10, updated: '' },
    { name: 'charlie', size: 5, updated: '2025-01-01T00:00:00Z' },
  ];
  assert.deepEqual(model.sortedCopy(items, x => x.size, x => x.name, 'number', 'asc').map(x => x.name), ['beta', 'charlie', 'Alpha']);
  assert.deepEqual(model.sortedCopy(items, x => x.updated, x => x.name, 'date', 'asc').map(x => x.name), ['charlie', 'beta', 'Alpha']);
});

test('descending numeric sorts keep empty values last', () => {
  const items = [
    { name: 'empty', size: null },
    { name: 'small', size: 5 },
    { name: 'large', size: 10 },
  ];
  assert.deepEqual(model.sortedCopy(items, x => x.size, x => x.name, 'number', 'desc').map(x => x.name), ['large', 'small', 'empty']);
});

test('descending date sorts keep empty values last', () => {
  const items = [
    { name: 'empty', updated: '' },
    { name: 'older', updated: '2025-01-01T00:00:00Z' },
    { name: 'newer', updated: '2025-01-02T00:00:00Z' },
  ];
  assert.deepEqual(model.sortedCopy(items, x => x.updated, x => x.name, 'date', 'desc').map(x => x.name), ['newer', 'older', 'empty']);
});

test('saved widths must match shape, total, and minimums', () => {
  const defaults = [28, 15, 14, 25, 18];
  const minimums = [14, 10, 10, 14, 12];
  assert.deepEqual(model.normalizeWidths('[30,15,15,22,18]', defaults, minimums), [30, 15, 15, 22, 18]);
  assert.deepEqual(model.normalizeWidths('bad', defaults, minimums), defaults);
  assert.deepEqual(model.normalizeWidths('[5,20,20,35,20]', defaults, minimums), defaults);
  assert.deepEqual(model.normalizeWidths('[28,15,14,25]', defaults, minimums), defaults);
});

test('adjacent width growth clamps exactly at the right minimum and preserves total', () => {
  assert.equal(typeof model.resizeAdjacentWidths, 'function');
  const widths = model.resizeAdjacentWidths([32, 11, 57], [14, 10, 12], 0, 2);
  assert.deepEqual(widths, [33, 10, 57]);
  assert.equal(widths.reduce((sum, width) => sum + width, 0), 100);

  const extreme = model.resizeAdjacentWidths([28, 15, 57], [14, 10, 12], 0, 100);
  assert.deepEqual(extreme, [33, 10, 57]);
});

test('adjacent width shrink clamps exactly at the left minimum and preserves total', () => {
  assert.equal(typeof model.resizeAdjacentWidths, 'function');
  const widths = model.resizeAdjacentWidths([28, 15, 57], [14, 10, 12], 0, -100);
  assert.deepEqual(widths, [14, 29, 57]);
  assert.equal(widths.reduce((sum, width) => sum + width, 0), 100);
});
