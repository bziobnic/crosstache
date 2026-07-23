import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import * as model from './ui-model.js';

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
    const styles = new Map();
    this.style = {
      setProperty: (name, value) => styles.set(name, String(value)),
      getPropertyValue: (name) => styles.get(name) || '',
    };
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  removeAttribute(name) {
    this.attributes.delete(name);
  }

  append(...children) {
    this.children.push(...children);
  }

  appendChild(child) {
    this.children.push(child);
    return child;
  }

  replaceChildren(...children) {
    this.children = children;
  }

  focus() {
    this.ownerDocument.activeElement = this;
  }

  contains(target) {
    if (this === target) return true;
    return this.children.some((child) => child.contains?.(target));
  }

  querySelectorAll(selector) {
    const matches = [];
    const visit = (node) => {
      if (selector === '[role="treeitem"]' && node.getAttribute?.('role') === 'treeitem') {
        matches.push(node);
      }
      for (const child of node.children || []) visit(child);
    };
    visit(this);
    return matches;
  }
}

function testDocument() {
  const document = {
    activeElement: null,
    createElement(tagName) {
      return new TestElement(document, tagName);
    },
  };
  return document;
}

function key(target, value) {
  let prevented = false;
  target.onkeydown?.({
    key: value,
    preventDefault() { prevented = true; },
  });
  return prevented;
}

test('mounted folder tree exposes semantic hierarchy, selection, and visible/total counts', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const items = [
    { name: 'prod', folder: 'apps/prod' },
    { name: 'stage', folder: 'apps/stage' },
    { name: 'loose', folder: null },
  ];
  const selected = [];
  const apps = model.folderIdentity('apps');

  const mounted = model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: [items[0], items[2]],
    expanded: new Map([[model.folderIdentityKey(apps), apps]]),
    selected: model.FOLDER_ALL,
    focusedId: model.FOLDER_ALL,
    onSelect: (id) => selected.push(id),
    onToggle() {},
  });

  assert.equal(container.getAttribute('role'), 'tree');
  const treeitems = container.querySelectorAll('[role="treeitem"]');
  assert.deepEqual(treeitems.map((item) => item.dataset.folderId), [
    'folder-node-0',
    'folder-node-1',
    'folder-node-2',
    'folder-node-3',
    'folder-node-4',
  ]);
  assert.equal(treeitems[0].getAttribute('aria-selected'), 'true');
  assert.equal(treeitems[0].tabIndex, 0);
  assert.equal(treeitems[2].getAttribute('aria-expanded'), 'true');
  assert.equal(treeitems[2].getAttribute('aria-level'), '1');
  assert.equal(treeitems[3].getAttribute('aria-level'), '2');
  assert.match(treeitems[2].getAttribute('aria-label'), /1 visible of 2 total/);
  assert.match(treeitems[4].getAttribute('aria-label'), /0 visible of 1 total/);

  treeitems[2].onclick();
  assert.deepEqual(selected, [apps]);
  assert.deepEqual(
    mounted.visibleIds.map(model.folderIdentityKey),
    treeitems.map((item) => item.__xvFolderIdentityKey),
  );
});

