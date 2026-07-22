import { isDraftDirty } from './store.js';
import { setBackgroundInert as updateBackgroundInert } from './accessibility.js';

const focusableSelector = [
  'button:not([disabled]):not([hidden])',
  'input:not([disabled]):not([hidden])',
  'select:not([disabled]):not([hidden])',
  'textarea:not([disabled]):not([hidden])',
  '[tabindex]:not([tabindex="-1"]):not([hidden])',
].join(', ');

function focusableElements(element) {
  return [...element.querySelectorAll(focusableSelector)]
    .filter((candidate) => {
      if (candidate.hidden || candidate.disabled || candidate.closest?.('[hidden]')) return false;
      const style = candidate.ownerDocument?.defaultView?.getComputedStyle?.(candidate) ?? candidate.style;
      if (style?.display === 'none' || style?.visibility === 'hidden' || style?.visibility === 'collapse') return false;
      return typeof candidate.getClientRects !== 'function' || candidate.getClientRects().length > 0;
    });
}

function setModalInert(element, active) {
  const supportsInert = typeof HTMLElement !== 'undefined' && 'inert' in HTMLElement.prototype;
  if (supportsInert) {
    element.inert = active;
  } else if (active) {
    element.setAttribute('aria-hidden', 'true');
  } else {
    element.removeAttribute('aria-hidden');
  }
}

export function createDialogManager(document) {
  const modals = [];

  function topModal() {
    return modals.at(-1)?.element ?? null;
  }

  function setBackgroundInert(active) {
    updateBackgroundInert(document, active);
  }

  function updateModalLayers() {
    const active = topModal();
    for (const modal of modals) setModalInert(modal.element, modal.element !== active);
  }

  function openModal(element, { initialFocus, invoker, onEscape } = {}) {
    const existing = modals.findIndex((modal) => modal.element === element);
    if (existing !== -1) modals.splice(existing, 1);
    modals.push({ element, invoker, onEscape });
    element.hidden = false;
    updateModalLayers();
    setBackgroundInert(true);
    (initialFocus ?? focusableElements(element)[0] ?? element).focus?.();
  }

  function closeModal(element) {
    const index = modals.findIndex((modal) => modal.element === element);
    if (index === -1) return;
    const [{ invoker }] = modals.splice(index, 1);
    element.hidden = true;
    updateModalLayers();
    setBackgroundInert(modals.length > 0);
    invoker?.focus?.();
  }

  document.addEventListener?.('keydown', (event) => {
    const modal = modals.at(-1);
    if (!modal) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      modal.onEscape?.();
      return;
    }
    if (event.key !== 'Tab') return;
    const focusable = focusableElements(modal.element);
    if (!focusable.length) {
      event.preventDefault();
      modal.element.focus?.();
      return;
    }
    const first = focusable[0];
    const last = focusable.at(-1);
    if ((!event.shiftKey && document.activeElement === last) || (event.shiftKey && document.activeElement === first)) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus?.();
    }
  });

  function confirmDiscard() {
    const dialog = document.getElementById('discard-confirmation');
    const keepEditing = document.getElementById('keep-editing');
    const discard = document.getElementById('discard-changes');
    return new Promise((resolve) => {
      const finish = (confirmed) => {
        keepEditing.onclick = null;
        discard.onclick = null;
        closeModal(dialog);
        resolve(confirmed);
      };
      keepEditing.onclick = () => finish(false);
      discard.onclick = () => finish(true);
      openModal(dialog, {
        initialFocus: keepEditing,
        invoker: document.activeElement,
        onEscape: () => finish(false),
      });
    });
  }

  return {
    openModal,
    closeModal,
    topModal,
    setBackgroundInert,
    confirmDiscard,
    show(selector) { document.querySelector(selector).hidden = false; },
    hide(selector) { document.querySelector(selector).hidden = true; },
  };
}

export function guardNavigation({ draft, savePending, confirmDiscard }) {
  if (savePending) return false;
  if (!isDraftDirty(draft)) return true;
  return confirmDiscard();
}
