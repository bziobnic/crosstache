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
const TERMINAL_OPERATION_STATUSES = new Set([
  'succeeded',
  'partially-succeeded',
  'cancelled',
  'failed',
]);
export const MAX_ROUTINE_OPERATION_HISTORY = 50;
export const MAX_OPERATION_TOMBSTONES = 100;

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

export function createOwnerRegistry() {
  const entries = new Map();
  const generations = new Map();

  function cleanup(entry) {
    try { entry?.cleanup?.(); } catch (_) { /* cleanup is best effort */ }
  }

  return Object.freeze({
    replace(key, { retained = null, cleanup: release } = {}) {
      cleanup(entries.get(key));
      const generation = (generations.get(key) || 0) + 1;
      generations.set(key, generation);
      entries.set(key, { generation, retained, cleanup: release });
      return generation;
    },
    clear(key, expectedGeneration) {
      const entry = entries.get(key);
      if (!entry || (expectedGeneration !== undefined && entry.generation !== expectedGeneration)) {
        return false;
      }
      cleanup(entry);
      entries.delete(key);
      generations.set(key, entry.generation + 1);
      return true;
    },
    isCurrent(key, generation) {
      return entries.get(key)?.generation === generation;
    },
    has(key) {
      return entries.has(key);
    },
  });
}

export function bindOwnedRetry({
  registry,
  key,
  generation,
  button,
  retry,
  publish = () => {},
  reject = () => {},
}) {
  button.disabled = false;
  button.onclick = async () => {
    if (button.disabled || !registry.isCurrent(key, generation)) return false;
    button.disabled = true;
    try {
      const result = await retry();
      if (!registry.isCurrent(key, generation)) return false;
      await publish(result);
      return true;
    } catch (error) {
      if (!registry.isCurrent(key, generation)) return false;
      await reject(error);
      return false;
    } finally {
      if (registry.isCurrent(key, generation)) button.disabled = false;
    }
  };
}

function boundedOperations(operations) {
  const routineTerminals = Object.entries(operations)
    .filter(([, operation]) => (
      !operation.durable && TERMINAL_OPERATION_STATUSES.has(operation.status)
    ))
    .sort(([, left], [, right]) => left.revision - right.revision);
  const excess = routineTerminals.length - MAX_ROUTINE_OPERATION_HISTORY;
  if (excess <= 0) return operations;
  const bounded = { ...operations };
  for (const [operationId] of routineTerminals.slice(0, excess)) delete bounded[operationId];
  return bounded;
}

function boundedTerminals(terminals) {
  const entries = Object.entries(terminals)
    .sort(([, leftRevision], [, rightRevision]) => leftRevision - rightRevision);
  const excess = entries.length - MAX_OPERATION_TOMBSTONES;
  if (excess <= 0) return terminals;
  const bounded = { ...terminals };
  for (const [operationId] of entries.slice(0, excess)) delete bounded[operationId];
  return bounded;
}

export function draftReducer(state, event) {
  switch (event.type) {
    case 'operation/status': {
      const existing = state.operations?.[event.operationId];
      if (state.operationTerminals?.[event.operationId]
        || (existing && TERMINAL_OPERATION_STATUSES.has(existing.status))) {
        return state;
      }
      const revision = (state.operationRevision || 0) + 1;
      const terminal = TERMINAL_OPERATION_STATUSES.has(event.status);
      const operations = boundedOperations({
        ...(state.operations || {}),
        [event.operationId]: {
          status: event.status,
          revision,
          durable: Boolean(event.durable),
          ...(event.diagnostic ? { diagnostic: safeDiagnostic(event.diagnostic) } : {}),
        },
      });
      return {
        ...state,
        operationRevision: revision,
        operationTerminals: terminal
          ? boundedTerminals({
            ...(state.operationTerminals || {}),
            [event.operationId]: revision,
          })
          : state.operationTerminals,
        operations,
      };
    }
    case 'operation/dismiss': {
      if (!state.operations?.[event.operationId]) return state;
      const operations = { ...state.operations };
      delete operations[event.operationId];
      return { ...state, operations };
    }
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
