import * as XvUiModel from './ui-model.js';
import { buildMetadataIndex, searchIndex, shortcutIntent } from './commands.js';
import { mountFilterControls } from './files.js';
import { guardNavigation } from './dialogs.js';
import { setProtectedValueStatus } from './accessibility.js';
import { contextQuery, formatContextLine } from './context.js';
import {
  bindOwnedRetry,
  createOwnerRegistry,
  operationEvent,
  operationResultStatus,
  safeDiagnostic,
} from './store.js';

export function createExposureTimer({ seconds, onTick, onExpire, clock = globalThis }) {
  const duration = Math.max(0, Math.floor(Number(seconds) || 0));
  let remaining = duration;
  let timerId = null;
  let generation = 0;

  function cancel() {
    generation++;
    if (timerId !== null) clock.clearTimeout(timerId);
    timerId = null;
  }

  function reset() {
    cancel();
    remaining = duration;
    const activeGeneration = generation;
    onTick?.(remaining);
    if (remaining === 0) {
      onExpire?.();
      return;
    }
    const tick = () => {
      if (generation !== activeGeneration) return;
      remaining--;
      if (remaining === 0) {
        timerId = null;
        onExpire?.();
        return;
      }
      onTick?.(remaining);
      timerId = clock.setTimeout(tick, 1000);
    };
    timerId = clock.setTimeout(tick, 1000);
  }

  reset();
  return Object.freeze({ cancel, reset, remaining: () => remaining });
}

export async function clearClipboardIfUnchanged({ clipboard, expected, isCurrent = () => true }) {
  try {
    if (typeof clipboard?.readText !== 'function' || typeof clipboard?.writeText !== 'function') return false;
    const current = await clipboard.readText();
    if (!isCurrent() || current !== expected) return false;
    await clipboard.writeText('');
    return true;
  } catch (_) {
    return false;
  }
}

export function deleteConfirmationModel({ backend, vault, names, recoverable, kind = 'secret' }) {
  const targets = [...names];
  const count = targets.length;
  const plural = kind === 'file' ? 'files' : 'secrets';
  const noun = count === 1 ? kind : plural;
  return {
    visibleNames: targets.slice(0, 5),
    overflow: Math.max(0, count - 5),
    message: `Delete ${count} ${noun} from ${backend} vault ${vault}?`,
    recovery: recoverable
      ? `Deleted ${plural} can be restored from Trash.`
      : `Recovery is unavailable for ${plural} on ${backend}.`,
  };
}

export function deletionNoticeModel(names, recoverable) {
  const count = names.length;
  return recoverable
    ? { message: `${count} ${count === 1 ? 'secret' : 'secrets'} moved to Trash.`, canUndo: true }
    : { message: `${count} ${count === 1 ? 'secret' : 'secrets'} deleted. Recovery is unavailable.`, canUndo: false };
}

export function canPurgeSecret(expectedName, typedName) {
  return typedName === expectedName;
}

