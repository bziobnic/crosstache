import test from 'node:test';
import assert from 'node:assert/strict';
import * as model from './ui-model.js';

function folderTokenIndex(scopeCharacter, paths) {
  return model.createFolderTokenIndex({
    version: 1,
    scope_token: scopeCharacter.repeat(43),
    folders: paths.map((path, index) => ({
      path,
      token: String.fromCharCode(65 + index).repeat(43),
    })),
  });
}

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

test('content mode changes at the approved breakpoint', () => {
  assert.equal(model.contentMode(769), 'table');
  assert.equal(model.contentMode(768), 'stacked');
  assert.equal(model.contentMode(390), 'stacked');
});

test('responsive content rows preserve complete identifiers and priority metadata', () => {
  const longName = `${'credential-'.repeat(10)}tail`;
  const [secret] = model.contentRows('secrets', [{
    name: longName,
    folder: 'teams/platform/production',
    groups: 'operators',
    note: 'rotation owner',
    updated_on: '2026-07-24T10:00:00Z',
  }]);
  assert.equal(secret.identifier, longName);
  assert.deepEqual(secret.metadata, [
    { label: 'Folder', value: 'teams/platform/production' },
    { label: 'Groups', value: 'operators' },
    { label: 'Note', value: 'rotation owner' },
    { label: 'Updated', value: '2026-07-24' },
  ]);

  const [file] = model.contentRows('files', [{
    name: 'nested/path/archive.tar',
    size: 2048,
    content_type: 'application/x-tar',
    last_modified: '2026-07-23T10:00:00Z',
  }], { formatSize: (size) => `${size} bytes` });
  assert.equal(file.identifier, 'nested/path/archive.tar');
  assert.deepEqual(file.metadata, [
    { label: 'Size', value: '2048 bytes' },
    { label: 'Type', value: 'application/x-tar' },
    { label: 'Modified', value: '2026-07-23' },
  ]);
});

test('stacked groups use one deterministic heading per folder and retain current row order', () => {
  const rows = model.contentRows('secrets', [
    { name: 'zeta', folder: 'teams/beta' },
    { name: 'second', folder: 'teams/alpha' },
    { name: 'first', folder: 'teams/alpha' },
    { name: 'root' },
    { name: 'again', folder: 'teams/beta' },
  ]);

  assert.deepEqual(model.groupContentRows(rows).map((group) => ({
    folder: group.folder,
    label: group.label,
    names: group.rows.map((row) => row.identifier),
  })), [
    { folder: '', label: 'Unfiled', names: ['root'] },
    { folder: 'teams/alpha', label: 'teams/alpha', names: ['second', 'first'] },
    { folder: 'teams/beta', label: 'teams/beta', names: ['zeta', 'again'] },
  ]);
});

function secretRows(items) {
  return model.contentRows('secrets', items);
}

function labelsOf(rows) {
  return rows.map((row) => (row.type === 'folder' ? `${row.label}/` : row.identifier));
}

test('slash paths become nested folder branches with loose items at the root', () => {
  const tree = model.buildContentTree(secretRows([
    { name: 'a', folder: 'apps/prod' },
    { name: 'b', folder: null },
  ]));

  assert.deepEqual(tree.children.map((node) => node.path), ['apps']);
  assert.deepEqual(tree.children[0].children.map((node) => node.path), ['apps/prod']);
  assert.deepEqual(tree.items.map((row) => row.identifier), ['b']);
  assert.deepEqual(tree.children[0].itemIds, ['a']);
});

test('folder paths normalize slashes and empty segments without duplicating parents', () => {
  const tree = model.buildContentTree(secretRows([
    { name: 'one', folder: '/apps//prod/' },
    { name: 'two', folder: 'apps/prod' },
    { name: 'three', folder: 'apps///stage/' },
    { name: 'four', folder: '///' },
  ]));

  assert.deepEqual(tree.children.map((node) => node.path), ['apps']);
  assert.deepEqual(tree.children[0].children.map((node) => node.path), ['apps/prod', 'apps/stage']);
  assert.equal(tree.children[0].items.length, 0);
  assert.equal(tree.children[0].totalCount, 3);
  assert.deepEqual(tree.children[0].children[0].itemIds, ['one', 'two']);
  assert.deepEqual(tree.items.map((row) => row.identifier), ['four']);
});

