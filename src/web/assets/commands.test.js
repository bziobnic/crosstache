import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {
  buildMetadataIndex,
  createCommandRegistry,
  searchIndex,
  shouldHandleShortcut,
  shortcutIntent,
} from './commands.js';

test('shortcuts do not fire in compatible form controls', () => {
  for (const target of [
    { tagName: 'INPUT', isContentEditable: false },
    { tagName: 'TEXTAREA', isContentEditable: false },
    { tagName: 'SELECT', isContentEditable: false },
    { tagName: 'DIV', isContentEditable: true },
  ]) {
    assert.equal(shouldHandleShortcut({
      target,
      key: '/',
      metaKey: false,
      ctrlKey: false,
    }), false);
  }
  assert.equal(shouldHandleShortcut({
    target: { tagName: 'MAIN', isContentEditable: false },
    key: '/',
    metaKey: false,
    ctrlKey: false,
  }), true);
  assert.equal(shouldHandleShortcut({
    target: { tagName: 'DIV', isContentEditable: true },
    key: '/',
    metaKey: false,
    ctrlKey: false,
  }), false);
});

test('every global shortcut uses one exact modifier composition and editable-control matrix', () => {
  const base = {
    target: { tagName: 'MAIN', isContentEditable: false, closest: () => null },
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ctrlKey: false,
    repeat: false,
    isComposing: false,
    getModifierState: () => false,
  };
  assert.equal(shortcutIntent({ ...base, key: 'k', metaKey: true }), 'open-palette');
  assert.equal(shortcutIntent({ ...base, key: 'k', ctrlKey: true }), 'open-palette');
  assert.equal(shortcutIntent({ ...base, key: 'n', metaKey: true }), 'new-secret');
  assert.equal(shortcutIntent({ ...base, key: '/' }), 'search-local');
  assert.equal(shortcutIntent({ ...base, key: 'Escape' }), 'dismiss-topmost');
  for (const key of ['ArrowLeft', 'ArrowRight', 'Home', 'End']) {
    assert.equal(shortcutIntent({ ...base, key }), `tab-${key.toLowerCase()}`);
  }

  for (const target of [
    { tagName: 'INPUT', isContentEditable: false },
    { tagName: 'TEXTAREA', isContentEditable: false },
    { tagName: 'SELECT', isContentEditable: false },
    { tagName: 'DIV', isContentEditable: true },
  ]) {
    for (const event of [
      { key: 'k', metaKey: true },
      { key: 'n', ctrlKey: true },
      { key: '/' },
      { key: 'Escape' },
      { key: 'ArrowRight' },
    ]) {
      assert.equal(shortcutIntent({
        ...base,
        ...event,
        target: { ...target, closest: () => null },
      }), null);
    }
  }
  assert.equal(shortcutIntent({
    ...base,
    key: 'Escape',
    target: {
      tagName: 'INPUT',
      isContentEditable: false,
      closest: (selector) => selector === '[role="dialog"]' ? {} : null,
    },
  }, { allowOwnedEscape: true }), 'dismiss-topmost');

  for (const rejected of [
    { key: 'k', metaKey: true, ctrlKey: true },
    { key: 'k', metaKey: true, shiftKey: true },
    { key: 'k', metaKey: true, altKey: true },
    { key: '/', ctrlKey: true },
    { key: '/', repeat: true },
    { key: '/', isComposing: true },
    { key: '/', keyCode: 229 },
    { key: '/', getModifierState: (name) => name === 'AltGraph' },
  ]) {
    assert.equal(shortcutIntent({ ...base, ...rejected }), null);
  }
});

test('registry results carry frozen exact target and operation generations', () => {
  const registry = createCommandRegistry();
  registry.replaceMetadata({
    secrets: [{ name: 'same-name' }],
    scope: { alias: 'one', backend: 'local', vault: 'first' },
    contextGeneration: 'v1',
  });
  const [result] = registry.search('same-name', {
    context: {
      version: 'v1',
      workspace: { alias: 'one', entries: [] },
      backend: 'local',
      vault: 'first',
    },
  });
  assert.deepEqual(result.target, {
    alias: 'one',
    backend: 'local',
    vault: 'first',
    surface: 'secrets',
    item: 'same-name',
  });
  assert.equal(Object.isFrozen(result.target), true);
  assert.equal(typeof result.operationGeneration, 'number');
  assert.equal(result.contextGeneration, 'v1');

  registry.replaceMetadata({
    secrets: [{ name: 'same-name' }],
    scope: { alias: 'two', backend: 'local', vault: 'second' },
    contextGeneration: 'v2',
  });
  assert.equal(registry.isCurrent(result, {
    version: 'v2',
    workspace: { alias: 'two' },
    backend: 'local',
    vault: 'second',
  }), false);
});

test('command registry exposes required shortcuts and explicit result surface and scope', () => {
  const registry = createCommandRegistry();
  assert.deepEqual(
    registry.commands().map(({ id, shortcut }) => [id, shortcut]),
    [
      ['open-palette', 'mod+k'],
      ['search-local', '/'],
      ['new-secret', 'mod+n'],
      ['dismiss-topmost', 'escape'],
    ],
  );
  assert.equal(registry.search('', {
    context: { workspace: { alias: '', entries: [] }, backend: '', vault: '' },
  }).some(({ id }) => id === 'open-palette'), false);

  registry.replaceMetadata({
    secrets: [{ name: 'database-login', folder: 'prod', groups: ['ops'] }],
    files: [{ name: 'reports/quarter.pdf', content_type: 'application/pdf' }],
    folders: [{ name: 'prod', surface: 'secrets' }],
    scope: { alias: 'main', backend: 'local', vault: 'work' },
  });
  const results = registry.search('prod', {
    context: {
      workspace: {
        alias: 'main',
        entries: [{ alias: 'other', backend: 'aws', vault: 'shared' }],
      },
      backend: 'local',
      vault: 'work',
    },
  });
  assert.equal(results.some(({ kind, name }) => kind === 'secret' && name === 'database-login'), true);
  assert.equal(results.some(({ kind, name }) => kind === 'folder' && name === 'prod'), true);
  for (const result of results) {
    assert.equal(typeof result.surface, 'string');
    assert.equal(typeof result.scope?.backend, 'string');
    assert.equal(typeof result.scope?.vault, 'string');
  }
});