test('mounted folder tree uses roving tabindex and complete tree keyboard navigation', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const toggles = [];
  const selections = [];
  const focused = [];
  const items = [
    { name: 'prod', folder: 'apps/prod' },
    { name: 'stage', folder: 'apps/stage' },
    { name: 'loose', folder: null },
  ];
  const apps = model.folderIdentity('apps');
  const prod = model.folderIdentity('apps/prod');
  const stage = model.folderIdentity('apps/stage');
  model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: items,
    expanded: new Map([[model.folderIdentityKey(apps), apps]]),
    selected: apps,
    focusedId: apps,
    onSelect: (id) => selections.push(id),
    onToggle: (id, expanded) => toggles.push([id, expanded]),
    onFocus: (id) => focused.push(id),
  });
  const treeitems = container.querySelectorAll('[role="treeitem"]');
  const byId = Object.fromEntries(
    treeitems.map((item) => [item.__xvFolderIdentityKey, item]),
  );
  const allKey = model.folderIdentityKey(model.FOLDER_ALL);
  const unfiledKey = model.folderIdentityKey(model.FOLDER_UNFILED);
  const appsKey = model.folderIdentityKey(apps);
  const prodKey = model.folderIdentityKey(prod);
  const stageKey = model.folderIdentityKey(stage);

  byId[appsKey].focus();
  assert.equal(key(byId[appsKey], 'ArrowRight'), true);
  assert.equal(document.activeElement, byId[prodKey]);
  assert.deepEqual(focused, [prod]);
  assert.equal(key(byId[prodKey], 'ArrowLeft'), true);
  assert.equal(document.activeElement, byId[appsKey]);
  assert.equal(key(byId[appsKey], 'ArrowLeft'), true);
  assert.deepEqual(toggles, [[apps, false]]);

  assert.equal(key(byId[appsKey], 'End'), true);
  assert.equal(document.activeElement, byId[stageKey]);
  assert.equal(key(byId[stageKey], 'Home'), true);
  assert.equal(document.activeElement, byId[allKey]);
  assert.equal(key(byId[allKey], 'ArrowDown'), true);
  assert.equal(document.activeElement, byId[unfiledKey]);
  assert.equal(key(byId[unfiledKey], 'ArrowUp'), true);
  assert.equal(document.activeElement, byId[allKey]);
  assert.equal(key(byId[appsKey], 'Enter'), true);
  assert.equal(key(byId[appsKey], ' '), true);
  assert.deepEqual(selections, [apps, apps]);
  byId[stageKey].onfocus();
  assert.equal(byId[stageKey].tabIndex, 0);
  assert.deepEqual(focused.at(-1), stage);
  assert.equal(container.querySelectorAll('[role="treeitem"]').filter((item) => item.tabIndex === 0).length, 1);
});

test('ArrowRight expands a collapsed parent without moving focus', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const toggles = [];
  const items = [{ name: 'prod', folder: 'apps/prod' }];
  const appsIdentity = model.folderIdentity('apps');
  model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: items,
    expanded: new Map(),
    selected: model.FOLDER_ALL,
    focusedId: appsIdentity,
    onSelect() {},
    onToggle: (id, expanded) => toggles.push([id, expanded]),
  });
  const apps = container.querySelectorAll('[role="treeitem"]').find(
    (item) => item.__xvFolderIdentityKey === model.folderIdentityKey(appsIdentity),
  );

  apps.focus();
  assert.equal(key(apps, 'ArrowRight'), true);
  assert.deepEqual(toggles, [[appsIdentity, true]]);
  assert.equal(document.activeElement, apps);
});

test('selection activation restores focus to the corresponding replacement treeitem', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const items = [{ name: 'prod', folder: 'apps/prod' }];
  const apps = model.folderIdentity('apps');
  let selected = model.FOLDER_ALL;
  let mounted;
  const render = () => {
    mounted = model.renderFolderTree({
      document,
      container,
      items,
      visibleItems: items,
      expanded: new Map([[model.folderIdentityKey(apps), apps]]),
      selected,
      focusedId: apps,
      onSelect: (id) => {
        selected = id;
        render();
      },
      onToggle() {},
    });
  };
  render();
  const before = container.querySelectorAll('[role="treeitem"]')
    .find((item) => item.getAttribute('aria-label').startsWith('apps,'));

  before.focus();
  before.onclick({});

  const after = container.querySelectorAll('[role="treeitem"]')
    .find((item) => item.getAttribute('aria-label').startsWith('apps,'));
  assert.notEqual(after, before);
  assert.equal(document.activeElement, after);
  assert.equal(after.getAttribute('aria-selected'), 'true');
  assert.deepEqual(mounted.focusedId(), apps);
  assert.equal(
    container.querySelectorAll('[role="treeitem"]')
      .filter((item) => item.getAttribute('aria-selected') === 'true').length,
    1,
  );
  assert.equal(
    container.querySelectorAll('[role="treeitem"]')
      .filter((item) => item.tabIndex === 0).length,
    1,
  );
});

test('all, unfiled, and reserved-name folders produce unique treeitem identities', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const items = [
    { name: 'all', folder: '__all__' },
    { name: 'reserved', folder: '__unfiled__' },
    { name: 'none', folder: null },
  ];
  model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: items,
    expanded: new Map(),
    selected: model.FOLDER_ALL,
    focusedId: model.FOLDER_ALL,
    onSelect() {},
    onToggle() {},
  });
  const treeitems = container.querySelectorAll('[role="treeitem"]');
  const ids = treeitems.map((item) => item.dataset.folderId);

  assert.equal(ids.length, 4);
  assert.equal(new Set(ids).size, ids.length);
  assert.ok(ids.every((id) => /^folder-node-\d+$/.test(id)));
  assert.equal(ids.includes(model.folderIdentityKey(model.folderIdentity('__all__'))), false);
  assert.equal(treeitems.filter((item) => item.tabIndex === 0).length, 1);
  assert.equal(
    treeitems.filter((item) => item.getAttribute('aria-selected') === 'true').length,
    1,
  );
});

