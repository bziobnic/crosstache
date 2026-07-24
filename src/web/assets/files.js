import { activeFilterChips } from './ui-model.js';

export const UPLOAD_STATES = Object.freeze([
  'queued',
  'preflighting',
  'awaiting-conflict',
  'uploading',
  'finishing',
  'reconciling',
  'completed',
  'failed',
  'cancelled',
  'ambiguous',
]);

const UPLOAD_TRANSITIONS = Object.freeze({
  queued: Object.freeze({
    'preflight-started': 'preflighting',
    'transfer-started': 'uploading',
    cancel: 'cancelled',
  }),
  preflighting: Object.freeze({
    'preflight-ready': 'queued',
    conflict: 'awaiting-conflict',
    failed: 'failed',
    cancel: 'cancelled',
  }),
  'awaiting-conflict': Object.freeze({
    'decision-upload': 'queued',
    skipped: 'completed',
    failed: 'failed',
    cancel: 'cancelled',
    'preference-changed': 'awaiting-conflict',
  }),
  uploading: Object.freeze({
    progress: 'uploading',
    'bytes-sent': 'finishing',
    'server-confirmed': 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
    uncertain: 'reconciling',
    conflict: 'awaiting-conflict',
  }),
  finishing: Object.freeze({
    progress: 'finishing',
    'server-confirmed': 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
    uncertain: 'reconciling',
    conflict: 'awaiting-conflict',
  }),
  reconciling: Object.freeze({
    'evidence-present': 'ambiguous',
    'evidence-missing': 'cancelled',
    'evidence-unavailable': 'ambiguous',
  }),
  completed: Object.freeze({}),
  failed: Object.freeze({ retry: 'queued' }),
  cancelled: Object.freeze({ retry: 'queued' }),
  ambiguous: Object.freeze({
    retry: 'queued',
    'server-confirmed': 'completed',
  }),
});

export function nextUploadState(current, event) {
  const next = UPLOAD_TRANSITIONS[current]?.[event?.type];
  if (!next) throw new TypeError(`Invalid upload transition: ${current} -> ${event?.type || 'unknown'}`);
  return next;
}

const ACTIVE_UPLOAD_STATES = new Set(['uploading', 'finishing', 'reconciling']);
const RETRYABLE_UPLOAD_STATES = new Set(['failed', 'cancelled', 'ambiguous']);

export function createUploadQueue(entries, { maxConcurrent = 1 } = {}) {
  const limit = Math.max(1, Math.floor(Number(maxConcurrent) || 1));
  const items = entries.map((entry) => ({
    ...entry,
    state: entry.state || 'queued',
    loaded: 0,
    total: Number(entry.size) || 0,
    error: null,
    evidence: '',
    policy: null,
    target: null,
    applyAll: false,
    preflightComplete: false,
  }));
  const byId = new Map(items.map((item) => [item.id, item]));

  function item(id) {
    const found = byId.get(id);
    if (!found) throw new TypeError(`Unknown upload: ${id}`);
    return found;
  }

  function activeCount() {
    return items.filter(({ state }) => ACTIVE_UPLOAD_STATES.has(state)).length;
  }

  return Object.freeze({
    items: () => items.map((entry) => ({ ...entry })),
    get: (id) => ({ ...item(id) }),
    event(id, event, patch = {}) {
      const entry = item(id);
      const state = nextUploadState(entry.state, event);
      const lifecycle = {};
      if (event.type === 'preflight-ready' || event.type === 'decision-upload') {
        lifecycle.preflightComplete = true;
      } else if (event.type === 'preflight-started' || event.type === 'retry') {
        lifecycle.preflightComplete = false;
      }
      if (event.type === 'decision-upload' || event.type === 'skipped') {
        Object.assign(lifecycle, {
          loaded: 0,
          error: null,
          evidence: '',
          result: null,
        });
      }
      Object.assign(entry, lifecycle, patch, { state });
      return { ...entry };
    },
    retryable: () => items.filter(({ state }) => RETRYABLE_UPLOAD_STATES.has(state))
      .map((entry) => ({ ...entry })),
    retry(ids = null) {
      const selected = ids ? new Set(ids) : null;
      const retried = [];
      for (const entry of items) {
        if (RETRYABLE_UPLOAD_STATES.has(entry.state) && (!selected || selected.has(entry.id))) {
          this.event(entry.id, { type: 'retry' }, {
            loaded: 0, error: null, evidence: '', policy: null, target: null, applyAll: false,
          });
          retried.push({ ...entry });
        }
      }
      return retried;
    },
    claimReady() {
      const available = Math.max(0, limit - activeCount());
      const claimed = items
        .filter(({ state, preflightComplete }) => state === 'queued' && preflightComplete)
        .slice(0, available);
      for (const entry of claimed) this.event(entry.id, { type: 'transfer-started' });
      return claimed.map((entry) => ({ ...entry }));
    },
    maxConcurrent: limit,
  });
}

