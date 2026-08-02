import test from 'node:test';
import assert from 'node:assert/strict';
import { createDialogManager, guardNavigation, resolveDialogInvoker } from './dialogs.js';

class DialogElement {
  constructor(document) {
    this.document = document;
    this.hidden = true;
    this.disabled = false;
    this.isConnected = true;
    this.inert = false;
    this.parentElement = null;
    this.tabIndex = 0;
    this.attributes = new Map();
    this.focusables = [];
  }

  setAttribute(name, value) { this.attributes.set(name, value); }
  removeAttribute(name) { this.attributes.delete(name); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  querySelectorAll() { return this.focusables; }
  focus() {
    this.onFocus?.();
    this.document.activeElement = this;
  }
}

function modalDocument() {
  const document = {
    activeElement: null,
    listeners: new Map(),
    header: null,
    main: null,
    contextRail: null,
    contextRailTop: null,
    vaultTabs: null,
    quickAccess: null,
    contextRailFooter: null,
    querySelectorAll(selector) {
      return selector === '#app-header, main, #context-rail, .context-rail-top, #vault-tabs, .quick-access, .context-rail-footer'
        ? [this.header, this.main, this.contextRail, this.contextRailTop, this.vaultTabs, this.quickAccess, this.contextRailFooter]
        : [];
    },
    addEventListener(type, listener) { this.listeners.set(type, listener); },
  };
  document.header = new DialogElement(document);
  document.main = new DialogElement(document);
  document.contextRail = new DialogElement(document);
  document.contextRailTop = new DialogElement(document);
  document.vaultTabs = new DialogElement(document);
  document.quickAccess = new DialogElement(document);
  document.contextRailFooter = new DialogElement(document);
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

test('dialog invoker resolution skips a CSS-hidden rail action for the visible command trigger', () => {
  const document = modalDocument();
  const hiddenRailAction = new DialogElement(document);
  const commandTrigger = new DialogElement(document);
  hiddenRailAction.hidden = false;
  commandTrigger.hidden = false;
  hiddenRailAction.style = { display: 'none' };
  document.activeElement = hiddenRailAction;

  assert.equal(resolveDialogInvoker(document, [hiddenRailAction, commandTrigger]), commandTrigger);

  document.activeElement = commandTrigger;
  assert.equal(resolveDialogInvoker(document, [hiddenRailAction, commandTrigger]), commandTrigger);
});

test('modal manager keeps the page unavailable until the nested modal closes', () => {
  const document = modalDocument();
  const manager = createDialogManager(document);
  const invoker = new DialogElement(document);
  const keepEditing = new DialogElement(document);
  const sheet = new DialogElement(document);
  const confirmation = new DialogElement(document);
  invoker.hidden = false;
  keepEditing.hidden = false;

  manager.openModal(sheet, { initialFocus: keepEditing, invoker });
  assert.equal(manager.topModal(), sheet);
  assert.equal(document.main.getAttribute('aria-hidden'), 'true');
  assert.equal(document.header.getAttribute('aria-hidden'), 'true');
  assert.equal(document.contextRail.getAttribute('aria-hidden'), 'true');
  assert.equal(document.contextRailTop.getAttribute('aria-hidden'), 'true');
  assert.equal(document.vaultTabs.getAttribute('aria-hidden'), 'true');
  assert.equal(document.quickAccess.getAttribute('aria-hidden'), 'true');
  assert.equal(document.contextRailFooter.getAttribute('aria-hidden'), 'true');

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
  assert.equal(document.contextRail.getAttribute('aria-hidden'), null);
  assert.equal(document.contextRailTop.getAttribute('aria-hidden'), null);
  assert.equal(document.vaultTabs.getAttribute('aria-hidden'), null);
  assert.equal(document.quickAccess.getAttribute('aria-hidden'), null);
  assert.equal(document.contextRailFooter.getAttribute('aria-hidden'), null);
});

test('modal manager validates a stored invoker at close after clearing modal inertness', () => {
  const cases = [
    ['hidden', (invoker) => { invoker.hidden = true; }],
    ['inert', (invoker) => { invoker.inert = true; }],
    ['inside an inert ancestor', (invoker) => {
      invoker.parentElement = { inert: true, parentElement: null, getAttribute: () => null };
    }],
    ['disconnected', (invoker) => { invoker.isConnected = false; }],
    ['disabled', (invoker) => { invoker.disabled = true; }],
    ['not focusable', (invoker) => { invoker.tabIndex = -1; }],
  ];

  for (const [label, invalidate] of cases) {
    const document = modalDocument();
    const manager = createDialogManager(document);
    const invoker = new DialogElement(document);
    const fallback = new DialogElement(document);
    const initialFocus = new DialogElement(document);
    const sheet = new DialogElement(document);
    for (const element of [invoker, fallback, initialFocus]) element.hidden = false;
    fallback.onFocus = () => {
      assert.equal(document.main.getAttribute('aria-hidden'), null, `${label} fallback focuses after inert cleanup`);
    };

    manager.openModal(sheet, {
      initialFocus,
      invoker,
      restoreFallbacks: [fallback],
    });
    invalidate(invoker);
    manager.closeModal(sheet);
    assert.equal(document.activeElement, fallback, `${label} invoker restores the visible fallback`);
  }
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

test('modal manager skips CSS-hidden focusables', () => {
  const document = modalDocument();
  const manager = createDialogManager(document);
  const first = new DialogElement(document);
  const displayNone = new DialogElement(document);
  const visibilityHidden = new DialogElement(document);
  const last = new DialogElement(document);
  for (const element of [first, displayNone, visibilityHidden, last]) element.hidden = false;
  displayNone.style = { display: 'none' };
  visibilityHidden.style = { visibility: 'hidden' };
  const sheet = new DialogElement(document);
  sheet.focusables = [displayNone, visibilityHidden, first, last];

  manager.openModal(sheet);
  assert.equal(document.activeElement, first);
  document.activeElement = last;
  document.listeners.get('keydown')({ key: 'Tab', preventDefault() {} });
  assert.equal(document.activeElement, first);
});
