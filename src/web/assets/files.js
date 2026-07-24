import { activeFilterChips } from './ui-model.js';

export const UPLOAD_STATES = Object.freeze([
  'queued',
  'preflighting',
  'awaiting-conflict',
  'uploading',
  'finishing',
  'completed',
  'failed',
  'cancelled',
  'ambiguous',
]);

const UPLOAD_TRANSITIONS = Object.freeze({
  queued: Object.freeze({ 'preflight-started': 'preflighting', cancelled: 'cancelled' }),
  preflighting: Object.freeze({
    ready: 'uploading',
    conflict: 'awaiting-conflict',
    failed: 'failed',
    cancelled: 'cancelled',
  }),
  'awaiting-conflict': Object.freeze({
    resolved: 'uploading',
    skipped: 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
  }),
  uploading: Object.freeze({
    'bytes-sent': 'finishing',
    'server-confirmed': 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
    uncertain: 'ambiguous',
  }),
  finishing: Object.freeze({
    'server-confirmed': 'completed',
    failed: 'failed',
    cancelled: 'cancelled',
    uncertain: 'ambiguous',
  }),
  completed: Object.freeze({}),
  failed: Object.freeze({ retry: 'queued' }),
  cancelled: Object.freeze({ retry: 'queued' }),
  ambiguous: Object.freeze({ retry: 'queued', 'server-confirmed': 'completed', cancelled: 'cancelled' }),
});

export function nextUploadState(current, event) {
  const next = UPLOAD_TRANSITIONS[current]?.[event?.type];
  if (!next) throw new TypeError(`Invalid upload transition: ${current} -> ${event?.type || 'unknown'}`);
  return next;
}

const ACTIVE_UPLOAD_STATES = new Set(['preflighting', 'uploading', 'finishing']);
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
    transition(id, state, patch = {}) {
      if (!UPLOAD_STATES.includes(state)) throw new TypeError(`Unknown upload state: ${state}`);
      Object.assign(item(id), patch, { state });
      return { ...item(id) };
    },
    event(id, event, patch = {}) {
      return this.transition(id, nextUploadState(item(id).state, event), patch);
    },
    retryable: () => items.filter(({ state }) => RETRYABLE_UPLOAD_STATES.has(state))
      .map((entry) => ({ ...entry })),
    retry(ids = null) {
      const selected = ids ? new Set(ids) : null;
      const retried = [];
      for (const entry of items) {
        if (RETRYABLE_UPLOAD_STATES.has(entry.state) && (!selected || selected.has(entry.id))) {
          Object.assign(entry, {
            state: 'queued', loaded: 0, error: null, evidence: '', policy: null, target: null,
          });
          retried.push({ ...entry });
        }
      }
      return retried;
    },
    claimReady() {
      const available = Math.max(0, limit - activeCount());
      const claimed = items.filter(({ state }) => state === 'queued').slice(0, available);
      for (const entry of claimed) entry.state = 'preflighting';
      return claimed.map((entry) => ({ ...entry }));
    },
    maxConcurrent: limit,
  });
}

