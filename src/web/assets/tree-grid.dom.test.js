import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as model from './ui-model.js';
import { renderTreeGrid } from './tree-grid.js';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

class TestElement {
  constructor(document, tagName) {
    this.ownerDocument = document;
    this.tagName = tagName.toUpperCase();
    this.children = [];
    this.attributes = new Map();
    this.dataset = {};
    this.className = '';
    this.tabIndex = 0;
    this.textContent = '';
    this.checked = false;
    this.indeterminate = false;
    this.disabled = false;
    this.classes = new Set();
    this.classList = {
      add: (name) => this.classes.add(name),
      remove: (name) => this.classes.delete(name),
      contains: (name) => this.classes.has(name),
      toggle: (name, on) => (on ? this.classes.add(name) : this.classes.delete(name)),
    };
    const styles = new Map();
    this.style = {
      setProperty: (name, value) => styles.set(name, String(value)),
      getPropertyValue: (name) => styles.get(name) || '',
    };
  }

  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  removeAttribute(name) { this.attributes.delete(name); }
  append(...children) { this.children.push(...children); }
  appendChild(child) { this.children.push(child); return child; }
  replaceChildren(...children) { this.children = children; }
  focus() { this.ownerDocument.activeElement = this; }
  contains(target) {
    if (this === target) return true;
    return this.children.some((child) => child.contains?.(target));
  }

  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) {
      const match = child.find?.(predicate);
      if (match) return match;
    }
    return null;
  }
}

function testDocument() {
  const document = {
    activeElement: null,
    createElement(tagName) { return new TestElement(document, tagName); },
  };
  return document;
}

const SECRETS = [
  { name: 'alpha', folder: 'apps/prod', groups: 'ops', note: 'n', updated_on: '2026-01-02' },
  { name: 'beta', folder: 'apps/prod' },
  { name: 'gamma', folder: 'apps/stage' },
  { name: 'loose', folder: null },
];

function mount({
  document = testDocument(),
  items = SECRETS,
  kind = 'secrets',
  expanded = null,
  selection = { enabled: true, pending: false, ids: new Set() },
  forced = false,
  focusKey = '',
  onActivate = null,
} = {}) {
  const rows = model.contentRows(kind, items, { formatSize: (value) => `${value}` });
  const tree = model.buildContentTree(rows);
  const expansion = expanded || new Map(
    model.treeFolderIdentities(tree).map((id) => [model.folderIdentityKey(id), id]),
  );
  const tbody = document.createElement('tbody');
  const toggles = [];
  const selections = [];
  const focuses = [];
  const activations = [];
  const state = {
    document,
    tbody,
    toggles,
    selections,
    focuses,
    activations,
    tree,
    expansion,
    selection,
    render() {
      const gridRows = model.flattenContentTree(tree, expansion, {
        forcedKeys: forced ? model.treeForcedKeys(tree) : null,
      });
      state.gridRows = gridRows;
      state.mounted = renderTreeGrid({
        document,
        tbody,
        rows: gridRows,
        selection,
        treeContent: (row) => {
          const node = document.createElement('span');
          node.className = 'item-name-content';
          node.textContent = row.type === 'folder' ? row.label : row.identifier;
          return node;
        },
        cells: (row) => [document.createElement('td'), document.createElement('td')].map((cell) => {
          cell.textContent = row.type === 'folder' ? '' : String(row.identifier);
          return cell;
        }),
        rowLabel: (row) => (row.type === 'folder' ? `Folder ${row.path}` : `Secret ${row.identifier}`),
        selectionLabel: (row) => (row.type === 'folder'
          ? `Select all in ${row.path}`
          : `Select ${row.identifier}`),
        onToggle: (row, value) => {
          toggles.push([row.key, value]);
          if (value) expansion.set(row.key, row.identity);
          else expansion.delete(row.key);
          state.render();
        },
        onSelectionChange: (row) => {
          selections.push(row.key);
          state.render();
        },
        onActivate: onActivate || ((row) => activations.push(row.identifier)),
        focusKey,
        onFocus: (key) => focuses.push(key),
      });
      return state.mounted;
    },
  };
  state.render();
  return state;
}

const rowsOf = (state) => state.tbody.children;
const labelOf = (tr) => tr.getAttribute('aria-label');
const rowByLabel = (state, label) => rowsOf(state).find((tr) => labelOf(tr) === label);
const checkboxOf = (tr) => tr.find((node) => node.className === 'tree-checkbox');
const disclosureOf = (tr) => tr.find((node) => node.classes?.has?.('tree-disclosure')
  || node.className === 'tree-disclosure');

function key(target, value, extra = {}) {
  let prevented = false;
  target.onkeydown?.({
    key: value,
    preventDefault() { prevented = true; },
    ...extra,
  });
  return prevented;
}

