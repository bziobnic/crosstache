import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { buildMetadataIndex, searchIndex } from './commands.js';

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
