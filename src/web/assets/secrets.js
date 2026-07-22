import * as XvUiModel from './ui-model.js';
import { guardNavigation } from './dialogs.js';

export function mountSecrets({ api, store, dialogs, token }) {

const $ = (sel) => document.querySelector(sel);
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
  $(`#${singular}-item-count`).textContent = `${visibleCount} ${noun}`;
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

function toast(msg, isError = false) {
  const t = $('#toast');
  t.replaceChildren(icon(isError ? 'alert' : 'check'), document.createTextNode(msg));
  t.className = `toast ${isError ? 'error' : 'success'}`;
  t.setAttribute('role', isError ? 'alert' : 'status');
  t.hidden = false;
  clearTimeout(t._timer);
  t._timer = setTimeout(() => { t.hidden = true; }, 4000);
}
const fail = (e) => toast(e.message, true);

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
      filtered: ['No matching secrets', 'Try a different name, folder, group, or note.'],
    },
    files: {
      failed: ['Couldn’t load files', 'The current vault could not be read.'],
      empty: ['No files yet', 'Upload the first encrypted file to this vault.'],
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

// Expanded folder groups per table. In-memory only: cleared on vault
// switch, deliberately NOT cleared on save/delete re-renders so an open
// folder stays open.
const expandedSecretFolders = new Set();
const expandedFileFolders = new Set();

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
  TABLE_WIDTHS[kind].widths = widths;
  dataColumns(kind).forEach((column, index) => { column.style.width = `${widths[index]}%`; });
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

// Renders `items` into `tbody` as collapsible folder groups (sorted,
// listed first) followed by loose items (folderOf(item) === '').
// forceExpand shows every group open without mutating `expanded` —
// used while a search filter is active.
function renderGrouped(tbody, items, folderOf, expanded, cols, renderRow, forceExpand, rerender) {
  const groups = new Map();
  const loose = [];
  const rendered = [];
  for (const it of items) {
    const f = folderOf(it);
    if (f) {
      if (!groups.has(f)) groups.set(f, []);
      groups.get(f).push(it);
    } else {
      loose.push(it);
    }
  }
  for (const name of [...groups.keys()].sort()) {
    const rows = groups.get(name);
    const open = forceExpand || expanded.has(name);
    const tr = document.createElement('tr');
    tr.className = 'folder-row';
    const td = document.createElement('td');
    td.colSpan = cols;
    td.className = 'folder-cell';
    const content = document.createElement('div');
    content.className = 'folder-cell-content';
    content.appendChild(icon(open ? 'chevron-down' : 'chevron-right'));
    content.appendChild(icon('folder'));
    const label = document.createElement('span');
    label.className = 'folder-name';
    label.textContent = name;
    const count = document.createElement('span');
    count.className = 'folder-count';
    count.textContent = `${rows.length} ${rows.length === 1 ? 'item' : 'items'}`;
    content.append(label, count);
    td.appendChild(content);
    td.setAttribute('aria-expanded', String(open));
    if (forceExpand) {
      tr.classList.add('static');
    } else {
      const toggle = () => {
        if (expanded.has(name)) expanded.delete(name);
        else expanded.add(name);
        rerender();
      };
      td.tabIndex = 0;
      td.setAttribute('role', 'button');
      tr.onclick = toggle;
      td.onkeydown = (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          if (e.key === ' ') e.preventDefault();
          toggle();
        }
      };
    }
    tr.appendChild(td);
    tbody.appendChild(tr);
    if (open) {
      for (const it of rows) {
        rendered.push(it);
        tbody.appendChild(renderRow(it, true));
      }
    }
  }
  for (const it of loose) {
    rendered.push(it);
    tbody.appendChild(renderRow(it, false));
  }
  return rendered;
}

// ---- state ----
let ctx = null;
let currentVault = null;
let secrets = [];
let editing = null; // name of secret open in drawer, null = new
let drawerGeneration = 0;
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
let drawerInvoker = null;

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
  };
}

