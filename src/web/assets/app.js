'use strict';

// ---- auth token: read once from URL, keep in memory only, scrub the URL ----
const params = new URLSearchParams(location.search);
const TOKEN = params.get('token') || '';
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
      throw new Error(msg);
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

// Dates only, never timestamps. Unparseable strings pass through raw.
function fmtDate(s) {
  if (!s) return '';
  const d = new Date(s);
  return isNaN(d) ? s : d.toISOString().slice(0, 10);
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

// ---- state ----
let ctx = null;
let currentVault = null;
let secrets = [];
let editing = null; // name of secret open in drawer, null = new
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

const vaultQS = () => `?vault=${encodeURIComponent(currentVault)}`;

// ---- context & vaults ----
async function init() {
  ctx = await api('GET', '/api/context');
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
    // Close the drawer: anything open in it belongs to the previous vault,
    // and saving/deleting it against the new vault would hit the wrong secret.
    closeDrawer();
    loadSecrets().catch(fail);
    loadFiles().catch(fail);
  };
  await loadSecrets();
  if (ctx.capabilities.files) await loadFiles();
}

// ---- secrets ----
async function loadSecrets() {
  showPlaceholder($('#secrets-table tbody'), 'Loading secrets…', 5);
  try {
    secrets = await api('GET', `/api/secrets${vaultQS()}`);
  } catch (e) {
    showPlaceholder($('#secrets-table tbody'), 'failed to load', 5);
    throw e;
  }
  renderSecrets();
}

function renderSecrets() {
  const filter = $('#search').value.toLowerCase();
  const tbody = $('#secrets-table tbody');
  tbody.innerHTML = '';
  for (const s of secrets) {
    const name = s.original_name || s.name;
    const hay = `${name} ${s.folder || ''} ${s.groups || ''} ${s.note || ''}`.toLowerCase();
    if (filter && !hay.includes(filter)) continue;
    const tr = document.createElement('tr');
    for (const cell of [name, s.folder, s.groups, s.note, fmtDate(s.updated_on)]) {
      const td = document.createElement('td');
      td.textContent = cell || '';
      tr.appendChild(td);
    }
    tr.onclick = () => openDrawer(name);
    tbody.appendChild(tr);
  }
  if (!tbody.children.length) {
    showPlaceholder(tbody, secrets.length ? 'no matching secrets' : 'no secrets', 5);
  }
}

$('#search').oninput = renderSecrets;
$('#new-secret').onclick = () => openDrawer(null);

function closeDrawer() {
  $('#drawer').hidden = true;
  editing = null;
  editingMeta = null;
  recordState = null;
}