test('folder identities preserve valid whitespace and cannot collide with reserved labels', () => {
  const tree = model.buildContentTree(secretRows([
    { name: 'spaced', folder: ' apps / prod ' },
    { name: 'plain', folder: 'apps/prod' },
    { name: 'reserved-all', folder: '__all__' },
    { name: 'reserved-unfiled', folder: '__unfiled__' },
    { name: 'unfiled', folder: null },
  ]));
  const expanded = new Map(
    model.treeFolderIdentities(tree).map((id) => [model.folderIdentityKey(id), id]),
  );
  const labels = model.flattenContentTree(tree, expanded)
    .filter((row) => row.type === 'folder')
    .map((row) => row.label);

  assert.ok(labels.includes(' apps '));
  assert.ok(labels.includes('apps'));
  assert.ok(labels.includes('__all__'));
  assert.ok(labels.includes('__unfiled__'));
  assert.notEqual(
    model.folderIdentityKey(model.folderIdentity('__unfiled__')),
    model.folderIdentityKey(model.FOLDER_UNFILED),
  );
  assert.equal(model.normalizeFolderPath('/ apps // prod /'), ' apps / prod ');
});

test('folder branches use the existing numeric case-insensitive collation', () => {
  const tree = model.buildContentTree(secretRows([
    { name: 'a', folder: 'Folder 10' },
    { name: 'b', folder: 'folder 2' },
    { name: 'c', folder: 'Alpha' },
  ]));

  assert.deepEqual(tree.children.map((node) => node.path), ['Alpha', 'folder 2', 'Folder 10']);
});

test('every folder is expandable so expand and collapse always have something to act on', () => {
  const flat = model.buildContentTree(secretRows([
    { name: 'a', folder: 'prod' },
    { name: 'b', folder: 'prod' },
    { name: 'c', folder: 'dev' },
    { name: 'd', folder: null },
  ]));

  // The pre-tree-grid model only treated a folder as expandable when it held
  // sub-folders, so a vault of flat folders had zero expandable nodes and the
  // expand/collapse controls were inert.
  assert.deepEqual(
    model.treeFolderIdentities(flat).map((id) => id.path),
    ['dev', 'prod'],
  );
  const collapsed = model.flattenContentTree(flat, new Map());
  assert.deepEqual(labelsOf(collapsed), ['dev/', 'prod/', 'd']);
  assert.deepEqual(collapsed.filter((row) => row.type === 'folder').map((row) => row.hasChildren), [true, true]);

  const expanded = new Map(
    model.treeFolderIdentities(flat).map((id) => [model.folderIdentityKey(id), id]),
  );
  assert.deepEqual(labelsOf(model.flattenContentTree(flat, expanded)), ['dev/', 'c', 'prod/', 'a', 'b', 'd']);
});

test('flattened rows carry depth, parent links, and stable keys for both surfaces', () => {
  const fileTree = model.buildContentTree(model.contentRows('files', [
    { name: 'docs/prod/report.txt', size: 10, content_type: 'text/plain' },
    { name: 'loose.txt', size: 4, content_type: 'text/plain' },
  ], { formatSize: (value) => `${value} B` }));
  const expanded = new Map(
    model.treeFolderIdentities(fileTree).map((id) => [model.folderIdentityKey(id), id]),
  );
  const rows = model.flattenContentTree(fileTree, expanded);

  assert.deepEqual(rows.map((row) => row.level), [1, 2, 3, 1]);
  assert.deepEqual(labelsOf(rows), ['docs/', 'prod/', 'docs/prod/report.txt', 'loose.txt']);
  assert.equal(rows[2].parentKey, model.folderIdentityKey(model.folderIdentity('docs/prod')));
  assert.equal(rows[2].key, model.treeItemKey('docs/prod/report.txt'));
  assert.equal(rows[3].parentKey, null);
});

