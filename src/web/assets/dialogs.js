export function createDialogManager(document) {
  return {
    show(selector) { document.querySelector(selector).hidden = false; },
    hide(selector) { document.querySelector(selector).hidden = true; },
  };
}