test('palette queries are ephemeral and never retained by the registry', () => {
  const registry = createCommandRegistry();
  registry.replaceMetadata({
    secrets: [{ name: 'one' }],
    scope: { alias: 'main', backend: 'local', vault: 'work' },
  });
  registry.search('one', {
    context: { workspace: { alias: 'main', entries: [] }, backend: 'local', vault: 'work' },
  });
  assert.doesNotMatch(JSON.stringify(registry.snapshot()), /query|one(?=.*query)/i);
});

test('metadata index excludes values notes and prior queries', () => {
  const index = buildMetadataIndex({
    secrets: [{
      name: 'db-url',
      folder: 'prod',
      groups: 'ops',
      note: 'private note',
      value: 'ultra-private-value',
      prior_query: 'remembered-query',
    }],
    files: [],
    folders: ['prod'],
  });
  const serialized = JSON.stringify(index);
  assert.match(serialized, /db-url/);
  assert.doesNotMatch(serialized, /private note|ultra-private-value|remembered-query/);
});

test('metadata index includes only approved normalized searchable fields', () => {
  const index = buildMetadataIndex({
    secrets: [{
      name: 'Ｃafé Login',
      folder: 'Pröd',
      groups: ['Ops', 'DBA'],
      content_type: 'application/vnd.xv.record',
      tags: { 'xv-type': 'Database', 'f.username': 'alice-private' },
    }],
    files: [{
      name: 'Reports/Quarter.PDF',
      content_type: 'Application/PDF',
      status: 'failed-private',
      tags: { description: 'hidden-file-tag' },
    }],
    folders: ['Pröd', 'Reports'],
  });

  assert.deepEqual(index.entries.map(({ surface, name }) => [surface, name]), [
    ['secrets', 'Ｃafé Login'],
    ['files', 'Reports/Quarter.PDF'],
    ['folders', 'Pröd'],
    ['folders', 'Reports'],
  ]);
  assert.match(JSON.stringify(index), /café login/);
  assert.doesNotMatch(JSON.stringify(index), /alice-private|failed-private|hidden-file-tag/);
});

test('search ranks exact name prefix word boundary substring then folder with stable name ties', () => {
  const index = buildMetadataIndex({
    secrets: [
      { name: 'prod', folder: 'other' },
      { name: 'production', folder: 'other' },
      { name: 'my prod login', folder: 'other' },
      { name: 'reproduce', folder: 'other' },
      { name: 'Zulu', folder: 'prod' },
      { name: 'alpha', folder: 'prod' },
    ],
    files: [],
    folders: [],
  });

  assert.deepEqual(
    searchIndex(index, 'ＰＲＯＤ').map(({ name }) => name),
    ['prod', 'production', 'my prod login', 'reproduce', 'alpha', 'Zulu'],
  );
});

test('file leaf-name matches rank ahead of folder matches and plain is a searchable record type', () => {
  const index = buildMetadataIndex({
    secrets: [{ name: 'plain-item', content_type: 'text/plain' }],
    files: [
      { name: 'prod/alpha.pdf', content_type: 'application/pdf' },
      { name: 'production.pdf', content_type: 'application/pdf' },
    ],
    folders: [],
  });
  assert.deepEqual(
    searchIndex(index, 'prod').filter(({ surface }) => surface === 'files').map(({ name }) => name),
    ['production.pdf', 'prod/alpha.pdf'],
  );
  assert.deepEqual(searchIndex(index, 'plain').map(({ name }) => name), ['plain-item']);
});

test('empty and whitespace queries return no palette results and are not stored in the index', () => {
  const index = buildMetadataIndex({
    secrets: [{ name: 'one' }],
    files: [],
    folders: [],
  });
  assert.deepEqual(searchIndex(index, ''), []);
  assert.deepEqual(searchIndex(index, '   '), []);
  searchIndex(index, 'one');
  assert.doesNotMatch(JSON.stringify(index), /query/);
});

test('production markup exposes local search clears and labelled filter controls for both surfaces', () => {
  const html = fs.readFileSync(new URL('./index.html', import.meta.url), 'utf8');
  for (const surface of ['secret', 'file']) {
    assert.match(html, new RegExp(`id="${surface}-search-clear"`));
    assert.match(html, new RegExp(`id="${surface}-filter-controls"[^>]*role="group"`));
    assert.match(html, new RegExp(`id="${surface}-filter-chips"[^>]*aria-live="polite"`));
    assert.match(html, new RegExp(`id="${surface}-filters-clear"`));
  }
  assert.doesNotMatch(html, /placeholder="[^"]*note/i);
  assert.doesNotMatch(html, /id="file-filter-uploadStatus"/);
});

test('no-results guidance names only fields that are actually searchable', () => {
  const source = fs.readFileSync(new URL('./secrets.js', import.meta.url), 'utf8');
  assert.match(source, /Try a different name, folder, group, or record type\./);
  assert.match(source, /Try a different name, folder, or type\./);
  assert.doesNotMatch(source, /Try a different[^.]*note/i);
  assert.doesNotMatch(source, /Try a different[^.]*status/i);
});
