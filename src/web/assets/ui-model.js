'use strict';
(function expose(root, factory) {
  const model = factory();
  if (typeof module === 'object' && module.exports) module.exports = model;
  else root.XvUiModel = model;
}(typeof globalThis === 'undefined' ? this : globalThis, () => {
  const PROTECTED_MASK = '***************';
  const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

  function formatDate(value) {
    if (!value) return '';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? String(value) : date.toISOString().slice(0, 10);
  }
  function expirationDate(value) {
    return typeof value === 'string' && value.length >= 10 ? value.slice(0, 10) : '';
  }
  function createProtectedState(value = null, hasStoredValue = value !== null) {
    return { value, hasStoredValue, masked: hasStoredValue, dirty: false, revision: 0, loadPromise: null };
  }
  function protectedDisplay(state) { return state.masked ? PROTECTED_MASK : (state.value ?? ''); }
  function revealProtected(state, loaded = state.value) {
    state.revision++; state.value = loaded ?? ''; state.hasStoredValue = true; state.masked = false; return state;
  }
  function editProtected(state, value) {
    state.revision++; state.value = value; state.hasStoredValue = true; state.dirty = true; return state;
  }
  function hideProtected(state) { state.revision++; if (state.hasStoredValue) state.masked = true; return state; }
  function loadProtected(state, loader) {
    if (state.value !== null) return Promise.resolve(state.value);
    if (state.loadPromise) return state.loadPromise;
    const revision = state.revision;
    let request;
    try { request = Promise.resolve(loader()); }
    catch (error) { request = Promise.reject(error); }
    let pending = request.then((loaded) => {
      if (state.revision === revision && state.value === null) state.value = loaded ?? '';
      return state.value;
    });
    pending = pending.finally(() => {
      if (state.loadPromise === pending) state.loadPromise = null;
    });
    state.loadPromise = pending;
    return pending;
  }

  function comparable(value, type) {
    if (type === 'number') return typeof value === 'number' && Number.isFinite(value) ? value : null;
    if (type === 'date') {
      if (!value) return null;
      const timestamp = new Date(value).getTime();
      return Number.isNaN(timestamp) ? null : timestamp;
    }
    return value === null || value === undefined || value === '' ? null : String(value);
  }
  function compareValues(left, right, type, direction) {
    const a = comparable(left, type); const b = comparable(right, type);
    if (a === null && b === null) return 0;
    if (a === null) return 1;
    if (b === null) return -1;
    const multiplier = direction === 'desc' ? -1 : 1;
    if (type === 'text') return collator.compare(a, b) * multiplier;
    return a === b ? 0 : (a < b ? -1 : 1) * multiplier;
  }
  function sortedCopy(items, valueOf, nameOf, type = 'text', direction = 'asc') {
    return [...items].sort((left, right) => {
      const primary = compareValues(valueOf(left), valueOf(right), type, direction);
      return primary || collator.compare(String(nameOf(left)), String(nameOf(right)));
    });
  }
  function normalizeWidths(serialized, defaults, minimums) {
    let widths;
    try { widths = JSON.parse(serialized); } catch (_) { return [...defaults]; }
    const valid = Array.isArray(widths) && widths.length === defaults.length
      && widths.every((width, i) => Number.isFinite(width) && width >= minimums[i])
      && Math.abs(widths.reduce((sum, width) => sum + width, 0) - 100) < 0.1;
    return valid ? widths : [...defaults];
  }
  return { PROTECTED_MASK, formatDate, expirationDate, createProtectedState,
    protectedDisplay, revealProtected, editProtected, hideProtected, loadProtected,
    sortedCopy, normalizeWidths };
}));
