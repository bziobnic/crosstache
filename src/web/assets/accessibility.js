export function announce(document, message, priority = 'polite') {
  const liveRegion = document.getElementById('toast');
  if (!liveRegion) return;
  liveRegion.setAttribute('aria-live', priority);
  liveRegion.textContent = message;
}