async function openDrawer(name) {
  editing = name;
  editingMeta = null;
  recordState = null;
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
      const meta = await api('GET', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`);
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
      if (isRecordMeta(meta)) await openRecord(name, tags);
    } catch (e) {
      // Without the fetched metadata a save would send enabled:true and no
      // custom tags — silently mutating the secret. Don't open the drawer.
      fail(e);
      editing = null;
      return;
    }
  }
  $('#drawer').hidden = false;
}

// Fetches the envelope so secret fields are editable. Values live in JS
// memory but display masked — the same exposure as the Reveal button.
async function openRecord(name, tags) {
  const { value } = await api('POST', `/api/secrets/${encodeURIComponent(name)}/value${vaultQS()}`);
  let secretFields;
  try {
    secretFields = parseEnvelope(value ?? '');
  } catch (e) {
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
  try {
    const { value } = await api('POST', `/api/secrets/${encodeURIComponent(editing)}/value${vaultQS()}`);
    $('#secret-form').elements.value.value = value ?? '';
  } catch (e) { fail(e); }
};

$('#copy').onclick = async () => {
  try {
    const { value } = await api('POST', `/api/secrets/${encodeURIComponent(editing)}/value${vaultQS()}`);
    await navigator.clipboard.writeText(value ?? '');
    toast('copied');
  } catch (e) { fail(e); }
};

$('#secret-form').onsubmit = async (ev) => {
  ev.preventDefault();
  const f = ev.target.elements;
  const name = f.name.value.trim();
  if (!name) return;
  const groups = f.groups.value.split(',').map(s => s.trim()).filter(Boolean);
  const expiresPut = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : null;
  const expiresPatch = f.expires_on.value ? `${f.expires_on.value}T00:00:00Z` : '';
  try {
    if (editing && name !== editing) {
      await api('POST', `/api/secrets/${encodeURIComponent(editing)}/move${vaultQS()}`, { new_name: name });
      editing = name;
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
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
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
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
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
    } else if (editing) {
      // metadata-only patch ("" clears)
      await api('PATCH', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
        folder: f.folder.value,
        note: f.note.value,
        groups,
        expires_on: expiresPatch,
      });
    } else {
      throw new Error('a new secret needs a value');
    }
    closeDrawer();
    toast('saved');
    await loadSecrets();
  } catch (e) { fail(e); }
};

$('#delete').onclick = async () => {
  const btn = $('#delete');
  if (btn.dataset.armed !== '1') {
    btn.dataset.armed = '1';
    btn.textContent = 'Really delete?';
    setTimeout(() => { btn.dataset.armed = ''; btn.textContent = 'Delete'; }, 3000);
    return;
  }
  try {
    await api('DELETE', `/api/secrets/${encodeURIComponent(editing)}${vaultQS()}`);
    $('#drawer').hidden = true;
    toast('deleted');
    await loadSecrets();
  } catch (e) { fail(e); }
};

// ---- tabs ----
$('#tab-secrets').onclick = () => switchTab('secrets');
$('#tab-files').onclick = () => switchTab('files');
function switchTab(which) {
  $('#secrets-view').hidden = which !== 'secrets';
  $('#files-view').hidden = which !== 'files';
  $('#tab-secrets').classList.toggle('active', which === 'secrets');
  $('#tab-files').classList.toggle('active', which === 'files');
}

// ---- files ----
async function loadFiles() {
  if (!ctx.capabilities.files) return;
  showPlaceholder($('#files-table tbody'), 'Loading files…', 5);
  let files;
  try {
    files = await api('GET', `/api/files${vaultQS()}`);
  } catch (e) {
    showPlaceholder($('#files-table tbody'), 'failed to load', 5);
    throw e;
  }
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  for (const f of files) {
    const tr = document.createElement('tr');
    const cells = [f.name, `${f.size}`, f.content_type, fmtDate(f.last_modified)];
    for (const c of cells) {
      const td = document.createElement('td');
      td.textContent = c || '';
      tr.appendChild(td);
    }
    const td = document.createElement('td');
    const dl = document.createElement('button');
    dl.textContent = '⬇';
    dl.onclick = () => downloadFile(f.name);
    const del = document.createElement('button');
    del.textContent = '✕';
    del.className = 'danger';
    del.onclick = async () => {
      try {
        await api('DELETE', `/api/files/${encodeURIComponent(f.name)}${vaultQS()}`);
        await loadFiles();
      } catch (e) { fail(e); }
    };
    td.append(dl, del);
    tr.appendChild(td);
    tbody.appendChild(tr);
  }
  if (!tbody.children.length) showPlaceholder(tbody, 'no files', 5);
}

async function downloadFile(name) {
  try {
    const res = await api('GET', `/api/files/${encodeURIComponent(name)}${vaultQS()}`, undefined, true);
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
      await api('POST', `/api/files${vaultQS()}`, form);
      toast(`uploaded ${file.name}`);
    } catch (e) { fail(e); }
  }
  await loadFiles();
}

const dz = $('#dropzone');
dz.ondragover = (e) => { e.preventDefault(); dz.classList.add('over'); };
dz.ondragleave = () => dz.classList.remove('over');
dz.ondrop = (e) => { e.preventDefault(); dz.classList.remove('over'); uploadFiles(e.dataTransfer.files); };
$('#file-input').onchange = (e) => uploadFiles(e.target.files);

init().catch(fail);