test('a filtered render force-expands surviving branches without persisting them', () => {
  const tree = model.buildContentTree(secretRows([{ name: 'match', folder: 'apps/prod' }]));
  const expanded = new Map();

  assert.deepEqual(labelsOf(model.flattenContentTree(tree, expanded)), ['apps/']);
  assert.deepEqual(
    labelsOf(model.flattenContentTree(tree, expanded, { forcedKeys: model.treeForcedKeys(tree) })),
    ['apps/', 'prod/', 'match'],
  );
  assert.equal(expanded.size, 0, 'forced expansion never mutates the persisted set');
});

test('branch selection is tri-state and propagates down and rolls up', () => {
  const tree = model.buildContentTree(secretRows([
    { name: 'a', folder: 'apps/prod' },
    { name: 'b', folder: 'apps/prod' },
    { name: 'c', folder: 'apps/stage' },
  ]));
  const apps = tree.children[0];
  const prod = apps.children[0];
  const selected = new Set();

  assert.equal(model.branchSelection(apps.itemIds, selected), 'unchecked');

  model.applyBranchSelection(selected, prod.itemIds, true);
  assert.deepEqual([...selected].sort(), ['a', 'b']);
  assert.equal(model.branchSelection(prod.itemIds, selected), 'checked');
  assert.equal(model.branchSelection(apps.itemIds, selected), 'mixed');

  selected.add('c');
  assert.equal(model.branchSelection(apps.itemIds, selected), 'checked');

  model.applyBranchSelection(selected, apps.itemIds, false);
  assert.equal(selected.size, 0);
  assert.equal(model.branchSelection(apps.itemIds, selected), 'unchecked');
  assert.equal(model.branchSelection([], selected), 'unchecked');
});

test('small vaults expand on first visit and saved expansion always wins', () => {
  assert.equal(model.initialExpansion({ total: 50, saved: null }), 'all');
  assert.equal(model.initialExpansion({ total: 51, saved: null }), 'collapsed');
  assert.deepEqual(model.initialExpansion({ total: 51, saved: ['apps'] }), ['apps']);
  assert.deepEqual(model.initialExpansion({ total: 10, saved: [] }), []);
});

test('folder preference keys use only server-issued opaque scope tokens', () => {
  const secrets = model.folderPreferenceKey(folderTokenIndex('S', []));
  const files = model.folderPreferenceKey(folderTokenIndex('F', []));
  const otherVault = model.folderPreferenceKey(folderTokenIndex('V', []));

  assert.match(secrets, /^xv\.ui\.folder-expansion\.v5:[A-Za-z0-9_-]{43}$/);
  assert.notEqual(secrets, files);
  assert.notEqual(secrets, otherVault);
  assert.equal(secrets.includes('azure'), false);
  assert.equal(secrets.includes('payments'), false);
});

test('folder persistence stores only versioned opaque scope and folder identifiers', () => {
  const values = new Map();
  values.set(
    'xv.ui.folder-expansion.v2:unrelated-backend:unrelated-vault:secrets',
    JSON.stringify(['legacy/raw/folder']),
  );
  const removed = [];
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => {
      removed.push(key);
      values.delete(key);
    },
    get length() { return values.size; },
    key: (index) => [...values.keys()][index] ?? null,
  };
  const scope = {
    backend: 'private-backend-name',
    vault: 'private-vault-name',
    surface: 'secrets',
  };
  const folder = model.folderIdentity(' private folder /prod');
  const tokenIndex = folderTokenIndex('S', [folder.path]);

  assert.equal(model.saveFolderExpansion(storage, tokenIndex, new Map([
    [model.folderIdentityKey(folder), folder],
  ])), true);
  const serialized = JSON.stringify([...values.entries()]);
  for (const source of [
    scope.backend,
    scope.vault,
    ' private folder ',
    'prod',
    encodeURIComponent(scope.backend),
    encodeURIComponent(scope.vault),
  ]) {
    assert.equal(serialized.includes(source), false, `storage leaked ${source}`);
  }
  assert.match([...values.keys()][0], /^xv\.ui\.folder-expansion\.v5:[A-Za-z0-9_-]{43}$/);
  assert.deepEqual(model.loadFolderExpansion(storage, tokenIndex), ['A'.repeat(43)]);
  assert.ok(removed.some((key) => key.startsWith('xv.ui.folder-expansion.v2:')));
  assert.equal(
    [...values.keys()].some((key) => key.startsWith('xv.ui.folder-expansion.v2:')),
    false,
  );
});