export function mountSecrets({
  api,
  store,
  dialogs,
  preferences = null,
  contextRail = null,
  commandRegistry = null,
  token,
  exposureClock = globalThis,
  clipboard = globalThis.navigator?.clipboard,
}) {

const $ = (sel) => document.querySelector(sel);
const errorOwners = createOwnerRegistry();
const SVG_NS = 'http://www.w3.org/2000/svg';
function icon(name) {
  const svg = document.createElementNS(SVG_NS, 'svg');
  svg.classList.add('icon');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('focusable', 'false');
  const use = document.createElementNS(SVG_NS, 'use');
  use.setAttribute('href', `#icon-${name}`);
  svg.appendChild(use);
  return svg;
}

function setListSummary(kind, visibleCount, totalCount, folderCount) {
  const singular = kind === 'secrets' ? 'secret' : 'file';
  const noun = visibleCount === 1 ? singular : kind;
  $(`#${singular}-item-count`).textContent = visibleCount === totalCount
    ? `${visibleCount} ${noun}`
    : `${visibleCount} / ${totalCount} ${kind}`;
  const visibility = visibleCount === totalCount
    ? `${totalCount} ${totalCount === 1 ? singular : kind}`
    : `${visibleCount} of ${totalCount} ${kind}`;
  const folders = `${folderCount} ${folderCount === 1 ? 'folder' : 'folders'}`;
  const safety = kind === 'secrets' ? 'Values remain hidden until revealed.' : 'Files remain encrypted in this vault.';
  $(`#${singular}-list-summary`).textContent = `${visibility} across ${folders}. ${safety}`;
}

function setListLoadStatus(kind, state) {
  const singular = kind === 'secrets' ? 'secret' : 'file';
  const copy = {
    secrets: {
      loading: ['Loading secrets…', 'Loading secrets from the current vault…'],
      failed: ['Secrets unavailable', 'Current vault secrets are unavailable.'],
    },
    files: {
      loading: ['Loading files…', 'Loading files from the current vault…'],
      failed: ['Files unavailable', 'Current vault files are unavailable.'],
    },
  };
  const [count, summary] = copy[kind][state];
  $(`#${singular}-item-count`).textContent = count;
  $(`#${singular}-list-summary`).textContent = summary;
}

function toast(msg) {
  const t = $('#toast');
  t.replaceChildren(icon('check'), document.createTextNode(msg));
  t.className = 'toast success';
  t.setAttribute('role', 'status');
  t.hidden = false;
  clearTimeout(t._timer);
  t._timer = setTimeout(() => { t.hidden = true; }, 4000);
}

function confirmDeletion(kind, names, scope) {
  const dialog = $('#delete-confirmation');
  const cancel = $('#cancel-delete');
  const confirm = $('#confirm-delete');
  const plural = kind === 'file' ? 'files' : 'secrets';
  const model = deleteConfirmationModel({
    backend: scope.backend,
    vault: scope.vault,
    names,
    recoverable: kind === 'secret' && !!scope.capabilities.soft_delete,
    kind,
  });
  $('#delete-confirmation-title').textContent = names.length === 1 ? `Delete ${kind}?` : `Delete ${plural}?`;
  $('#delete-confirmation-message').textContent = model.message;
  $('#delete-recovery').textContent = model.recovery;
  $('#delete-targets').replaceChildren(...model.visibleNames.map((name) => {
    const item = document.createElement('li');
    item.textContent = name;
    return item;
  }));
  const overflow = $('#delete-overflow');
  overflow.hidden = model.overflow === 0;
  overflow.textContent = model.overflow ? `and ${model.overflow} more` : '';
  confirm.textContent = names.length === 1 ? `Delete ${kind}` : `Delete ${names.length} ${plural}`;

  return new Promise((resolve) => {
    let settled = false;
    const finish = (confirmed) => {
      if (settled) return;
      settled = true;
      cancel.onclick = null;
      confirm.onclick = null;
      dialogs.closeModal(dialog);
      resolve(confirmed);
    };
    cancel.onclick = () => finish(false);
    confirm.onclick = () => finish(true);
    dialogs.openModal(dialog, {
      initialFocus: cancel,
      invoker: document.activeElement,
      onEscape: () => finish(false),
    });
  });
}

let pendingUndo = null;
function showDeletionNotice(names, scope = captureOperationScope()) {
  if (!names.length) return;
  const recoverable = !!scope.capabilities.soft_delete;
  const model = deletionNoticeModel(names, recoverable);
  const notice = $('#action-notice');
  notice.classList.remove('error');
  notice.querySelector('.action-notice-message').textContent = model.message;
  $('#action-notice-context').textContent = formatContextLine(scope);
  notice.hidden = false;
  const undo = $('#undo-delete');
  undo.hidden = !(model.canUndo && scope.capabilities.restore);
  undo.disabled = false;
  pendingUndo = undo.hidden ? null : { names: [...names], scope };
}

$('#dismiss-action-notice').onclick = () => {
  pendingUndo = null;
  $('#action-notice').hidden = true;
};

$('#undo-delete').onclick = async () => {
  if (!pendingUndo) return;
  const notice = $('#action-notice');
  const button = $('#undo-delete');
  const { names, scope } = pendingUndo;
  if (!canStartScopedAction(scope)) return;
  const vault = scope.vault;
  beginScopedMutation();
  button.disabled = true;
  button.textContent = 'Restoring…';
  try {
    const results = await runBounded(names, 4, (name) => (
      api('POST', `/api/secrets/${encodeURIComponent(name)}/restore${vaultQS(vault, scope)}`)
    ));
    const restored = results.filter((result) => result.ok);
    const failed = results.filter((result) => !result.ok);
    if (scopeMatchesCurrent(scope)) {
      await loadSecrets(vault, scope);
      if (activeTab === 'trash') await loadDeleted(vault, scope);
    }
    if (failed.length) {
      pendingUndo = { names: failed.map((result) => result.item), scope };
      notice.classList.add('error');
      notice.querySelector('.action-notice-message').textContent = `Undo failed: ${failed[0].error.message}`;
      button.disabled = false;
      button.textContent = 'Retry Undo';
      showListError(activeTab, failed[0].error);
    } else {
      pendingUndo = null;
      notice.querySelector('.action-notice-message').textContent = `${restored.length} ${restored.length === 1 ? 'secret' : 'secrets'} restored.`;
      button.hidden = true;
      button.textContent = 'Undo';
    }
  } finally {
    endScopedMutation();
  }
};
function isAborted(error) {
  return error?.name === 'AbortError';
}

function errorCopy(error) {
  return {
    message: error?.message || 'The request could not be completed.',
    hint: error?.hint || '',
    field: error?.field || null,
  };
}

function resetErrorHandlers(panel) {
  for (const selector of ['.error-retry', '.error-copy', '.error-dismiss']) {
    const button = panel.querySelector(selector);
    if (button) {
      button.onclick = null;
      button.disabled = false;
    }
  }
}

function clearError(surface, expectedGeneration) {
  const panel = $(surface);
  const released = errorOwners.clear(surface, expectedGeneration);
  if (expectedGeneration !== undefined && !released) return false;
  panel.hidden = true;
  delete panel.dataset.source;
  panel.querySelector('.error-message').textContent = '';
  panel.querySelector('.error-hint').textContent = '';
  panel.querySelector('.error-retry')?.setAttribute('hidden', '');
  panel.querySelector('.error-copy')?.setAttribute('hidden', '');
  resetErrorHandlers(panel);
  return true;
}

function showError(surface, error, retry, owner = {}) {
  if (isAborted(error)) return;
  const panel = $(surface);
  const generation = errorOwners.replace(surface, {
    retained: owner,
    cleanup: () => {
      resetErrorHandlers(panel);
      if (owner.operationId) {
        store.dispatch({ type: 'operation/dismiss', operationId: owner.operationId });
      }
    },
  });
  const { message, hint } = errorCopy(error);
  panel.querySelector('.error-copy')?.setAttribute('hidden', '');
  panel.querySelector('.error-message').textContent = message;
  panel.querySelector('.error-hint').textContent = hint;
  const retryButton = panel.querySelector('.error-retry');
  if (retryButton) {
    retryButton.textContent = 'Retry';
    retryButton.hidden = !retry;
    if (retry) {
      bindOwnedRetry({
        registry: errorOwners,
        key: surface,
        generation,
        button: retryButton,
        retry: () => retry({ surface, generation }),
      });
    } else {
      retryButton.disabled = false;
      retryButton.onclick = null;
    }
  }
  const dismiss = panel.querySelector('.error-dismiss');
  if (dismiss) dismiss.onclick = () => clearError(surface, generation);
  panel.hidden = false;
  return generation;
}

function diagnosticsScope(scope, failures) {
  return safeDiagnostic({
    code: failures[0]?.error?.code || 'xv-partial-failure',
    message: `${failures.length} item${failures.length === 1 ? '' : 's'} failed.`,
    hint: failures[0]?.error?.hint || 'Retry the failed items.',
    backend: scope.backend,
    vault: scope.vault,
    failedNames: failures.map(({ item }) => item),
  });
}

function diagnosticText(diagnostic) {
  return [
    `code: ${diagnostic.code}`,
    `message: ${diagnostic.message}`,
    diagnostic.hint && `hint: ${diagnostic.hint}`,
    `backend: ${diagnostic.backend}`,
    `vault: ${diagnostic.vault}`,
    `failed names: ${diagnostic.failedNames.join(', ')}`,
  ].filter(Boolean).join('\n');
}

function showListError(kind, error) {
  const surface = kind === 'trash'
    ? '#trash-error'
    : `#${kind === 'secrets' ? 'secret' : 'file'}-error`;
  $(surface).dataset.source = 'action';
  showError(surface, error);
}

function showListLoadError(kind, error) {
  // Keep the two-argument compatibility signature while requiring load callers
  // to pass the immutable target that actually failed.
  const vault = arguments[2];
  const scope = arguments[3];
  return showScopedListLoadError(kind, error, vault, scope);
}

function showScopedListLoadError(kind, error, vault, scope) {
  const surface = `#${kind === 'secrets' ? 'secret' : 'file'}-refresh-error`;
  const panel = $(surface);
  panel.dataset.source = 'load';
  const stale = kind === 'secrets' ? hasSuccessfulSecretsSnapshot : hasSuccessfulFilesSnapshot;
  const copy = errorCopy(error);
  const retryVault = vault;
  const retryScope = Object.freeze(structuredClone(scope));
  showError(surface, {
    ...copy,
    message: stale ? `Stale — ${copy.message}` : copy.message,
  }, (owner) => (
    kind === 'secrets'
      ? loadSecrets(retryVault, retryScope, owner)
      : loadFiles(retryVault, retryScope, owner)
  ));
}

function clearListLoadError(kind) {
  const panel = $(`#${kind === 'secrets' ? 'secret' : 'file'}-refresh-error`);
  if (panel.dataset.source === 'load') clearError(`#${panel.id}`);
}

function clearRefreshOwnersForScopeTransition() {
  secretLoadController?.abort();
  fileLoadController?.abort();
  secretLoadController = null;
  fileLoadController = null;
  secretLoadGeneration++;
  fileLoadGeneration++;
  clearError('#secret-refresh-error');
  clearError('#file-refresh-error');
}

function showFormError(error) {
  if (isAborted(error)) return;
  showError('#secret-form-error', error);
  const field = errorCopy(error).field;
  const mapped = {
    target: '#conversion-target',
    target_type: '#conversion-target',
    supplied_fields: '#conversion-required-fields input',
    source_revision: '#conversion-preview',
    confirm_lossy: '#conversion-preview',
    new_name: '#rename-name',
  };
  const suppliedField = field?.startsWith('supplied_fields.')
    ? field.slice('supplied_fields.'.length)
    : null;
  const workflowField = field === 'name' && !$('#rename-workflow').hidden
    ? $('#rename-name')
    : null;
  const input = field && (
    workflowField
    || $('#secret-form').elements[field]
    || (suppliedField
      ? [...$('#conversion-required-fields').querySelectorAll('input[data-conversion-field]')]
        .find((candidate) => candidate.dataset.conversionField === suppliedField)
      : null)
    || (mapped[field] ? $(mapped[field]) : null)
  );
  if (input) {
    input.setAttribute('aria-invalid', 'true');
    const describedBy = new Set((input.getAttribute('aria-describedby') || '').split(/\s+/).filter(Boolean));
    describedBy.add('secret-form-error');
    input.setAttribute('aria-describedby', [...describedBy].join(' '));
    input.focus?.();
  }
}

function clearFormError() {
  clearError('#secret-form-error');
  for (const control of $('#secret-form').querySelectorAll('[aria-invalid="true"]')) {
    control.removeAttribute('aria-invalid');
    const describedBy = (control.getAttribute('aria-describedby') || '')
      .split(/\s+/).filter((id) => id && id !== 'secret-form-error');
    if (describedBy.length) control.setAttribute('aria-describedby', describedBy.join(' '));
    else control.removeAttribute('aria-describedby');
  }
}

function fail(error) {
  if (isAborted(error)) return;
  if (!$('#drawer').hidden) showFormError(error);
  else showListError(activeTab, error);
}

function resetConfirmation(button, label) {
  clearTimeout(button._confirmTimer);
  button._confirmTimer = null;
  button.dataset.armed = '';
  button.disabled = false;
  button.classList.remove('pending');
  button.textContent = label;
}

function beginPendingAction(button, label) {
  clearTimeout(button._confirmTimer);
  button._confirmTimer = null;
  button.dataset.armed = '';
  button.disabled = true;
  button.classList.add('pending');
  button.textContent = label;
}

function armConfirmation(button, armedLabel, timeoutMs = 3000) {
  if (button.dataset.armed === '1') return true;
  button.dataset.armed = '1';
  button.textContent = armedLabel;
  clearTimeout(button._confirmTimer);
  button._confirmTimer = setTimeout(
    () => resetConfirmation(button, button.dataset.defaultLabel),
    timeoutMs,
  );
  return false;
}

// Mirrors src/utils/format.rs::format_size: binary (1024) steps, whole
// bytes without decimals, larger units with two decimals.
function fmtSize(bytes) {
  if (typeof bytes !== 'number' || !isFinite(bytes)) return '';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let size = bytes;
  let i = 0;
  while (size >= 1024 && i < units.length - 1) { size /= 1024; i++; }
  return i === 0 ? `${Math.floor(size)} B` : `${size.toFixed(2)} ${units[i]}`;
}

function showListState(tbody, kind, state, cols) {
  tbody.innerHTML = '';
  if (state === 'loading') {
    for (let index = 0; index < 3; index++) {
      const tr = document.createElement('tr');
      tr.className = 'skeleton-row';
      const td = document.createElement('td');
      td.colSpan = cols;
      const content = document.createElement('div');
      content.className = 'skeleton-content';
      content.append(document.createElement('span'), document.createElement('span'), document.createElement('span'));
      td.appendChild(content);
      tr.appendChild(td);
      tbody.appendChild(tr);
    }
    return;
  }

  const copy = {
    secrets: {
      failed: ['Couldn’t load secrets', 'The current vault could not be read.'],
      empty: ['No secrets yet', 'Create the first secret in this vault.'],
      filtered: ['No matching secrets', 'Try a different name, folder, group, or record type.'],
    },
    files: {
      failed: ['Couldn’t load files', 'The current vault could not be read.'],
      empty: ['No files yet', 'Upload the first encrypted file to this vault.'],
      filtered: ['No matching files', 'Try a different name, folder, or type.'],
    },
  };
  const [title, description] = copy[kind][state];
  const tr = document.createElement('tr');
  const td = document.createElement('td');
  td.colSpan = cols;
  const container = document.createElement('div');
  container.className = `empty-state ${state}`;
  const heading = document.createElement('strong');
  heading.textContent = title;
  const message = document.createElement('span');
  message.textContent = description;
  container.append(heading, message);
  if (state === 'empty') {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'button secondary';
    button.textContent = kind === 'secrets' ? 'New secret' : 'Browse files';
    if (kind === 'secrets') button.onclick = () => openDrawer(null);
    else button.onclick = () => $('#file-input').click();
    container.appendChild(button);
  }
  td.appendChild(container);
  tr.appendChild(td);
  tbody.appendChild(tr);
}

const secretFolderNavigation = XvUiModel.createFolderNavigationState(globalThis.localStorage);
const fileFolderNavigation = XvUiModel.createFolderNavigationState(globalThis.localStorage);
const folderNavigationFocus = {
  secrets: { desktop: XvUiModel.FOLDER_ALL, mobile: XvUiModel.FOLDER_ALL },
  files: { desktop: XvUiModel.FOLDER_ALL, mobile: XvUiModel.FOLDER_ALL },
};
const folderTokenIndexes = { secrets: null, files: null };
const listFilters = {
  secrets: { group: '', type: '', expiry: '', enabled: null },
  files: { type: '' },
};

function navigationFor(kind) {
  return kind === 'secrets' ? secretFolderNavigation : fileFolderNavigation;
}

function folderOf(kind, item) {
  if (kind === 'secrets') return XvUiModel.normalizeFolderPath(item.folder);
  const name = item.name || '';
  return XvUiModel.normalizeFolderPath(
    name.includes('/') ? name.slice(0, name.lastIndexOf('/')) : '',
  );
}

function folderModelItems(kind, items) {
  return items.map((item) => ({ ...item, folder: folderOf(kind, item) }));
}

function folderPaths(kind, items) {
  const paths = new Set();
  for (const item of items) {
    const path = folderOf(kind, item);
    if (!path) continue;
    const segments = path.split('/');
    for (let index = 1; index <= segments.length; index++) {
      paths.add(segments.slice(0, index).join('/'));
    }
  }
  return [...paths];
}

function requestFolderTokenIndex(kind, items, scope) {
  const folders = folderPaths(kind, items);
  return api(
    'POST',
    `/api/folder-tokens${vaultQS(scope.vault, scope)}`,
    { surface: kind, folders },
  ).then((response) => XvUiModel.createFolderTokenIndex(response));
}

function folderScope(kind) {
  const backend = typeof ctx?.backend === 'string' ? ctx.backend : ctx?.backend?.name;
  return { backend: backend || '', vault: currentVault || '', surface: kind };
}

function renderFolderNavigation(kind, allItems, visibleItems) {
  const navigation = navigationFor(kind);
  const modelItems = folderModelItems(kind, allItems);
  const modelVisibleItems = folderModelItems(kind, visibleItems);
  const viewModel = XvUiModel.buildFolderViewModel(modelItems, modelVisibleItems);
  navigation.sync(folderScope(kind), {
    total: allItems.length,
    folderIds: viewModel.folderIds,
    expandableIds: viewModel.expandableIds,
    tokenIndex: folderTokenIndexes[kind],
  });
  const renderOne = (mobile) => {
    const mode = mobile ? 'mobile' : 'desktop';
    const container = $(`#${kind}${mobile ? '-mobile' : ''}-folder-tree`);
    const snapshot = navigation.snapshot();
    XvUiModel.renderFolderTree({
      document,
      container,
      items: modelItems,
      visibleItems: modelVisibleItems,
      viewModel,
      expanded: navigation.expanded,
      selected: snapshot.selected,
      focusedId: folderNavigationFocus[kind][mode],
      onFocus: (id) => { folderNavigationFocus[kind][mode] = id; },
      onSelect: (id) => {
        folderNavigationFocus[kind][mode] = id;
        navigation.select(id);
        renderSelectionKind(kind);
        if (mobile) {
          dialogs.closeModal($(`#${kind}-folder-sheet`));
          return false;
        }
        return true;
      },
      onToggle: (id, expanded) => {
        folderNavigationFocus[kind][mode] = id;
        navigation.toggle(id, expanded);
        renderSelectionKind(kind);
      },
    });
  };
  renderOne(false);
  renderOne(true);
  return viewModel.folderCount;
}

function setAllFolderExpansion(kind, expanded) {
  const navigation = navigationFor(kind);
  if (expanded) navigation.expandAll();
  else {
    navigation.collapseAll();
    folderNavigationFocus[kind] = {
      desktop: XvUiModel.FOLDER_ALL,
      mobile: XvUiModel.FOLDER_ALL,
    };
  }
  renderSelectionKind(kind);
}

function initFolderNavigationControls() {
  for (const kind of ['secrets', 'files']) {
    for (const id of [
      `${kind}-folders-expand-all`,
      `${kind}-mobile-folders-expand-all`,
      `${kind}-mobile-sheet-expand-all`,
    ]) {
      $(`#${id}`).onclick = () => setAllFolderExpansion(kind, true);
    }
    for (const id of [
      `${kind}-folders-collapse-all`,
      `${kind}-mobile-folders-collapse-all`,
      `${kind}-mobile-sheet-collapse-all`,
    ]) {
      $(`#${id}`).onclick = () => setAllFolderExpansion(kind, false);
    }
    const opener = $(`#${kind}-folder-filter-open`);
    const sheet = $(`#${kind}-folder-sheet`);
    const close = $(`#${kind}-folder-sheet-close`);
    const dismiss = () => dialogs.closeModal(sheet);
    opener.onclick = () => dialogs.openModal(sheet, {
      initialFocus: sheet.querySelector('[role="treeitem"]') || close,
      invoker: opener,
      onEscape: dismiss,
    });
    close.onclick = dismiss;
  }
}

function selectedFolderLabel(kind) {
  const selected = navigationFor(kind).snapshot().selected;
  if (selected.kind === 'folder') return selected.path;
  if (selected.kind === 'unfiled') return 'Unfiled';
  return '';
}

function clearSelectedFolder(kind) {
  navigationFor(kind).select(XvUiModel.FOLDER_ALL);
  folderNavigationFocus[kind] = {
    desktop: XvUiModel.FOLDER_ALL,
    mobile: XvUiModel.FOLDER_ALL,
  };
}

const filterControls = {
  secrets: mountFilterControls({
    document,
    surface: 'secret',
    filters: listFilters.secrets,
    keys: ['group', 'type', 'expiry', 'enabled'],
    dynamicKeys: ['group', 'type'],
    labels: {
      folder: 'Folder',
      group: 'Group',
      type: 'Record type',
      expiry: 'Expiry',
      enabled: 'Status',
    },
    onChange: () => renderSecrets(),
    folderValue: () => selectedFolderLabel('secrets'),
    clearFolder: () => clearSelectedFolder('secrets'),
  }),
  files: mountFilterControls({
    document,
    surface: 'file',
    filters: listFilters.files,
    keys: ['type'],
    dynamicKeys: ['type'],
    labels: {
      folder: 'Folder',
      type: 'Type',
    },
    onChange: () => renderFiles(),
    folderValue: () => selectedFolderLabel('files'),
    clearFolder: () => clearSelectedFolder('files'),
  }),
};

function groupFilterOptions(items) {
  return items.flatMap((item) => (
    Array.isArray(item.groups)
      ? item.groups
      : (typeof item.groups === 'string' ? item.groups.split(',') : [])
  )).map((group) => group.trim()).filter(Boolean);
}

function recordTypeFilterOptions(items) {
  return items.map((item) => (
    item?.tags?.[TYPE_TAG]
      || item?.record_type
      || item?.type
      || (item?.content_type === RECORD_CONTENT_TYPE ? 'record' : 'plain')
  ));
}

const tableSort = {
  secrets: { key: 'name', direction: 'asc' },
  files: { key: 'name', direction: 'asc' },
};
const SORT_COLUMNS = {
  secrets: {
    name: [(item) => item.original_name || item.name, 'text'],
    folder: [(item) => item.folder || '', 'text'],
    groups: [(item) => item.groups || '', 'text'],
    note: [(item) => item.note || '', 'text'],
    updated: [(item) => item.updated_on || '', 'date'],
  },
  files: {
    name: [(item) => item.name, 'text'],
    size: [(item) => item.size, 'number'],
    type: [(item) => item.content_type || '', 'text'],
    modified: [(item) => item.last_modified || '', 'date'],
  },
};
function sortedTableItems(kind, items) {
  const state = tableSort[kind];
  const [valueOf, type] = SORT_COLUMNS[kind][state.key];
  const nameOf = kind === 'secrets'
    ? (item) => item.original_name || item.name
    : (item) => item.name;
  return XvUiModel.sortedCopy(items, valueOf, nameOf, type, state.direction);
}
function syncSortHeaders(kind) {
  const state = tableSort[kind];
  for (const header of document.querySelectorAll(`#${kind}-table th[data-sort-key]`)) {
    const active = header.dataset.sortKey === state.key;
    header.setAttribute('aria-sort', active ? (state.direction === 'asc' ? 'ascending' : 'descending') : 'none');
    header.querySelector('.sort-indicator').textContent = active ? (state.direction === 'asc' ? '▲' : '▼') : '';
  }
}
function setSort(kind, key) {
  const state = tableSort[kind];
  if (state.key === key) state.direction = state.direction === 'asc' ? 'desc' : 'asc';
  else { state.key = key; state.direction = 'asc'; }
  syncSortHeaders(kind);
  renderSelectionKind(kind);
}
function initSorting() {
  for (const kind of ['secrets', 'files']) {
    for (const header of document.querySelectorAll(`#${kind}-table th[data-sort-key]`)) {
      header.querySelector('.sort-button').onclick = () => setSort(kind, header.dataset.sortKey);
    }
    syncSortHeaders(kind);
  }
}

const TABLE_WIDTHS = {
  secrets: { defaults:[28,15,14,25,18], minimums:[14,10,10,14,12], storageKey:'xv.ui.columns.secrets.v1' },
  files: { defaults:[42,12,24,22], minimums:[20,10,14,14], storageKey:'xv.ui.columns.files.v1' },
};
function dataColumns(kind) {
  return [...document.querySelectorAll(`#${kind}-table colgroup col:not(.selection-col)`)];
}
function applyColumnWidths(kind, widths) {
  const config = TABLE_WIDTHS[kind];
  config.widths = widths;
  dataColumns(kind).forEach((column, index) => { column.style.width = `${widths[index]}%`; });
  document.querySelectorAll(`#${kind}-table .column-resizer`).forEach((handle, index) => {
    handle.setAttribute('aria-valuemin', String(config.minimums[index]));
    handle.setAttribute('aria-valuemax', String(widths[index] + widths[index + 1] - config.minimums[index + 1]));
    handle.setAttribute('aria-valuenow', String(widths[index]));
  });
}
function saveColumnWidths(kind) {
  const config = TABLE_WIDTHS[kind];
  try { localStorage.setItem(config.storageKey, JSON.stringify(config.widths)); } catch (_) { /* use in-memory widths */ }
}
function resizeColumns(kind, index, deltaPercent) {
  const config = TABLE_WIDTHS[kind];
  const widths = XvUiModel.resizeAdjacentWidths(config.widths, config.minimums, index, deltaPercent);
  applyColumnWidths(kind, widths);
  saveColumnWidths(kind);
}
function initColumnResizing() {
  for (const kind of ['secrets', 'files']) {
    const config = TABLE_WIDTHS[kind];
    let saved = '';
    try { saved = localStorage.getItem(config.storageKey) || ''; } catch (_) { saved = ''; }
    applyColumnWidths(kind, XvUiModel.normalizeWidths(saved, config.defaults, config.minimums));
    const table = $(`#${kind}-table`);
    [...table.querySelectorAll('.column-resizer')].forEach((handle, index) => {
      handle.onpointerdown = (event) => {
        event.preventDefault();
        const startX = event.clientX;
        const startWidths = [...config.widths];
        const move = (moveEvent) => {
          config.widths = [...startWidths];
          resizeColumns(kind, index, ((moveEvent.clientX - startX) / table.clientWidth) * 100);
        };
        const stop = () => {
          window.removeEventListener('pointermove', move);
          window.removeEventListener('pointerup', stop);
          window.removeEventListener('pointercancel', stop);
        };
        window.addEventListener('pointermove', move);
        window.addEventListener('pointerup', stop, { once: true });
        window.addEventListener('pointercancel', stop, { once: true });
      };
      handle.onkeydown = (event) => {
        if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
        event.preventDefault();
        resizeColumns(kind, index, event.key === 'ArrowLeft' ? -2 : 2);
      };
    });
  }
}

const secretSelection = { enabled: false, pending: false, ids: new Set(), visibleIds: [], generation: 0 };
const fileSelection = { enabled: false, pending: false, ids: new Set(), visibleIds: [], generation: 0 };

function selectionState(kind) {
  return kind === 'secrets' ? secretSelection : fileSelection;
}

function selectionElements(kind) {
  const singular = kind === 'secrets' ? 'secret' : 'file';
  return {
    table: $(`#${kind}-table`),
    toggle: $(`#select-${kind}`),
    selectAll: $(`#select-all-${kind}`),
    bulkBar: $(`#${singular}-bulk-bar`),
    count: $(`#${singular}-selection-count`),
    deleteButton: $(`#bulk-delete-${kind}`),
  };
}

function resetBulkConfirmation(kind) {
  const state = selectionState(kind);
  if (!state.pending) resetConfirmation(selectionElements(kind).deleteButton, 'Delete');
}

function renderSelectionKind(kind) {
  if (kind === 'secrets') renderSecrets();
  else renderFiles();
}

function resetSelectionControls(kind) {
  const singular = kind === 'secrets' ? 'secret' : 'file';
  const cancelButton = $(`#cancel-${singular}-selection`);
  cancelButton.disabled = false;
  if (kind === 'secrets') {
    const moveButton = $('#bulk-move-secrets');
    resetConfirmation(moveButton, 'Move');
    $('#secret-move-folder').disabled = false;
  }
}

function setSelectionMode(kind, enabled) {
  const state = selectionState(kind);
  const elements = selectionElements(kind);
  state.enabled = enabled;
  state.generation++;
  if (!enabled) {
    resetSelectionControls(kind);
    state.pending = false;
    state.ids.clear();
    state.visibleIds = [];
    resetConfirmation(elements.deleteButton, 'Delete');
  }
  elements.toggle.hidden = enabled;
  elements.bulkBar.hidden = !enabled;
  elements.table.querySelector('thead .selection-column').hidden = !enabled;
  elements.table.querySelector('col.selection-col').hidden = !enabled;
  elements.table.classList.toggle('selection-mode', enabled);
  renderSelectionKind(kind);
}

function clearSelection(kind) {
  setSelectionMode(kind, false);
}

function syncSelectionUi(kind, visibleIds) {
  const state = selectionState(kind);
  state.visibleIds = visibleIds;
  const available = new Set(
    (kind === 'secrets' ? secrets : files).map((item) => (
      kind === 'secrets' ? (item.original_name || item.name) : item.name
    )),
  );
  let selectionChanged = false;
  for (const id of [...state.ids]) {
    if (!available.has(id)) {
      state.ids.delete(id);
      selectionChanged = true;
    }
  }
  if (selectionChanged) resetBulkConfirmation(kind);
  updateSelectionControls(kind);
}

function updateSelectionControls(kind) {
  const state = selectionState(kind);
  const elements = selectionElements(kind);
  const visibleIds = state.visibleIds;
  const selectedVisible = visibleIds.filter((id) => state.ids.has(id)).length;
  const allVisible = visibleIds.length > 0 && selectedVisible === visibleIds.length;
  elements.selectAll.checked = allVisible;
  elements.selectAll.indeterminate = selectedVisible > 0 && !allVisible;
  elements.selectAll.disabled = visibleIds.length === 0;
  elements.count.textContent = `${state.ids.size} selected`;
  elements.deleteButton.disabled = state.pending || state.ids.size === 0;
  elements.selectAll.disabled = state.pending || visibleIds.length === 0;
  if (kind === 'secrets') $('#bulk-move-secrets').disabled = state.pending || state.ids.size === 0;
}

function selectionCell(kind, id) {
  const state = selectionState(kind);
  const td = document.createElement('td');
  td.className = 'selection-column';
  const checkbox = document.createElement('input');
  checkbox.type = 'checkbox';
  checkbox.checked = state.ids.has(id);
  checkbox.disabled = state.pending;
  checkbox.setAttribute('aria-label', `Select ${kind === 'secrets' ? 'secret' : 'file'} ${id}`);
  checkbox.onclick = (e) => e.stopPropagation();
  checkbox.onchange = () => {
    if (checkbox.checked) state.ids.add(id);
    else state.ids.delete(id);
    resetBulkConfirmation(kind);
    renderSelectionKind(kind);
  };
  td.onclick = (e) => e.stopPropagation();
  td.appendChild(checkbox);
  return td;
}

function toggleSelected(kind, id) {
  const state = selectionState(kind);
  if (state.pending) return;
  if (state.ids.has(id)) state.ids.delete(id);
  else state.ids.add(id);
  resetBulkConfirmation(kind);
  renderSelectionKind(kind);
}

// ---- state ----
let ctx = null;
let currentVault = null;
let secrets = [];
let secretSearchIndex = buildMetadataIndex();
let editing = null; // name of secret open in drawer, null = new
let drawerGeneration = 0;
let drawerScope = null;
// content_type + non-canonical tags of the secret open in drawer, so a value
// edit doesn't silently drop them (the form has no fields for them).
let editingMeta = null;
const CANONICAL_TAGS = new Set(['folder', 'groups', 'note', 'original_name', 'created_by']);

// Must match src/records/envelope.rs exactly.
const RECORD_CONTENT_TYPE = 'application/vnd.xv.record';
const TYPE_TAG = 'xv-type';
const FIELD_TAG_PREFIX = 'f.';

let types = []; // resolved record types from /api/types
// Non-null while the drawer holds a typed record:
// { typeName, secretFields: {name: value}, metaFields: {name: value} }
let recordState = null;
let plainSecretState = null;
let selectedGroups = [];
let conversionPreviewState = null;
let conversionOperationGeneration = 0;
let conversionLifecycleEpoch = 0;
const conversionFieldStates = new Map();
let drawerInvoker = null;
const revealTimers = new Map();
let clipboardExposure = null;
let clipboardOperation = Promise.resolve();
let latestClipboardGeneration = 0;
let nextClipboardGeneration = 0;
let exposureLifecycleEpoch = 0;
let nextExposureToken = 0;
let protectedStatusOwner = null;
let nextProtectionDescriptionId = 0;
let nextRecordFieldId = 0;

function secondsLabel(seconds) {
  return `${seconds} ${seconds === 1 ? 'second' : 'seconds'}`;
}

async function exposureTimeoutSeconds() {
  try { await preferences?.load?.(); } catch (_) { /* preference client reports its own safe error */ }
  const preferred = preferences?.get?.('exposure_timeout_seconds', null)
    ?? preferences?.snapshot?.()?.exposure_timeout_seconds;
  return Number.isSafeInteger(preferred) && preferred >= 0 ? preferred : 30;
}

function updateProtectionDescription(input, state) {
  const description = input?._protectionDescription;
  if (description) description.textContent = `Protected value is ${state.masked ? 'hidden' : 'revealed'}.`;
}

function captureExposureScope() {
  return { drawerGeneration, lifecycleEpoch: exposureLifecycleEpoch };
}

function isExposureScopeCurrent(scope) {
  return scope.drawerGeneration === drawerGeneration
    && scope.lifecycleEpoch === exposureLifecycleEpoch
    && !$('#drawer').hidden;
}

function claimProtectedStatus(scope) {
  const token = ++nextExposureToken;
  if (isExposureScopeCurrent(scope)) protectedStatusOwner = token;
  return { token, scope };
}

function setScopedProtectedStatus(owner, message) {
  if (protectedStatusOwner !== owner.token || !isExposureScopeCurrent(owner.scope)) return;
  setProtectedValueStatus(document, message);
}

function stopRevealTimer(state) {
  const exposure = revealTimers.get(state);
  exposure?.timer.cancel();
  revealTimers.delete(state);
  return exposure;
}

function hideProtectedField(state, { announceStatus = true, claimStatus = false } = {}) {
  const exposure = stopRevealTimer(state);
  if (!exposure) return;
  XvUiModel.hideProtected(state);
  renderProtectedControl(exposure.input, exposure.button, state);
  if (claimStatus) exposure.statusOwner = claimProtectedStatus(exposure.scope);
  if (announceStatus) setScopedProtectedStatus(exposure.statusOwner, `${exposure.field} hidden.`);
}

function invalidateExposureLifecycle({ announceHidden = true, clearStatus = false } = {}) {
  const fields = [...revealTimers.values()].map(({ field }) => field);
  exposureLifecycleEpoch++;
  for (const state of [...revealTimers.keys()]) hideProtectedField(state, { announceStatus: false });
  protectedStatusOwner = null;
  if (clearStatus) {
    setProtectedValueStatus(document, '');
  } else if (announceHidden && fields.length) {
    const owner = claimProtectedStatus(captureExposureScope());
    setScopedProtectedStatus(owner, `${fields.at(-1)} hidden.`);
  }
}

function startRevealTimer({ field, input, button, state, seconds, scope }) {
  stopRevealTimer(state);
  if (seconds === 0) {
    XvUiModel.hideProtected(state);
    renderProtectedControl(input, button, state);
    const owner = claimProtectedStatus(scope);
    setScopedProtectedStatus(owner, `${field} hidden.`);
    return;
  }
  const exposure = {
    field,
    input,
    button,
    state,
    scope,
    statusOwner: claimProtectedStatus(scope),
    timer: null,
  };
  exposure.timer = createExposureTimer({
    seconds,
    clock: exposureClock,
    onTick: (remaining) => {
      setScopedProtectedStatus(exposure.statusOwner, `${field} revealed. Hides in ${secondsLabel(remaining)}.`);
    },
    onExpire: () => hideProtectedField(state),
  });
  revealTimers.set(state, exposure);
}

function resetRevealTimer(state) {
  const exposure = revealTimers.get(state);
  if (!exposure) return;
  exposure.statusOwner = claimProtectedStatus(exposure.scope);
  exposure.timer.reset();
}

function bindProtectedInteractions(input, state) {
  for (const eventName of ['focus', 'pointerdown', 'keydown']) {
    input.addEventListener?.(eventName, () => resetRevealTimer(state));
  }
}

function forgetProtectedValues() {
  const states = [];
  if (plainSecretState) states.push(plainSecretState);
  for (const input of $('#record-fields').querySelectorAll('input[data-field-kind="secret"]')) {
    if (input._protectedState) states.push(input._protectedState);
  }
  for (const state of states) {
    state.revision++;
    state.value = null;
    state.hasStoredValue = false;
    state.dirty = false;
    state.loadPromise = null;
  }
  if (recordState?.secretFields) {
    for (const name of Object.keys(recordState.secretFields)) delete recordState.secretFields[name];
  }
  clearConversionFields();
  const valueInput = $('#secret-form').elements.value;
  valueInput.value = '';
  setProtectedValueStatus(document, '');
}

function queueClipboardOperation(operation) {
  const result = clipboardOperation.then(operation, operation);
  clipboardOperation = result.catch(() => {});
  return result;
}

function ownsClipboardExposure(exposure) {
  return clipboardExposure === exposure
    && latestClipboardGeneration === exposure.generation;
}

function expireClipboardExposure(exposure) {
  return queueClipboardOperation(async () => {
    const cleared = await clearClipboardIfUnchanged({
      clipboard,
      expected: exposure.expected,
      isCurrent: () => ownsClipboardExposure(exposure),
    });
    if (!ownsClipboardExposure(exposure)) return false;
    clipboardExposure = null;
    setScopedProtectedStatus(exposure.statusOwner, cleared
      ? `${exposure.field} clipboard cleared.`
      : `${exposure.field} clipboard clearing could not be confirmed.`);
    return cleared;
  });
}

function armClipboardExposure({ field, state, expected, seconds, scope, generation }) {
  clipboardExposure?.timer?.cancel();
  const exposure = {
    field,
    state,
    expected,
    scope,
    generation,
    statusOwner: latestClipboardGeneration === generation
      ? claimProtectedStatus(scope)
      : { token: -1, scope },
    timer: null,
  };
  clipboardExposure = exposure;
  if (seconds === 0) {
    void expireClipboardExposure(exposure);
    return exposure;
  }
  exposure.timer = createExposureTimer({
    seconds,
    clock: exposureClock,
    onTick: (remaining) => {
      if (!ownsClipboardExposure(exposure)) return;
      setScopedProtectedStatus(exposure.statusOwner, `${field} copied. Clipboard clears in ${secondsLabel(remaining)}.`);
    },
    onExpire: () => { void expireClipboardExposure(exposure); },
  });
  return exposure;
}

function recoverClipboardExposure(generation) {
  if (latestClipboardGeneration !== generation) return;
  if (!clipboardExposure) {
    latestClipboardGeneration = 0;
    return;
  }
  latestClipboardGeneration = clipboardExposure.generation;
  clipboardExposure.timer?.cancel();
  clipboardExposure.timer = null;
  void expireClipboardExposure(clipboardExposure);
}

async function copyProtectedValue({ field, state, expected, seconds, scope, isCurrent }) {
  const generation = ++nextClipboardGeneration;
  latestClipboardGeneration = generation;
  try {
    const result = await queueClipboardOperation(async () => {
      if (latestClipboardGeneration !== generation) return 'superseded';
      if (!isCurrent()) return 'aborted';
      await clipboard.writeText(expected);
      armClipboardExposure({ field, state, expected, seconds, scope, generation });
      return 'written';
    });
    if (result === 'aborted') recoverClipboardExposure(generation);
    return result === 'written';
  } catch (error) {
    recoverClipboardExposure(generation);
    throw error;
  }
}

function syncGroupValue() {
  $('#secret-form').elements.groups.value = selectedGroups.join(',');
}

function renderGroupEditor() {
  syncGroupValue();
  const chips = selectedGroups.map((group) => {
    const chip = document.createElement('span');
    chip.className = 'chip';
    const label = document.createElement('span');
    label.textContent = group;
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.textContent = '×';
    remove.setAttribute('aria-label', `Remove group ${group}`);
    remove.onclick = () => {
      selectedGroups = selectedGroups.filter((candidate) => candidate !== group);
      renderGroupEditor();
      updateDraft();
    };
    chip.append(label, remove);
    return chip;
  });
  $('#group-chips').replaceChildren(...chips);
  const suggestions = XvUiModel.groupSuggestions(secrets, selectedGroups);
  $('#group-suggestions').replaceChildren(...suggestions.map((group) => {
    const option = document.createElement('option');
    option.value = group;
    return option;
  }));
}

function addGroup() {
  const entry = $('#group-entry');
  const group = entry.value.trim();
  if (!group || selectedGroups.some((candidate) => candidate.toLocaleLowerCase() === group.toLocaleLowerCase())) {
    entry.value = '';
    return;
  }
  selectedGroups = [...selectedGroups, group];
  entry.value = '';
  renderGroupEditor();
  updateDraft();
}

function renderFolderSuggestions() {
  $('#folder-suggestions').replaceChildren(...folderPaths('secrets', secrets).map((path) => {
    const option = document.createElement('option');
    option.value = path;
    return option;
  }));
}

function setNoExpiry(noExpiry) {
  const input = $('#secret-form').elements.expires_on;
  if (noExpiry) input.value = '';
  input.disabled = noExpiry;
  $('#no-expiry').setAttribute('aria-pressed', String(noExpiry));
  updateDraft();
}

function renderTypeCards() {
  const cards = [
    { name: '', label: 'Plain', source: 'Built in', required: [], protected: ['value'] },
    ...XvUiModel.typeCards(types),
  ];
  $('#type-cards').replaceChildren(...cards.map((card) => {
    const label = document.createElement('label');
    label.className = 'type-card';
    const input = document.createElement('input');
    input.type = 'radio';
    input.name = 'guided_type';
    input.value = card.name;
    input.checked = $('#type-picker').value === card.name;
    const heading = document.createElement('strong');
    heading.textContent = card.label;
    const help = document.createElement('small');
    help.textContent = card.name
      ? `${card.required.length} required · ${card.protected.length} protected · ${card.source}`
      : 'One protected value';
    input.onchange = () => {
      if (!input.checked) return;
      $('#type-picker').value = card.name;
      applyTypeSelection(card.name);
    };
    label.append(input, heading, help);
    return label;
  }));
}

function applyTypeSelection(typeName) {
  if (!typeName) {
    recordState = null;
    $('#record-section').hidden = true;
    $('#value-section').hidden = false;
    $('#record-fields').innerHTML = '';
  } else {
    const type = types.find((candidate) => candidate.name === typeName);
    const draft = XvUiModel.buildTypedDraft(type, {});
    const secretFields = {};
    const metaFields = {};
    for (const field of Object.values(draft.fields)) {
      if (field.value === '') continue;
      if (field.kind === 'secret') secretFields[field.name] = field.value;
      else metaFields[field.name] = field.value;
    }
    recordState = { typeName, secretFields, metaFields };
    renderRecordFields(typeName, secretFields, metaFields, true);
  }
  updateDraft();
}

function drawerDraft() {
  const form = $('#secret-form');
  const fields = form.elements;
  const recordFields = {};
  for (const input of form.querySelectorAll('input[data-field-name]')) {
    recordFields[input.dataset.fieldName] = input.dataset.fieldKind === 'secret'
      ? input._protectedState.value
      : input.value;
  }
  return {
    name: fields.name.value,
    value: plainSecretState ? plainSecretState.value : fields.value.value,
    folder: fields.folder.value,
    groups: fields.groups.value,
    note: fields.note.value,
    expires_on: fields.expires_on.value,
    type: $('#type-picker').value,
    record: recordState ? { type: recordState.typeName, fields: recordFields } : null,
    rename: $('#rename-name').value,
    conversion: {
      target: conversionTargetBody(),
      supplied_fields: conversionSuppliedFields(),
    },
  };
}

function syncPendingDisabled(control, pending) {
  if (pending) {
    if (control.dataset.pendingDisabled === undefined) {
      control.dataset.pendingDisabled = String(control.disabled);
    }
    control.disabled = true;
  } else if (control.dataset.pendingDisabled !== undefined) {
    control.disabled = control.dataset.pendingDisabled === 'true';
    delete control.dataset.pendingDisabled;
  }
}

function syncDraftControls() {
  const snapshot = store.snapshot();
  const pending = Boolean(snapshot.savePending || snapshot.contextSwitchPending);
  for (const selector of [
    '#close-drawer', '#new-secret', '#tab-secrets', '#tab-files', '#tab-trash',
    '#save', '#delete', '#conversion-toggle', '#rename-toggle',
    '#conversion-preview', '#conversion-confirm', '#rename-submit',
  ]) {
    syncPendingDisabled($(selector), pending);
  }
  const conversionWorkflow = $('#conversion-workflow');
  if (pending) conversionWorkflow.setAttribute('inert', '');
  else conversionWorkflow.removeAttribute('inert');
  conversionWorkflow.setAttribute('aria-disabled', String(pending));
  for (const control of conversionWorkflow.querySelectorAll('input, select, button')) {
    syncPendingDisabled(control, pending);
  }
  const backdrop = $('#drawer-backdrop');
  backdrop.dataset.pending = String(pending);
  backdrop.classList.toggle('pending-disabled', pending);
  $('#drawer').setAttribute('aria-busy', String(pending));
}

function setSavePending(value) {
  store.dispatch({ type: 'draft/save-pending', value });
  const eventApi = globalThis.__TAURI__?.event;
  if (eventApi?.emit) {
    Promise.resolve(eventApi.emit('xv://save-pending-changed', Boolean(value))).catch(() => {});
  }
}

function currentContextLine() {
  return formatContextLine(store.snapshot().context || ctx);
}

function captureOperationScope() {
  return structuredClone(store.snapshot().context || ctx);
}

function scopeMatchesCurrent(scope) {
  const current = store.snapshot().context || ctx;
  return current?.workspace?.alias === scope?.workspace?.alias
    && current?.backend === scope?.backend
    && current?.vault === scope?.vault;
}

function canStartScopedAction(scope = captureOperationScope()) {
  const snapshot = store.snapshot();
  return !snapshot.contextSwitchPending
    && !snapshot.savePending
    && !snapshot.scopedMutationPending
    && scopeMatchesCurrent(scope);
}

let scopedMutationDepth = 0;
function beginScopedMutation() {
  scopedMutationDepth++;
  if (scopedMutationDepth === 1) {
    store.dispatch({ type: 'mutation/pending', value: true });
  }
}

function endScopedMutation() {
  scopedMutationDepth = Math.max(0, scopedMutationDepth - 1);
  if (scopedMutationDepth === 0) {
    store.dispatch({ type: 'mutation/pending', value: false });
  }
}

function beginDraft() {
  store.dispatch({ type: 'draft/open', draft: drawerDraft() });
}

function updateDraft() {
  if (!$('#drawer').hidden && store.snapshot().draft) {
    store.dispatch({ type: 'draft/change', draft: drawerDraft() });
  }
}

async function allowNavigation() {
  return await guardNavigation({
    draft: store.snapshot().draft,
    savePending: store.snapshot().savePending,
    confirmDiscard: () => dialogs.confirmDiscard(),
  });
}

function closeDrawer({ restoreFocus = false } = {}) {
  drawerGeneration++;
  resetConfirmation($('#delete'), 'Delete');
  invalidateExposureLifecycle({ announceHidden: false, clearStatus: true });
  forgetProtectedValues();
  if (typeof dialogs.closeModal === 'function') dialogs.closeModal($('#drawer'));
  else $('#drawer').hidden = true;
  $('#drawer-backdrop').hidden = true;
  clearDrawerState();
  store.dispatch({ type: 'draft/close' });
  const invoker = drawerInvoker;
  drawerInvoker = null;
  if (restoreFocus && typeof dialogs.closeModal !== 'function' && typeof invoker?.focus === 'function') invoker.focus();
}

async function requestDrawerClose(afterClose) {
  invalidateExposureLifecycle();
  if (!(await allowNavigation())) return false;
  if (!$('#drawer').hidden) closeDrawer({ restoreFocus: true });
  if (afterClose) await afterClose();
  return true;
}

store.subscribe((snapshot, event) => {
  if (event.type === 'context/switch-succeeded') {
    clearRefreshOwnersForScopeTransition();
  }
  syncDraftControls();
  if (snapshot.contextSwitchPending) {
    if (!$('#drawer').hidden) closeDrawer();
    else if (event.type === 'context/switch-started') {
      drawerGeneration++;
      clearDrawerState();
    }
  }
  if (event.type !== 'context/switch-succeeded') return;
  resetListDiscoveryState();
  ctx = snapshot.context;
  currentVault = ctx.vault;
  secrets = snapshot.initialSecrets;
  secretSearchIndex = buildMetadataIndex({
    secrets,
    folders: folderPaths('secrets', secrets),
  });
  secretsState = 'ready';
  hasSuccessfulSecretsSnapshot = true;
  files = [];
  filesState = 'ready';
  hasSuccessfulFilesSnapshot = false;
  folderTokenIndexes.secrets = null;
  folderTokenIndexes.files = null;
  clearSelection('secrets');
  clearSelection('files');
  secretFolderNavigation.select(XvUiModel.FOLDER_ALL);
  fileFolderNavigation.select(XvUiModel.FOLDER_ALL);
  folderNavigationFocus.secrets = {
    desktop: XvUiModel.FOLDER_ALL,
    mobile: XvUiModel.FOLDER_ALL,
  };
  folderNavigationFocus.files = {
    desktop: XvUiModel.FOLDER_ALL,
    mobile: XvUiModel.FOLDER_ALL,
  };
  applyContextCapabilities();
  clearListLoadError('secrets');
  renderSecrets();
  const generation = secretLoadGeneration;
  const scope = captureOperationScope();
  requestFolderTokenIndex('secrets', secrets, scope).then((tokenIndex) => {
    if (!tokenIndex || generation !== secretLoadGeneration || !scopeMatchesCurrent(scope)) return;
    folderTokenIndexes.secrets = tokenIndex;
    renderSecrets();
  }).catch(() => { /* folder state remains safely in memory for this session */ });
  if (ctx.capabilities.files) loadFiles(currentVault).catch(fail);
  if (activeTab === 'trash') loadDeleted(currentVault).catch(fail);
});

function setRevealLabel(button, label) {
  if (button.id === 'reveal') {
    button.replaceChildren(icon('eye'), label);
    button.setAttribute('aria-label', `${label} value`);
  }
  else {
    button.textContent = label;
    if (button.dataset.protectedField) button.setAttribute('aria-label', `${label} ${button.dataset.protectedField}`);
  }
}

function renderProtectedControl(input, button, state) {
  input.readOnly = state.masked;
  input.value = XvUiModel.protectedDisplay(state);
  setRevealLabel(button, state.masked ? 'Reveal' : 'Hide');
  updateProtectionDescription(input, state);
}

// Same rule as the TUI: the xv-type tag OR the exact record content type.
function isRecordMeta(meta) {
  return meta.content_type === RECORD_CONTENT_TYPE || !!(meta.tags || {})[TYPE_TAG];
}

// Strict, mirroring records::parse_envelope: a JSON object of strings.
function parseEnvelope(raw) {
  const obj = JSON.parse(raw);
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) throw new Error('not a JSON object');
  for (const [k, v] of Object.entries(obj)) {
    if (typeof v !== 'string') throw new Error(`field '${k}' is not a string`);
  }
  return obj;
}