function isSafeLogicalName(name) {
  const components = typeof name === 'string' ? name.split('/') : [];
  return Boolean(name)
    && !name.startsWith('/')
    && !name.includes('\\')
    && !name.includes('\0')
    && components.every((component) => component && component !== '.' && component !== '..');
}

export function validatePreflightResults(candidates, results) {
  if (!Array.isArray(results)) throw new TypeError('Preflight must return exactly one result per candidate.');
  const expected = new Set(candidates.map(({ id }) => id));
  const seen = new Set();
  const allowed = new Set(['ready', 'conflict', 'too-large', 'unsupported']);
  for (const result of results) {
    if (!result || typeof result.client_id !== 'string' || !expected.has(result.client_id)) {
      throw new TypeError('Preflight returned an unknown candidate.');
    }
    if (seen.has(result.client_id)) {
      throw new TypeError('Preflight must return exactly one result per candidate.');
    }
    if (!allowed.has(result.status)) throw new TypeError('Preflight returned an unknown status.');
    if (result.status === 'conflict' && !isSafeLogicalName(result.suggested_name)) {
      throw new TypeError('Preflight conflict has an unsafe rename suggestion.');
    }
    seen.add(result.client_id);
  }
  if (seen.size !== expected.size) {
    throw new TypeError('Preflight must return exactly one result per candidate.');
  }
  return candidates.map(({ id }) => results.find(({ client_id }) => client_id === id));
}

export function validateUploadConfirmation(response, { expectedName, policy }) {
  const matchesName = response
    && typeof response === 'object'
    && response.name === expectedName;
  const matchesPolicy = policy === 'skip'
    ? response?.status === undefined || response?.status === 'skipped'
    : response?.status !== 'skipped';
  if (!matchesName || !matchesPolicy) {
    throw Object.assign(
      new Error('The server confirmation did not match the requested upload target.'),
      { name: 'AmbiguousUploadError', ambiguous: true },
    );
  }
  return response;
}

export function uploadConflictDecision({ policy, suggestedName = null, allowReplace = true }) {
  if (!policy) return null;
  if (!['skip', 'replace', 'rename'].includes(policy)) throw new TypeError('Unknown conflict policy');
  if (policy === 'replace' && !allowReplace) throw new TypeError('Replace is unsupported by this backend');
  if (policy === 'rename') {
    if (!isSafeLogicalName(suggestedName)) {
      throw new TypeError('Rename requires a safe suggested name');
    }
  }
  return { policy, target: policy === 'rename' ? suggestedName : null };
}

export function uploadEvidenceState({ before, after, expectedSize }) {
  if (!after) {
    return { state: 'cancelled', evidence: 'Server metadata confirms no destination file.' };
  }
  if (!before && Number(after.size) === Number(expectedSize)) {
    return { state: 'ambiguous', evidence: 'The destination now exists, but this upload could not be confirmed.' };
  }
  return { state: 'ambiguous', evidence: 'The file exists, but this upload could not be confirmed.' };
}

const UPLOAD_STATUS_LABELS = Object.freeze({
  queued: 'Queued',
  preflighting: 'Checking destination…',
  'awaiting-conflict': 'Needs a conflict decision',
  uploading: 'Uploading…',
  finishing: 'Finishing…',
  reconciling: 'Reconciling server evidence…',
  completed: 'Completed',
  failed: 'Failed',
  cancelled: 'Cancelled',
  ambiguous: 'Completion could not be confirmed',
});

function fullUploadName(destination, name) {
  const folder = String(destination || '').replace(/\/+$/, '');
  return folder ? `${folder}/${name}` : name;
}

function appendQuery(path, query) {
  return `${path}${query ? `${path.includes('?') ? '&' : '?'}${query.replace(/^\?/, '')}` : ''}`;
}