test('a v4 payload is discarded because the tree grid changed what expansion means', () => {
  const tokenIndex = folderTokenIndex('S', ['apps']);
  const values = new Map([
    [`xv.ui.folder-expansion.v4:${'S'.repeat(43)}`, JSON.stringify({
      version: 4,
      expanded: ['A'.repeat(43)],
    })],
  ]);
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
    get length() { return values.size; },
    key: (index) => [...values.keys()][index] ?? null,
  };

  assert.equal(model.loadFolderExpansion(storage, tokenIndex), null);
  assert.equal(
    [...values.keys()].some((key) => key.startsWith('xv.ui.folder-expansion.v4:')),
    false,
    'the superseded payload is removed rather than left to rot',
  );
});

test('folder expansion persistence is explicit and isolated by context and surface', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const one = folderTokenIndex('S', [apps.path, prod.path]);
  const two = folderTokenIndex('V', [apps.path, prod.path]);
  const files = folderTokenIndex('F', [apps.path, prod.path]);
  const backend = folderTokenIndex('B', [apps.path, prod.path]);

  assert.equal(model.loadFolderExpansion(storage, one), null);
  assert.equal(model.saveFolderExpansion(storage, one, new Map([
    [model.folderIdentityKey(apps), apps],
    [model.folderIdentityKey(prod), prod],
  ])), true);
  assert.deepEqual(new Set(model.loadFolderExpansion(storage, one)), new Set([
    'A'.repeat(43),
    'B'.repeat(43),
  ]));
  assert.equal(model.loadFolderExpansion(storage, two), null);
  assert.equal(model.loadFolderExpansion(storage, files), null);
  assert.equal(model.loadFolderExpansion(storage, backend), null);
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
  assert.equal(model.saveFolderExpansion(null, scope, new Map()), false);
});

test('tree expansion restores per-scope state when the workspace changes', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const expansion = model.createTreeExpansionState(storage);
  const one = { backend: 'local', vault: 'one', surface: 'secrets' };
  const two = { backend: 'local', vault: 'two', surface: 'secrets' };
  const apps = model.folderIdentity('apps');
  const other = model.folderIdentity('other');
  const oneTokens = folderTokenIndex('S', [apps.path]);
  const twoTokens = folderTokenIndex('T', [other.path]);

  expansion.sync(one, { total: 51, expandableIds: [apps], tokenIndex: oneTokens });
  assert.deepEqual(expansion.snapshot(), { expanded: [] });
  expansion.toggle(apps, true);
  assert.deepEqual(expansion.snapshot(), { expanded: [apps] });

  expansion.sync(two, { total: 2, expandableIds: [other], tokenIndex: twoTokens });
  assert.deepEqual(expansion.snapshot(), { expanded: [other] });

  expansion.sync(one, { total: 51, expandableIds: [apps], tokenIndex: oneTokens });
  assert.deepEqual(expansion.snapshot(), { expanded: [apps] });
});

test('tree expansion keeps files independent from secrets in one vault', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const expansion = model.createTreeExpansionState(storage);
  const secrets = { backend: 'azure', vault: 'payments', surface: 'secrets' };
  const files = { backend: 'azure', vault: 'payments', surface: 'files' };
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const documents = model.folderIdentity('documents');
  const secretTokens = folderTokenIndex('S', [apps.path, prod.path]);
  const fileTokens = folderTokenIndex('F', [documents.path]);

  expansion.sync(secrets, {
    total: 60, expandableIds: [apps, prod], tokenIndex: secretTokens,
  });
  expansion.expandAll();
  expansion.sync(files, { total: 60, expandableIds: [documents], tokenIndex: fileTokens });
  assert.deepEqual(expansion.snapshot(), { expanded: [] });
  expansion.toggle(documents, true);
  expansion.sync(secrets, {
    total: 60, expandableIds: [apps, prod], tokenIndex: secretTokens,
  });
  assert.deepEqual(expansion.snapshot(), { expanded: [apps, prod] });
});