test('one grid holds folders and their contents with treegrid row semantics', () => {
  const state = mount();
  const labels = rowsOf(state).map(labelOf);

  assert.deepEqual(labels, [
    'Folder apps',
    'Folder apps/prod',
    'Secret alpha',
    'Secret beta',
    'Folder apps/stage',
    'Secret gamma',
    'Secret loose',
  ]);
  assert.deepEqual(rowsOf(state).map((tr) => tr.getAttribute('role')), Array(7).fill('row'));
  assert.deepEqual(
    rowsOf(state).map((tr) => tr.getAttribute('aria-level')),
    ['1', '2', '3', '3', '2', '3', '1'],
  );
  assert.equal(rowByLabel(state, 'Folder apps').getAttribute('aria-expanded'), 'true');
  assert.equal(rowByLabel(state, 'Secret alpha').getAttribute('aria-expanded'), null);
  assert.equal(rowByLabel(state, 'Secret alpha').getAttribute('aria-selected'), 'false');
  assert.equal(
    rowByLabel(state, 'Secret loose').children[0].style.getPropertyValue('--tree-depth'),
    '0',
  );
  assert.equal(
    rowByLabel(state, 'Secret alpha').children[0].style.getPropertyValue('--tree-depth'),
    '2',
  );
  assert.equal(rowsOf(state).filter((tr) => tr.tabIndex === 0).length, 1);
});

test('a flat vault still expands and collapses because folders hold their items', () => {
  const state = mount({
    items: [{ name: 'a', folder: 'prod' }, { name: 'b', folder: 'dev' }],
    expanded: new Map(),
  });

  assert.deepEqual(rowsOf(state).map(labelOf), ['Folder dev', 'Folder prod']);
  const prod = rowByLabel(state, 'Folder prod');
  assert.equal(prod.getAttribute('aria-expanded'), 'false');
  assert.equal(disclosureOf(prod).textContent, '▸');

  disclosureOf(prod).onclick({ preventDefault() {}, stopPropagation() {} });
  assert.deepEqual(rowsOf(state).map(labelOf), ['Folder dev', 'Folder prod', 'Secret a']);
  assert.equal(rowByLabel(state, 'Folder prod').getAttribute('aria-expanded'), 'true');

  disclosureOf(rowByLabel(state, 'Folder prod')).onclick({ preventDefault() {}, stopPropagation() {} });
  assert.deepEqual(rowsOf(state).map(labelOf), ['Folder dev', 'Folder prod']);
});

test('checking a folder selects every descendant and unchecking clears them', () => {
  const state = mount();
  const ids = state.selection.ids;

  checkboxOf(rowByLabel(state, 'Folder apps/prod')).onchange();
  assert.deepEqual([...ids].sort(), ['alpha', 'beta']);
  assert.equal(checkboxOf(rowByLabel(state, 'Folder apps/prod')).checked, true);
  assert.equal(rowByLabel(state, 'Secret alpha').classes.has('selected-row'), true);
  assert.equal(rowByLabel(state, 'Secret alpha').getAttribute('aria-selected'), 'true');

  checkboxOf(rowByLabel(state, 'Folder apps/prod')).onchange();
  assert.deepEqual([...ids], []);
});

test('partial descendant selection renders the indeterminate ancestor state', () => {
  const state = mount();

  checkboxOf(rowByLabel(state, 'Secret alpha')).onchange();
  const prod = checkboxOf(rowByLabel(state, 'Folder apps/prod'));
  const apps = checkboxOf(rowByLabel(state, 'Folder apps'));

  assert.equal(prod.indeterminate, true);
  assert.equal(prod.checked, false);
  assert.equal(prod.getAttribute('aria-checked'), 'mixed');
  assert.equal(apps.getAttribute('aria-checked'), 'mixed');
  assert.equal(rowByLabel(state, 'Folder apps/prod').getAttribute('aria-selected'), 'false');

  checkboxOf(rowByLabel(state, 'Secret beta')).onchange();
  assert.equal(checkboxOf(rowByLabel(state, 'Folder apps/prod')).getAttribute('aria-checked'), 'true');
  assert.equal(checkboxOf(rowByLabel(state, 'Folder apps')).getAttribute('aria-checked'), 'mixed');

  // Checking a mixed ancestor completes the branch rather than clearing it.
  checkboxOf(rowByLabel(state, 'Folder apps')).onchange();
  assert.deepEqual([...state.selection.ids].sort(), ['alpha', 'beta', 'gamma']);
});