test('pointer disclosure toggles a branch without selecting its treeitem', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const apps = model.folderIdentity('apps');
  const toggles = [];
  const selections = [];
  model.renderFolderTree({
    document,
    container,
    items: [{ name: 'prod', folder: 'apps/prod' }],
    visibleItems: [{ name: 'prod', folder: 'apps/prod' }],
    expanded: new Map(),
    selected: model.FOLDER_ALL,
    focusedId: apps,
    onSelect: (id) => selections.push(id),
    onToggle: (id, value) => toggles.push([id, value]),
  });
  const appsItem = container.querySelectorAll('[role="treeitem"]')
    .find((item) => item.getAttribute('aria-label').startsWith('apps,'));
  const disclosure = appsItem.children.find(
    (child) => child.className === 'folder-tree-disclosure',
  );
  let stopped = false;

  disclosure.onclick({
    pointerType: 'touch',
    preventDefault() {},
    stopPropagation() { stopped = true; },
  });

  assert.equal(stopped, true);
  assert.deepEqual(toggles, [[apps, true]]);
  assert.deepEqual(selections, []);
});

test('ten-level folder trees expose dynamic depth instead of a level-six ceiling', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const segments = 'a/b/c/d/e/f/g/h/i/j'.split('/');
  const expanded = new Map();
  for (let index = 1; index < segments.length; index++) {
    const identity = model.folderIdentity(segments.slice(0, index).join('/'));
    expanded.set(model.folderIdentityKey(identity), identity);
  }
  model.renderFolderTree({
    document,
    container,
    items: [{ name: 'deep', folder: segments.join('/') }],
    visibleItems: [{ name: 'deep', folder: segments.join('/') }],
    expanded,
    selected: model.FOLDER_ALL,
    focusedId: model.FOLDER_ALL,
    onSelect() {},
    onToggle() {},
  });
  const deepest = container.querySelectorAll('[role="treeitem"]')
    .find((item) => item.getAttribute('aria-label').startsWith('j,'));

  assert.equal(deepest.getAttribute('aria-level'), '10');
  assert.equal(deepest.style.getPropertyValue('--folder-depth'), '9');
});

test('responsive CSS keeps primary identifiers untruncated through the 48rem breakpoint', () => {
  const css = fs.readFileSync(path.join(__dirname, 'style.css'), 'utf8');
  const start = css.indexOf('@media (max-width: 48rem)');
  const end = css.indexOf('@media (max-width: 34rem)', start);
  const mobile48 = css.slice(start, end);

  assert.match(mobile48, /\.item-name-content strong\s*\{[^}]*overflow:visible/);
  assert.match(css, /padding-inline-start:calc\([^;]*var\(--folder-depth/);
  assert.doesNotMatch(css, /\.folder-tree-item\[data-level="6"\]/);
});

test('production markup provides desktop trees and labelled mobile filter sheet for both surfaces', () => {
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');

  for (const surface of ['secrets', 'files']) {
    assert.match(html, new RegExp(`id="${surface}-workspace"`));
    assert.match(html, new RegExp(`id="${surface}-folder-tree"[^>]*role="tree"`));
    assert.match(html, new RegExp(`id="${surface}-folder-filter-open"`));
    assert.match(html, new RegExp(`id="${surface}-folder-sheet"[^>]*role="dialog"`));
    assert.match(html, new RegExp(`id="${surface}-mobile-folder-tree"[^>]*role="tree"`));
    assert.match(html, new RegExp(`id="${surface}-folders-expand-all"`));
    assert.match(html, new RegExp(`id="${surface}-folders-collapse-all"`));
    assert.match(html, new RegExp(`id="${surface}-mobile-folders-expand-all"`));
    assert.match(html, new RegExp(`id="${surface}-mobile-folders-collapse-all"`));
  }
  assert.match(html, /id="secrets-folder-sheet"[^>]*aria-labelledby="secrets-folder-sheet-title"/);
  assert.match(html, /id="files-folder-sheet"[^>]*aria-labelledby="files-folder-sheet-title"/);
  const main = html.split(/<main[^>]*>/)[1].split('</main>')[0];
  assert.doesNotMatch(main, /class="folder-sheet"/, 'modal sheets must remain outside inert main content');
});
