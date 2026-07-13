'use strict';

// ---- auth token: persist per tab for reloads, scrub the URL ----
const TOKEN_STORAGE_KEY = 'xv.ui.token';
const params = new URLSearchParams(location.search);
const queryToken = params.get('token') || '';
if (queryToken) sessionStorage.setItem(TOKEN_STORAGE_KEY, queryToken);
const TOKEN = queryToken || sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
if (params.has('token')) history.replaceState(null, '', location.pathname);

let inflight = 0;
async function api(method, path, body, raw = false) {
  inflight++;
  document.getElementById('progress').hidden = false;
  try {
    const opts = { method, headers: { Authorization: `Bearer ${TOKEN}` } };
    if (body instanceof FormData) {
      opts.body = body;
    } else if (body !== undefined) {
      opts.headers['Content-Type'] = 'application/json';
      opts.body = JSON.stringify(body);
    }
    const res = await fetch(path, opts);
    if (!res.ok) {
      let msg = res.statusText;
      try { msg = (await res.json()).error || msg; } catch { /* not json */ }
      const error = new Error(msg);
      error.status = res.status;
      throw error;
    }
    if (raw) return res;
    const text = await res.text();
    return text ? JSON.parse(text) : null;
  } finally {
    if (--inflight <= 0) { inflight = 0; document.getElementById('progress').hidden = true; }
  }
}

const $ = (sel) => document.querySelector(sel);
function toast(msg, isError = false) {
  const t = $('#toast');
  t.textContent = msg;
  t.className = isError ? 'error' : '';
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
  button.textContent = label;
}

