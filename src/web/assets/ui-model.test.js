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

test('slash paths become nested folder nodes with stable unfiled node', () => {
  const tree = model.buildFolderTree([
    { name: 'a', folder: 'apps/prod' },
    { name: 'b', folder: null },
  ]);

  assert.deepEqual(tree.map((node) => node.id.kind), ['unfiled', 'folder']);
  assert.equal(tree[0].label, 'Unfiled');
  assert.equal(tree[1].id.path, 'apps');
  assert.equal(tree[1].children[0].id.path, 'apps/prod');
});

test('folder paths normalize slashes and empty segments without duplicating parents', () => {
  const tree = model.buildFolderTree([
    { name: 'one', folder: '/apps//prod/' },
    { name: 'two', folder: 'apps/prod' },
    { name: 'three', folder: 'apps///stage/' },
    { name: 'four', folder: '///' },
  ]);

  assert.deepEqual(tree.map((node) => node.id.kind), ['unfiled', 'folder']);
  assert.deepEqual(tree[1].children.map((node) => node.id.path), ['apps/prod', 'apps/stage']);
  assert.equal(tree[1].directCount, 0);
  assert.equal(tree[1].totalCount, 3);
  assert.equal(tree[1].children[0].directCount, 2);
  assert.equal(tree[0].totalCount, 1);
});

test('folder identities preserve valid whitespace and cannot collide with reserved labels', () => {
  const tree = model.buildFolderTree([
    { name: 'spaced', folder: ' apps / prod ' },
    { name: 'plain', folder: 'apps/prod' },
    { name: 'reserved-all', folder: '__all__' },
    { name: 'reserved-unfiled', folder: '__unfiled__' },
    { name: 'unfiled', folder: null },
  ]);
  const rows = model.flattenFolderTree(tree, new Map(
    tree.map((node) => [model.folderIdentityKey(node.id), node.id]),
  ));
  const labels = rows.map((row) => row.label);

  assert.ok(labels.includes(' apps '));
  assert.ok(labels.includes('apps'));
  assert.ok(labels.includes('__all__'));
  assert.ok(labels.includes('__unfiled__'));
  assert.equal(tree.find((node) => node.label === 'Unfiled').id.kind, 'unfiled');
  assert.equal(tree.find((node) => node.label === '__unfiled__').id.kind, 'folder');
  assert.notEqual(
    model.folderIdentityKey(tree.find((node) => node.label === 'Unfiled').id),
    model.folderIdentityKey(tree.find((node) => node.label === '__unfiled__').id),
  );
  assert.equal(model.normalizeFolderPath('/ apps // prod /'), ' apps / prod ');
});

test('folder nodes use the existing numeric case-insensitive collation', () => {
  const tree = model.buildFolderTree([
    { name: 'a', folder: 'Folder 10' },
    { name: 'b', folder: 'folder 2' },
    { name: 'c', folder: 'Alpha' },
  ]);

  assert.deepEqual(tree.map((node) => node.id.path), ['Alpha', 'folder 2', 'Folder 10']);
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

  assert.match(secrets, /^xv\.ui\.folder-expansion\.v4:[A-Za-z0-9_-]{43}$/);
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
  assert.match([...values.keys()][0], /^xv\.ui\.folder-expansion\.v4:[A-Za-z0-9_-]{43}$/);
  assert.deepEqual(model.loadFolderExpansion(storage, tokenIndex), ['A'.repeat(43)]);
  assert.ok(removed.some((key) => key.startsWith('xv.ui.folder-expansion.v2:')));
  assert.equal(
    [...values.keys()].some((key) => key.startsWith('xv.ui.folder-expansion.v2:')),
    false,
  );
});

test('typed folder matching keeps reserved-name folders distinct from all and unfiled', () => {
  assert.equal(model.itemMatchesFolder({ folder: '__all__' }, model.FOLDER_ALL), true);
  assert.equal(model.itemMatchesFolder({ folder: 'other' }, model.FOLDER_ALL), true);
  assert.equal(model.itemMatchesFolder({ folder: null }, model.FOLDER_UNFILED), true);
  assert.equal(model.itemMatchesFolder({ folder: '__unfiled__' }, model.FOLDER_UNFILED), false);
  assert.equal(
    model.itemMatchesFolder({ folder: '__unfiled__' }, model.folderIdentity('__unfiled__')),
    true,
  );
  assert.equal(
    model.itemMatchesFolder({ folder: ' apps /prod' }, model.folderIdentity(' apps ')),
    true,
  );
});

test('collapsing or removing a selected descendant reconciles selection to a visible item', () => {
  const navigation = model.createFolderNavigationState(null);
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const stage = model.folderIdentity('apps/stage');
  const scope = { backend: 'local', vault: 'one', surface: 'secrets' };

  navigation.sync(scope, {
    total: 2,
    folderIds: [apps, prod, stage],
    expandableIds: [apps],
  });
  navigation.select(prod);
  navigation.toggle(apps, false);
  assert.deepEqual(navigation.snapshot().selected, apps);

  navigation.select(stage);
  navigation.collapseAll();
  assert.deepEqual(navigation.snapshot().selected, model.FOLDER_ALL);

  navigation.select(prod);
  navigation.sync(scope, {
    total: 1,
    folderIds: [apps, stage],
    expandableIds: [apps],
  });
  assert.deepEqual(navigation.snapshot().selected, model.FOLDER_ALL);
});