function recordExposureIsCurrent({ scope, record, state, input, revision }) {
  return isExposureScopeCurrent(scope)
    && recordState === record
    && input._protectedState === state
    && state.revision === revision;
}

function fieldRow(name, kind, value, required, primary = false) {
  const field = document.createElement('div');
  field.className = 'form-field';
  const label = document.createElement('label');
  const inputId = `record-field-${++nextRecordFieldId}`;
  label.className = 'field-label';
  label.htmlFor = inputId;
  const heading = document.createElement('span');
  heading.append(name);
  if (required || kind === 'secret') {
    const hint = document.createElement('span');
    hint.className = 'field-hint';
    hint.textContent = required ? 'Required' : 'Protected';
    heading.appendChild(hint);
  }
  label.appendChild(heading);
  const input = document.createElement('input');
  input.id = inputId;
  input.dataset.fieldName = name;
  input.dataset.fieldKind = kind;
  if (required) input.required = true;
  const help = document.createElement('span');
  help.className = 'field-help';
  help.id = `${inputId}-field-help`;
  help.textContent = [
    required ? 'Required' : 'Optional',
    kind === 'secret' ? 'protected value' : 'visible metadata',
    primary ? 'primary field' : '',
  ].filter(Boolean).join(' · ');
  input.setAttribute('aria-describedby', help.id);
  if (kind === 'secret') {
    const state = XvUiModel.createProtectedState(value, value !== undefined);
    input._protectedState = state;
    input.autocomplete = 'new-password';
    const protection = document.createElement('span');
    protection.className = 'sr-only';
    protection.id = `protected-field-state-${++nextProtectionDescriptionId}`;
    protection.textContent = 'Protected value is hidden.';
    input._protectionDescription = protection;
    input.setAttribute('aria-describedby', `${help.id} ${protection.id} protected-value-status`);
    const row = document.createElement('span');
    row.className = 'field-actions';
    const rev = document.createElement('button');
    rev.type = 'button';
    rev.className = 'button secondary';
    rev.dataset.protectedField = name;
    rev.setAttribute('aria-label', `Reveal ${name}`);
    rev.setAttribute('aria-controls', inputId);
    rev.setAttribute('aria-describedby', `${protection.id} protected-value-status`);
    renderProtectedControl(input, rev, state);
    rev.onclick = async () => {
      if (state.masked) {
        const scope = captureExposureScope();
        const record = recordState;
        const revision = state.revision;
        const seconds = await exposureTimeoutSeconds();
        if (!recordExposureIsCurrent({ scope, record, state, input, revision })) return;
        XvUiModel.revealProtected(state);
        startRevealTimer({ field: name, input, button: rev, state, seconds, scope });
      } else {
        hideProtectedField(state, { claimStatus: true });
      }
      renderProtectedControl(input, rev, state);
    };
    input.oninput = () => {
      XvUiModel.editProtected(state, input.value);
      resetRevealTimer(state);
    };
    bindProtectedInteractions(input, state);
    const cp = document.createElement('button');
    cp.type = 'button';
    cp.className = 'button secondary';
    cp.setAttribute('aria-label', `Copy ${name}`);
    cp.setAttribute('aria-controls', inputId);
    cp.setAttribute('aria-describedby', `${protection.id} protected-value-status`);
    cp.textContent = 'Copy';
    cp.onclick = async () => {
      try {
        const scope = captureExposureScope();
        const record = recordState;
        const revision = state.revision;
        const seconds = await exposureTimeoutSeconds();
        if (!recordExposureIsCurrent({ scope, record, state, input, revision })) return;
        const expected = state.value ?? '';
        await copyProtectedValue({
          field: name,
          state,
          expected,
          seconds,
          scope,
          isCurrent: () => recordExposureIsCurrent({ scope, record, state, input, revision }),
        });
      }
      catch (e) { fail(e); }
    };
    row.append(rev, cp);
    field.append(label, input, help, protection, row);
  } else {
    input.value = value || '';
    field.append(label, input, help);
  }
  return field;
}

