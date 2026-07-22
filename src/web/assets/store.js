export function createStore(initialState, reducer) {
  let state = structuredClone(initialState);
  const listeners = new Set();
  return Object.freeze({
    snapshot: () => structuredClone(state),
    subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
    dispatch(event) {
      state = reducer(state, Object.freeze({ ...event }));
      const snapshot = structuredClone(state);
      for (const listener of listeners) listener(snapshot, event);
      return snapshot;
    },
  });
}

export function normalizeSecretDraft(input) {
  return Object.fromEntries(Object.entries(input || {}).map(([key, value]) => {
    if (key === 'name' && typeof value === 'string') return [key, value.trim()];
    return [key, value === undefined ? null : structuredClone(value)];
  }));
}

export function isDraftDirty(draft) {
  if (!draft) return false;
  return JSON.stringify(draft.baseline) !== JSON.stringify(draft.working);
}

export function draftReducer(state, event) {
  switch (event.type) {
    case 'draft/open': {
      const baseline = normalizeSecretDraft(event.draft);
      return { ...state, draft: { baseline, working: structuredClone(baseline) }, savePending: false };
    }
    case 'draft/change':
      return state.draft
        ? { ...state, draft: { ...state.draft, working: normalizeSecretDraft(event.draft) } }
        : state;
    case 'draft/save-pending':
      return { ...state, savePending: Boolean(event.value) };
    case 'draft/close':
      return { ...state, draft: null, savePending: false };
    default:
      return state;
  }
}