test('large vaults build one tree with every folder expandable', () => {
  const items = Array.from({ length: 10_000 }, (_, index) => ({
    name: `secret-${index}`,
    folder: `team-${index % 100}/service-${index % 500}/env-${index % 3}`,
  }));

  const tree = model.buildContentTree(secretRows(items));

  assert.equal(tree.itemIds.length, 10_000);
  assert.equal(model.treeFolderCount(tree), 2_100);
  assert.equal(model.treeFolderIdentities(tree).length, 2_100);
});

test('server token indexes reject duplicate tokens and preserve collision-free raw identities', () => {
  const tokenA = 'A'.repeat(43);
  const tokenB = 'B'.repeat(43);
  const scopeToken = 'S'.repeat(43);
  const valid = model.createFolderTokenIndex({
    version: 1,
    scope_token: scopeToken,
    folders: [
      { path: ' apps ', token: tokenA },
      { path: '__unfiled__', token: tokenB },
    ],
  });

  assert.ok(valid);
  assert.equal(
    valid.byIdentityKey.get(model.folderIdentityKey(model.folderIdentity(' apps '))),
    tokenA,
  );
  assert.notEqual(
    model.folderIdentityKey(model.folderIdentity('__unfiled__')),
    model.folderIdentityKey(model.FOLDER_UNFILED),
  );
  assert.equal(model.createFolderTokenIndex({
    version: 1,
    scope_token: scopeToken,
    folders: [
      { path: 'one', token: tokenA },
      { path: 'two', token: tokenA },
    ],
  }), null);
});

test('pruned expansion persists across fresh state and does not return when a folder is re-added', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
    get length() { return values.size; },
    key: (index) => [...values.keys()][index] ?? null,
  };
  const scope = { backend: 'private-backend', vault: 'private-vault', surface: 'secrets' };
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const tokenIndex = model.createFolderTokenIndex({
    version: 1,
    scope_token: 'S'.repeat(43),
    folders: [
      { path: apps.path, token: 'A'.repeat(43) },
      { path: prod.path, token: 'P'.repeat(43) },
    ],
  });
  const expansion = model.createTreeExpansionState(storage);

  expansion.sync(scope, { total: 51, expandableIds: [apps, prod], tokenIndex });
  expansion.toggle(apps, true);
  expansion.sync(scope, { total: 1, expandableIds: [], tokenIndex });
  assert.deepEqual(expansion.snapshot().expanded, []);
  assert.equal(
    [...values.values()].some((serialized) => serialized.includes('A'.repeat(43))),
    false,
  );

  const fresh = model.createTreeExpansionState(storage);
  fresh.sync(scope, { total: 51, expandableIds: [apps, prod], tokenIndex });
  assert.deepEqual(fresh.snapshot().expanded, []);
  assert.equal(
    JSON.stringify([...values.entries()]).includes('private-backend'),
    false,
  );
  assert.equal(
    JSON.stringify([...values.entries()]).includes('private-vault'),
    false,
  );
});

test('secret filters compose with AND semantics', () => {
  const fixtures = [
    {
      name: 'prod-login',
      folder: 'prod',
      groups: ['ops', 'dba'],
      tags: { 'xv-type': 'login' },
      enabled: true,
      expires_on: '2030-01-02T00:00:00Z',
    },
    {
      name: 'disabled-login',
      folder: 'prod',
      groups: 'ops',
      tags: { 'xv-type': 'login' },
      enabled: false,
      expires_on: '2030-01-02T00:00:00Z',
    },
    {
      name: 'prod-note',
      folder: 'prod',
      groups: 'ops',
      enabled: true,
      expires_on: '2030-01-02T00:00:00Z',
    },
  ];
  const result = model.filterSecrets(fixtures, {
    folder: 'prod',
    group: 'OPS',
    type: 'login',
    enabled: true,
    expiry: 'expiring',
  }, { now: new Date('2029-01-01T00:00:00Z') });
  assert.deepEqual(result.map((item) => item.name), ['prod-login']);
});