export function uploadConflictDecision({ policy, suggestedName = null, allowReplace = true }) {
  if (!policy) return null;
  if (!['skip', 'replace', 'rename'].includes(policy)) throw new TypeError('Unknown conflict policy');
  if (policy === 'replace' && !allowReplace) throw new TypeError('Replace is unsupported by this backend');
  if (policy === 'rename' && !suggestedName) throw new TypeError('Rename requires a suggested name');
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
  let batch = null;
  let nextId = 0;
  let scopeGeneration = 0;

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

  function exactScopeCurrent() {
    return batch
      && batch.scopeGeneration === scopeGeneration
      && isScopeCurrent(batch.scope);
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

  function resolveConflict(id, policy, applyAll) {
    if (!batch || !exactScopeCurrent()) return;
    const source = batch.queue.get(id);
    const decision = uploadConflictDecision({
      policy,
      suggestedName: source.suggestedName,
      allowReplace: true,
    });
    const targets = applyAll
      ? batch.queue.items().filter(({ state }) => state === 'awaiting-conflict')
      : [source];
    for (const targetItem of targets) {
      const targetDecision = uploadConflictDecision({
        policy,
        suggestedName: targetItem.suggestedName,
        allowReplace: true,
      });
      if (targetDecision.policy === 'skip') {
        batch.queue.transition(targetItem.id, 'completed', {
          policy: 'skip',
          result: 'Skipped because a destination file already exists.',
        });
      } else {
        batch.queue.transition(targetItem.id, 'queued', targetDecision);
      }
    }
    render();
    schedule();
    return decision;
  }

  function renderConflictActions(entry, row) {
    const fieldset = document.createElement('fieldset');
    fieldset.className = 'upload-conflict-actions';
    const legend = document.createElement('legend');
    legend.textContent = `Resolve conflict for ${entry.name}`;
    const applyLabel = document.createElement('label');
    const apply = document.createElement('input');
    apply.type = 'checkbox';
    applyLabel.append(apply, ' Apply to all remaining conflicts');
    fieldset.append(
      legend,
      button('Skip', () => resolveConflict(entry.id, 'skip', apply.checked)),
      button('Replace', () => resolveConflict(entry.id, 'replace', apply.checked)),
      button('Rename', () => resolveConflict(entry.id, 'rename', apply.checked)),
      applyLabel,
    );
    row.appendChild(fieldset);
  }

  function render() {
    if (!batch) return;
    const entries = batch.queue.items();
    queueSurface.hidden = false;
    byId('upload-queue-context').textContent = `${formatScope(batch.scope)} · destination: ${batch.destination || 'Vault root'}`;
    queueItems.replaceChildren(...entries.map((entry) => {
      const row = document.createElement('li');
      row.className = 'upload-item';
      row.dataset.uploadId = entry.id;
      const name = document.createElement('strong');
      name.className = 'upload-item-name';
      name.textContent = entry.displayName;
      const status = document.createElement('span');
      status.className = 'upload-item-status';
      status.textContent = UPLOAD_STATUS_LABELS[entry.state];
      row.append(name, status);
      if (['uploading', 'finishing'].includes(entry.state)) {
        const progress = document.createElement('progress');
        progress.max = entry.total || entry.size || 1;
        if (entry.state === 'uploading') progress.value = entry.loaded || 0;
        progress.setAttribute('aria-label', `Upload progress for ${entry.name}`);
        row.appendChild(progress);
      }
      if (entry.state === 'awaiting-conflict') renderConflictActions(entry, row);
      if (entry.evidence || entry.error || entry.result) {
        const detail = document.createElement('small');
        detail.className = 'upload-item-evidence';
        detail.textContent = entry.evidence || entry.error || entry.result;
        row.appendChild(detail);
      }
      if (['uploading', 'finishing'].includes(entry.state)) {
        const actions = document.createElement('div');
        actions.className = 'upload-item-actions';
        actions.appendChild(button(
          `Cancel ${entry.name}`,
          () => batch?.controllers.get(entry.id)?.abort(),
          'button ghost compact',
        ));
        row.appendChild(actions);
      }
      return row;
    }));
    retry.hidden = batch.queue.retryable().length === 0;
    finishIfSettled();
  }

  function appendSummary(entries) {
    summaryItems.append(...entries.map((entry) => {
      const item = document.createElement('li');
      item.textContent = `${entry.displayName}: ${UPLOAD_STATUS_LABELS[entry.state]}${entry.result ? ` — ${entry.result}` : ''}${entry.evidence ? ` — ${entry.evidence}` : ''}`;
      return item;
    }));
    summary.hidden = false;
  }

  function finishIfSettled() {
    if (!batch || batch.released) return;
    const entries = batch.queue.items();
    if (entries.some(({ state }) => !['completed', 'failed', 'cancelled', 'ambiguous'].includes(state))) return;
    batch.released = true;
    destination.disabled = false;
    setPending(false);
    appendSummary(entries);
    void refreshFiles(batch.scope).then(() => {
      if (exactScopeCurrent()) updateDestinationOptions(getFiles());
    }).catch(() => {});
  }

  async function reconcileUncertain(id, error) {
    if (!batch) return;
    const entry = batch.queue.get(id);
    try {
      await refreshFiles(batch.scope);
      if (!exactScopeCurrent()) return;
      const targetName = entry.target || entry.fullName;
      const after = getFiles().find(({ name }) => name === targetName) || null;
      const evidence = uploadEvidenceState({
        before: entry.before,
        after,
        expectedSize: entry.size,
      });
      batch.queue.transition(id, evidence.state, {
        evidence: evidence.evidence,
        result: evidence.state === 'completed' ? 'Confirmed after refreshing file metadata.' : null,
      });
    } catch {
      if (exactScopeCurrent()) {
        batch.queue.transition(id, 'ambiguous', {
          evidence: 'The server could not be refreshed, so completion remains unconfirmed.',
        });
      }
    }
    render();
    schedule();
  }

  async function transfer(entry) {
    if (!batch || !exactScopeCurrent()) return;
    const controller = new AbortController();
    batch.controllers.set(entry.id, controller);
    const attempt = (entry.attempt || 0) + 1;
    batch.queue.transition(entry.id, 'uploading', { attempt });
    render();
    const form = new FormData();
    form.append('file', entry.file, entry.name);
    let path = appendQuery('/api/files', scopeQuery(batch.scope));
    if (entry.policy) {
      const policyQuery = new URLSearchParams({ policy: entry.policy });
      if (entry.target) policyQuery.set('target', entry.target);
      path = appendQuery(path, policyQuery.toString());
    }
    try {
      const response = await api.upload({
        path,
        formData: form,
        signal: controller.signal,
        operationId: `upload-${entry.id}-${attempt}`,
        onProgress: ({ loaded, total, finishing }) => {
          if (!batch || !exactScopeCurrent()) return;
          batch.queue.transition(entry.id, finishing ? 'finishing' : 'uploading', { loaded, total });
          render();
        },
      });
      if (!batch || !exactScopeCurrent()) return;
      batch.queue.transition(entry.id, 'completed', {
        loaded: entry.size,
        total: entry.size,
        result: response?.status === 'skipped'
          ? 'Skipped because a destination file already exists.'
          : 'Server confirmed the upload.',
      });
      render();
      schedule();
    } catch (error) {
      if (!batch || !exactScopeCurrent()) return;
      if (error?.code === 'xv-file-conflict') {
        batch.queue.transition(entry.id, 'awaiting-conflict', {
          suggestedName: error.details?.suggested_name,
          error: 'The destination changed after preflight. Choose what to do.',
        });
        render();
        schedule();
      } else if (error?.name === 'AbortError' || error?.name === 'NetworkError' || error?.ambiguous) {
        batch.queue.transition(entry.id, 'ambiguous', {
          evidence: 'Refreshing server metadata to determine the outcome…',
        });
        render();
        await reconcileUncertain(entry.id, error);
      } else {
        batch.queue.transition(entry.id, 'failed', { error: safeUploadError(error) });
        render();
        schedule();
      }
    } finally {
      batch?.controllers.delete(entry.id);
    }
  }

  function schedule() {
    if (!batch || !exactScopeCurrent()) return;
    for (const entry of batch.queue.claimReady()) void transfer(entry);
    render();
  }

  async function preflight() {
    const targetBatch = batch;
    const candidates = targetBatch.queue.items()
      .filter(({ state }) => state === 'preflighting');
    if (!candidates.length) {
      schedule();
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
      if (batch !== targetBatch || !exactScopeCurrent()) return;
      for (const result of response.results || []) {
        if (result.status === 'ready') {
          batch.queue.transition(result.client_id, 'queued');
        } else if (result.status === 'conflict') {
          batch.queue.transition(result.client_id, 'awaiting-conflict', {
            suggestedName: result.suggested_name,
          });
        } else {
          batch.queue.transition(result.client_id, 'failed', {
            error: result.status === 'too-large'
              ? `This file exceeds the ${Math.floor(result.max_bytes / 1024 / 1024)} MB limit.`
              : 'This backend cannot guarantee a conflict-safe upload.',
          });
        }
      }
      render();
      schedule();
    } catch (error) {
      if (batch !== targetBatch || !exactScopeCurrent()) return;
      for (const entry of batch.queue.items()) {
        if (entry.state === 'preflighting') {
          batch.queue.transition(entry.id, 'failed', { error: safeUploadError(error) });
        }
      }
      render();
    }
  }

  async function start(fileList) {
    const files = [...(fileList || [])];
    if (!files.length || activeBatch()) return false;
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
      destination: chosenDestination,
      scope,
      scopeGeneration,
      released: false,
    };
    destination.disabled = true;
    setPending(true);
    for (const entry of entries) batch.queue.transition(entry.id, 'preflighting');
    render();
    await preflight();
    return true;
  }

  retry.onclick = () => {
    if (!batch || !exactScopeCurrent()) return;
    if (batch.released) {
      batch.released = false;
      destination.disabled = true;
      setPending(true);
    }
    const retried = batch.queue.retry();
    for (const entry of retried) batch.queue.transition(entry.id, 'preflighting');
    render();
    void preflight();
  };
  byId('dismiss-upload-summary').onclick = () => {
    summary.hidden = true;
    summaryItems.replaceChildren();
  };

  syncContextCopy();
  updateDestinationOptions(getFiles());
  return Object.freeze({
    start,
    refreshContext() {
      scopeGeneration++;
      syncContextCopy();
      updateDestinationOptions(getFiles());
      if (batch && !batch.released) {
        for (const controller of batch.controllers.values()) controller.abort();
        batch.released = true;
        destination.disabled = false;
        setPending(false);
      }
    },
    updateDestinations: () => updateDestinationOptions(getFiles()),
    hasPending: activeBatch,
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