function beginPendingAction(button, label) {
  clearTimeout(button._confirmTimer);
  button._confirmTimer = null;
  button.dataset.armed = '';
  button.disabled = true;
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

// Dates only, never timestamps. Unparseable strings pass through raw.
function fmtDate(s) {
  if (!s) return '';
  const d = new Date(s);
  return isNaN(d) ? s : d.toISOString().slice(0, 10);
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

function showPlaceholder(tbody, text, cols) {
  tbody.innerHTML = '';
  const tr = document.createElement('tr');
  const td = document.createElement('td');
  td.colSpan = cols;
  td.className = 'placeholder';
  td.textContent = text;
  tr.appendChild(td);
  tbody.appendChild(tr);
}

// Expanded folder groups per table. In-memory only: cleared on vault
// switch, deliberately NOT cleared on save/delete re-renders so an open
// folder stays open.
const expandedSecretFolders = new Set();
const expandedFileFolders = new Set();

// Renders `items` into `tbody` as collapsible folder groups (sorted,
// listed first) followed by loose items (folderOf(item) === '').
// forceExpand shows every group open without mutating `expanded` —
// used while a search filter is active.
function renderGrouped(tbody, items, folderOf, expanded, cols, renderRow, forceExpand, rerender) {
  const groups = new Map();
  const loose = [];
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
    td.textContent = `${open ? '▾' : '▸'} ${name} (${rows.length})`;
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
    if (open) for (const it of rows) tbody.appendChild(renderRow(it));
  }
  for (const it of loose) tbody.appendChild(renderRow(it));
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
  label.append(`${name}${required ? ' *' : ''}`);
  const input = document.createElement('input');
  input.dataset.fieldName = name;
  input.dataset.fieldKind = kind;
  input.value = value || '';
  if (required) input.required = true;
  if (kind === 'secret') {
    input.type = 'password';
    input.autocomplete = 'new-password';
    const row = document.createElement('span');
    row.className = 'row';
    const rev = document.createElement('button');
    rev.type = 'button';
    rev.textContent = 'Reveal';
    rev.onclick = () => {
      const showing = input.type === 'text';
      input.type = showing ? 'password' : 'text';
      rev.textContent = showing ? 'Reveal' : 'Hide';
    };
    const cp = document.createElement('button');
    cp.type = 'button';
    cp.textContent = 'Copy';
    cp.onclick = async () => {
      try {
        await navigator.clipboard.writeText(input.value);
        toast('copied');
      } catch (e) { fail(e); }
    };
    row.append(rev, cp);
    label.append(input, row);
  } else {
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
  $('#secrets-view').hidden = true;
  $('#files-view').hidden = true;
  $('#auth-recovery').hidden = false;
}

async function init() {
  if (!TOKEN) {
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
  sel.onchange = () => {
    currentVault = sel.value;
    const vault = currentVault;
    secretLoadGeneration++;
    fileLoadGeneration++;
    fileActionGeneration++;
    // Close the drawer: anything open in it belongs to the previous vault,
    // and saving/deleting it against the new vault would hit the wrong secret.
    closeDrawer();
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
  showPlaceholder($('#secrets-table tbody'), 'Loading secrets…', 5);
  try {
    const loadedSecrets = await api('GET', `/api/secrets${vaultQS(vault)}`);
    if (generation !== secretLoadGeneration) return false;
    secrets = loadedSecrets;
  } catch (e) {
    if (generation !== secretLoadGeneration) return false;
    secretsState = 'failed';
    secrets = [];
    showPlaceholder($('#secrets-table tbody'), 'failed to load', 5);
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
  // While filtering, collapse state is ignored so matches are never
  // hidden inside a collapsed folder; empty groups drop out because
  // their rows are filtered before grouping.
  renderGrouped(tbody, visible, (s) => s.folder || '', expandedSecretFolders, 5, secretRow, !!filter, renderSecrets);
  if (!tbody.children.length) {
    showPlaceholder(tbody, secrets.length ? 'no matching secrets' : 'no secrets', 5);
  }
}

function secretRow(s) {
  const name = s.original_name || s.name;
  const tr = document.createElement('tr');
  for (const cell of [name, s.folder, s.groups, s.note, fmtDate(s.updated_on)]) {
    const td = document.createElement('td');
    td.textContent = cell || '';
    tr.appendChild(td);
  }
  tr.onclick = () => openDrawer(name);
  return tr;
}

$('#search').oninput = renderSecrets;
$('#new-secret').onclick = () => openDrawer(null);

function isCurrentDrawer(generation, selection) {
  return generation === drawerGeneration && selection === editing;
}

function clearDrawerState() {
  editing = null;
  editingMeta = null;
  recordState = null;
}

function closeDrawer() {
  drawerGeneration++;
  resetConfirmation($('#delete'), 'Delete');
  $('#drawer').hidden = true;
  clearDrawerState();
}

async function openDrawer(name) {
  const generation = ++drawerGeneration;
  $('#drawer').hidden = true;
  resetConfirmation($('#delete'), 'Delete');
  clearDrawerState();
  editing = name;
  const f = $('#secret-form');
  f.reset();
  $('#drawer-title').textContent = name ? `Edit: ${name}` : 'New secret';
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
      // Use the stored literal date on purpose, not fmtDate: fmtDate's
      // toISOString conversion could shift the date across a timezone boundary.
      f.elements.expires_on.value = meta.expires_on ? meta.expires_on.slice(0, 10) : '';
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

$('#close-drawer').onclick = closeDrawer;

$('#reveal').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const { value } = await api('POST', `/api/secrets/${encodeURIComponent(selection)}/value${vaultQS(currentVault)}`);
    if (!isCurrentDrawer(generation, selection)) return;
    $('#secret-form').elements.value.value = value ?? '';
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    fail(e);
  }
};

$('#copy').onclick = async () => {
  const generation = drawerGeneration;
  const selection = editing;
  try {
    const { value } = await api('POST', `/api/secrets/${encodeURIComponent(selection)}/value${vaultQS(currentVault)}`);
    if (!isCurrentDrawer(generation, selection)) return;
    await navigator.clipboard.writeText(value ?? '');
    if (!isCurrentDrawer(generation, selection)) return;
    toast('copied');
  } catch (e) {
    if (!isCurrentDrawer(generation, selection)) return;
    fail(e);
  }
};

$('#secret-form').onsubmit = async (ev) => {
  ev.preventDefault();
  const generation = drawerGeneration;
  let selection = editing;
  const f = ev.target.elements;
  const name = f.name.value.trim();
  if (!name) return;
  const groups = f.groups.value.split(',').map(s => s.trim()).filter(Boolean);
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
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
        if (!input.value) continue; // empty = omit field / drop tag
        if (input.dataset.fieldKind === 'secret') envelope[input.dataset.fieldName] = input.value;
        else fieldTags[FIELD_TAG_PREFIX + input.dataset.fieldName] = input.value;
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
    } else if (f.value.value) {
      // full write: value + all metadata
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS(currentVault)}`, {
        value: f.value.value,
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
  }
};

$('#delete').onclick = async () => {
  const btn = $('#delete');
  if (btn.disabled) return;
  if (!armConfirmation(btn, 'Really delete?')) return;
  beginPendingAction(btn, 'Deleting…');
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
  }
};

// ---- tabs ----
$('#tab-secrets').onclick = () => switchTab('secrets');
$('#tab-files').onclick = () => switchTab('files');
function switchTab(which) {
  if (authRecoveryActive) return;
  $('#secrets-view').hidden = which !== 'secrets';
  $('#files-view').hidden = which !== 'files';
  $('#tab-secrets').classList.toggle('active', which === 'secrets');
  $('#tab-files').classList.toggle('active', which === 'files');
}

// ---- files ----
let files = [];
let fileLoadGeneration = 0;
let fileActionGeneration = 0;
async function loadFiles(vault) {
  const generation = ++fileLoadGeneration;
  if (!ctx.capabilities.files) return false;
  showPlaceholder($('#files-table tbody'), 'Loading files…', 5);
  try {
    const loadedFiles = await api('GET', `/api/files${vaultQS(vault)}`);
    if (generation !== fileLoadGeneration) return false;
    files = loadedFiles;
  } catch (e) {
    if (generation !== fileLoadGeneration) return false;
    files = [];
    showPlaceholder($('#files-table tbody'), 'failed to load', 5);
    throw e;
  }
  renderFiles();
  return true;
}

function renderFiles() {
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  const dirOf = (f) => (f.name.includes('/') ? f.name.slice(0, f.name.lastIndexOf('/')) : '');
  renderGrouped(tbody, files, dirOf, expandedFileFolders, 5, fileRow, false, renderFiles);
  if (!tbody.children.length) showPlaceholder(tbody, 'no files', 5);
}

function isCurrentFileAction(generation, vault) {
  return generation === fileActionGeneration && vault === currentVault;
}

function fileRow(f) {
  const tr = document.createElement('tr');
  const cells = [f.name, fmtSize(f.size), f.content_type, fmtDate(f.last_modified)];
  for (const c of cells) {
    const td = document.createElement('td');
    td.textContent = c || '';
    tr.appendChild(td);
  }
  const td = document.createElement('td');
  td.className = 'file-actions';
  const dl = document.createElement('button');
  dl.textContent = 'Download';
  dl.onclick = () => downloadFile(f.name);
  const del = document.createElement('button');
  del.textContent = 'Delete';
  del.dataset.defaultLabel = 'Delete';
  del.className = 'danger';
  del.onclick = async () => {
    if (del.disabled) return;
    if (!armConfirmation(del, 'Really delete?')) return;
    beginPendingAction(del, 'Deleting…');
    const generation = fileActionGeneration;
    const vault = currentVault;
    const name = f.name;
    try {
      await api('DELETE', `/api/files/${encodeURIComponent(name)}${vaultQS(vault)}`);
      if (!isCurrentFileAction(generation, vault)) return;
      await loadFiles(vault);
    } catch (e) {
      if (!isCurrentFileAction(generation, vault)) return;
      if (!del.isConnected) return;
      resetConfirmation(del, 'Delete');
      fail(e);
    }
  };
  td.append(dl, del);
  tr.appendChild(td);
  return tr;
}

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
$('#file-input').onchange = (e) => uploadFiles(e.target.files).catch(fail);

init().catch(fail);