test('secret expiry and enabled filters distinguish missing expired and active metadata', () => {
  const fixtures = [
    { name: 'none', enabled: true },
    { name: 'expired', enabled: false, expires_on: '2025-01-01T00:00:00Z' },
    { name: 'future', enabled: true, expires_on: '2030-01-01T00:00:00Z' },
  ];
  const options = { now: new Date('2029-01-01T00:00:00Z') };
  assert.deepEqual(model.filterSecrets(fixtures, { expiry: 'none' }, options).map(x => x.name), ['none']);
  assert.deepEqual(model.filterSecrets(fixtures, { expiry: 'expired' }, options).map(x => x.name), ['expired']);
  assert.deepEqual(model.filterSecrets(fixtures, { expiry: 'expiring' }, options).map(x => x.name), ['future']);
  assert.deepEqual(model.filterSecrets(fixtures, { enabled: false }, options).map(x => x.name), ['expired']);
});

test('expiry filtering compares canonical instants at timezone and equality boundaries', () => {
  const fixtures = [
    { name: 'equal', expires_on: '2030-01-01T00:00:00Z' },
    { name: 'after', expires_on: '2030-01-01T00:00:00.001Z' },
    { name: 'none', expires_on: null },
  ];
  const options = { now: new Date('2030-01-01T14:00:00+14:00') };
  assert.deepEqual(
    model.filterSecrets(fixtures, { expiry: 'expired' }, options).map(x => x.name),
    ['equal'],
  );
  assert.deepEqual(
    model.filterSecrets(fixtures, { expiry: 'expiring' }, options).map(x => x.name),
    ['after'],
  );
  assert.deepEqual(
    model.filterSecrets(fixtures, { expiry: 'none' }, options).map(x => x.name),
    ['none'],
  );
});

test('file filters compose real persisted folder and MIME metadata without mutating rows', () => {
  const fixtures = [
    { name: 'prod/report.pdf', content_type: 'application/pdf', upload_status: 'completed' },
    { name: 'prod/draft.txt', content_type: 'text/plain', upload_status: 'failed' },
    { name: 'dev/report.pdf', content_type: 'application/pdf', upload_status: 'completed' },
  ];
  const before = structuredClone(fixtures);
  const result = model.filterFiles(fixtures, {
    folder: 'prod',
    type: 'APPLICATION/PDF',
  });
  assert.deepEqual(result.map((item) => item.name), ['prod/report.pdf']);
  assert.deepEqual(fixtures, before);
});

test('file filters do not fabricate an upload lifecycle for persisted FileInfo rows', () => {
  const fixtures = [
    { name: 'one.pdf', content_type: 'application/pdf' },
    { name: 'two.txt', content_type: 'text/plain' },
  ];
  assert.deepEqual(
    model.filterFiles(fixtures, { uploadStatus: 'completed' }),
    fixtures,
  );
});

test('blank filter values are inactive and group matching is token exact', () => {
  const fixtures = [
    { name: 'one', groups: 'devops, dba' },
    { name: 'two', groups: ['ops'] },
  ];
  assert.deepEqual(model.filterSecrets(fixtures, {
    folder: '',
    group: '',
    type: '',
    expiry: '',
    enabled: null,
  }), fixtures);
  assert.deepEqual(model.filterSecrets(fixtures, { group: 'ops' }).map(x => x.name), ['two']);
});

test('active filter chips have stable labels and preserve boolean false', () => {
  assert.deepEqual(model.activeFilterChips({
    folder: 'prod',
    group: 'ops',
    type: '',
    expiry: 'expired',
    enabled: false,
  }, {
    folder: 'Folder',
    group: 'Group',
    type: 'Type',
    expiry: 'Expiry',
    enabled: 'Status',
  }), [
    { key: 'folder', label: 'Folder: prod' },
    { key: 'group', label: 'Group: ops' },
    { key: 'expiry', label: 'Expiry: expired' },
    { key: 'enabled', label: 'Status: disabled' },
  ]);
});