test('folder membership includes descendants but keeps unfiled stable', () => {
  assert.equal(model.itemMatchesFolder({ folder: 'apps/prod' }, model.folderIdentity('apps')), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps/prod' }, model.folderIdentity('apps/prod')), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps/production' }, model.folderIdentity('apps/prod')), false);
  assert.equal(model.itemMatchesFolder({ folder: '' }, model.FOLDER_UNFILED), true);
  assert.equal(model.itemMatchesFolder({ folder: 'apps' }, model.FOLDER_UNFILED), false);
  assert.equal(model.itemMatchesFolder({ folder: 'apps' }, model.FOLDER_ALL), true);
});

test('folder expansion persistence is explicit and isolated by context and surface', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const scope = { backend: 'local-a', vault: 'one', surface: 'secrets' };
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

test('folder navigation resets selection and restores expansion when workspace scope changes', () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
  };
  const navigation = model.createFolderNavigationState(storage);
  const one = { backend: 'local', vault: 'one', surface: 'secrets' };
  const two = { backend: 'local', vault: 'two', surface: 'secrets' };
  const apps = model.folderIdentity('apps');
  const other = model.folderIdentity('other');
  const oneTokens = folderTokenIndex('S', [apps.path]);
  const twoTokens = folderTokenIndex('T', [other.path]);

  navigation.sync(one, {
    total: 51, folderIds: [apps], expandableIds: [apps], tokenIndex: oneTokens,
  });
  assert.deepEqual(navigation.snapshot(), { selected: model.FOLDER_ALL, expanded: [] });
  navigation.select(apps);
  navigation.toggle(apps, true);
  assert.deepEqual(navigation.snapshot(), { selected: apps, expanded: [apps] });

  navigation.sync(two, {
    total: 2, folderIds: [other], expandableIds: [other], tokenIndex: twoTokens,
  });
  assert.deepEqual(navigation.snapshot(), { selected: model.FOLDER_ALL, expanded: [other] });
  navigation.select(other);

  navigation.sync(one, {
    total: 51, folderIds: [apps], expandableIds: [apps], tokenIndex: oneTokens,
  });
  assert.deepEqual(navigation.snapshot(), { selected: model.FOLDER_ALL, expanded: [apps] });
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
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const documents = model.folderIdentity('documents');
  const secretTokens = folderTokenIndex('S', [apps.path, prod.path]);
  const fileTokens = folderTokenIndex('F', [documents.path]);

  navigation.sync(secrets, {
    total: 60,
    folderIds: [apps, prod],
    expandableIds: [apps, prod],
    tokenIndex: secretTokens,
  });
  navigation.expandAll();
  navigation.sync(files, {
    total: 60,
    folderIds: [documents],
    expandableIds: [documents],
    tokenIndex: fileTokens,
  });
  assert.deepEqual(navigation.snapshot(), { selected: model.FOLDER_ALL, expanded: [] });
  navigation.toggle(documents, true);
  navigation.sync(secrets, {
    total: 60,
    folderIds: [apps, prod],
    expandableIds: [apps, prod],
    tokenIndex: secretTokens,
  });
  assert.deepEqual(navigation.snapshot(), {
    selected: model.FOLDER_ALL,
    expanded: [apps, prod],
  });
});

test('large folder view models build total and visible trees exactly once each', () => {
  const items = Array.from({ length: 10_000 }, (_, index) => ({
    name: `secret-${index}`,
    folder: `team-${index % 100}/service-${index % 500}/env-${index % 3}`,
  }));
  let builds = 0;
  const buildTree = (source) => {
    builds++;
    return model.buildFolderTree(source);
  };

  const view = model.buildFolderViewModel(
    items,
    items.filter((_, index) => index % 2 === 0),
    { buildTree },
  );

  assert.equal(builds, 2);
  assert.equal(view.totalCount, 10_000);
  assert.equal(view.visibleCount, 5_000);
  assert.equal(view.folderCount, 2_100);
  assert.equal(view.folderIds.length, 2_100);
  assert.ok(view.expandableIds.length > 0);
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
  const navigation = model.createFolderNavigationState(storage);

  navigation.sync(scope, {
    total: 51,
    folderIds: [apps, prod],
    expandableIds: [apps],
    tokenIndex,
  });
  navigation.toggle(apps, true);
  navigation.sync(scope, {
    total: 1,
    folderIds: [],
    expandableIds: [],
    tokenIndex,
  });
  assert.deepEqual(navigation.snapshot().expanded, []);
  assert.equal(
    [...values.values()].some((serialized) => serialized.includes('A'.repeat(43))),
    false,
  );

  const fresh = model.createFolderNavigationState(storage);
  fresh.sync(scope, {
    total: 51,
    folderIds: [apps, prod],
    expandableIds: [apps],
    tokenIndex,
  });
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

test('file filters compose folder MIME type and upload status without mutating rows', () => {
  const fixtures = [
    { name: 'prod/report.pdf', content_type: 'application/pdf', upload_status: 'completed' },
    { name: 'prod/draft.txt', content_type: 'text/plain', upload_status: 'failed' },
    { name: 'dev/report.pdf', content_type: 'application/pdf', upload_status: 'completed' },
  ];
  const before = structuredClone(fixtures);
  const result = model.filterFiles(fixtures, {
    folder: 'prod',
    type: 'APPLICATION/PDF',
    uploadStatus: 'completed',
  });
  assert.deepEqual(result.map((item) => item.name), ['prod/report.pdf']);
  assert.deepEqual(fixtures, before);
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
