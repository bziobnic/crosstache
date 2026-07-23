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

test('slash paths become nested folder nodes with stable unfiled node', () => {
  const tree = model.buildFolderTree([
    { name: 'a', folder: 'apps/prod' },
    { name: 'b', folder: null },
  ]);

  assert.deepEqual(tree.map((node) => node.id), ['__unfiled__', 'apps']);
  assert.equal(tree[0].label, 'Unfiled');
  assert.equal(tree[1].children[0].id, 'apps/prod');
});

test('folder paths normalize slashes and empty segments without duplicating parents', () => {
  const tree = model.buildFolderTree([
    { name: 'one', folder: '/ apps // prod /' },
    { name: 'two', folder: 'apps/prod' },
    { name: 'three', folder: 'apps///stage/' },
    { name: 'four', folder: '///' },
  ]);

  assert.deepEqual(tree.map((node) => node.id), ['__unfiled__', 'apps']);
  assert.deepEqual(tree[1].children.map((node) => node.id), ['apps/prod', 'apps/stage']);
  assert.equal(tree[1].directCount, 0);
  assert.equal(tree[1].totalCount, 3);
  assert.equal(tree[1].children[0].directCount, 2);
  assert.equal(tree[0].totalCount, 1);
});

test('folder nodes use the existing numeric case-insensitive collation', () => {
  const tree = model.buildFolderTree([
    { name: 'a', folder: 'Folder 10' },
    { name: 'b', folder: 'folder 2' },
    { name: 'c', folder: 'Alpha' },
  ]);

  assert.deepEqual(tree.map((node) => node.id), ['Alpha', 'folder 2', 'Folder 10']);
});

test('small vaults expand on first visit and saved expansion always wins', () => {
  assert.equal(model.initialExpansion({ total: 50, saved: null }), 'all');
  assert.equal(model.initialExpansion({ total: 51, saved: null }), 'collapsed');
  assert.deepEqual(model.initialExpansion({ total: 51, saved: ['apps'] }), ['apps']);
  assert.deepEqual(model.initialExpansion({ total: 10, saved: [] }), []);
});

test('folder preference keys isolate backend registry name, vault, and surface', () => {
  const secrets = model.folderPreferenceKey({
    backend: 'azure/prod',
    vault: 'payments east',
    surface: 'secrets',
  });
  const files = model.folderPreferenceKey({
    backend: 'azure/prod',
    vault: 'payments east',
    surface: 'files',
  });
  const otherVault = model.folderPreferenceKey({
    backend: 'azure/prod',
    vault: 'payments west',
    surface: 'secrets',
  });

  assert.match(secrets, /^xv\.ui\.folder-expansion\.v2:/);
  assert.notEqual(secrets, files);
  assert.notEqual(secrets, otherVault);
  assert.ok(secrets.includes(encodeURIComponent('azure/prod')));
  assert.ok(secrets.includes(encodeURIComponent('payments east')));
});

test('folder membership includes descendants but keeps unfiled stable', () => {
  assert.equal(model.itemMatchesFolder({ folder: 'apps/prod' }, 'apps'), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps/prod' }, 'apps/prod'), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps/production' }, 'apps/prod'), false);
  assert.equal(model.itemMatchesFolder({ folder: '' }, '__unfiled__'), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps' }, '__unfiled__'), false);
  assert.equal(model.itemMatchesFolder({ folder: 'apps' }, null), true);
});

test('folder expansion persistence is explicit and isolated by context and surface', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const scope = { backend: 'local-a', vault: 'one', surface: 'secrets' };

  assert.equal(model.loadFolderExpansion(storage, scope), null);
  assert.equal(model.saveFolderExpansion(storage, scope, new Set(['apps', 'apps/prod'])), true);
  assert.deepEqual(model.loadFolderExpansion(storage, scope), ['apps', 'apps/prod']);
  assert.equal(model.loadFolderExpansion(storage, { ...scope, vault: 'two' }), null);
  assert.equal(model.loadFolderExpansion(storage, { ...scope, surface: 'files' }), null);
  assert.equal(model.loadFolderExpansion(storage, { ...scope, backend: 'local-b' }), null);
});

test('legacy global folder expansion booleans never become per-context authority', () => {
  const storage = {
    getItem(key) {
      if (key === 'folder_expansion' || key === 'xv.ui.folder-expansion.v1') return 'false';
      return null;
    },
    setItem() {
      throw new Error('an absent scoped value must not be migrated from a global boolean');
    },
  };

  const saved = model.loadFolderExpansion(storage, {
    backend: 'local',
    vault: 'payments',
    surface: 'secrets',
  });
  assert.equal(saved, null);
  assert.equal(model.initialExpansion({ total: 50, saved }), 'all');
});

test('invalid or unavailable folder expansion storage safely uses first-visit defaults', () => {
  const invalid = { getItem: () => '{"expanded":"all"}' };
  const unavailable = { getItem: () => { throw new Error('storage denied'); } };
  const scope = { backend: 'local', vault: 'payments', surface: 'secrets' };

  assert.equal(model.loadFolderExpansion(invalid, scope), null);
  assert.equal(model.loadFolderExpansion(unavailable, scope), null);
  assert.equal(model.saveFolderExpansion(null, scope, new Set(['apps'])), false);
});

test('folder navigation resets selection and restores expansion when workspace scope changes', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const navigation = model.createFolderNavigationState(storage);
  const one = { backend: 'local', vault: 'one', surface: 'secrets' };
  const two = { backend: 'local', vault: 'two', surface: 'secrets' };

  navigation.sync(one, { total: 51, expandableIds: ['apps'] });
  assert.deepEqual(navigation.snapshot(), { selected: null, expanded: [] });
  navigation.select('apps');
  navigation.toggle('apps', true);
  assert.deepEqual(navigation.snapshot(), { selected: 'apps', expanded: ['apps'] });

  navigation.sync(two, { total: 2, expandableIds: ['other'] });
  assert.deepEqual(navigation.snapshot(), { selected: null, expanded: ['other'] });
  navigation.select('other');

  navigation.sync(one, { total: 51, expandableIds: ['apps'] });
  assert.deepEqual(navigation.snapshot(), { selected: null, expanded: ['apps'] });
});

test('folder navigation keeps files expansion independent from secrets in one vault', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const navigation = model.createFolderNavigationState(storage);
  const secrets = { backend: 'azure', vault: 'payments', surface: 'secrets' };
  const files = { backend: 'azure', vault: 'payments', surface: 'files' };

  navigation.sync(secrets, { total: 60, expandableIds: ['apps', 'apps/prod'] });
  navigation.expandAll();
  navigation.sync(files, { total: 60, expandableIds: ['documents'] });
  assert.deepEqual(navigation.snapshot(), { selected: null, expanded: [] });
  navigation.toggle('documents', true);
  navigation.sync(secrets, { total: 60, expandableIds: ['apps', 'apps/prod'] });
  assert.deepEqual(navigation.snapshot(), {
    selected: null,
    expanded: ['apps', 'apps/prod'],
  });
});
