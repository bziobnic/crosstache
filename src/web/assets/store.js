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

const OPERATION_STATUSES = new Set([
  'started',
  'succeeded',
  'partially-succeeded',
  'cancelled',
  'failed',
]);

export function operationEvent(operationId, status, diagnostic) {
  if (!OPERATION_STATUSES.has(status)) throw new TypeError(`Unsupported operation status: ${status}`);
  return {
    type: 'operation/status',
    operationId,
    status,
    ...(diagnostic ? { diagnostic: safeDiagnostic(diagnostic) } : {}),
  };
}

export function operationResultStatus(results) {
  const succeeded = results.filter(({ ok }) => ok).length;
  if (succeeded === results.length) return 'succeeded';
  return succeeded === 0 ? 'failed' : 'partially-succeeded';
}

export function safeDiagnostic(input = {}) {
  const diagnostic = {
    code: typeof input.code === 'string' ? input.code : 'xv-request-failed',
    message: typeof input.message === 'string' ? input.message : 'The request could not be completed.',
    hint: typeof input.hint === 'string' ? input.hint : '',
    backend: typeof input.backend === 'string' ? input.backend : '',
    vault: typeof input.vault === 'string' ? input.vault : '',
    failedNames: Array.isArray(input.failedNames)
      ? input.failedNames.filter((name) => typeof name === 'string')
      : [],
  };
  return diagnostic;
}

export function draftReducer(state, event) {
  switch (event.type) {
    case 'operation/status':
      return {
        ...state,
        operations: {
          ...(state.operations || {}),
          [event.operationId]: {
            status: event.status,
            ...(event.diagnostic ? { diagnostic: safeDiagnostic(event.diagnostic) } : {}),
          },
        },
      };
    case 'context/loaded':
      return { ...state, context: structuredClone(event.context), contextError: null };
    case 'context/load-failed':
      return { ...state, contextError: structuredClone(event.error) };
    case 'context/switch-started':
      return { ...state, contextSwitchPending: true, contextError: null };
    case 'context/switch-succeeded':
      return {
        ...state,
        context: structuredClone(event.context),
        initialSecrets: structuredClone(event.secrets),
        contextSwitchPending: false,
        contextError: null,
      };
    case 'context/switch-failed':
      return {
        ...state,
        contextSwitchPending: false,
        contextError: structuredClone(event.error),
      };
    case 'context/switch-cancelled':
      return { ...state, contextSwitchPending: false };
    case 'mutation/pending':
      return { ...state, scopedMutationPending: Boolean(event.value) };
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
