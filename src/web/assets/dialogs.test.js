import test from 'node:test';
import assert from 'node:assert/strict';
import { createDialogManager, guardNavigation } from './dialogs.js';

class DialogElement {
  constructor(document) {
    this.document = document;
    this.hidden = true;
    this.disabled = false;
    this.attributes = new Map();
    this.focusables = [];
  }

  setAttribute(name, value) { this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  querySelectorAll() { return this.focusables; }
  focus() { this.document.activeElement = this; }
}

function modalDocument() {
  const document = {
    activeElement: null,
    listeners: new Map(),
    header: null,
    main: null,
    querySelectorAll(selector) { return selector === 'header, main' ? [this.header, this.main] : []; },
    addEventListener(type, listener) { this.listeners.set(type, listener); },
  };
  document.header = new DialogElement(document);
  document.main = new DialogElement(document);
  return document;
}

test('navigation guard keeps a dirty draft unless discard is confirmed', async () => {
  const draft = { baseline: { name: 'a' }, working: { name: 'b' } };
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => false }), false);
  assert.equal(await guardNavigation({ draft, savePending: false, confirmDiscard: async () => true }), true);
  assert.equal(await guardNavigation({ draft, savePending: true, confirmDiscard: async () => true }), false);
});

test('navigation guard proceeds without a draft or confirmation', async () => {
  assert.equal(await guardNavigation({ draft: null, savePending: false, confirmDiscard: async () => {
    throw new Error('confirmation should not be requested');
  } }), true);
});

test('modal manager keeps the page unavailable until the nested modal closes', () => {
  const document = modalDocument();
  const manager = createDialogManager(document);
  const invoker = new DialogElement(document);
  const keepEditing = new DialogElement(document);
  const sheet = new DialogElement(document);
  const confirmation = new DialogElement(document);

  manager.openModal(sheet, { initialFocus: keepEditing, invoker });
  assert.equal(manager.topModal(), sheet);
  assert.equal(document.main.getAttribute('aria-hidden'), 'true');
  assert.equal(document.header.getAttribute('aria-hidden'), 'true');

  manager.openModal(confirmation, { initialFocus: keepEditing, invoker: keepEditing });
  assert.equal(sheet.getAttribute('aria-hidden'), 'true');
  manager.closeModal(confirmation);
  assert.equal(manager.topModal(), sheet);
  assert.equal(document.activeElement, keepEditing);
  assert.equal(document.main.getAttribute('aria-hidden'), 'true');
  assert.equal(sheet.getAttribute('aria-hidden'), null);

  manager.closeModal(sheet);
  assert.equal(manager.topModal(), null);
  assert.equal(document.activeElement, invoker);
  assert.equal(document.main.getAttribute('aria-hidden'), null);
});

test('modal manager cycles Tab and delegates Escape to the top modal', () => {
  const document = modalDocument();
  const manager = createDialogManager(document);
  const first = new DialogElement(document);
  const last = new DialogElement(document);
  first.hidden = false;
  last.hidden = false;
  const sheet = new DialogElement(document);
  sheet.focusables = [first, last];
  let escaped = 0;

  manager.openModal(sheet, { initialFocus: first, onEscape: () => { escaped++; } });
  document.activeElement = last;
  let prevented = false;
  document.listeners.get('keydown')({ key: 'Tab', preventDefault: () => { prevented = true; } });
  assert.equal(prevented, true);
  assert.equal(document.activeElement, first);
  document.listeners.get('keydown')({ key: 'Escape', preventDefault() {} });
  assert.equal(escaped, 1);
});
