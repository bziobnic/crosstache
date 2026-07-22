import { isDraftDirty } from './store.js';

export function createDialogManager(document) {
  return {
    show(selector) { document.querySelector(selector).hidden = false; },
    hide(selector) { document.querySelector(selector).hidden = true; },
    confirmDiscard() { return globalThis.confirm('Discard unsaved changes?'); },
  };
}

export function guardNavigation({ draft, savePending, confirmDiscard }) {
  if (savePending) return false;
  if (!isDraftDirty(draft)) return true;
  return confirmDiscard();
}
