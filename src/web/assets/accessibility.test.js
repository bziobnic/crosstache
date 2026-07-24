import test from 'node:test';
import assert from 'node:assert/strict';

import {
  mountRovingFocus,
  mountTabs,
  syncVisibleSelection,
} from './accessibility.js';

class TestElement {
  constructor(id, { role = '', hidden = false } = {}) {
    this.id = id;
    this.hidden = hidden;
    this.disabled = false;
    this.tabIndex = 0;
    this.attributes = new Map();
    this.listeners = new Map();
    this.ownerDocument = null;
    this.clicked = 0;
    if (role) this.setAttribute('role', role);
  }

  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) || [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }
  removeEventListener(type, listener) {
    this.listeners.set(type, (this.listeners.get(type) || []).filter((item) => item !== listener));
  }
  dispatch(type, event = {}) {
    for (const listener of this.listeners.get(type) || []) listener({ target: this, ...event });
  }
  focus() { this.ownerDocument.activeElement = this; }
  click() { this.clicked++; this.dispatch('click'); }
  closest(selector) { return selector === '[role="tab"]' && this.getAttribute('role') === 'tab' ? this : null; }
}

function tabFixture() {
  const document = {
    activeElement: null,
    elements: new Map(),
    getElementById(id) { return this.elements.get(id) || null; },
  };
  const tabs = ['secrets', 'files', 'trash'].map((name, index) => {
    const tab = new TestElement(`tab-${name}`, { role: 'tab' });
    tab.ownerDocument = document;
    tab.setAttribute('aria-controls', `${name}-view`);
    tab.setAttribute('aria-selected', String(index === 0));
    tab.tabIndex = index === 0 ? 0 : -1;
    document.elements.set(tab.id, tab);
    const panel = new TestElement(`${name}-view`, { role: 'tabpanel', hidden: index !== 0 });
    panel.ownerDocument = document;
    document.elements.set(panel.id, panel);
    return tab;
  });
  const tablist = new TestElement('vault-tabs', { role: 'tablist' });
  tablist.ownerDocument = document;
  tablist.querySelectorAll = () => tabs;
  tablist.contains = (target) => tabs.includes(target);
  return { document, tablist, tabs };
}

function key(container, target, key) {
  let prevented = false;
  container.dispatch('keydown', {
    target,
    key,
    preventDefault() { prevented = true; },
  });
  return prevented;
}

test('mountRovingFocus skips unavailable items and wraps keyboard focus', () => {
  const { document, tablist, tabs } = tabFixture();
  tabs[1].hidden = true;
  const mounted = mountRovingFocus(tablist, '[role="tab"]');

  tabs[0].focus();
  assert.equal(key(tablist, tabs[0], 'ArrowRight'), true);
  assert.equal(document.activeElement, tabs[2]);
  assert.deepEqual(tabs.map((tab) => tab.tabIndex), [-1, -1, 0]);

  assert.equal(key(tablist, tabs[2], 'Home'), true);
  assert.equal(document.activeElement, tabs[0]);
  mounted.destroy();
});

test('mountTabs activates focused tabs with arrows and Home/End', () => {
  const { document, tablist, tabs } = tabFixture();
  const mounted = mountTabs(tablist);

  tabs[0].focus();
  assert.equal(key(tablist, tabs[0], 'ArrowRight'), true);
  assert.equal(document.activeElement, tabs[1]);
  assert.equal(tabs[1].clicked, 1);

  assert.equal(key(tablist, tabs[1], 'End'), true);
  assert.equal(document.activeElement, tabs[2]);
  assert.equal(tabs[2].clicked, 1);
  mounted.destroy();
});

test('mountTabs sync replaces a dynamically unavailable selected tab exactly once', async () => {
  const { document, tablist, tabs } = tabFixture();
  const mounted = mountTabs(tablist);
  tabs[0].setAttribute('aria-selected', 'false');
  tabs[0].tabIndex = -1;
  tabs[1].setAttribute('aria-selected', 'true');
  tabs[1].tabIndex = 0;
  document.getElementById('secrets-view').hidden = true;
  document.getElementById('files-view').hidden = false;
  tabs[1].hidden = true;

  const selected = mounted.sync();

  assert.equal(selected, tabs[0]);
  assert.deepEqual(tabs.map((tab) => tab.getAttribute('aria-selected')), ['true', 'false', 'false']);
  assert.deepEqual(tabs.map((tab) => tab.tabIndex), [0, -1, -1]);
  assert.equal(document.getElementById('secrets-view').hidden, false);
  assert.equal(document.getElementById('files-view').hidden, true);
  assert.equal(document.activeElement, tabs[0]);
  assert.equal(tabs[0].clicked, 1);

  mounted.sync();
  assert.equal(tabs[0].clicked, 1);

  await Promise.resolve();
  tabs[0].setAttribute('aria-disabled', 'true');
  tabs[1].hidden = false;
  tabs[1].disabled = true;
  mounted.sync();
  assert.deepEqual(tabs.map((tab) => tab.getAttribute('aria-selected')), ['false', 'false', 'true']);
  assert.deepEqual(tabs.map((tab) => tab.tabIndex), [-1, -1, 0]);
  assert.equal(document.activeElement, tabs[2]);
  assert.equal(tabs[2].clicked, 1);
  mounted.destroy();
});

test('syncVisibleSelection reports visible-only checked and mixed state', () => {
  const selectedIds = new Set(['visible-a', 'filtered-out']);
  assert.deepEqual(
    syncVisibleSelection({
      visibleIds: ['visible-a', 'visible-b'],
      selectedIds,
    }),
    {
      visibleCount: 2,
      selectedVisibleCount: 1,
      checked: false,
      mixed: true,
    },
  );
  assert.deepEqual(
    syncVisibleSelection({ visibleIds: ['visible-a'], selectedIds }),
    {
      visibleCount: 1,
      selectedVisibleCount: 1,
      checked: true,
      mixed: false,
    },
  );
});