// One input per field: declared fields in CLI prompt order (non-primary
// first, primary last), then undeclared extras sorted by name.
function renderRecordFields(typeName, secretFields, metaFields, forNew) {
  const type = types.find((t) => t.name === typeName);
  $('#record-type').textContent = `type: ${typeName || '(unknown)'}`;
  const container = $('#record-fields');
  container.innerHTML = '';
  const seen = new Set();
  const declared = type
    ? [...type.fields.filter((f) => !f.primary), ...type.fields.filter((f) => f.primary)]
    : [];
  for (const def of declared) {
    seen.add(def.name);
    const value = def.kind === 'secret' ? secretFields[def.name] : metaFields[def.name];
    container.appendChild(fieldRow(def.name, def.kind, value, forNew && def.required, def.primary));
  }
  const extras = [
    ...Object.keys(secretFields).filter((n) => !seen.has(n)).map((n) => [n, 'secret']),
    ...Object.keys(metaFields).filter((n) => !seen.has(n)).map((n) => [n, 'metadata']),
  ].sort((a, b) => a[0].localeCompare(b[0]));
  for (const [n, kind] of extras) {
    container.appendChild(fieldRow(n, kind, kind === 'secret' ? secretFields[n] : metaFields[n], false));
  }
  $('#record-section').hidden = false;
  $('#value-section').hidden = true;
}

const vaultQS = (vault, scope = captureOperationScope()) => contextQuery({ ...scope, vault });

// ---- context & vaults ----
let authRecoveryActive = false;
function showAuthRecovery() {
  authRecoveryActive = true;
  $('#context-rail').hidden = false;
  $('#context-rail').classList.add('auth-recovery-mode');
  $('#vault-context').hidden = true;
  $('#vault-tabs').hidden = true;
  $('#secrets-view').hidden = true;
  $('#files-view').hidden = true;
  $('#trash-view').hidden = true;
  $('#auth-recovery').hidden = false;
}

function applyContextCapabilities() {
  $('#backend-badge').textContent = ctx.backend;
  $('#tab-files').hidden = !ctx.capabilities.files;
  $('#tab-trash').hidden = !ctx.capabilities.soft_delete;
}

async function init() {
  if (!token) {
    showAuthRecovery();
    return;
  }
  try {
    ctx = contextRail ? await contextRail.ready : await api('GET', '/api/context');
  } catch (e) {
    if (e.status === 401) {
      showAuthRecovery();
      return;
    }
    throw e;
  }
  currentVault = ctx.vault;
  applyContextCapabilities();
  ({ types } = await api('GET', '/api/types'));
  const picker = $('#type-picker');
  picker.innerHTML = '';
  const plain = document.createElement('option');
  plain.value = '';
  plain.textContent = 'plain secret';
  picker.appendChild(plain);
  for (const rt of types) {
    const opt = document.createElement('option');
    opt.value = opt.textContent = rt.name;
    picker.appendChild(opt);
  }
  picker.onchange = () => applyTypeSelection(picker.value);
  renderTypeCards();
  const conversionTarget = $('#conversion-target');
  conversionTarget.replaceChildren(...[
    ['', 'Plain'],
    ...types.map((type) => [type.name, type.name]),
  ].map(([value, label]) => {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    return option;
  }));
  if (!contextRail) {
    const { vaults } = await api('GET', '/api/vaults');
    const legacySelector = $('#vault-select');
    legacySelector.innerHTML = '';
    for (const vaultEntry of vaults) {
      const option = document.createElement('option');
      option.value = option.textContent = vaultEntry.name;
      option.selected = vaultEntry.name === currentVault;
      legacySelector.appendChild(option);
    }
    legacySelector.onchange = async () => {
      const nextVault = legacySelector.value;
      if (!(await requestDrawerClose())) {
        legacySelector.value = currentVault;
        return;
      }
      clearRefreshOwnersForScopeTransition();
      resetListDiscoveryState();
      currentVault = nextVault;
      secrets = [];
      files = [];
      hasSuccessfulSecretsSnapshot = false;
      hasSuccessfulFilesSnapshot = false;
      const selectedVault = currentVault;
      folderTokenIndexes.secrets = null;
      folderTokenIndexes.files = null;
      clearSelection('secrets');
      clearSelection('files');
      secretFolderNavigation.select(XvUiModel.FOLDER_ALL);
      fileFolderNavigation.select(XvUiModel.FOLDER_ALL);
      folderNavigationFocus.secrets = {
        desktop: XvUiModel.FOLDER_ALL,
        mobile: XvUiModel.FOLDER_ALL,
      };
      folderNavigationFocus.files = {
        desktop: XvUiModel.FOLDER_ALL,
        mobile: XvUiModel.FOLDER_ALL,
      };
      const loads = [loadSecrets(selectedVault).catch(fail)];
      if (ctx.capabilities.files) loads.push(loadFiles(selectedVault).catch(fail));
      if (activeTab === 'trash') loads.push(loadDeleted(selectedVault).catch(fail));
      await Promise.all(loads);
    };
  }
  const vault = currentVault;
  if (!(await loadSecrets(vault))) return;
  if (ctx.capabilities.files) await loadFiles(vault);
}