function safeUploadError(error) {
  if (error?.code === 'xv-file-conflict') return 'The destination changed after preflight.';
  if (error?.status === 413) return 'The file exceeds the 100 MB upload limit.';
  return 'The upload could not be completed.';
}

export function mountUploadQueue({
  document,
  api,
  getContext,
  scopeQuery,
  formatScope,
  refreshFiles,
  getFiles,
  setPending = () => {},
  isScopeCurrent = () => true,
  dismissOperation = () => {},
}) {
  if (typeof document?.createElement !== 'function') {
    return Object.freeze({
      start: async () => false,
      refreshContext() {},
      updateDestinations() {},
      hasPending: () => false,
    });
  }
  const byId = (id) => document.getElementById(id);
  const queueSurface = byId('upload-queue');
  const queueItems = byId('upload-queue-items');
  const summary = byId('upload-summary');
  const summaryItems = byId('upload-summary-items');
  const retry = byId('retry-uploads');
  const destination = byId('upload-destination');
  const concurrencyCopy = byId('upload-concurrency');
  const newBatchMessage = byId('upload-new-batch-message');
  let batch = null;
  let nextId = 0;
  let scopeGeneration = 0;
  const ownedOperationIds = new Set();

  function currentLimit() {
    return Math.max(1, Math.floor(Number(getContext()?.transfers?.max_concurrent_uploads) || 1));
  }

  function syncContextCopy() {
    const limit = currentLimit();
    concurrencyCopy.textContent = `up to ${limit} ${limit === 1 ? 'upload' : 'uploads'} at a time`;
  }

  function activeBatch() {
    return batch && batch.queue.items().some(({ state }) => (
      !['completed', 'failed', 'cancelled', 'ambiguous'].includes(state)
    ));
  }

  function exactBatchCurrent(targetBatch = batch) {
    return targetBatch
      && batch === targetBatch
      && targetBatch.scopeGeneration === scopeGeneration
      && isScopeCurrent(targetBatch.scope);
  }

  function clearOwnedState({ clearSummary = true } = {}) {
    if (batch) {
      for (const controller of batch.controllers.values()) controller.abort();
      if (!batch.released) setPending(false);
    }
    for (const operationId of ownedOperationIds) dismissOperation(operationId);
    ownedOperationIds.clear();
    batch = null;
    queueItems.replaceChildren();
    queueSurface.hidden = true;
    retry.hidden = true;
    destination.disabled = false;
    newBatchMessage.hidden = true;
    if (clearSummary) {
      summaryItems.replaceChildren();
      summary.hidden = true;
    }
  }

  function updateDestinationOptions(files) {
    const selected = destination.value;
    const folders = [...new Set((files || []).map(({ name }) => (
      String(name || '').includes('/') ? String(name).split('/').slice(0, -1).join('/') : ''
    )).filter(Boolean))].sort((a, b) => a.localeCompare(b));
    destination.replaceChildren(...[
      ['', 'Vault root'],
      ...folders.map((folder) => [folder, folder]),
    ].map(([value, label]) => {
      const option = document.createElement('option');
      option.value = value;
      option.textContent = label;
      return option;
    }));
    if ([...destination.children].some((option) => option.value === selected)) destination.value = selected;
  }

  function button(label, handler, className = 'button secondary compact') {
    const control = document.createElement('button');
    control.type = 'button';
    control.className = className;
    control.textContent = label;
    control.onclick = handler;
    return control;
  }

  function applyConflictPolicy(targetBatch, targetItem, policy, { retrySkip = false } = {}) {
    let decision;
    try {
      decision = uploadConflictDecision({
        policy,
        suggestedName: targetItem.suggestedName,
        allowReplace: true,
      });
    } catch {
      targetBatch.queue.event(targetItem.id, { type: 'failed' }, {
        error: 'The server did not provide a safe conflict target.',
      });
      return false;
    }
    if (decision.policy === 'skip' && !retrySkip) {
      targetBatch.queue.event(targetItem.id, { type: 'skipped' }, {
        policy: 'skip',
        result: 'Skipped because a destination file already exists.',
        file: null,
      });
    } else {
      targetBatch.queue.event(targetItem.id, { type: 'decision-upload' }, decision);
    }
    return true;
  }

  function resolveConflict(id, policy, applyAll) {
    const targetBatch = batch;
    if (!exactBatchCurrent(targetBatch)) return;
    const source = targetBatch.queue.get(id);
    let decision;
    try {
      decision = uploadConflictDecision({
        policy,
        suggestedName: source.suggestedName,
        allowReplace: true,
      });
    } catch {
      applyConflictPolicy(targetBatch, source, policy);
      render();
      schedule(targetBatch);
      return null;
    }
    if (applyAll) targetBatch.applyAllPolicy = policy;
    const targets = applyAll
      ? targetBatch.queue.items().filter(({ state }) => state === 'awaiting-conflict')
      : [source];
    for (const targetItem of targets) {
      applyConflictPolicy(targetBatch, targetItem, policy);
    }
    render();
    schedule(targetBatch);
    return decision;
  }

  function retryItems(ids) {
    const targetBatch = batch;
    if (!exactBatchCurrent(targetBatch)) return;
    if (targetBatch.released) {
      targetBatch.released = false;
      destination.disabled = true;
      setPending(true);
      summary.hidden = true;
      summaryItems.replaceChildren();
    }
    const retried = targetBatch.queue.retry(ids);
    if (retried.length) targetBatch.evidenceWave = null;
    for (const entry of retried) targetBatch.queue.event(entry.id, { type: 'preflight-started' });
    render();
    if (retried.length) void preflight(targetBatch);
  }

  function cancelItem(id) {
    const targetBatch = batch;
    if (!exactBatchCurrent(targetBatch)) return;
    const entry = targetBatch.queue.get(id);
    if (['uploading', 'finishing'].includes(entry.state)) {
      targetBatch.controllers.get(id)?.abort();
      return;
    }
    if (['queued', 'preflighting', 'awaiting-conflict'].includes(entry.state)) {
      targetBatch.queue.event(id, { type: 'cancel' });
      render();
      schedule(targetBatch);
    }
  }

  function createRow(id, targetBatch) {
    const binding = Object.freeze({ id });
    const row = document.createElement('li');
    row.className = 'upload-item';
    row.dataset.uploadId = id;
    const name = document.createElement('strong');
    name.className = 'upload-item-name';
    const status = document.createElement('span');
    status.className = 'upload-item-status';
    const progress = document.createElement('progress');
    const fieldset = document.createElement('fieldset');
    fieldset.className = 'upload-conflict-actions';
    const legend = document.createElement('legend');
    const applyLabel = document.createElement('label');
    const apply = document.createElement('input');
    apply.type = 'checkbox';
    apply.onchange = () => {
      if (!exactBatchCurrent(targetBatch)) return;
      const current = targetBatch.queue.get(id);
      if (current.state !== 'awaiting-conflict') return;
      targetBatch.queue.event(id, { type: 'preference-changed' }, {
        applyAll: apply.checked,
      });
    };
    applyLabel.append(apply, ' Apply to all remaining conflicts');
    fieldset.append(
      legend,
      button('Skip', () => resolveConflict(
        id,
        'skip',
        targetBatch.queue.get(id).applyAll,
      )),
      button('Replace', () => resolveConflict(
        id,
        'replace',
        targetBatch.queue.get(id).applyAll,
      )),
      button('Rename', () => resolveConflict(
        id,
        'rename',
        targetBatch.queue.get(id).applyAll,
      )),
      applyLabel,
    );
    const detail = document.createElement('small');
    detail.className = 'upload-item-evidence';
    const cancelAction = document.createElement('div');
    cancelAction.className = 'upload-item-actions';
    const cancel = button(
      'Cancel upload',
      () => cancelItem(id),
      'button ghost compact',
    );
    cancelAction.appendChild(cancel);
    const retryAction = document.createElement('div');
    retryAction.className = 'upload-item-actions';
    const retryItem = button(
      'Retry upload',
      () => retryItems([id]),
      'button secondary compact',
    );
    retryAction.appendChild(retryItem);
    row.append(name, status, progress, fieldset, detail, cancelAction, retryAction);
    targetBatch.rows.set(id, {
      binding,
      row,
      name,
      status,
      progress,
      fieldset,
      legend,
      apply,
      detail,
      cancelAction,
      cancel,
      retryAction,
      retryItem,
    });
    queueItems.appendChild(row);
    return targetBatch.rows.get(id);
  }

  function render() {
    const targetBatch = batch;
    if (!exactBatchCurrent(targetBatch)) return;
    const entries = targetBatch.queue.items();
    queueSurface.hidden = false;
    byId('upload-queue-context').textContent = `${formatScope(targetBatch.scope)} · destination: ${targetBatch.destination || 'Vault root'}`;
    for (const entry of entries) {
      const controls = targetBatch.rows.get(entry.id) || createRow(entry.id, targetBatch);
      controls.name.textContent = entry.displayName;
      controls.status.textContent = UPLOAD_STATUS_LABELS[entry.state];
      if (['finishing', 'reconciling', 'awaiting-conflict', 'failed', 'ambiguous'].includes(entry.state)) {
        controls.status.setAttribute('role', 'status');
        controls.status.setAttribute('aria-live', 'polite');
      } else {
        controls.status.removeAttribute('role');
        controls.status.removeAttribute('aria-live');
      }
      controls.progress.hidden = !['uploading', 'finishing'].includes(entry.state);
      controls.progress.max = entry.total || entry.size || 1;
      if (entry.state === 'uploading') {
        controls.progress.value = entry.loaded || 0;
      } else {
        controls.progress.removeAttribute('value');
      }
      controls.progress.setAttribute('aria-label', `Upload progress for ${entry.name}`);
      controls.fieldset.hidden = entry.state !== 'awaiting-conflict';
      controls.legend.textContent = `Resolve conflict for ${entry.name}`;
      controls.apply.checked = entry.applyAll;
      const detail = entry.evidence || entry.error || entry.result;
      controls.detail.hidden = !detail;
      controls.detail.textContent = detail || '';
      controls.cancelAction.hidden = ![
        'queued', 'preflighting', 'awaiting-conflict', 'uploading', 'finishing',
      ].includes(entry.state);
      controls.cancel.textContent = `Cancel ${entry.name}`;
      controls.retryAction.hidden = !RETRYABLE_UPLOAD_STATES.has(entry.state);
      controls.retryItem.textContent = `Retry ${entry.name}`;
    }
    retry.hidden = targetBatch.queue.retryable().length === 0;
    finishIfSettled(targetBatch);
  }

  function renderSummary(entries) {
    summaryItems.replaceChildren(...entries.map((entry) => {
      const item = document.createElement('li');
      item.textContent = `${entry.displayName}: ${UPLOAD_STATUS_LABELS[entry.state]}${entry.result ? ` — ${entry.result}` : ''}${entry.evidence ? ` — ${entry.evidence}` : ''}`;
      return item;
    }));
    summary.hidden = false;
  }

  function finishIfSettled(targetBatch) {
    if (!exactBatchCurrent(targetBatch) || targetBatch.released) return;
    const entries = targetBatch.queue.items();
    if (entries.some(({ state }) => !['completed', 'failed', 'cancelled', 'ambiguous'].includes(state))) return;
    targetBatch.released = true;
    destination.disabled = false;
    setPending(false);
    renderSummary(entries);
    if (targetBatch.evidenceWave) {
      updateDestinationOptions(getFiles());
      return;
    }
    void refreshFiles(targetBatch.scope).then(() => {
      if (exactBatchCurrent(targetBatch)) updateDestinationOptions(getFiles());
    }).catch(() => {});
  }

  function reconcilingTargets(targetBatch) {
    return targetBatch.queue.items()
      .filter(({ state }) => state === 'reconciling')
      .map((entry) => ({
        id: entry.id,
        targetName: entry.target || entry.fullName,
        before: entry.before
          ? { name: entry.before.name, size: entry.before.size }
          : null,
        size: entry.size,
      }));
  }

  function acquireEvidenceWave(targetBatch) {
    if (!targetBatch.evidenceWave) {
      const wave = {
        generation: ++targetBatch.evidenceWaveGeneration,
        consumers: 0,
        targetNames: reconcilingTargets(targetBatch).map(({ targetName }) => targetName),
        metadataReferences: 0,
        promise: null,
      };
      wave.promise = (async () => {
        try {
          const refreshed = await refreshFiles(targetBatch.scope);
          if (!exactBatchCurrent(targetBatch) || refreshed !== true) {
            return { available: false, outcomes: new Map() };
          }
          const targets = reconcilingTargets(targetBatch);
          wave.targetNames = targets.map(({ targetName }) => targetName);
          const files = getFiles();
          const outcomes = new Map();
          for (const target of targets) {
            const current = files.find(({ name }) => name === target.targetName);
            const after = current ? { name: current.name, size: current.size } : null;
            outcomes.set(target.id, uploadEvidenceState({
              before: target.before,
              after,
              expectedSize: target.size,
            }));
          }
          return { available: true, outcomes };
        } catch {
          return { available: false, outcomes: new Map() };
        }
      })();
      targetBatch.evidenceWave = wave;
    }
    targetBatch.evidenceWave.consumers++;
    return targetBatch.evidenceWave;
  }

  function releaseEvidenceWave(targetBatch, wave, snapshot) {
    wave.consumers = Math.max(0, wave.consumers - 1);
    if (wave.consumers !== 0 || targetBatch.evidenceWave !== wave) return;
    snapshot?.outcomes?.clear();
    wave.targetNames = [];
    wave.promise = null;
    targetBatch.evidenceWave = null;
  }

  async function reconcileUncertain(targetBatch, id) {
    if (!exactBatchCurrent(targetBatch)) return;
    const wave = acquireEvidenceWave(targetBatch);
    let snapshot = null;
    try {
      snapshot = await wave.promise;
      if (!exactBatchCurrent(targetBatch)) return;
      if (targetBatch.queue.get(id).state !== 'reconciling') return;
      const evidence = snapshot.available ? snapshot.outcomes.get(id) : null;
      if (evidence) {
        targetBatch.queue.event(id, {
          type: evidence.state === 'cancelled' ? 'evidence-missing' : 'evidence-present',
        }, {
          evidence: evidence.evidence,
        });
      } else {
        targetBatch.queue.event(id, { type: 'evidence-unavailable' }, {
          evidence: 'The server metadata refresh was interrupted, so completion remains unconfirmed.',
        });
      }
      if (!exactBatchCurrent(targetBatch)) return;
      render();
      schedule(targetBatch);
    } finally {
      releaseEvidenceWave(targetBatch, wave, snapshot);
    }
  }

  async function transfer(targetBatch, entry) {
    if (!exactBatchCurrent(targetBatch)) return;
    const controller = new AbortController();
    targetBatch.controllers.set(entry.id, controller);
    const attempt = (entry.attempt || 0) + 1;
    targetBatch.queue.event(entry.id, { type: 'progress' }, { attempt });
    render();
    const form = new FormData();
    form.append('file', entry.file, entry.name);
    let path = appendQuery('/api/files', scopeQuery(targetBatch.scope));
    const uploadQuery = new URLSearchParams({ destination: targetBatch.destination });
    if (entry.policy) {
      uploadQuery.set('policy', entry.policy);
      if (entry.target) uploadQuery.set('target', entry.target);
    }
    path = appendQuery(path, uploadQuery.toString());
    try {
      const operationId = `upload-${entry.id}-${attempt}`;
      ownedOperationIds.add(operationId);
      const response = await api.upload({
        path,
        formData: form,
        signal: controller.signal,
        operationId,
        onProgress: ({ loaded, total, finishing }) => {
          if (!exactBatchCurrent(targetBatch)) return;
          const current = targetBatch.queue.get(entry.id);
          if (!['uploading', 'finishing'].includes(current.state)) return;
          const event = finishing && current.state === 'uploading'
            ? { type: 'bytes-sent' }
            : { type: 'progress' };
          targetBatch.queue.event(entry.id, event, { loaded, total });
          render();
        },
      });
      if (!exactBatchCurrent(targetBatch)) return;
      const confirmed = validateUploadConfirmation(response, {
        expectedName: entry.target || entry.fullName,
        policy: entry.policy,
      });
      targetBatch.queue.event(entry.id, { type: 'server-confirmed' }, {
        loaded: entry.size,
        total: entry.size,
        file: null,
        result: confirmed.status === 'skipped'
          ? 'Skipped because a destination file already exists.'
          : entry.policy === 'skip'
            ? 'Uploaded because the destination no longer existed.'
            : 'Server confirmed the upload.',
      });
      render();
      schedule(targetBatch);
    } catch (error) {
      if (!exactBatchCurrent(targetBatch)) return;
      if (error?.code === 'xv-file-conflict') {
        targetBatch.queue.event(entry.id, { type: 'conflict' }, {
          suggestedName: error.details?.suggested_name,
          error: 'The destination changed after preflight. Choose what to do.',
        });
        if (targetBatch.applyAllPolicy) {
          applyConflictPolicy(
            targetBatch,
            targetBatch.queue.get(entry.id),
            targetBatch.applyAllPolicy,
            { retrySkip: true },
          );
        }
        render();
        schedule(targetBatch);
      } else if (error?.name === 'AbortError' || error?.name === 'NetworkError' || error?.ambiguous) {
        targetBatch.queue.event(entry.id, { type: 'uncertain' }, {
          evidence: 'Refreshing server metadata to determine the outcome…',
        });
        render();
        await reconcileUncertain(targetBatch, entry.id);
      } else {
        targetBatch.queue.event(entry.id, { type: 'failed' }, { error: safeUploadError(error) });
        render();
        schedule(targetBatch);
      }
    } finally {
      if (
        exactBatchCurrent(targetBatch)
        && targetBatch.controllers.get(entry.id) === controller
      ) {
        targetBatch.controllers.delete(entry.id);
      }
    }
  }

  function schedule(targetBatch = batch) {
    if (!exactBatchCurrent(targetBatch)) return;
    for (const entry of targetBatch.queue.claimReady()) void transfer(targetBatch, entry);
    render();
  }

  async function preflight(targetBatch = batch) {
    if (!exactBatchCurrent(targetBatch)) return;
    const candidates = targetBatch.queue.items()
      .filter(({ state }) => state === 'preflighting');
    if (!candidates.length) {
      schedule(targetBatch);
      return;
    }
    try {
      const response = await api(
        'POST',
        appendQuery('/api/files/preflight', scopeQuery(targetBatch.scope)),
        {
          files: candidates.map((entry) => ({
            client_id: entry.id,
            name: entry.name,
            size: entry.size,
            content_type: entry.contentType,
            destination: targetBatch.destination,
          })),
        },
      );
      if (!exactBatchCurrent(targetBatch)) return;
      const results = validatePreflightResults(candidates, response?.results);
      for (const result of results) {
        if (targetBatch.queue.get(result.client_id).state !== 'preflighting') continue;
        if (result.status === 'ready') {
          targetBatch.queue.event(result.client_id, { type: 'preflight-ready' });
        } else if (result.status === 'conflict') {
          targetBatch.queue.event(result.client_id, { type: 'conflict' }, {
            suggestedName: result.suggested_name,
          });
          if (targetBatch.applyAllPolicy) {
            applyConflictPolicy(
              targetBatch,
              targetBatch.queue.get(result.client_id),
              targetBatch.applyAllPolicy,
            );
          }
        } else {
          targetBatch.queue.event(result.client_id, { type: 'failed' }, {
            error: result.status === 'too-large'
              ? `This file exceeds the ${Math.floor(result.max_bytes / 1024 / 1024)} MB limit.`
              : 'This backend cannot guarantee a conflict-safe upload.',
          });
        }
      }
      render();
      schedule(targetBatch);
    } catch {
      if (!exactBatchCurrent(targetBatch)) return;
      for (const entry of targetBatch.queue.items()) {
        if (entry.state === 'preflighting') {
          targetBatch.queue.event(entry.id, { type: 'failed' }, {
            error: 'The upload preflight response was invalid. Retry this item.',
          });
        }
      }
      render();
    }
  }

  async function start(fileList) {
    const files = [...(fileList || [])];
    if (!files.length) return false;
    if (batch) {
      if (batch.released) {
        newBatchMessage.hidden = false;
        newBatchMessage.focus();
      }
      return false;
    }
    newBatchMessage.hidden = true;
    const scope = structuredClone(getContext());
    const chosenDestination = destination.value;
    const existing = getFiles();
    const limit = currentLimit();
    const entries = files.map((file) => {
      const fullName = fullUploadName(chosenDestination, file.name);
      return {
        id: `file-${++nextId}`,
        file,
        name: file.name,
        displayName: fullName,
        fullName,
        size: file.size,
        contentType: file.type || 'application/octet-stream',
        before: existing.find(({ name }) => name === fullName) || null,
      };
    });
    batch = {
      queue: createUploadQueue(entries, { maxConcurrent: limit }),
      controllers: new Map(),
      rows: new Map(),
      destination: chosenDestination,
      scope,
      scopeGeneration,
      applyAllPolicy: null,
      evidenceWave: null,
      evidenceWaveGeneration: 0,
      released: false,
    };
    const targetBatch = batch;
    destination.disabled = true;
    setPending(true);
    for (const entry of entries) targetBatch.queue.event(entry.id, { type: 'preflight-started' });
    render();
    await preflight(targetBatch);
    return exactBatchCurrent(targetBatch);
  }

  retry.onclick = () => {
    if (!exactBatchCurrent(batch)) return;
    retryItems();
  };
  byId('dismiss-upload-summary').onclick = () => {
    clearOwnedState();
  };

  syncContextCopy();
  updateDestinationOptions(getFiles());
  return Object.freeze({
    start,
    refreshContext() {
      scopeGeneration++;
      clearOwnedState();
      syncContextCopy();
      updateDestinationOptions(getFiles());
    },
    updateDestinations: () => updateDestinationOptions(getFiles()),
    hasPending: activeBatch,
    debugRetained: () => ({
      hasBatch: Boolean(batch),
      fileReferences: batch?.queue.items().filter(({ file }) => file).length || 0,
      rowFileReferences: batch
        ? [...batch.rows.values()].filter(({ binding }) => 'file' in binding).length
        : 0,
      evidenceMetadataReferences: batch?.evidenceWave?.metadataReferences || 0,
      evidenceTargetNames: [...(batch?.evidenceWave?.targetNames || [])],
      names: batch?.queue.items().map(({ name }) => name) || [],
      controllers: batch?.controllers.size || 0,
      operationIds: ownedOperationIds.size,
    }),
  });
}

