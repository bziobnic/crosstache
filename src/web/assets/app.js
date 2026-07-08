'use strict';

// ---- auth token: read once from URL, keep in memory only, scrub the URL ----
const params = new URLSearchParams(location.search);
const TOKEN = params.get('token') || '';
if (params.has('token')) history.replaceState(null, '', location.pathname);

async function api(method, path, body, raw = false) {
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

// ---- state ----
let ctx = null;
let currentVault = null;
let secrets = [];
let editing = null; // name of secret open in drawer, null = new
// content_type + non-canonical tags of the secret open in drawer, so a value
// edit doesn't silently drop them (the form has no fields for them).
let editingMeta = null;
const CANONICAL_TAGS = new Set(['folder', 'groups', 'note', 'original_name', 'created_by']);

const vaultQS = () => `?vault=${encodeURIComponent(currentVault)}`;

// ---- context & vaults ----
async function init() {
  ctx = await api('GET', '/api/context');
  currentVault = ctx.vault;
  $('#backend-badge').textContent = ctx.backend;
  $('#tab-files').hidden = !ctx.capabilities.files;
  const { vaults } = await api('GET', '/api/vaults');
  const sel = $('#vault-select');
  sel.innerHTML = '';
  for (const v of vaults) {
    const opt = document.createElement('option');
    opt.value = opt.textContent = v.name;
    opt.selected = v.name === currentVault;
    sel.appendChild(opt);
  }
  sel.onchange = () => { currentVault = sel.value; loadSecrets(); loadFiles(); };
  await loadSecrets();
  if (ctx.capabilities.files) await loadFiles();
}

// ---- secrets ----
async function loadSecrets() {
  secrets = await api('GET', `/api/secrets${vaultQS()}`);
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
    for (const cell of [name, s.folder, s.groups, s.note, s.updated_on]) {
      const td = document.createElement('td');
      td.textContent = cell || '';
      tr.appendChild(td);
    }
    tr.onclick = () => openDrawer(name);
    tbody.appendChild(tr);
  }
}

$('#search').oninput = renderSecrets;
$('#new-secret').onclick = () => openDrawer(null);

async function openDrawer(name) {
  editing = name;
  editingMeta = null;
  const f = $('#secret-form');
  f.reset();
  $('#drawer-title').textContent = name ? `Edit: ${name}` : 'New secret';
  f.elements.name.value = name || '';
  f.elements.name.readOnly = false;
  $('#reveal').hidden = $('#copy').hidden = $('#delete').hidden = !name;
  if (name) {
    try {
      const meta = await api('GET', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`);
      const tags = meta.tags || {};
      f.elements.folder.value = tags.folder || '';
      f.elements.groups.value = tags.groups || '';
      f.elements.note.value = tags.note || '';
      f.elements.expires_on.value = meta.expires_on || '';
      const customTags = {};
      for (const [k, v] of Object.entries(tags)) {
        if (!CANONICAL_TAGS.has(k)) customTags[k] = v;
      }
      editingMeta = {
        content_type: meta.content_type || '',
        tags: customTags,
        enabled: meta.enabled,
        not_before: meta.not_before || null,
      };
    } catch (e) { fail(e); }
  }
  $('#drawer').hidden = false;
}

$('#close-drawer').onclick = () => { $('#drawer').hidden = true; };

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
  try {
    if (editing && name !== editing) {
      await api('POST', `/api/secrets/${encodeURIComponent(editing)}/move${vaultQS()}`, { new_name: name });
      editing = name;
    }
    if (f.value.value) {
      // full write: value + all metadata
      await api('PUT', `/api/secrets/${encodeURIComponent(name)}${vaultQS()}`, {
        value: f.value.value,
        folder: f.folder.value || null,
        note: f.note.value || null,
        groups: groups.length ? groups : null,
        expires_on: f.expires_on.value || null,
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
        expires_on: f.expires_on.value,
      });
    } else {
      throw new Error('a new secret needs a value');
    }
    $('#drawer').hidden = true;
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
  const files = await api('GET', `/api/files${vaultQS()}`);
  const tbody = $('#files-table tbody');
  tbody.innerHTML = '';
  for (const f of files) {
    const tr = document.createElement('tr');
    const cells = [f.name, `${f.size}`, f.content_type, f.last_modified];
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