// ---- secrets ----
// 'ready' | 'loading' | 'failed' — guards renderSecrets so a search-box
// input during a vault switch can't paint the previous vault's rows (or
// clobber the failed placeholder) while the fetch is in flight.
let secretsState = 'ready';
let secretLoadGeneration = 0;
let secretLoadController = null;
let hasSuccessfulSecretsSnapshot = false;
async function loadSecrets(vault, scope = captureOperationScope(), errorOwner = null) {
  const generation = ++secretLoadGeneration;
  secretLoadController?.abort();
  secretLoadController = new AbortController();
  folderTokenIndexes.secrets = null;
  secretsState = 'loading';
  if (!hasSuccessfulSecretsSnapshot) {
    secrets = [];
    setListLoadStatus('secrets', 'loading');
    showListState($('#secrets-table tbody'), 'secrets', 'loading', secretSelection.enabled ? 6 : 5);
  }
  try {
    const loadedSecrets = await api(
      'GET',
      `/api/secrets${vaultQS(vault, scope)}`,
      undefined,
      false,
      { signal: secretLoadController.signal },
    );
    if (generation !== secretLoadGeneration) return false;
    let tokenIndex = null;
    try {
      tokenIndex = await requestFolderTokenIndex('secrets', loadedSecrets, scope);
    } catch (_) {
      tokenIndex = null;
    }
    if (generation !== secretLoadGeneration) return false;
    folderTokenIndexes.secrets = tokenIndex;
    secrets = loadedSecrets;
    secretSearchIndex = buildMetadataIndex({
      secrets,
      folders: folderPaths('secrets', secrets),
    });
  } catch (e) {
    if (generation !== secretLoadGeneration || isAborted(e)) return false;
    secretsState = hasSuccessfulSecretsSnapshot ? 'ready' : 'failed';
    if (!hasSuccessfulSecretsSnapshot) {
      secrets = [];
      setListLoadStatus('secrets', 'failed');
      showListState($('#secrets-table tbody'), 'secrets', 'failed', secretSelection.enabled ? 6 : 5);
    }
    if (!errorOwner || errorOwners.isCurrent(errorOwner.surface, errorOwner.generation)) {
      showListLoadError('secrets', e, vault, scope);
    }
    return false;
  }
  secretsState = 'ready';
  hasSuccessfulSecretsSnapshot = true;
  if (!errorOwner || errorOwners.isCurrent(errorOwner.surface, errorOwner.generation)) {
    clearListLoadError('secrets');
  }
  renderSecrets();
  return true;
}

$('#refresh-secrets').onclick = () => loadSecrets(currentVault, captureOperationScope());

function renderSecrets() {
  if (secretsState !== 'ready') return; // keep the loading/failed placeholder
  publishCommandMetadata();
  const query = $('#search').value;
  const tbody = $('#secrets-table tbody');
  tbody.innerHTML = '';
  const searchVisible = query.trim()
    ? searchIndex(secretSearchIndex, query)
      .filter((entry) => entry.surface === 'secrets')
      .map((entry) => secrets[entry.sourceIndex])
    : secrets;
  const filtered = XvUiModel.filterSecrets(searchVisible, listFilters.secrets);
  const folderCount = renderFolderNavigation('secrets', secrets, filtered);
  const selectedFolder = secretFolderNavigation.snapshot().selected;
  const visible = filtered.filter((secret) => (
    XvUiModel.itemMatchesFolder(secret, selectedFolder)
  ));
  const sorted = query.trim() ? visible : sortedTableItems('secrets', visible);
  filterControls.secrets.setOptions('group', [
    ...groupFilterOptions(secrets),
    listFilters.secrets.group,
  ]);
  filterControls.secrets.setOptions('type', [
    ...recordTypeFilterOptions(secrets),
    listFilters.secrets.type,
  ]);
  filterControls.secrets.render();
  $('#secret-search-clear').hidden = !query;
  setListSummary(
    'secrets',
    visible.length,
    secrets.length,
    folderCount,
  );
  const cols = secretSelection.enabled ? 6 : 5;
  for (const secret of sorted) tbody.appendChild(secretRow(secret));
  syncSelectionUi('secrets', sorted.map((secret) => secret.original_name || secret.name));
  if (!tbody.children.length) {
    showListState(tbody, 'secrets', secrets.length ? 'filtered' : 'empty', cols);
  }
}

function itemNameCell(kind, name, activate, accessibleLabel) {
  const td = document.createElement('td');
  td.classList.add('item-name');
  const content = document.createElement('div');
  content.className = 'item-name-content';
  content.appendChild(kind === 'secret' ? icon('secret') : icon('file'));
  const label = document.createElement('strong');
  label.textContent = name || '';
  content.appendChild(label);
  if (!activate) {
    td.appendChild(content);
    return td;
  }
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'item-name-content row-action';
  button.setAttribute('aria-label', accessibleLabel);
  button.replaceChildren(...content.childNodes);
  button.onclick = (event) => {
    event.stopPropagation();
    activate();
  };
  td.appendChild(button);
  return td;
}

function secretRow(s) {
  const name = s.original_name || s.name;
  const activate = () => {
    if (secretSelection.enabled) toggleSelected('secrets', name);
    else openDrawer(name);
  };
  const tr = document.createElement('tr');
  if (secretSelection.ids.has(name)) tr.classList.add('selected-row');
  if (secretSelection.enabled) tr.appendChild(selectionCell('secrets', name));
  for (const [index, cell] of [name, s.folder, s.groups, s.note, XvUiModel.formatDate(s.updated_on)].entries()) {
    if (index === 0) {
      const actionLabel = secretSelection.enabled ? `Select secret ${name}` : `Edit secret ${name}`;
      const nameCell = itemNameCell('secret', name, activate, actionLabel);
      nameCell.classList.add('column-secret-name');
      tr.appendChild(nameCell);
      continue;
    }
    const td = document.createElement('td');
    if (index === 1) td.classList.add('column-secret-folder');
    if (index === 2) td.classList.add('column-groups');
    if (index === 3) td.classList.add('column-note');
    if (index === 4) td.classList.add('column-secret-updated');
    if (index === 2 && cell) {
      const tag = document.createElement('span');
      tag.className = 'tag';
      tag.textContent = cell;
      td.appendChild(tag);
    } else {
      td.textContent = cell || '';
    }
    tr.appendChild(td);
  }
  tr.onclick = activate;
  return tr;
}

// ---- trash ----
let deletedSecrets = [];
let trashState = 'ready';
let trashLoadGeneration = 0;

function showTrashPlaceholder(title, description) {
  const tbody = $('#trash-table tbody');
  tbody.innerHTML = '';
  const row = document.createElement('tr');
  const cell = document.createElement('td');
  cell.colSpan = 4;
  const state = document.createElement('div');
  state.className = 'empty-state';
  const heading = document.createElement('strong');
  heading.textContent = title;
  const message = document.createElement('span');
  message.textContent = description;
  state.append(heading, message);
  cell.appendChild(state);
  row.appendChild(cell);
  tbody.appendChild(row);
}

async function loadDeleted(vault, scope = captureOperationScope()) {
  if (!scope.capabilities.soft_delete) return false;
  const generation = ++trashLoadGeneration;
  trashState = 'loading';
  deletedSecrets = [];
  $('#trash-item-count').textContent = 'Loading Trash…';
  showTrashPlaceholder('Loading Trash…', 'Loading recoverable secrets from the current vault…');
  try {
    const loaded = await api('GET', `/api/secrets/deleted${vaultQS(vault, scope)}`);
    if (generation !== trashLoadGeneration) return false;
    deletedSecrets = loaded;
  } catch (error) {
    if (generation !== trashLoadGeneration || isAborted(error)) return false;
    trashState = 'failed';
    showTrashPlaceholder('Couldn’t load Trash', 'The current vault Trash could not be read.');
    showError('#trash-error', error, () => loadDeleted(currentVault));
    return false;
  }
  trashState = 'ready';
  clearError('#trash-error');
  renderTrash();
  return true;
}

function renderTrash() {
  if (trashState !== 'ready') return;
  const tbody = $('#trash-table tbody');
  tbody.innerHTML = '';
  const sorted = [...deletedSecrets].sort((a, b) => (
    (a.original_name || a.name).localeCompare(b.original_name || b.name)
  ));
  $('#trash-item-count').textContent = `${sorted.length} deleted ${sorted.length === 1 ? 'secret' : 'secrets'}`;
  $('#trash-list-summary').textContent = sorted.length
    ? `${sorted.length} recoverable ${sorted.length === 1 ? 'secret' : 'secrets'}. Purge is permanent.`
    : 'Trash is empty.';
  if (!sorted.length) {
    showTrashPlaceholder('Trash is empty', 'Deleted secrets will appear here when recovery is available.');
    return;
  }
  for (const secret of sorted) tbody.appendChild(trashRow(secret));
}

function trashRow(secret) {
  const name = secret.original_name || secret.name;
  const row = document.createElement('tr');
  const nameCell = document.createElement('td');
  nameCell.className = 'item-name';
  nameCell.textContent = name;
  const deletedCell = document.createElement('td');
  deletedCell.textContent = secret.deleted_on ? `Deleted ${XvUiModel.formatDate(secret.deleted_on)}` : 'Deleted date unavailable';
  const purgeDateCell = document.createElement('td');
  purgeDateCell.textContent = secret.scheduled_purge_on
    ? XvUiModel.formatDate(secret.scheduled_purge_on)
    : (ctx.capabilities.scheduled_purge ? 'Date unavailable' : 'Not scheduled');
  const actions = document.createElement('td');
  actions.className = 'trash-actions';
  if (ctx.capabilities.restore) {
    const restore = document.createElement('button');
    restore.type = 'button';
    restore.className = 'button secondary compact';
    restore.textContent = 'Restore';
    restore.setAttribute('aria-label', `Restore ${name}`);
    restore.onclick = async () => {
      const scope = captureOperationScope();
      if (!canStartScopedAction(scope)) return;
      beginScopedMutation();
      restore.disabled = true;
      try {
        await api('POST', `/api/secrets/${encodeURIComponent(name)}/restore${vaultQS(scope.vault, scope)}`);
        toast(`${name} restored in ${formatContextLine(scope)}`);
        if (scopeMatchesCurrent(scope)) {
          await Promise.all([
            loadDeleted(scope.vault, scope),
            loadSecrets(scope.vault, scope),
          ]);
        }
      } catch (error) {
        showListError('trash', error);
        restore.disabled = false;
      } finally {
        endScopedMutation();
      }
    };
    actions.appendChild(restore);
  }
  if (ctx.capabilities.purge) {
    const purge = document.createElement('button');
    purge.type = 'button';
    purge.className = 'button danger compact';
    purge.textContent = 'Purge';
    purge.setAttribute('aria-label', `Purge ${name}`);
    purge.onclick = async () => {
      const scope = captureOperationScope();
      if (!scope.capabilities.purge || !canStartScopedAction(scope)) return;
      beginScopedMutation();
      try {
        if (!(await confirmPurge(name, scope)) || !scopeMatchesCurrent(scope)) return;
        purge.disabled = true;
        await api('DELETE', `/api/secrets/${encodeURIComponent(name)}/purge${vaultQS(scope.vault, scope)}`);
        toast(`${name} permanently purged from ${formatContextLine(scope)}`);
        if (scopeMatchesCurrent(scope)) await loadDeleted(scope.vault, scope);
      } catch (error) {
        showListError('trash', error);
        purge.disabled = false;
      } finally {
        endScopedMutation();
      }
    };
    actions.appendChild(purge);
  }
  row.append(nameCell, deletedCell, purgeDateCell, actions);
  return row;
}

function confirmPurge(name, scope) {
  if (!scope.capabilities.purge) return Promise.resolve(false);
  const dialog = $('#purge-confirmation');
  const input = $('#purge-name');
  const cancel = $('#cancel-purge');
  const confirm = $('#confirm-purge');
  $('#purge-title').textContent = `Permanently purge ${name}?`;
  $('#purge-message').textContent = `Permanently purging ${name} from ${scope.backend} vault ${scope.vault} cannot be undone.`;
  $('#purge-input-label').textContent = `Type ${name} to confirm`;
  input.setAttribute('aria-label', `Type ${name} to confirm`);
  input.value = '';
  confirm.disabled = true;
  input.oninput = () => { confirm.disabled = !canPurgeSecret(name, input.value); };
  return new Promise((resolve) => {
    let settled = false;
    const finish = (confirmed) => {
      if (settled) return;
      settled = true;
      cancel.onclick = null;
      confirm.onclick = null;
      input.oninput = null;
      dialogs.closeModal(dialog);
      resolve(confirmed);
    };
    cancel.onclick = () => finish(false);
    confirm.onclick = () => {
      if (canPurgeSecret(name, input.value)) finish(true);
    };
    dialogs.openModal(dialog, {
      initialFocus: input,
      invoker: document.activeElement,
      onEscape: () => finish(false),
    });
  });
}

$('#search').oninput = renderSecrets;
$('#secret-search-clear').onclick = () => {
  $('#search').value = '';
  renderSecrets();
  $('#search').focus();
};
$('#new-secret').onclick = (event) => openDrawer(null, event.currentTarget);

function isCurrentDrawer(generation, selection) {
  return generation === drawerGeneration && selection === editing;
}

function clearDrawerState() {
  clearConversionPreview();
  clearConversionFields();
  editing = null;
  drawerScope = null;
  editingMeta = null;
  recordState = null;
  plainSecretState = null;
  selectedGroups = [];
}

function setWorkflowOpen(kind, open) {
  const toggle = $(`#${kind}-toggle`);
  const panel = $(`#${kind}-workflow`);
  toggle.setAttribute('aria-expanded', String(open));
  panel.hidden = !open;
  if (open) {
    const other = kind === 'conversion' ? 'rename' : 'conversion';
    $(`#${other}-toggle`).setAttribute('aria-expanded', 'false');
    $(`#${other}-workflow`).hidden = true;
  }
}

function conversionTargetBody() {
  const targetType = $('#conversion-target').value;
  return targetType
    ? { kind: 'typed', target_type: targetType }
    : { kind: 'plain' };
}

function conversionSuppliedFields() {
  return Object.fromEntries(
    [...$('#conversion-required-fields').querySelectorAll('input[data-conversion-field]')]
      .map((input) => [
        input.dataset.conversionField,
        input.dataset.fieldKind === 'secret' ? input._protectedState?.value : input.value,
      ])
      .filter(([, value]) => value !== '' && value != null),
  );
}

function conversionRequestSnapshot() {
  return structuredClone({
    target: conversionTargetBody(),
    supplied_fields: conversionSuppliedFields(),
  });
}

function conversionTargetDefinition(name, targetType = $('#conversion-target').value) {
  return types.find((type) => type.name === targetType)
    ?.fields?.find((field) => field.name === name) || null;
}

function sameOperationScope(left, right) {
  return left?.workspace?.alias === right?.workspace?.alias
    && left?.backend === right?.backend
    && left?.vault === right?.vault;
}

