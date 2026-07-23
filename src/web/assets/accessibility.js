export function announce(document, message, priority = 'polite') {
  const liveRegion = document.getElementById('toast');
  if (!liveRegion) return;
  liveRegion.setAttribute('aria-live', priority);
  liveRegion.textContent = message;
}

export function setProtectedValueStatus(document, message) {
  const liveRegion = document.getElementById('protected-value-status');
  if (!liveRegion) return;
  liveRegion.textContent = message;
  liveRegion.hidden = !message;
}

export function setBackgroundInert(document, active) {
  const supportsInert = typeof HTMLElement !== 'undefined' && 'inert' in HTMLElement.prototype;
  for (const element of document.querySelectorAll('header, main, #context-rail')) {
    if (supportsInert) {
      element.inert = active;
    } else if (active) {
      element.setAttribute('aria-hidden', 'true');
    } else {
      element.removeAttribute('aria-hidden');
    }
  }
}