test('keyboard navigation moves, expands, collapses, and toggles selection', () => {
  const state = mount({ expanded: new Map(), focusKey: '' });
  const appsKey = model.folderIdentityKey(model.folderIdentity('apps'));
  const prodKey = model.folderIdentityKey(model.folderIdentity('apps/prod'));

  const apps = () => rowByLabel(state, 'Folder apps');
  apps().focus();
  assert.equal(key(apps(), 'ArrowRight'), true);
  assert.deepEqual(state.toggles.at(-1), [appsKey, true]);
  assert.ok(rowByLabel(state, 'Folder apps/prod'));

  assert.equal(key(apps(), 'ArrowRight'), true);
  assert.equal(state.document.activeElement, rowByLabel(state, 'Folder apps/prod'));
  assert.equal(key(rowByLabel(state, 'Folder apps/prod'), 'ArrowLeft'), true);
  assert.equal(state.document.activeElement, apps());

  assert.equal(key(rowByLabel(state, 'Folder apps/prod'), 'ArrowRight'), true);
  assert.deepEqual(state.toggles.at(-1), [prodKey, true]);
  assert.equal(key(rowByLabel(state, 'Secret alpha'), 'ArrowLeft'), true);
  assert.equal(state.document.activeElement, rowByLabel(state, 'Folder apps/prod'));

  assert.equal(key(rowByLabel(state, 'Secret loose'), 'Home'), true);
  assert.equal(state.document.activeElement, apps());
  assert.equal(key(apps(), 'End'), true);
  assert.equal(state.document.activeElement, rowByLabel(state, 'Secret loose'));
  assert.equal(key(rowByLabel(state, 'Secret loose'), 'ArrowUp'), true);
  assert.equal(key(rowByLabel(state, 'Folder apps'), 'ArrowDown'), true);

  assert.equal(key(rowByLabel(state, 'Secret alpha'), ' '), true);
  assert.deepEqual([...state.selection.ids], ['alpha']);
  assert.equal(key(rowByLabel(state, 'Secret alpha'), ' '), true);
  assert.deepEqual([...state.selection.ids], []);
  assert.equal(key(rowByLabel(state, 'Folder apps/prod'), ' '), true);
  assert.deepEqual([...state.selection.ids].sort(), ['alpha', 'beta']);
  assert.equal(rowsOf(state).filter((tr) => tr.tabIndex === 0).length, 1);
});

test('Enter opens an item row and toggles a folder row', () => {
  const state = mount({ expanded: new Map(), selection: { enabled: false, pending: false, ids: new Set() } });

  assert.equal(key(rowByLabel(state, 'Folder apps'), 'Enter'), true);
  assert.equal(rowByLabel(state, 'Folder apps').getAttribute('aria-expanded'), 'true');
  assert.equal(key(rowByLabel(state, 'Secret loose'), 'Enter'), true);
  assert.deepEqual(state.activations, ['loose']);
});

test('a rerender keeps focus on the same row instead of dropping to the top', () => {
  const state = mount();
  const alpha = rowByLabel(state, 'Secret alpha');

  alpha.focus();
  checkboxOf(rowByLabel(state, 'Secret alpha')).onchange();

  const replacement = rowByLabel(state, 'Secret alpha');
  assert.notEqual(replacement, alpha);
  assert.equal(state.document.activeElement, replacement);
  assert.equal(replacement.tabIndex, 0);
  assert.equal(rowsOf(state).filter((tr) => tr.tabIndex === 0).length, 1);
});

test('a disabled selection keeps checkboxes inert while rows still expand', () => {
  const state = mount({
    expanded: new Map(),
    selection: { enabled: true, pending: true, ids: new Set() },
  });

  assert.equal(checkboxOf(rowByLabel(state, 'Folder apps')).disabled, true);
  assert.equal(key(rowByLabel(state, 'Folder apps'), ' '), true);
  assert.deepEqual([...state.selection.ids], []);
  assert.equal(key(rowByLabel(state, 'Folder apps'), 'ArrowRight'), true);
  assert.ok(rowByLabel(state, 'Folder apps/prod'));
});

test('the files surface renders the same grid with its own columns', () => {
  const state = mount({
    kind: 'files',
    items: [
      { name: 'docs/prod/report.txt', size: 10, content_type: 'text/plain', last_modified: '2026-01-01' },
      { name: 'loose.txt', size: 4, content_type: 'text/plain', last_modified: '2026-01-02' },
    ],
  });

  assert.deepEqual(rowsOf(state).map(labelOf), [
    'Folder docs',
    'Folder docs/prod',
    'Secret docs/prod/report.txt',
    'Secret loose.txt',
  ]);
  checkboxOf(rowByLabel(state, 'Folder docs')).onchange();
  assert.deepEqual([...state.selection.ids], ['docs/prod/report.txt']);
});

test('tree grid CSS indents by depth and keeps identifiers untruncated on small screens', () => {
  const css = fs.readFileSync(path.join(__dirname, 'style.css'), 'utf8');
  const start = css.indexOf('@media (max-width: 48rem)');
  const end = css.indexOf('@media (max-width: 34rem)', start);
  const mobile48 = css.slice(start, end);

  assert.match(mobile48, /\.item-name-content strong\s*\{[^}]*overflow:visible/);
  assert.match(css, /padding-inline-start:calc\([^;]*var\(--tree-depth/);
  assert.doesNotMatch(css, /\.folder-sidebar/);
});

test('production markup exposes one treegrid per surface with expand and collapse controls', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');

  for (const surface of ['secrets', 'files']) {
    assert.match(html, new RegExp(`id="${surface}-workspace"`));
    assert.match(html, new RegExp(`id="${surface}-table"[^>]*role="treegrid"`));
    assert.match(html, new RegExp(`id="${surface}-expand-all"`));
    assert.match(html, new RegExp(`id="${surface}-collapse-all"`));
  }
  assert.doesNotMatch(html, /folder-sidebar|folder-sheet|id="secrets-folder-tree"/);
  assert.doesNotMatch(html, /class="selection-col"/);
});