function conversionDrawerIsCurrent(operation) {
  return operation.drawerGeneration === drawerGeneration
    && operation.selection === editing
    && !$('#drawer').hidden
    && sameOperationScope(operation.scope, drawerScope)
    && scopeMatchesCurrent(operation.scope);
}

function conversionOperationIsCurrent(operation) {
  return operation.operationGeneration === conversionOperationGeneration
    && operation.lifecycleEpoch === conversionLifecycleEpoch
    && conversionDrawerIsCurrent(operation);
}

function clearConversionPreview({ clearSummary = true } = {}) {
  conversionOperationGeneration++;
  conversionLifecycleEpoch++;
  conversionPreviewState = null;
  $('#conversion-confirm').hidden = true;
  if (clearSummary) {
    $('#conversion-summary').hidden = true;
    $('#conversion-summary').replaceChildren();
  }
}

function scrubConversionProtectedState(entry) {
  stopRevealTimer(entry.state);
  entry.state.revision++;
  entry.state.value = null;
  entry.state.hasStoredValue = false;
  entry.state.masked = false;
  entry.state.dirty = false;
  entry.state.loadPromise = null;
  entry.input.value = '';
}

function scrubConversionFields({ remove = false } = {}) {
  for (const entry of conversionFieldStates.values()) scrubConversionProtectedState(entry);
  if (remove) {
    conversionFieldStates.clear();
    $('#conversion-required-fields').replaceChildren();
  }
  // A confirmed context switch closes the draft immediately. Dispatching an
  // intermediate draft change after activation starts would look like new
  // scoped activity and correctly cancel that activation.
  if (!store.snapshot().contextSwitchPending) updateDraft();
}

function clearConversionFields() {
  scrubConversionFields({ remove: true });
}

function conversionExposureIsCurrent({ entry, revision, epoch, generation }) {
  return generation === drawerGeneration
    && epoch === conversionLifecycleEpoch
    && conversionFieldStates.get(entry.name) === entry
    && entry.input.isConnected
    && entry.state.revision === revision
    && !store.snapshot().savePending
    && !$('#drawer').hidden;
}

function conversionProtectedField(name, definition) {
  const row = document.createElement('div');
  row.className = 'form-field';
  const label = document.createElement('label');
  const inputId = `conversion-field-${++nextRecordFieldId}`;
  label.className = 'field-label';
  label.htmlFor = inputId;
  label.textContent = `${name} (required for conversion)`;
  const input = document.createElement('input');
  input.id = inputId;
  input.dataset.conversionField = name;
  input.dataset.fieldKind = 'secret';
  input.required = true;
  input.autocomplete = 'new-password';
  const state = XvUiModel.createProtectedState('', false);
  input._protectedState = state;
  const help = document.createElement('span');
  help.className = 'field-help';
  help.id = `${inputId}-help`;
  help.textContent = [
    definition?.required ? 'Required' : 'Optional',
    'protected value',
    definition?.primary ? 'primary field' : '',
  ].filter(Boolean).join(' · ');
  const protection = document.createElement('span');
  protection.className = 'sr-only';
  protection.id = `protected-field-state-${++nextProtectionDescriptionId}`;
  protection.textContent = 'Protected value is revealed.';
  input._protectionDescription = protection;
  input.setAttribute('aria-describedby', `${help.id} ${protection.id} protected-value-status`);
  const actions = document.createElement('span');
  actions.className = 'field-actions';
  const reveal = document.createElement('button');
  reveal.type = 'button';
  reveal.className = 'button secondary';
  reveal.dataset.protectedField = name;
  reveal.setAttribute('aria-controls', inputId);
  reveal.setAttribute('aria-describedby', `${protection.id} protected-value-status`);
  const copy = document.createElement('button');
  copy.type = 'button';
  copy.className = 'button secondary';
  copy.textContent = 'Copy';
  copy.setAttribute('aria-label', `Copy ${name}`);
  copy.setAttribute('aria-controls', inputId);
  copy.setAttribute('aria-describedby', `${protection.id} protected-value-status`);
  const entry = { name, row, input, state, reveal };
  conversionFieldStates.set(name, entry);
  renderProtectedControl(input, reveal, state);
  reveal.onclick = async () => {
    const generation = drawerGeneration;
    const epoch = conversionLifecycleEpoch;
    const revision = state.revision;
    const seconds = await exposureTimeoutSeconds();
    if (!conversionExposureIsCurrent({ entry, revision, epoch, generation })) return;
    if (state.masked) {
      XvUiModel.revealProtected(state);
      startRevealTimer({
        field: name,
        input,
        button: reveal,
        state,
        seconds,
        scope: captureExposureScope(),
      });
    } else {
      stopRevealTimer(state);
      XvUiModel.hideProtected(state);
      const owner = claimProtectedStatus(captureExposureScope());
      setScopedProtectedStatus(owner, `${name} hidden.`);
    }
    renderProtectedControl(input, reveal, state);
  };
  input.oninput = () => {
    if (store.snapshot().savePending) return;
    XvUiModel.editProtected(state, input.value);
    resetRevealTimer(state);
    clearConversionPreview();
    updateDraft();
  };
  bindProtectedInteractions(input, state);
  copy.onclick = async () => {
    try {
      const generation = drawerGeneration;
      const epoch = conversionLifecycleEpoch;
      const revision = state.revision;
      const seconds = await exposureTimeoutSeconds();
      if (!conversionExposureIsCurrent({ entry, revision, epoch, generation })) return;
      await copyProtectedValue({
        field: name,
        state,
        expected: state.value ?? '',
        seconds,
        scope: captureExposureScope(),
        isCurrent: () => conversionExposureIsCurrent({
          entry, revision, epoch, generation,
        }),
      });
    } catch (error) {
      showFormError(error);
    }
  };
  actions.append(reveal, copy);
  row.append(label, input, help, protection, actions);
  return input;
}

function ensureConversionField(name, targetType = $('#conversion-target').value) {
  const required = $('#conversion-required-fields');
  const existing = [...required.querySelectorAll('input[data-conversion-field]')]
    .find((input) => input.dataset.conversionField === name);
  if (existing) return existing;
  const definition = conversionTargetDefinition(name, targetType);
  if (definition?.kind === 'secret') {
    const input = conversionProtectedField(name, definition);
    required.appendChild(conversionFieldStates.get(name).row);
    updateDraft();
    return input;
  }
  const label = document.createElement('label');
  label.className = 'form-field';
  const text = document.createElement('span');
  text.className = 'field-label';
  text.textContent = `${name} (required for conversion)`;
  const input = document.createElement('input');
  input.dataset.conversionField = name;
  input.dataset.fieldKind = definition?.kind || 'metadata';
  input.required = true;
  input.oninput = () => {
    if (store.snapshot().savePending) return;
    clearConversionPreview();
    updateDraft();
  };
  label.append(text, input);
  required.appendChild(label);
  updateDraft();
  return input;
}

function renderConversionPreview(preview, operation) {
  const summary = XvUiModel.conversionSummary(preview);
  conversionPreviewState = {
    ...summary,
    request: structuredClone(operation.request),
    selection: operation.selection,
    scope: structuredClone(operation.scope),
    drawerGeneration: operation.drawerGeneration,
    lifecycleEpoch: operation.lifecycleEpoch,
    operationGeneration: operation.operationGeneration,
  };
  const container = $('#conversion-summary');
  const heading = document.createElement('strong');
  heading.textContent = summary.description;
  const list = document.createElement('ul');
  for (const [label, values] of [
    ['Dropped', summary.dropped],
    ['Exposed', summary.exposed],
    ['Renamed', summary.renamed],
  ]) {
    if (!values.length) continue;
    const item = document.createElement('li');
    item.textContent = `${label}: ${values.join(', ')}`;
    list.appendChild(item);
  }
  container.replaceChildren(heading, list);
  container.hidden = false;
  $('#conversion-confirm').hidden = false;
  for (const name of summary.missing) ensureConversionField(name);
}

async function previewConversion() {
  if (!editing || !canStartScopedAction(drawerScope)) return;
  conversionPreviewState = null;
  $('#conversion-confirm').hidden = true;
  const operation = {
    operationGeneration: ++conversionOperationGeneration,
    lifecycleEpoch: conversionLifecycleEpoch,
    drawerGeneration,
    selection: editing,
    scope: structuredClone(drawerScope),
    request: conversionRequestSnapshot(),
  };
  clearFormError();
  try {
    const preview = await api(
      'POST',
      `/api/secrets/${encodeURIComponent(operation.selection)}/conversion/preview${vaultQS(operation.scope.vault, operation.scope)}`,
      operation.request,
    );
    if (!conversionOperationIsCurrent(operation)) return;
    renderConversionPreview(preview, operation);
  } catch (error) {
    if (!conversionOperationIsCurrent(operation)) return;
    scrubConversionFields();
    if (error?.field?.startsWith('supplied_fields.')) {
      ensureConversionField(error.field.slice('supplied_fields.'.length));
    }
    showFormError(error);
  }
}

async function confirmConversion() {
  if (!editing || !conversionPreviewState || !canStartScopedAction(drawerScope)) return;
  const preview = conversionPreviewState;
  const currentRequest = conversionRequestSnapshot();
  if (JSON.stringify(currentRequest) !== JSON.stringify(preview.request)
    || preview.drawerGeneration !== drawerGeneration
    || preview.selection !== editing
    || !sameOperationScope(preview.scope, drawerScope)
    || !scopeMatchesCurrent(preview.scope)) {
    clearConversionPreview();
    return;
  }
  const operation = {
    operationGeneration: ++conversionOperationGeneration,
    lifecycleEpoch: conversionLifecycleEpoch,
    drawerGeneration,
    selection: preview.selection,
    scope: structuredClone(preview.scope),
  };
  beginScopedMutation();
  setSavePending(true);
  clearFormError();
  let applyError = null;
  try {
    await api(
      'POST',
      `/api/secrets/${encodeURIComponent(operation.selection)}/conversion${vaultQS(operation.scope.vault, operation.scope)}`,
      {
        ...structuredClone(preview.request),
        confirm_lossy: true,
        source_revision: preview.sourceRevision,
      },
    );
    if (!conversionOperationIsCurrent(operation)) return;
    closeDrawer();
    toast(`Converted ${operation.selection} in ${formatContextLine(operation.scope)}`);
    if (scopeMatchesCurrent(operation.scope)) await loadSecrets(operation.scope.vault, operation.scope);
  } catch (error) {
    if (conversionOperationIsCurrent(operation)) {
      clearConversionPreview();
      clearConversionFields();
      applyError = error;
    }
  } finally {
    setSavePending(false);
    endScopedMutation();
  }
  if (applyError && conversionDrawerIsCurrent(operation)) {
    const recoveryTargetType = preview.request.target.kind === 'typed'
      ? preview.request.target.target_type
      : '';
    $('#conversion-target').value = recoveryTargetType;
    if (applyError?.field?.startsWith('supplied_fields.')) {
      ensureConversionField(
        applyError.field.slice('supplied_fields.'.length),
        recoveryTargetType,
      );
    }
    updateDraft();
    showFormError(applyError);
  }
}

async function renameSecret() {
  if (!editing || !canStartScopedAction(drawerScope)) return;
  const newName = $('#rename-name').value.trim();
  if (!newName) return;
  const selection = editing;
  const operationScope = structuredClone(drawerScope);
  beginScopedMutation();
  setSavePending(true);
  clearFormError();
  try {
    await api(
      'POST',
      `/api/secrets/${encodeURIComponent(selection)}/rename${vaultQS(operationScope.vault, operationScope)}`,
      { new_name: newName },
    );
    closeDrawer();
    toast(`Renamed ${selection} to ${newName}`);
    if (scopeMatchesCurrent(operationScope)) await loadSecrets(operationScope.vault, operationScope);
  } catch (error) {
    if (editing === selection) showFormError(error);
  } finally {
    setSavePending(false);
    endScopedMutation();
  }
}

async function openDrawer(name, invoker = document.activeElement) {
  const scope = captureOperationScope();
  if (!canStartScopedAction(scope)) return false;
  if (!$('#drawer').hidden && !(await requestDrawerClose())) return;
  if (!canStartScopedAction(scope)) return false;
  return openDrawerNow(name, invoker, scope);
}

async function openDrawerNow(name, invoker, scope) {
  const generation = ++drawerGeneration;
  $('#drawer').hidden = true;
  clearFormError();
  resetConfirmation($('#delete'), 'Delete');
  clearDrawerState();
  editing = name;
  drawerScope = structuredClone(scope);
  const f = $('#secret-form');
  f.reset();
  f.elements.expires_on.value = '';
  f.elements.expires_on.disabled = false;
  $('#no-expiry').setAttribute('aria-pressed', 'false');
  $('#group-entry').value = '';
  selectedGroups = [];
  renderGroupEditor();
  renderFolderSuggestions();
  conversionPreviewState = null;
  $('#conversion-summary').hidden = true;
  $('#conversion-summary').replaceChildren();
  $('#conversion-required-fields').replaceChildren();
  $('#conversion-confirm').hidden = true;
  setWorkflowOpen('conversion', false);
  setWorkflowOpen('rename', false);
  plainSecretState = XvUiModel.createProtectedState(name ? null : '', !!name);
  f.elements.value._protectionDescription = $('#value-protection-state');
  renderProtectedControl(f.elements.value, $('#reveal'), plainSecretState);
  f.elements.value.oninput = () => {
    XvUiModel.editProtected(plainSecretState, f.elements.value.value);
    resetRevealTimer(plainSecretState);
  };
  $('#drawer-kicker').textContent = name ? 'Edit secret' : 'Create secret';
  $('#drawer-title').textContent = name || 'New secret';
  $('#drawer-context').textContent = formatContextLine(scope);
  $('#save').textContent = name ? 'Save changes' : 'Create secret';
  f.elements.name.value = name || '';
  f.elements.name.readOnly = !!name;
  $('#reveal').hidden = $('#copy').hidden = $('#delete').hidden = !name;
  $('#record-section').hidden = true;
  $('#value-section').hidden = false;
  $('#record-fields').innerHTML = '';
  setProtectedValueStatus(document, '');
  $('#save').disabled = false;
  $('#type-picker-label').hidden = !!name; // type is chosen at creation only
  $('#type-picker').value = '';
  renderTypeCards();
  $('#secret-workflows').hidden = !name
    || !(ctx.capabilities.conditional_conversion || ctx.capabilities.atomic_rename);
  $('#current-secret-type').hidden = !name;
  $('#current-secret-type').textContent = 'Current type: Plain';
  $('#conversion-toggle').hidden = !name || !ctx.capabilities.conditional_conversion;
  $('#rename-toggle').hidden = !name || !ctx.capabilities.atomic_rename;
  $('#rename-name').value = '';
  if (name) {
    try {
      const meta = await api('GET', `/api/secrets/${encodeURIComponent(name)}${vaultQS(scope.vault, scope)}`);
      if (generation !== drawerGeneration || !canStartScopedAction(scope)) return;
      const tags = meta.tags || {};
      f.elements.folder.value = tags.folder || '';
      selectedGroups = Array.isArray(tags.groups)
        ? tags.groups.filter(Boolean)
        : String(tags.groups || '').split(',').map((group) => group.trim()).filter(Boolean);
      renderGroupEditor();
      f.elements.note.value = tags.note || '';
      f.elements.expires_on.value = XvUiModel.expirationDate(meta.expires_on);
      const customTags = {};
      for (const [k, v] of Object.entries(tags)) {
        // xv-type and f.* are managed by the record editor, not echoed blindly.
        if (!CANONICAL_TAGS.has(k) && k !== TYPE_TAG && !k.startsWith(FIELD_TAG_PREFIX)) {
          customTags[k] = v;
        }
      }
      editingMeta = {
        content_type: meta.content_type || '',
        tags: customTags,
        enabled: meta.enabled,
        not_before: meta.not_before || null,
      };
      $('#current-secret-type').textContent = `Current type: ${isRecordMeta(meta) ? (tags[TYPE_TAG] || 'Typed') : 'Plain'}`;
      if (isRecordMeta(meta)) await openRecord(name, meta, tags, generation, scope);
      if (generation !== drawerGeneration || !canStartScopedAction(scope)) return;
    } catch (e) {
      if (generation !== drawerGeneration) return;
      // Without the fetched metadata a save would send enabled:true and no
      // custom tags — silently mutating the secret. Don't open the drawer.
      showListError('secrets', e);
      clearDrawerState();
      return;
    }
  }
  $('#drawer-backdrop').hidden = false;
  drawerInvoker = invoker;
  beginDraft();
  if (typeof dialogs.openModal === 'function') {
    dialogs.openModal($('#drawer'), {
      initialFocus: f.elements.name,
      invoker,
      onEscape: () => requestDrawerClose(),
    });
  } else {
    $('#drawer').hidden = false;
    f.elements.name.focus?.();
  }
}

