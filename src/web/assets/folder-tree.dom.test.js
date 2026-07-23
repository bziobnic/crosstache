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

  const mounted = model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: [items[0], items[2]],
    expanded: new Set(['apps']),
    selected: null,
    focusedId: '__all__',
    onSelect: (id) => selected.push(id),
    onToggle() {},
  });

  assert.equal(container.getAttribute('role'), 'tree');
  const treeitems = container.querySelectorAll('[role="treeitem"]');
  assert.deepEqual(treeitems.map((item) => item.dataset.folderId), [
    '__all__', '__unfiled__', 'apps', 'apps/prod', 'apps/stage',
  ]);
  assert.equal(treeitems[0].getAttribute('aria-selected'), 'true');
  assert.equal(treeitems[0].tabIndex, 0);
  assert.equal(treeitems[2].getAttribute('aria-expanded'), 'true');
  assert.equal(treeitems[2].getAttribute('aria-level'), '1');
  assert.equal(treeitems[3].getAttribute('aria-level'), '2');
  assert.match(treeitems[2].getAttribute('aria-label'), /1 visible of 2 total/);
  assert.match(treeitems[4].getAttribute('aria-label'), /0 visible of 1 total/);

  treeitems[2].onclick();
  assert.deepEqual(selected, ['apps']);
  assert.deepEqual(mounted.visibleIds, treeitems.map((item) => item.dataset.folderId));
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
  model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: items,
    expanded: new Set(['apps']),
    selected: 'apps',
    focusedId: 'apps',
    onSelect: (id) => selections.push(id),
    onToggle: (id, expanded) => toggles.push([id, expanded]),
    onFocus: (id) => focused.push(id),
  });
  const treeitems = container.querySelectorAll('[role="treeitem"]');
  const byId = Object.fromEntries(treeitems.map((item) => [item.dataset.folderId, item]));

  byId.apps.focus();
  assert.equal(key(byId.apps, 'ArrowRight'), true);
  assert.equal(document.activeElement, byId['apps/prod']);
  assert.deepEqual(focused, ['apps/prod']);
  assert.equal(key(byId['apps/prod'], 'ArrowLeft'), true);
  assert.equal(document.activeElement, byId.apps);
  assert.equal(key(byId.apps, 'ArrowLeft'), true);
  assert.deepEqual(toggles, [['apps', false]]);

  assert.equal(key(byId.apps, 'End'), true);
  assert.equal(document.activeElement, byId['apps/stage']);
  assert.equal(key(byId['apps/stage'], 'Home'), true);
  assert.equal(document.activeElement, byId.__all__);
  assert.equal(key(byId.__all__, 'ArrowDown'), true);
  assert.equal(document.activeElement, byId.__unfiled__);
  assert.equal(key(byId.__unfiled__, 'ArrowUp'), true);
  assert.equal(document.activeElement, byId.__all__);
  assert.equal(key(byId.apps, 'Enter'), true);
  assert.equal(key(byId.apps, ' '), true);
  assert.deepEqual(selections, ['apps', 'apps']);
  byId['apps/stage'].onfocus();
  assert.equal(byId['apps/stage'].tabIndex, 0);
  assert.equal(focused.at(-1), 'apps/stage');
  assert.equal(container.querySelectorAll('[role="treeitem"]').filter((item) => item.tabIndex === 0).length, 1);
});

test('ArrowRight expands a collapsed parent without moving focus', () => {
  const document = testDocument();
  const container = document.createElement('nav');
  const toggles = [];
  const items = [{ name: 'prod', folder: 'apps/prod' }];
  model.renderFolderTree({
    document,
    container,
    items,
    visibleItems: items,
    expanded: new Set(),
    selected: null,
    focusedId: 'apps',
    onSelect() {},
    onToggle: (id, expanded) => toggles.push([id, expanded]),
  });
  const apps = container.querySelectorAll('[role="treeitem"]').find((item) => item.dataset.folderId === 'apps');

  apps.focus();
  assert.equal(key(apps, 'ArrowRight'), true);
  assert.deepEqual(toggles, [['apps', true]]);
  assert.equal(document.activeElement, apps);
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