function inactiveValue(key) {
  return key === 'enabled' ? null : '';
}

export function syncFilterOptions(select, values, selected = select.value) {
  const first = select.options?.[0] || select.children?.[0] || null;
  const document = select.ownerDocument || select.document;
  const normalized = [...new Set(values.filter(Boolean))]
    .sort((left, right) => left.localeCompare(right, undefined, {
      sensitivity: 'base',
      numeric: true,
    }));
  const options = normalized.map((value) => {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = value;
    return option;
  });
  select.replaceChildren(...(first ? [first, ...options] : options));
  select.value = normalized.includes(selected) ? selected : '';
}

export function mountFilterControls({
  document,
  surface,
  filters,
  labels,
  keys,
  dynamicKeys = [],
  onChange,
  folderValue,
  clearFolder,
}) {
  const controls = new Map(keys.map((key) => [
    key,
    document.querySelector(`#${surface}-filter-${key}`),
  ]));
  const chips = document.querySelector(`#${surface}-filter-chips`);
  const clearAll = document.querySelector(`#${surface}-filters-clear`);
  const dynamic = new Set(dynamicKeys);
  const baseOptions = new Map(
    [...controls].map(([key, control]) => [key, [...(control.children || [])]]),
  );

  function readControl(key, control) {
    if (key !== 'enabled') return control.value;
    return control.value === '' ? null : control.value === 'true';
  }

  function render() {
    const values = { ...filters };
    const folder = folderValue?.();
    if (folder) values.folder = folder;
    const descriptors = activeFilterChips(values, labels);
    chips.replaceChildren(...descriptors.map(({ key, label }) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'filter-chip';
      button.textContent = `${label} ×`;
      button.setAttribute('aria-label', `Remove ${label} filter`);
      button.onclick = () => {
        if (key === 'folder') clearFolder?.();
        else {
          filters[key] = inactiveValue(key);
          const control = controls.get(key);
          if (control) control.value = '';
        }
        onChange();
      };
      return button;
    }));
    chips.hidden = descriptors.length === 0;
    clearAll.hidden = descriptors.length === 0;
  }

  for (const [key, control] of controls) {
    control.onchange = () => {
      filters[key] = readControl(key, control);
      onChange();
    };
  }
  clearAll.onclick = () => {
    for (const [key, control] of controls) {
      filters[key] = inactiveValue(key);
      control.value = '';
    }
    clearFolder?.();
    onChange();
  };

  return Object.freeze({
    render,
    reset() {
      for (const [key, control] of controls) {
        filters[key] = inactiveValue(key);
        control.value = '';
        if (dynamic.has(key)) {
          control.replaceChildren(...baseOptions.get(key));
        }
      }
      chips.replaceChildren();
      chips.hidden = true;
      clearAll.hidden = true;
    },
    setOptions(key, values) {
      const control = controls.get(key);
      if (control) syncFilterOptions(control, values, filters[key] || '');
    },
  });
}