// Fetches the envelope so secret fields are editable. Values live in JS
// memory but display masked — the same exposure as the Reveal button.
async function openRecord(name, meta, tags, generation, scope) {
  const { value } = await api('POST', `/api/secrets/${encodeURIComponent(name)}/value${vaultQS(scope.vault, scope)}`);
  if (generation !== drawerGeneration || !canStartScopedAction(scope)) return;
  let secretFields;
  try {
    secretFields = parseEnvelope(value ?? '');
  } catch (e) {
    if (meta.content_type !== RECORD_CONTENT_TYPE) {
      // Only an xv-type tag marked this as a record, and the value isn't
      // an envelope. Content type decides record-ness (same rule as the
      // CLI), so treat it as a plain secret: fully editable, no record UI.
      return;
    }
    // Content type says record but the value isn't a valid envelope: open
    // read-only in the plain view rather than pretending fields are empty.
    // Whole-value Reveal/Copy stay visible here (unlike the valid-record
    // path below) because they're the only diagnostic escape hatch for
    // inspecting a corrupt envelope.
    showFormError(new Error('The record data is invalid and cannot be edited safely.'));
    $('#save').disabled = true;
    return;
  }
  const metaFields = {};
  for (const [k, v] of Object.entries(tags)) {
    if (k.startsWith(FIELD_TAG_PREFIX)) metaFields[k.slice(FIELD_TAG_PREFIX.length)] = v;
  }
  recordState = { typeName: tags[TYPE_TAG] || '', secretFields, metaFields };
  // Whole-value reveal/copy would expose the raw envelope JSON.
  $('#reveal').hidden = $('#copy').hidden = true;
  renderRecordFields(recordState.typeName, secretFields, metaFields, false);
}

$('#close-drawer').onclick = () => requestDrawerClose();
$('#group-add').onclick = addGroup;
$('#group-entry').onkeydown = (event) => {
  if (event.key !== 'Enter' && event.key !== ',') return;
  event.preventDefault();
  addGroup();
};
$('#no-expiry').onclick = () => setNoExpiry(true);
$('#clear-expiry').onclick = () => setNoExpiry(false);
$('#conversion-toggle').onclick = () => setWorkflowOpen(
  'conversion',
  $('#conversion-workflow').hidden,
);
$('#rename-toggle').onclick = () => setWorkflowOpen('rename', $('#rename-workflow').hidden);
$('#conversion-target').onchange = () => {
  if (store.snapshot().savePending) return;
  clearConversionPreview();
  clearConversionFields();
  updateDraft();
};
$('#conversion-preview').onclick = previewConversion;
$('#conversion-confirm').onclick = confirmConversion;
$('#rename-submit').onclick = renameSecret;
$('#drawer-backdrop').onclick = (event) => {
  if (store.snapshot().savePending || dialogs.topModal() !== $('#drawer')) {
    event.preventDefault();
    event.stopPropagation();
    return false;
  }
  return requestDrawerClose();
};
$('#secret-form').addEventListener?.('input', updateDraft);
$('#secret-form').addEventListener?.('change', updateDraft);
for (const eventName of ['focus', 'pointerdown', 'keydown']) {
  $('#secret-form').elements.value.addEventListener?.(eventName, () => {
    if (plainSecretState) resetRevealTimer(plainSecretState);
  });
}
document.addEventListener?.('visibilitychange', () => {
  if (document.visibilityState === 'hidden') invalidateExposureLifecycle();
});
globalThis.addEventListener?.('blur', () => invalidateExposureLifecycle());

globalThis.addEventListener?.('beforeunload', (event) => {
  const allowed = guardNavigation({
    draft: store.snapshot().draft,
    savePending: store.snapshot().savePending,
    confirmDiscard: () => false,
  });
  if (allowed === false) {
    event.preventDefault();
    event.returnValue = '';
  }
});

const tauriEvents = globalThis.__TAURI__?.event;
if (tauriEvents?.listen) {
  tauriEvents.listen('xv://window-close-requested', () => requestDrawerClose(async () => {
    await tauriEvents.emit('xv://window-close-approved');
  }));
}

async function loadPlainSecretValue(generation, selection) {
  const state = plainSecretState;
  const value = await XvUiModel.loadProtected(state, async () => {
    const response = await api('POST', `/api/secrets/${encodeURIComponent(selection)}/value${vaultQS(currentVault)}`);
    return response.value ?? '';
  });
  if (!isCurrentDrawer(generation, selection) || state !== plainSecretState) return null;
  return value;
}

function plainExposureIsCurrent({ scope, generation, selection, state, revision }) {
  return isExposureScopeCurrent(scope)
    && isCurrentDrawer(generation, selection)
    && state === plainSecretState
    && state.revision === revision;
}

$('#reveal').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const state = plainSecretState;
    const revision = state.revision;
    const scope = captureExposureScope();
    if (state.masked) {
      const value = await loadPlainSecretValue(generation, selection);
      if (value === null || !plainExposureIsCurrent({ scope, generation, selection, state, revision })) return;
      const seconds = await exposureTimeoutSeconds();
      if (!plainExposureIsCurrent({ scope, generation, selection, state, revision })) return;
      XvUiModel.revealProtected(state, value);
      startRevealTimer({
        field: 'Value',
        input: $('#secret-form').elements.value,
        button: $('#reveal'),
        state,
        seconds,
        scope,
      });
    } else {
      hideProtectedField(state, { claimStatus: true });
    }
    if (!isCurrentDrawer(generation, selection)) return;
    renderProtectedControl($('#secret-form').elements.value, $('#reveal'), state);
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    showFormError(e);
  }
};

$('#copy').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const state = plainSecretState;
    const revision = state.revision;
    const scope = captureExposureScope();
    const value = await loadPlainSecretValue(generation, selection);
    if (value === null || !plainExposureIsCurrent({ scope, generation, selection, state, revision })) return;
    const seconds = await exposureTimeoutSeconds();
    if (!plainExposureIsCurrent({ scope, generation, selection, state, revision })) return;
    await copyProtectedValue({
      field: 'Value',
      state,
      expected: value,
      seconds,
      scope,
      isCurrent: () => plainExposureIsCurrent({ scope, generation, selection, state, revision }),
    });
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    showFormError(e);
  }
};

$('#secret-form').onsubmit = async (ev) => {
  ev.preventDefault();
  const operationScope = structuredClone(drawerScope || captureOperationScope());
  if (!canStartScopedAction(operationScope)) return;
  const generation = drawerGeneration;
  let selection = editing;
  const f = ev.target.elements;
  const name = f.name.value.trim();
  if (!name) return;
  clearFormError();
  invalidateExposureLifecycle();
  const groups = f.groups.value.split(',').map(s => s.trim()).filter(Boolean);
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
  beginScopedMutation();
  setSavePending(true);
  try {
    if (recordState) {
      // Records always take the full-PUT path: field edits change the value.
      const envelope = {};
      const fieldTags = {};
      for (const input of $('#record-fields').querySelectorAll('input[data-field-name]')) {
        const value = input.dataset.fieldKind === 'secret' ? input._protectedState.value : input.value;
        if (!value) continue; // empty = omit field / drop tag
        if (input.dataset.fieldKind === 'secret') envelope[input.dataset.fieldName] = value;
        else fieldTags[FIELD_TAG_PREFIX + input.dataset.fieldName] = value;
      }
      const sorted = {};
      for (const k of Object.keys(envelope).sort()) sorted[k] = envelope[k];
      const tags = { ...(editingMeta?.tags || {}), ...fieldTags };
      if (recordState.typeName) tags[TYPE_TAG] = recordState.typeName;
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(operationScope.vault, operationScope)}`, {
        value: JSON.stringify(sorted),
        content_type: RECORD_CONTENT_TYPE,
        folder: f.folder.value || null,
        note: f.note.value || null,
        groups: groups.length ? groups : null,
        expires_on: expiresPut,
        tags,
        enabled: editingMeta ? editingMeta.enabled : true,
        not_before: editingMeta?.not_before || null,
      });
    } else if (plainSecretState?.dirty || (!selection && f.value.value)) {
      // full write: value + all metadata
      const value = selection ? plainSecretState.value : f.value.value;
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(operationScope.vault, operationScope)}`, {
        value,
        folder: f.folder.value || null,
        note: f.note.value || null,
        groups: groups.length ? groups : null,
        expires_on: expiresPut,
        content_type: editingMeta?.content_type || null,
        tags: editingMeta && Object.keys(editingMeta.tags).length ? editingMeta.tags : null,
        enabled: editingMeta ? editingMeta.enabled : true,
        not_before: editingMeta?.not_before || null,
      });
    } else if (selection) {
      // metadata-only patch ("" clears)
      await api('PATCH', `/api/secrets/${encodeURIComponent(name)}${vaultQS(operationScope.vault, operationScope)}`, {
        folder: f.folder.value,
        note: f.note.value,
        groups,
        expires_on: expiresPatch,
      });
    } else {
      throw new Error('a new secret needs a value');
    }
    if (!isCurrentDrawer(generation, selection)) return;
    closeDrawer();
    toast(`Saved in ${formatContextLine(operationScope)}`);
    if (scopeMatchesCurrent(operationScope)) {
      await loadSecrets(operationScope.vault, operationScope);
    }
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    showFormError(e);
  } finally {
    setSavePending(false);
    endScopedMutation();
  }
};

$('#delete').onclick = async () => {
  const btn = $('#delete');
  const operationScope = structuredClone(drawerScope || captureOperationScope());
  if (btn.disabled || !canStartScopedAction(operationScope)) return;
  const selection = editing;
  if (!selection) return;
  const vault = operationScope.vault;
  const generation = drawerGeneration;
  beginScopedMutation();
  try {
    if (!(await confirmDeletion('secret', [selection], operationScope))
      || !scopeMatchesCurrent(operationScope)) return;
    setSavePending(true);
    beginPendingAction(btn, 'Deleting…');
    await api('DELETE', `/api/secrets/${encodeURIComponent(selection)}${vaultQS(vault, operationScope)}`);
    if (!isCurrentDrawer(generation, selection)) return;
    closeDrawer();
    showDeletionNotice([selection], operationScope);
    await loadSecrets(vault, operationScope);
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    resetConfirmation(btn, 'Delete');
    showFormError(e);
  } finally {
    setSavePending(false);
    endScopedMutation();
  }
};

// ---- tabs ----
$('#tab-secrets').onclick = () => switchTab('secrets');
$('#tab-files').onclick = () => switchTab('files');
$('#tab-trash').onclick = () => switchTab('trash');
for (const tab of [$('#tab-secrets'), $('#tab-files'), $('#tab-trash')]) {
  tab.onkeydown = async (event) => {
    const intent = shortcutIntent(event);
    if (!intent?.startsWith('tab-')) return;
    const tabs = [$('#tab-secrets'), $('#tab-files'), $('#tab-trash')].filter((candidate) => !candidate.hidden);
    const index = tabs.indexOf(tab);
    let target = null;
    if (intent === 'tab-arrowright') target = tabs[(index + 1) % tabs.length];
    if (intent === 'tab-arrowleft') target = tabs[(index - 1 + tabs.length) % tabs.length];
    if (intent === 'tab-home') target = tabs[0];
    if (intent === 'tab-end') target = tabs.at(-1);
    if (!target) return;
    event.preventDefault();
    await target.onclick();
    target.focus();
  };
}
let activeTab = 'secrets';
async function switchTab(which) {
  if (authRecoveryActive || store.snapshot().contextSwitchPending) return;
  if (which === 'trash' && !ctx.capabilities.soft_delete) return;
  if (which !== activeTab) {
    if (!(await requestDrawerClose())) return;
    clearSelection('secrets');
    clearSelection('files');
    activeTab = which;
  }
  $('#secrets-view').hidden = which !== 'secrets';
  $('#files-view').hidden = which !== 'files';
  $('#trash-view').hidden = which !== 'trash';
  $('#tab-secrets').classList.toggle('active', which === 'secrets');
  $('#tab-files').classList.toggle('active', which === 'files');
  $('#tab-trash').classList.toggle('active', which === 'trash');
  $('#tab-secrets').setAttribute('aria-selected', String(which === 'secrets'));
  $('#tab-files').setAttribute('aria-selected', String(which === 'files'));
  $('#tab-trash').setAttribute('aria-selected', String(which === 'trash'));
  $('#tab-secrets').tabIndex = which === 'secrets' ? 0 : -1;
  $('#tab-files').tabIndex = which === 'files' ? 0 : -1;
  $('#tab-trash').tabIndex = which === 'trash' ? 0 : -1;
  if (which === 'trash') await loadDeleted(currentVault);
}

// ---- files ----
let files = [];
let fileSearchIndex = buildMetadataIndex();
let filesState = 'ready';
let fileLoadGeneration = 0;
let fileLoadController = null;
let hasSuccessfulFilesSnapshot = false;

function publishCommandMetadata() {
  if (!commandRegistry) return;
  const scope = captureOperationScope();
  commandRegistry.replaceMetadata({
    secrets,
    files,
    folders: [
      ...folderPaths('secrets', secrets).map((name) => ({ name, surface: 'secrets' })),
      ...folderPaths('files', files).map((name) => ({ name, surface: 'files' })),
    ],
    scope,
    contextGeneration: scope?.version,
    dataGeneration: `${secretLoadGeneration}:${fileLoadGeneration}`,
  });
}

function resetListDiscoveryState() {
  $('#search').value = '';
  $('#file-search').value = '';
  $('#secret-search-clear').hidden = true;
  $('#file-search-clear').hidden = true;
  filterControls.secrets.reset();
  filterControls.files.reset();
  secretSearchIndex = buildMetadataIndex();
  fileSearchIndex = buildMetadataIndex();
}

async function loadFiles(vault, scope = captureOperationScope(), errorOwner = null) {
  const generation = ++fileLoadGeneration;
  fileLoadController?.abort();
  fileLoadController = new AbortController();
  if (!ctx.capabilities.files) return false;
  folderTokenIndexes.files = null;
  filesState = 'loading';
  if (!hasSuccessfulFilesSnapshot) {
    files = [];
    setListLoadStatus('files', 'loading');
    showListState($('#files-table tbody'), 'files', 'loading', fileSelection.enabled ? 5 : 4);
  }
  try {
    const loadedFiles = await api(
      'GET',
      `/api/files${vaultQS(vault, scope)}`,
      undefined,
      false,
      { signal: fileLoadController.signal },
    );
    if (generation !== fileLoadGeneration) return false;
    let tokenIndex = null;
    try {
      tokenIndex = await requestFolderTokenIndex('files', loadedFiles, scope);
    } catch (_) {
      tokenIndex = null;
    }
    if (generation !== fileLoadGeneration) return false;
    folderTokenIndexes.files = tokenIndex;
    files = loadedFiles;
    fileSearchIndex = buildMetadataIndex({
      files,
      folders: folderPaths('files', files),
    });
  } catch (e) {
    if (generation !== fileLoadGeneration || isAborted(e)) return false;
    filesState = hasSuccessfulFilesSnapshot ? 'ready' : 'failed';
    if (!hasSuccessfulFilesSnapshot) {
      files = [];
      setListLoadStatus('files', 'failed');
      showListState($('#files-table tbody'), 'files', 'failed', fileSelection.enabled ? 5 : 4);
    }
    if (!errorOwner || errorOwners.isCurrent(errorOwner.surface, errorOwner.generation)) {
      showListLoadError('files', e, vault, scope);
    }
    return false;
  }
  filesState = 'ready';
  hasSuccessfulFilesSnapshot = true;
  if (!errorOwner || errorOwners.isCurrent(errorOwner.surface, errorOwner.generation)) {
    clearListLoadError('files');
  }
  renderFiles();
  return true;
}