function syncDraftControls() {
  const pending = store.snapshot().savePending;
  for (const selector of ['#close-drawer', '#drawer-backdrop', '#new-secret', '#tab-secrets', '#tab-files', '#vault-select']) {
    $(selector).disabled = pending;
  }
  $('#drawer').setAttribute('aria-busy', String(pending));
}

function setSavePending(value) {
  store.dispatch({ type: 'draft/save-pending', value });
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
  $('#drawer').hidden = true;
  $('#drawer-backdrop').hidden = true;
  clearDrawerState();
  store.dispatch({ type: 'draft/close' });
  const invoker = drawerInvoker;
  drawerInvoker = null;
  if (restoreFocus && typeof invoker?.focus === 'function') invoker.focus();
}

async function requestDrawerClose(afterClose) {
  if (!(await allowNavigation())) return false;
  if (!$('#drawer').hidden) closeDrawer({ restoreFocus: true });
  if (afterClose) await afterClose();
  return true;
}

store.subscribe(syncDraftControls);

function setRevealLabel(button, label) {
  if (button.id === 'reveal') button.replaceChildren(icon('eye'), label);
  else button.textContent = label;
}

function renderProtectedControl(input, button, state) {
  input.readOnly = state.masked;
  input.value = XvUiModel.protectedDisplay(state);
  setRevealLabel(button, state.masked ? 'Reveal' : 'Hide');
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

function fieldRow(name, kind, value, required) {
  const label = document.createElement('label');
  label.className = 'form-field';
  const heading = document.createElement('span');
  heading.className = 'field-label';
  heading.append(name);
  if (required || kind === 'secret') {
    const hint = document.createElement('span');
    hint.className = 'field-hint';
    hint.textContent = required ? 'Required' : 'Protected';
    heading.appendChild(hint);
  }
  label.appendChild(heading);
  const input = document.createElement('input');
  input.dataset.fieldName = name;
  input.dataset.fieldKind = kind;
  if (required) input.required = true;
  if (kind === 'secret') {
    const state = XvUiModel.createProtectedState(value, value !== undefined);
    input._protectedState = state;
    input.autocomplete = 'new-password';
    const row = document.createElement('span');
    row.className = 'field-actions';
    const rev = document.createElement('button');
    rev.type = 'button';
    rev.className = 'button secondary';
    renderProtectedControl(input, rev, state);
    rev.onclick = () => {
      if (state.masked) XvUiModel.revealProtected(state);
      else XvUiModel.hideProtected(state);
      renderProtectedControl(input, rev, state);
    };
    input.oninput = () => XvUiModel.editProtected(state, input.value);
    const cp = document.createElement('button');
    cp.type = 'button';
    cp.className = 'button secondary';
    cp.textContent = 'Copy';
    cp.onclick = async () => {
      try { await navigator.clipboard.writeText(state.value ?? ''); toast('copied'); }
      catch (e) { fail(e); }
    };
    row.append(rev, cp);
    label.append(input, row);
  } else {
    input.value = value || '';
    label.append(input);
  }
  return label;
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
    container.appendChild(fieldRow(def.name, def.kind, value, forNew && def.required));
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

const vaultQS = (vault) => `?vault=${encodeURIComponent(vault)}`;

// ---- context & vaults ----
let authRecoveryActive = false;
function showAuthRecovery() {
  authRecoveryActive = true;
  $('#vault-context').hidden = true;
  $('#vault-tabs').hidden = true;
  $('#secrets-view').hidden = true;
  $('#files-view').hidden = true;
  $('#auth-recovery').hidden = false;
}

async function init() {
  if (!token) {
    showAuthRecovery();
    return;
  }
  try {
    ctx = await api('GET', '/api/context');
  } catch (e) {
    if (e.status === 401) {
      showAuthRecovery();
      return;
    }
    throw e;
  }
  currentVault = ctx.vault;
  $('#backend-badge').textContent = ctx.backend;
  $('#tab-files').hidden = !ctx.capabilities.files;
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
  picker.onchange = () => {
    if (!picker.value) {
      recordState = null;
      $('#record-section').hidden = true;
      $('#value-section').hidden = false;
      $('#record-fields').innerHTML = '';
    } else {
      recordState = { typeName: picker.value, secretFields: {}, metaFields: {} };
      renderRecordFields(picker.value, {}, {}, true);
    }
    updateDraft();
  };
  const { vaults } = await api('GET', '/api/vaults');
  const sel = $('#vault-select');
  sel.innerHTML = '';
  for (const v of vaults) {
    const opt = document.createElement('option');
    opt.value = opt.textContent = v.name;
    opt.selected = v.name === currentVault;
    sel.appendChild(opt);
  }
  sel.onchange = async () => {
    const nextVault = sel.value;
    if (!(await requestDrawerClose())) {
      sel.value = currentVault;
      return;
    }
    currentVault = nextVault;
    const vault = currentVault;
    secretLoadGeneration++;
    fileLoadGeneration++;
    clearSelection('secrets');
    clearSelection('files');
    expandedSecretFolders.clear();
    expandedFileFolders.clear();
    loadSecrets(vault).catch(fail);
    loadFiles(vault).catch(fail);
  };
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
async function loadSecrets(vault) {
  const generation = ++secretLoadGeneration;
  secretsState = 'loading';
  secrets = [];
  setListLoadStatus('secrets', 'loading');
  showListState($('#secrets-table tbody'), 'secrets', 'loading', secretSelection.enabled ? 6 : 5);
  try {
    const loadedSecrets = await api('GET', `/api/secrets${vaultQS(vault)}`);
    if (generation !== secretLoadGeneration) return false;
    secrets = loadedSecrets;
  } catch (e) {
    if (generation !== secretLoadGeneration) return false;
    secretsState = 'failed';
    secrets = [];
    setListLoadStatus('secrets', 'failed');
    showListState($('#secrets-table tbody'), 'secrets', 'failed', secretSelection.enabled ? 6 : 5);
    throw e;
  }
  secretsState = 'ready';
  renderSecrets();
  return true;
}

function renderSecrets() {
  if (secretsState !== 'ready') return; // keep the loading/failed placeholder
  const filter = $('#search').value.toLowerCase();
  const tbody = $('#secrets-table tbody');
  tbody.innerHTML = '';
  const visible = secrets.filter((s) => {
    if (!filter) return true;
    const name = s.original_name || s.name;
    const hay = `${name} ${s.folder || ''} ${s.groups || ''} ${s.note || ''}`.toLowerCase();
    return hay.includes(filter);
  });
  const sorted = sortedTableItems('secrets', visible);
  const secretFolders = new Set(visible.map((secret) => secret.folder).filter(Boolean));
  setListSummary('secrets', visible.length, secrets.length, secretFolders.size);
  // While filtering, collapse state is ignored so matches are never
  // hidden inside a collapsed folder; empty groups drop out because
  // their rows are filtered before grouping.
  const cols = secretSelection.enabled ? 6 : 5;
  const rendered = renderGrouped(
    tbody,
    sorted,
    (s) => s.folder || '',
    expandedSecretFolders,
    cols,
    secretRow,
    !!filter,
    renderSecrets,
  );
  syncSelectionUi('secrets', rendered.map((s) => s.original_name || s.name));
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

function secretRow(s, grouped = false) {
  const name = s.original_name || s.name;
  const activate = () => {
    if (secretSelection.enabled) toggleSelected('secrets', name);
    else openDrawer(name);
  };
  const tr = document.createElement('tr');
  if (grouped) tr.classList.add('folder-child');
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

$('#search').oninput = renderSecrets;
$('#new-secret').onclick = (event) => openDrawer(null, event.currentTarget);

function isCurrentDrawer(generation, selection) {
  return generation === drawerGeneration && selection === editing;
}

function clearDrawerState() {
  editing = null;
  editingMeta = null;
  recordState = null;
  plainSecretState = null;
}

async function openDrawer(name, invoker = document.activeElement) {
  if (!$('#drawer').hidden && !(await requestDrawerClose())) return;
  return openDrawerNow(name, invoker);
}

async function openDrawerNow(name, invoker) {
  const generation = ++drawerGeneration;
  $('#drawer').hidden = true;
  resetConfirmation($('#delete'), 'Delete');
  clearDrawerState();
  editing = name;
  const f = $('#secret-form');
  f.reset();
  f.elements.expires_on.value = '';
  plainSecretState = XvUiModel.createProtectedState(name ? null : '', !!name);
  renderProtectedControl(f.elements.value, $('#reveal'), plainSecretState);
  f.elements.value.oninput = () => XvUiModel.editProtected(plainSecretState, f.elements.value.value);
  $('#drawer-kicker').textContent = name ? 'Edit secret' : 'Create secret';
  $('#drawer-title').textContent = name || 'New secret';
  $('#save').textContent = name ? 'Save changes' : 'Create secret';
  f.elements.name.value = name || '';
  f.elements.name.readOnly = false;
  $('#reveal').hidden = $('#copy').hidden = $('#delete').hidden = !name;
  $('#record-section').hidden = true;
  $('#value-section').hidden = false;
  $('#record-fields').innerHTML = '';
  $('#save').disabled = false;
  $('#type-picker-label').hidden = !!name; // type is chosen at creation only
  $('#type-picker').value = '';
  if (name) {
    try {
      const meta = await api('GET', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`);
      if (generation !== drawerGeneration) return;
      const tags = meta.tags || {};
      f.elements.folder.value = tags.folder || '';
      f.elements.groups.value = tags.groups || '';
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
      if (isRecordMeta(meta)) await openRecord(name, meta, tags, generation);
      if (generation !== drawerGeneration) return;
    } catch (e) {
      if (generation !== drawerGeneration) return;
      // Without the fetched metadata a save would send enabled:true and no
      // custom tags — silently mutating the secret. Don't open the drawer.
      fail(e);
      clearDrawerState();
      return;
    }
  }
  $('#drawer').hidden = false;
  $('#drawer-backdrop').hidden = false;
  drawerInvoker = invoker;
  beginDraft();
}

// Fetches the envelope so secret fields are editable. Values live in JS
// memory but display masked — the same exposure as the Reveal button.
async function openRecord(name, meta, tags, generation) {
  const { value } = await api('POST', `/api/secrets/${encodeURIComponent(name)}/value${vaultQS(currentVault)}`);
  if (generation !== drawerGeneration) return;
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
    toast(`record envelope is invalid: ${e.message}`, true);
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
$('#drawer-backdrop').onclick = () => requestDrawerClose();
$('#secret-form').addEventListener?.('input', updateDraft);
$('#secret-form').addEventListener?.('change', updateDraft);
document.addEventListener?.('keydown', (event) => {
  if (event.key === 'Escape' && !$('#drawer').hidden) {
    event.preventDefault();
    requestDrawerClose();
  }
});

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

$('#reveal').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const state = plainSecretState;
    const transition = state.revision;
    if (state.masked) {
      const value = await loadPlainSecretValue(generation, selection);
      if (!isCurrentDrawer(generation, selection) || state !== plainSecretState
        || state.revision !== transition || value === null) return;
      XvUiModel.revealProtected(state, value);
    } else {
      XvUiModel.hideProtected(state);
    }
    if (!isCurrentDrawer(generation, selection)) return;
    renderProtectedControl($('#secret-form').elements.value, $('#reveal'), state);
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    fail(e);
  }
};

$('#copy').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const state = plainSecretState;
    const value = await loadPlainSecretValue(generation, selection);
    if (!isCurrentDrawer(generation, selection) || state !== plainSecretState || value === null) return;
    await navigator.clipboard.writeText(value);
    if (!isCurrentDrawer(generation, selection)) return;
    toast('copied');
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    fail(e);
  }
};

$('#secret-form').onsubmit = async (ev) => {
  ev.preventDefault();
  if (store.snapshot().savePending) return;
  const generation = drawerGeneration;
  let selection = editing;
  const f = ev.target.elements;
  const name = f.name.value.trim();
  if (!name) return;
  const groups = f.groups.value.split(',').map(s => s.trim()).filter(Boolean);
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
  setSavePending(true);
  try {
    if (selection && name !== selection) {
      await api('POST', `/api/secrets/${encodeURIComponent(selection)}/move${vaultQS(currentVault)}`, { new_name: name });
      if (!isCurrentDrawer(generation, selection)) return;
      editing = name;
      selection = name;
    }
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
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`, {
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
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`, {
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
      await api('PATCH', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`, {
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
    toast('saved');
    await loadSecrets(currentVault);
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    fail(e);
  } finally {
    setSavePending(false);
  }
};

$('#delete').onclick = async () => {
  const btn = $('#delete');
  if (btn.disabled || store.snapshot().savePending) return;
  if (!armConfirmation(btn, 'Really delete?')) return;
  beginPendingAction(btn, 'Deleting…');
  setSavePending(true);
  const generation = drawerGeneration;
  const selection = editing;
  const vault = currentVault;
  try {
    await api('DELETE', `/api/secrets/${encodeURIComponent(selection)}${vaultQS(vault)}`);
    if (!isCurrentDrawer(generation, selection)) return;
    closeDrawer();
    toast('deleted');
    await loadSecrets(vault);
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    resetConfirmation(btn, 'Delete');
    fail(e);
  } finally {
    setSavePending(false);
  }
};

// ---- tabs ----
$('#tab-secrets').onclick = () => switchTab('secrets');
$('#tab-files').onclick = () => switchTab('files');
let activeTab = 'secrets';
async function switchTab(which) {
  if (authRecoveryActive) return;
  if (which !== activeTab) {
    if (!(await requestDrawerClose())) return;
    clearSelection('secrets');
    clearSelection('files');
    activeTab = which;
  }
  $('#secrets-view').hidden = which !== 'secrets';
  $('#files-view').hidden = which !== 'files';
  $('#tab-secrets').classList.toggle('active', which === 'secrets');
  $('#tab-files').classList.toggle('active', which === 'files');
}

// ---- files ----
let files = [];
let filesState = 'ready';
let fileLoadGeneration = 0;

async function loadFiles(vault) {
  const generation = ++fileLoadGeneration;
  if (!ctx.capabilities.files) return false;
  filesState = 'loading';
  files = [];
  setListLoadStatus('files', 'loading');
  showListState($('#files-table tbody'), 'files', 'loading', fileSelection.enabled ? 5 : 4);
  try {
    const loadedFiles = await api('GET', `/api/files${vaultQS(vault)}`);
    if (generation !== fileLoadGeneration) return false;
    files = loadedFiles;
  } catch (e) {
    if (generation !== fileLoadGeneration) return false;
    filesState = 'failed';
    files = [];
    setListLoadStatus('files', 'failed');
    showListState($('#files-table tbody'), 'files', 'failed', fileSelection.enabled ? 5 : 4);
    throw e;
  }
  filesState = 'ready';
  renderFiles();
  return true;
}

function renderFiles() {
  if (filesState !== 'ready') return;
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  const dirOf = (f) => (f.name.includes('/') ? f.name.slice(0, f.name.lastIndexOf('/')) : '');
  const fileFolders = new Set(files.map(dirOf).filter(Boolean));
  setListSummary('files', files.length, files.length, fileFolders.size);
  const cols = fileSelection.enabled ? 5 : 4;
  const sorted = sortedTableItems('files', files);
  const rendered = renderGrouped(tbody, sorted, dirOf, expandedFileFolders, cols, fileRow, false, renderFiles);
  syncSelectionUi('files', rendered.map((f) => f.name));
  if (!tbody.children.length) showListState(tbody, 'files', 'empty', cols);
}

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

function fileRow(f, grouped = false) {
  const name = f.name;
  const tr = document.createElement('tr');
  if (grouped) tr.classList.add('folder-child');
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

function reportBulkResults(verb, results) {
  const succeeded = results.filter((result) => result.ok).length;
  const failures = results.filter((result) => !result.ok);
  if (!failures.length) {
    toast(`${verb} ${succeeded} item${succeeded === 1 ? '' : 's'}`);
    return;
  }
  const details = failures
    .map(({ item, error }) => `${item}: ${error.message}`)
    .join('; ');
  toast(`${verb} ${succeeded}; ${failures.length} failed — ${details}`, true);
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
  const button = selectionElements(kind).deleteButton;
  if (kind === 'secrets') {
    if (!armConfirmation(button, `Delete ${items.length} secrets?`)) return;
  } else if (!armConfirmation(button, `Delete ${items.length} files?`)) {
    return;
  }

  const generation = state.generation;
  const vault = currentVault;
  setBulkPending(kind, true, 'Deleting…');
  const results = await runBounded(items, 4, (item) => {
    if (kind === 'secrets') {
      return api('DELETE', `/api/secrets/${encodeURIComponent(item)}${vaultQS(vault)}`);
    }
    return api('DELETE', `/api/files/${encodeURIComponent(item)}${vaultQS(vault)}`);
  });
  if (vault !== currentVault) return;

  const selectionIsCurrent = generation === state.generation;
  if (selectionIsCurrent) {
    for (const result of results) {
      if (result.ok) state.ids.delete(result.item);
    }
    state.pending = false;
  }
  try {
    if (kind === 'secrets') await loadSecrets(vault);
    else await loadFiles(vault);
  } catch (e) {
    fail(e);
  }
  if (vault !== currentVault) return;
  if (!selectionIsCurrent || generation !== state.generation) return;
  setBulkPending(kind, false, '');
  reportBulkResults('Deleted', results);
}

async function bulkMoveSecrets() {
  const state = secretSelection;
  const items = [...state.ids];
  if (!items.length || state.pending) return;
  const folder = $('#secret-move-folder').value.trim();
  if (!folder) {
    toast('enter a destination folder', true);
    return;
  }

  const generation = state.generation;
  const vault = currentVault;
  const moveButton = $('#bulk-move-secrets');
  state.pending = true;
  $('#cancel-secret-selection').disabled = true;
  $('#secret-move-folder').disabled = true;
  $('#bulk-delete-secrets').disabled = true;
  beginPendingAction(moveButton, 'Moving…');
  renderSecrets();
  const results = await runBounded(items, 4, (item) => (
    api('POST', `/api/secrets/${encodeURIComponent(item)}/move${vaultQS(vault)}`, { folder })
  ));
  if (vault !== currentVault) return;

  const selectionIsCurrent = generation === state.generation;
  if (selectionIsCurrent) {
    for (const result of results) {
      if (result.ok) state.ids.delete(result.item);
    }
    state.pending = false;
  }
  try {
    await loadSecrets(vault);
  } catch (e) {
    fail(e);
  }
  if (vault !== currentVault) return;
  if (!selectionIsCurrent || generation !== state.generation) return;
  $('#cancel-secret-selection').disabled = false;
  $('#secret-move-folder').disabled = false;
  resetConfirmation(moveButton, 'Move');
  updateSelectionControls('secrets');
  renderSecrets();
  reportBulkResults('Moved', results);
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
  try {
    const res = await api('GET', `/api/files/${encodeURIComponent(name)}${vaultQS(currentVault)}`, undefined, true);
    const blob = await res.blob();
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = name;
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (e) { fail(e); }
}

async function uploadFiles(fileList) {
  for (const file of fileList) {
    const form = new FormData();
    form.append('file', file, file.name);
    try {
      await api('POST', `/api/files${vaultQS(currentVault)}`, form);
      toast(`uploaded ${file.name}`);
    } catch (e) { fail(e); }
  }
  await loadFiles(currentVault);
}

const dz = $('#dropzone');
dz.ondragover = (e) => { e.preventDefault(); dz.classList.add('over'); };
dz.ondragleave = () => dz.classList.remove('over');
dz.ondrop = (e) => { e.preventDefault(); dz.classList.remove('over'); uploadFiles(e.dataTransfer.files).catch(fail); };
$('#browse-files').onclick = () => $('#file-input').click();
$('#file-input').onchange = (e) => uploadFiles(e.target.files).catch(fail);

initColumnResizing();
initSorting();
init().catch(fail);
}