$('#refresh-files').onclick = () => loadFiles(currentVault, captureOperationScope());

function renderFiles() {
  if (filesState !== 'ready') return;
  publishCommandMetadata();
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  const query = $('#file-search').value;
  const searchVisible = query.trim()
    ? searchIndex(fileSearchIndex, query)
      .filter((entry) => entry.surface === 'files')
      .map((entry) => files[entry.sourceIndex])
    : files;
  const filtered = XvUiModel.filterFiles(searchVisible, listFilters.files);
  const folderCount = renderFolderNavigation('files', files, filtered);
  const selectedFolder = fileFolderNavigation.snapshot().selected;
  const visible = filtered.filter((file) => XvUiModel.itemMatchesFolder(
    { folder: folderOf('files', file) },
    selectedFolder,
  ));
  setListSummary(
    'files',
    visible.length,
    files.length,
    folderCount,
  );
  const cols = fileSelection.enabled ? 5 : 4;
  const sorted = query.trim() ? visible : sortedTableItems('files', visible);
  filterControls.files.setOptions('type', [
    ...files.map((file) => file.content_type).filter(Boolean),
    listFilters.files.type,
  ]);
  filterControls.files.render();
  $('#file-search-clear').hidden = !query;
  for (const file of sorted) tbody.appendChild(fileRow(file));
  syncSelectionUi('files', sorted.map((file) => file.name));
  if (!tbody.children.length) {
    showListState(tbody, 'files', files.length ? 'filtered' : 'empty', cols);
  }
}

$('#file-search').oninput = renderFiles;
$('#file-search-clear').onclick = () => {
  $('#file-search').value = '';
  renderFiles();
  $('#file-search').focus();
};

function fileNameCell(name) {
  if (fileSelection.enabled) {
    const cell = itemNameCell('file', name, () => toggleSelected('files', name), `Select file ${name}`);
    cell.classList.add('column-file-name');
    return cell;
  }
  const td = document.createElement('td');
  td.className = 'item-name column-file-name';
  const link = document.createElement('a');
  link.className = 'item-name-content file-link';
  link.href = `/api/files/${encodeURIComponent(name)}${vaultQS(currentVault)}`;
  link.download = name;
  link.appendChild(icon('file'));
  const label = document.createElement('strong');
  label.textContent = name;
  link.appendChild(label);
  link.onclick = (event) => { event.preventDefault(); downloadFile(name); };
  td.appendChild(link);
  return td;
}

function fileRow(f) {
  const name = f.name;
  const tr = document.createElement('tr');
  if (fileSelection.ids.has(name)) tr.classList.add('selected-row');
  if (fileSelection.enabled) tr.appendChild(selectionCell('files', name));
  for (const [index, cell] of [f.name, fmtSize(f.size), f.content_type, XvUiModel.formatDate(f.last_modified)].entries()) {
    if (index === 0) {
      tr.appendChild(fileNameCell(name));
      continue;
    }
    const td = document.createElement('td');
    if (index === 1) td.classList.add('column-file-size');
    if (index === 2) td.classList.add('column-file-type');
    if (index === 3) td.classList.add('column-file-modified');
    td.textContent = cell || '';
    tr.appendChild(td);
  }
  if (fileSelection.enabled) {
    tr.onclick = () => toggleSelected('files', name);
  }
  return tr;
}

function setVisibleSelection(kind, checked) {
  const state = selectionState(kind);
  const visibleIds = state.visibleIds;
  for (const id of visibleIds) {
    if (checked) state.ids.add(id);
    else state.ids.delete(id);
  }
  resetBulkConfirmation(kind);
  renderSelectionKind(kind);
}

async function runBounded(items, limit, operation) {
  const results = new Array(items.length);
  let next = 0;
  async function worker() {
    while (next < items.length) {
      const index = next++;
      const item = items[index];
      try {
        await operation(item);
        results[index] = { item, ok: true };
      } catch (error) {
        results[index] = { item, ok: false, error };
      }
    }
  }
  const workerCount = Math.min(limit, items.length);
  await Promise.all(Array.from({ length: workerCount }, () => worker()));
  return results;
}

let nextBulkOperationId = 0;

function setBulkOperationStatus(operationId, status, diagnostic, durable = false) {
  store.dispatch({ ...operationEvent(operationId, status, diagnostic), durable });
}

function reportBulkResults(kind, verb, results, scope, retryFailed, operationId) {
  const succeeded = results.filter((result) => result.ok).length;
  const failures = results.filter((result) => !result.ok);
  const contextLine = formatContextLine(scope);
  if (!failures.length) {
    const surface = `#${kind === 'secrets' ? 'secret' : 'file'}-error`;
    if ($(surface).dataset.source === 'bulk') clearError(surface);
    toast(`${verb} ${succeeded} item${succeeded === 1 ? '' : 's'} in ${contextLine}`);
    return;
  }
  const diagnostic = diagnosticsScope(scope, failures);
  const surface = `#${kind === 'secrets' ? 'secret' : 'file'}-error`;
  const panel = $(surface);
  panel.dataset.source = 'bulk';
  const ownerGeneration = showError(surface, {
    message: `${verb} ${succeeded} in ${contextLine}; ${failures.length} failed — ${diagnostic.failedNames.join(', ')}`,
    hint: diagnostic.hint,
  }, null, {
    operationId,
    diagnostic,
    scope: structuredClone(scope),
    failedNames: [...diagnostic.failedNames],
    retryFailed,
  });
  const retryButton = panel.querySelector('.error-retry');
  retryButton.hidden = !retryFailed;
  retryButton.textContent = 'Retry failed';
  bindOwnedRetry({
    registry: errorOwners,
    key: surface,
    generation: ownerGeneration,
    button: retryButton,
    retry: async () => {
      if (!scopeMatchesCurrent(scope)) {
        panel.querySelector('.error-hint').textContent =
          `Switch back to ${formatContextLine(scope)} to retry these exact items.`;
        return { operationId: null, retried: null };
      }
      const operationId = `bulk-${++nextBulkOperationId}`;
      setBulkOperationStatus(operationId, 'started');
      try {
        const retried = await retryFailed([...diagnostic.failedNames]);
        const remaining = retried.filter((result) => !result.ok);
        const ownerIsCurrent = errorOwners.isCurrent(surface, ownerGeneration);
        setBulkOperationStatus(
          operationId,
          operationResultStatus(retried),
          remaining.length ? diagnosticsScope(scope, remaining) : null,
          ownerIsCurrent && remaining.length > 0,
        );
        return { operationId, retried };
      } catch (error) {
        const failed = diagnostic.failedNames.map((item) => ({ item, ok: false, error }));
        const ownerIsCurrent = errorOwners.isCurrent(surface, ownerGeneration);
        setBulkOperationStatus(
          operationId,
          'failed',
          diagnosticsScope(scope, failed),
          ownerIsCurrent,
        );
        throw Object.assign(error, { operationId, failed });
      }
    },
    publish: ({ operationId, retried }) => {
      if (!operationId || !retried) return;
      reportBulkResults(kind, verb, retried, scope, retryFailed, operationId);
    },
    reject: (error) => {
      if (!error.operationId || !error.failed) return;
      reportBulkResults(kind, verb, error.failed, scope, retryFailed, error.operationId);
    },
  });
  const copyButton = panel.querySelector('.error-copy');
  copyButton.hidden = false;
  copyButton.onclick = async () => {
    try {
      await clipboard?.writeText?.(diagnosticText(diagnostic));
    } catch (_) {
      panel.querySelector('.error-hint').textContent =
        'Copy was blocked. The safe details remain visible here.';
    }
  };
}

function setBulkPending(kind, pending, label) {
  const state = selectionState(kind);
  const elements = selectionElements(kind);
  state.pending = pending;
  const singular = kind === 'secrets' ? 'secret' : 'file';
  $(`#cancel-${singular}-selection`).disabled = pending;
  if (kind === 'secrets') {
    $('#secret-move-folder').disabled = pending;
    $('#bulk-move-secrets').disabled = pending || state.ids.size === 0;
  }
  if (pending) beginPendingAction(elements.deleteButton, label);
  else resetConfirmation(elements.deleteButton, 'Delete');
  updateSelectionControls(kind);
  renderSelectionKind(kind);
}

async function bulkDelete(kind) {
  const state = selectionState(kind);
  const items = [...state.ids];
  if (!items.length || state.pending) return;
  const operationScope = captureOperationScope();
  if (!canStartScopedAction(operationScope)) return;
  const vault = operationScope.vault;
  const operationId = `bulk-${++nextBulkOperationId}`;
  setBulkOperationStatus(operationId, 'started');
  beginScopedMutation();
  try {
    if (!(await confirmDeletion(
      kind === 'secrets' ? 'secret' : 'file',
      items,
      operationScope,
    )) || !scopeMatchesCurrent(operationScope)) {
      setBulkOperationStatus(operationId, 'cancelled');
      return;
    }
    const generation = state.generation;
    setBulkPending(kind, true, 'Deleting…');
    const deleteItems = (targets) => runBounded(targets, 4, (item) => {
      if (kind === 'secrets') {
        return api('DELETE', `/api/secrets/${encodeURIComponent(item)}${vaultQS(vault, operationScope)}`);
      }
      return api('DELETE', `/api/files/${encodeURIComponent(item)}${vaultQS(vault, operationScope)}`);
    });
    const results = await deleteItems(items);
    const failures = results.filter((result) => !result.ok);
    setBulkOperationStatus(
      operationId,
      operationResultStatus(results),
      failures.length ? diagnosticsScope(operationScope, failures) : null,
      failures.length > 0,
    );
    if (!scopeMatchesCurrent(operationScope)) return;

    const selectionIsCurrent = generation === state.generation;
    if (selectionIsCurrent) {
      for (const result of results) {
        if (result.ok) state.ids.delete(result.item);
      }
      state.pending = false;
    }
    try {
      if (kind === 'secrets') await loadSecrets(vault, operationScope);
      else await loadFiles(vault, operationScope);
    } catch (e) {
      fail(e);
    }
    if (!scopeMatchesCurrent(operationScope)) return;
    if (!selectionIsCurrent || generation !== state.generation) return;
    setBulkPending(kind, false, '');
    if (kind === 'secrets') {
      showDeletionNotice(
        results.filter((result) => result.ok).map((result) => result.item),
        operationScope,
      );
      if (results.some((result) => !result.ok)) {
        reportBulkResults(kind, 'Deleted', results, operationScope, async (failedNames) => {
          const retried = await deleteItems(failedNames);
          if (scopeMatchesCurrent(operationScope)) await loadSecrets(vault, operationScope);
          return retried;
        }, operationId);
      }
    } else {
      reportBulkResults(kind, 'Deleted', results, operationScope, async (failedNames) => {
        const retried = await deleteItems(failedNames);
        if (scopeMatchesCurrent(operationScope)) await loadFiles(vault, operationScope);
        return retried;
      }, operationId);
    }
  } finally {
    endScopedMutation();
  }
}

async function bulkMoveSecrets() {
  const state = secretSelection;
  const items = [...state.ids];
  if (!items.length || state.pending) return;
  const folder = $('#secret-move-folder').value.trim();
  if (!folder) {
    showListError('secrets', { message: 'Enter a destination folder.', field: 'folder' });
    return;
  }

  const operationScope = captureOperationScope();
  if (!canStartScopedAction(operationScope)) return;
  const generation = state.generation;
  const vault = operationScope.vault;
  beginScopedMutation();
  const moveButton = $('#bulk-move-secrets');
  state.pending = true;
  $('#cancel-secret-selection').disabled = true;
  $('#secret-move-folder').disabled = true;
  $('#bulk-delete-secrets').disabled = true;
  beginPendingAction(moveButton, 'Moving…');
  renderSecrets();
  try {
    const operationId = `bulk-${++nextBulkOperationId}`;
    setBulkOperationStatus(operationId, 'started');
    const moveItems = (targets) => runBounded(targets, 4, (item) => (
      api('POST', `/api/secrets/${encodeURIComponent(item)}/move${vaultQS(vault, operationScope)}`, { folder })
    ));
    const results = await moveItems(items);
    const failures = results.filter((result) => !result.ok);
    setBulkOperationStatus(
      operationId,
      operationResultStatus(results),
      failures.length ? diagnosticsScope(operationScope, failures) : null,
      failures.length > 0,
    );
    if (!scopeMatchesCurrent(operationScope)) return;

    const selectionIsCurrent = generation === state.generation;
    if (selectionIsCurrent) {
      for (const result of results) {
        if (result.ok) state.ids.delete(result.item);
      }
      state.pending = false;
    }
    try {
      await loadSecrets(vault, operationScope);
    } catch (e) {
      fail(e);
    }
    if (!scopeMatchesCurrent(operationScope)) return;
    if (!selectionIsCurrent || generation !== state.generation) return;
    $('#cancel-secret-selection').disabled = false;
    $('#secret-move-folder').disabled = false;
    resetConfirmation(moveButton, 'Move');
    updateSelectionControls('secrets');
    renderSecrets();
    reportBulkResults('secrets', 'Moved', results, operationScope, async (failedNames) => {
      const retried = await moveItems(failedNames);
      if (scopeMatchesCurrent(operationScope)) await loadSecrets(vault, operationScope);
      return retried;
    }, operationId);
  } finally {
    endScopedMutation();
  }
}

$('#select-secrets').onclick = () => setSelectionMode('secrets', true);
$('#select-files').onclick = () => setSelectionMode('files', true);
$('#cancel-secret-selection').onclick = () => clearSelection('secrets');
$('#cancel-file-selection').onclick = () => clearSelection('files');
$('#select-all-secrets').onchange = (e) => setVisibleSelection('secrets', e.target.checked);
$('#select-all-files').onchange = (e) => setVisibleSelection('files', e.target.checked);
$('#bulk-delete-secrets').onclick = () => bulkDelete('secrets').catch(fail);
$('#bulk-delete-files').onclick = () => bulkDelete('files').catch(fail);
$('#bulk-move-secrets').onclick = () => bulkMoveSecrets().catch(fail);

async function downloadFile(name) {
  const scope = captureOperationScope();
  if (!canStartScopedAction(scope)) return;
  try {
    const res = await api('GET', `/api/files/${encodeURIComponent(name)}${vaultQS(scope.vault, scope)}`, undefined, true);
    const blob = await res.blob();
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = name;
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (e) { fail(e); }
}

async function uploadFiles(fileList) {
  const operationScope = captureOperationScope();
  if (!canStartScopedAction(operationScope)) return;
  const uploadVault = operationScope.vault;
  const uploadScope = formatContextLine(operationScope);
  beginScopedMutation();
  setSavePending(true);
  try {
    for (const file of fileList) {
      const form = new FormData();
      form.append('file', file, file.name);
      try {
        await api('POST', `/api/files${vaultQS(uploadVault, operationScope)}`, form);
        toast(`Uploaded ${file.name} to ${uploadScope}`);
      } catch (e) { fail(e); }
    }
    if (scopeMatchesCurrent(operationScope)) await loadFiles(uploadVault, operationScope);
  } finally {
    setSavePending(false);
    endScopedMutation();
  }
}

const dz = $('#dropzone');
dz.ondragover = (e) => { e.preventDefault(); dz.classList.add('over'); };
dz.ondragleave = () => dz.classList.remove('over');
dz.ondrop = (e) => { e.preventDefault(); dz.classList.remove('over'); uploadFiles(e.dataTransfer.files).catch(fail); };
$('#browse-files').onclick = () => {
  if (!canStartScopedAction()) return;
  $('#file-input').click();
};
$('#file-input').onchange = (e) => uploadFiles(e.target.files).catch(fail);

initColumnResizing();
initSorting();
initFolderNavigationControls();
init().catch(fail);
}
