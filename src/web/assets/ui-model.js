const PROTECTED_MASK = '***************';
const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true });

  function formatDate(value) {
    if (!value) return '';
    if (typeof value === 'string' && /^[0-9]{4}-[0-9]{2}-[0-9]{2}/.test(value)) return value.slice(0, 10);
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? String(value) : date.toISOString().slice(0, 10);
  }
  function expirationDate(value) {
    return typeof value === 'string' && value.length >= 10 ? value.slice(0, 10) : '';
  }
  function typeCards(types = []) {
    return types.map((type) => {
      const fields = (type.fields || []).map((field) => ({ ...field }));
      return {
        name: type.name,
        label: type.name,
        source: type.source || '',
        fields,
        required: fields.filter((field) => field.required).map((field) => field.name),
        protected: fields.filter((field) => field.kind === 'secret').map((field) => field.name),
        primary: fields.find((field) => field.primary)?.name || null,
      };
    });
  }
  function buildTypedDraft(type, properties = {}) {
    const tags = { ...(properties.tags || {}) };
    const protectedValues = properties.protected || {};
    const fields = {};
    for (const field of type?.fields || []) {
      const tagName = `f.${field.name}`;
      const value = field.kind === 'secret'
        ? protectedValues[field.name]
        : (tags[tagName] ?? '');
      fields[field.name] = {
        name: field.name,
        kind: field.kind,
        required: !!field.required,
        primary: !!field.primary,
        value: value ?? '',
        dirty: false,
      };
      delete tags[tagName];
    }
    delete tags['xv-type'];
    return {
      type: type?.name || '',
      fields,
      customTags: tags,
      enabled: properties.enabled ?? true,
      notBefore: properties.not_before ?? null,
    };
  }
  function groupSuggestions(items = [], selected = []) {
    const excluded = new Set(selected.map((value) => String(value).trim().toLocaleLowerCase()));
    const suggestions = new Map();
    for (const item of items) {
      const groups = Array.isArray(item?.groups)
        ? item.groups
        : String(item?.groups || '').split(',');
      for (const raw of groups) {
        const value = String(raw).trim();
        const key = value.toLocaleLowerCase();
        if (!value || excluded.has(key) || suggestions.has(key)) continue;
        suggestions.set(key, value);
      }
    }
    return [...suggestions.values()].sort((left, right) => collator.compare(left, right));
  }
  function conversionSummary(preview = {}) {
    const dropped = [...(preview.dropped || [])];
    const exposed = [...(preview.exposed || [])];
    const renamed = (preview.renamed || []).map((value) => String(value).replace(/\s*->\s*/g, ' → '));
    const missing = [...(preview.missing_required || preview.missing || [])];
    const parts = [];
    if (dropped.length) parts.push(`Drops ${dropped.length} ${dropped.length === 1 ? 'field' : 'fields'}`);
    if (exposed.length) parts.push(`exposes ${exposed.length} protected ${exposed.length === 1 ? 'field' : 'fields'}`);
    if (renamed.length) parts.push(`renames ${renamed.length} ${renamed.length === 1 ? 'field' : 'fields'}`);
    if (missing.length) parts.push(`needs ${missing.length} required ${missing.length === 1 ? 'field' : 'fields'}`);
    return {
      dropped,
      exposed,
      renamed,
      missing,
      requiresConfirmation: !!preview.requires_confirmation,
      sourceRevision: preview.source_revision || '',
      description: parts.length ? `${parts.join('; ')}.` : 'No fields are lost or exposed.',
    };
  }
  function createProtectedState(value = null, hasStoredValue = value !== null) {
    return { value, hasStoredValue, masked: hasStoredValue, dirty: false, revision: 0, loadPromise: null };
  }
  function protectedDisplay(state) { return state.masked ? PROTECTED_MASK : (state.value ?? ''); }
  function revealProtected(state, loaded = state.value) {
    state.revision++; state.value = loaded ?? ''; state.hasStoredValue = true; state.masked = false; return state;
  }
  function editProtected(state, value) {
    state.revision++; state.value = value; state.hasStoredValue = true; state.dirty = true; return state;
  }
  function hideProtected(state) { state.revision++; if (state.hasStoredValue) state.masked = true; return state; }
  function loadProtected(state, loader) {
    if (state.value !== null) return Promise.resolve(state.value);
    if (state.loadPromise) return state.loadPromise;
    const revision = state.revision;
    let request;
    try { request = Promise.resolve(loader()); }
    catch (error) { request = Promise.reject(error); }
    let pending = request.then((loaded) => {
      if (state.revision === revision && state.value === null) state.value = loaded ?? '';
      return state.value;
    });
    pending = pending.finally(() => {
      if (state.loadPromise === pending) state.loadPromise = null;
    });
    state.loadPromise = pending;
    return pending;
  }

  function comparable(value, type) {
    if (type === 'number') return typeof value === 'number' && Number.isFinite(value) ? value : null;
    if (type === 'date') {
      if (!value) return null;
      const timestamp = new Date(value).getTime();
      return Number.isNaN(timestamp) ? null : timestamp;
    }
    return value === null || value === undefined || value === '' ? null : String(value);
  }
  function compareValues(left, right, type, direction) {
    const a = comparable(left, type); const b = comparable(right, type);
    if (a === null && b === null) return 0;
    if (a === null) return 1;
    if (b === null) return -1;
    const multiplier = direction === 'desc' ? -1 : 1;
    if (type === 'text') return collator.compare(a, b) * multiplier;
    return a === b ? 0 : (a < b ? -1 : 1) * multiplier;
  }
  function sortedCopy(items, valueOf, nameOf, type = 'text', direction = 'asc') {
    return [...items].sort((left, right) => {
      const primary = compareValues(valueOf(left), valueOf(right), type, direction);
      return primary || collator.compare(String(nameOf(left)), String(nameOf(right)));
    });
  }
  function normalizeWidths(serialized, defaults, minimums) {
    let widths;
    try { widths = JSON.parse(serialized); } catch (_) { return [...defaults]; }
    const valid = Array.isArray(widths) && widths.length === defaults.length
      && widths.every((width, i) => Number.isFinite(width) && width >= minimums[i])
      && Math.abs(widths.reduce((sum, width) => sum + width, 0) - 100) < 0.1;
    return valid ? widths : [...defaults];
  }
  function resizeAdjacentWidths(widths, minimums, index, delta) {
    const resized = [...widths];
    const pairTotal = widths[index] + widths[index + 1];
    const left = Math.min(
      pairTotal - minimums[index + 1],
      Math.max(minimums[index], widths[index] + delta),
    );
    resized[index] = left;
    resized[index + 1] = pairTotal - left;
    return resized;
  }

  function contentMode(width) {
    return width > 768 ? 'table' : 'stacked';
  }

  function contentRows(kind, items = [], { formatSize = (value) => String(value ?? '') } = {}) {
    return items.map((item) => {
      if (kind === 'secrets') {
        const identifier = item.original_name || item.name || '';
        return Object.freeze({
          identifier,
          source: item,
          folder: item.folder || '',
          metadata: Object.freeze([
            { label: 'Folder', value: item.folder || '' },
            { label: 'Groups', value: item.groups || '' },
            { label: 'Note', value: item.note || '' },
            { label: 'Updated', value: formatDate(item.updated_on) },
          ]),
        });
      }
      if (kind === 'files') {
        const identifier = item.name || '';
        return Object.freeze({
          identifier,
          source: item,
          folder: fileFolder(item),
          metadata: Object.freeze([
            { label: 'Size', value: formatSize(item.size) },
            { label: 'Type', value: item.content_type || '' },
            { label: 'Modified', value: formatDate(item.last_modified) },
          ]),
        });
      }
      throw new TypeError(`Unknown content row kind: ${kind}`);
    });
  }

  function groupContentRows(rows = []) {
    const groups = new Map();
    for (const row of rows) {
      const folder = row.folder || '';
      if (!groups.has(folder)) groups.set(folder, []);
      groups.get(folder).push(row);
    }
    return [...groups]
      .sort(([left], [right]) => {
        if (!left) return right ? -1 : 0;
        if (!right) return 1;
        return collator.compare(left, right);
      })
      .map(([folder, groupedRows]) => Object.freeze({
        folder,
        label: folder || 'Unfiled',
        rows: Object.freeze([...groupedRows]),
      }));
  }

  function normalizeFolderPath(value) {
    if (typeof value !== 'string') return '';
    return value.split('/').filter((segment) => segment !== '').join('/');
  }

  function normalizeFilterValue(value) {
    return typeof value === 'string'
      ? value.normalize('NFKC').toLocaleLowerCase().trim()
      : '';
  }

  function groupValues(value) {
    const values = Array.isArray(value)
      ? value
      : (typeof value === 'string' ? value.split(',') : []);
    return values.map(normalizeFilterValue).filter(Boolean);
  }

  function recordType(item) {
    return item?.tags?.['xv-type']
      || item?.record_type
      || item?.type
      || (item?.content_type === 'application/vnd.xv.record' ? 'record' : 'plain');
  }

  function matchesExpiry(item, expiry, now) {
    if (!expiry) return true;
    if (!item?.expires_on) return expiry === 'none';
    const timestamp = new Date(item.expires_on).getTime();
    if (Number.isNaN(timestamp)) return false;
    if (expiry === 'expired') return timestamp <= now.getTime();
    if (expiry === 'expiring') return timestamp > now.getTime();
    return false;
  }

  function filterSecrets(items, filters = {}, { now = new Date() } = {}) {
    const folder = normalizeFilterValue(filters.folder);
    const group = normalizeFilterValue(filters.group);
    const type = normalizeFilterValue(filters.type);
    const expiry = normalizeFilterValue(filters.expiry);
    const hasEnabled = typeof filters.enabled === 'boolean';
    return (items || []).filter((item) => (
      (!folder || normalizeFilterValue(item?.folder) === folder)
      && (!group || groupValues(item?.groups).includes(group))
      && (!type || normalizeFilterValue(recordType(item)) === type)
      && (!hasEnabled || item?.enabled === filters.enabled)
      && matchesExpiry(item, expiry, now)
    ));
  }

  function fileFolder(item) {
    if (typeof item?.folder === 'string') return item.folder;
    const name = typeof item?.name === 'string' ? item.name : '';
    const separator = name.lastIndexOf('/');
    return separator < 0 ? '' : name.slice(0, separator);
  }

  function filterFiles(items, filters = {}) {
    const folder = normalizeFilterValue(filters.folder);
    const type = normalizeFilterValue(filters.type);
    return (items || []).filter((item) => (
      (!folder || normalizeFilterValue(fileFolder(item)) === folder)
      && (!type || normalizeFilterValue(item?.content_type) === type)
    ));
  }

  function activeFilterChips(filters = {}, labels = {}) {
    return Object.entries(filters).flatMap(([key, value]) => {
      if (value === '' || value === null || value === undefined) return [];
      const display = typeof value === 'boolean'
        ? (value ? 'enabled' : 'disabled')
        : String(value);
      return [{ key, label: `${labels[key] || key}: ${display}` }];
    });
  }

  const FOLDER_UNFILED = Object.freeze({ kind: 'unfiled' });

  function folderIdentity(path) {
    const normalized = normalizeFolderPath(path);
    if (normalized === '') return FOLDER_UNFILED;
    return Object.freeze({
      kind: 'folder',
      path: normalized,
    });
  }

  function folderIdentityKey(identity) {
    if (identity?.kind === 'unfiled') return '["unfiled"]';
    if (identity?.kind === 'folder' && typeof identity.path === 'string') {
      return JSON.stringify(['folder', identity.path]);
    }
    return '';
  }

  function treeItemKey(identifier) {
    return JSON.stringify(['item', String(identifier ?? '')]);
  }

  // One table holds folders and their contents: every folder is a branch row and
  // every secret/file is a leaf row underneath it. Rows arrive pre-sorted, so the
  // per-folder item order is whatever the active column sort produced.
  function buildContentTree(rows = []) {
    const root = { children: new Map(), items: [] };
    for (const row of rows) {
      const path = normalizeFolderPath(row?.folder);
      if (!path) {
        root.items.push(row);
        continue;
      }
      let node = root;
      let parentPath = '';
      for (const segment of path.split('/')) {
        const folderPath = parentPath ? `${parentPath}/${segment}` : segment;
        if (!node.children.has(segment)) {
          node.children.set(segment, {
            identity: folderIdentity(folderPath),
            path: folderPath,
            label: segment,
            children: new Map(),
            items: [],
          });
        }
        node = node.children.get(segment);
        parentPath = folderPath;
      }
      node.items.push(row);
    }

    const finalizeChildren = (source) => [...source.children.values()]
      .sort((left, right) => collator.compare(left.label, right.label))
      .map(finalize);
    const finalize = (source) => {
      const children = finalizeChildren(source);
      const items = [...source.items];
      const itemIds = Object.freeze([
        ...children.flatMap((child) => child.itemIds),
        ...items.map((item) => item.identifier),
      ]);
      return Object.freeze({
        kind: 'folder',
        key: folderIdentityKey(source.identity),
        identity: source.identity,
        path: source.path,
        label: source.label,
        children: Object.freeze(children),
        items: Object.freeze(items),
        itemIds,
        totalCount: itemIds.length,
      });
    };

    const children = finalizeChildren(root);
    const items = [...root.items];
    return Object.freeze({
      kind: 'root',
      key: '',
      children: Object.freeze(children),
      items: Object.freeze(items),
      itemIds: Object.freeze([
        ...children.flatMap((child) => child.itemIds),
        ...items.map((item) => item.identifier),
      ]),
    });
  }

  function treeFolderIdentities(node, collected = []) {
    for (const child of node?.children || []) {
      collected.push(child.identity);
      treeFolderIdentities(child, collected);
    }
    return collected;
  }

  function treeFolderCount(node) {
    return treeFolderIdentities(node).length;
  }

  // A filtered/searched list must stay reachable, so every folder that survived the
  // filter is force-expanded for that render without touching the persisted state.
  function treeForcedKeys(node, forced = new Set()) {
    for (const child of node?.children || []) {
      forced.add(child.key);
      treeForcedKeys(child, forced);
    }
    return forced;
  }

  function flattenContentTree(tree, expanded, { forcedKeys = null } = {}) {
    const rows = [];
    const isOpen = (key) => (forcedKeys ? forcedKeys.has(key) : false)
      || (expanded instanceof Map ? expanded.has(key) : Boolean(expanded?.has?.(key)));
    const visit = (node, level, parentKey) => {
      for (const child of node.children) {
        const open = isOpen(child.key);
        rows.push(Object.freeze({
          type: 'folder',
          key: child.key,
          node: child,
          identity: child.identity,
          label: child.label,
          path: child.path,
          level,
          parentKey,
          expanded: open,
          hasChildren: child.children.length > 0 || child.items.length > 0,
          itemIds: child.itemIds,
        }));
        if (open) visit(child, level + 1, child.key);
      }
      for (const item of node.items) {
        rows.push(Object.freeze({
          type: 'item',
          key: treeItemKey(item.identifier),
          row: item,
          identifier: item.identifier,
          level,
          parentKey,
          expanded: false,
          hasChildren: false,
          itemIds: Object.freeze([item.identifier]),
        }));
      }
    };
    visit(tree, 1, null);
    return rows;
  }

  // Tri-state: a branch is checked when every descendant item is selected, mixed
  // when only some are, unchecked otherwise. Branches are never selection targets
  // themselves — they are a proxy for the items below them.
  function branchSelection(itemIds, selectedIds) {
    const ids = [...(itemIds || [])];
    if (!ids.length) return 'unchecked';
    const selected = selectedIds instanceof Set ? selectedIds : new Set(selectedIds || []);
    let hits = 0;
    for (const id of ids) if (selected.has(id)) hits++;
    if (hits === 0) return 'unchecked';
    return hits === ids.length ? 'checked' : 'mixed';
  }

  function applyBranchSelection(selectedIds, itemIds, checked) {
    for (const id of itemIds || []) {
      if (checked) selectedIds.add(id);
      else selectedIds.delete(id);
    }
    return selectedIds;
  }

  function initialExpansion({ total, saved }) {
    if (Array.isArray(saved)) return [...saved];
    return Number(total) <= 50 ? 'all' : 'collapsed';
  }

  const OPAQUE_TOKEN = /^[A-Za-z0-9_-]{43}$/;

  function createFolderTokenIndex(response) {
    if (response?.version !== 1 || !OPAQUE_TOKEN.test(response?.scope_token)
      || !Array.isArray(response?.folders)) return null;
    const byIdentityKey = new Map();
    const byToken = new Map();
    for (const entry of response.folders) {
      if (typeof entry?.path !== 'string' || entry.path === ''
        || normalizeFolderPath(entry.path) !== entry.path
        || !OPAQUE_TOKEN.test(entry?.token)) return null;
      const identity = folderIdentity(entry.path);
      const identityKey = folderIdentityKey(identity);
      if (byIdentityKey.has(identityKey) || byToken.has(entry.token)) return null;
      byIdentityKey.set(identityKey, entry.token);
      byToken.set(entry.token, identity);
    }
    return Object.freeze({
      scopeToken: response.scope_token,
      byIdentityKey,
      byToken,
    });
  }

  // v5: the tree grid makes every folder expandable (v1-v4 could only persist
  // folders that had sub-folders), so a v4 payload would restore a set that no
  // longer describes what the user actually opened.
  const FOLDER_EXPANSION_VERSION = 5;

  function folderPreferenceKey(tokenIndex) {
    return OPAQUE_TOKEN.test(tokenIndex?.scopeToken)
      ? `xv.ui.folder-expansion.v${FOLDER_EXPANSION_VERSION}:${tokenIndex.scopeToken}`
      : '';
  }

  function cleanupLegacyFolderExpansion(storage) {
    if (typeof storage?.removeItem !== 'function') return;
    const legacyKeys = new Set([
      'xv.ui.folder-expansion.v1',
      'folder_expansion',
    ]);
    if (Number.isInteger(storage.length) && typeof storage.key === 'function') {
      for (let index = 0; index < storage.length; index++) {
        const key = storage.key(index);
        if (key?.startsWith('xv.ui.folder-expansion.v2:')
          || key?.startsWith('xv.ui.folder-expansion.v3:')
          || key?.startsWith('xv.ui.folder-expansion.v4:')) legacyKeys.add(key);
      }
    }
    for (const key of legacyKeys) {
      try { storage.removeItem(key); } catch (_) { /* storage cleanup is best effort */ }
    }
  }

  function loadFolderExpansion(storage, tokenIndex) {
    if (typeof storage?.getItem !== 'function') return null;
    try {
      cleanupLegacyFolderExpansion(storage);
      const key = folderPreferenceKey(tokenIndex);
      if (!key) return null;
      const serialized = storage.getItem(key);
      if (serialized === null) return null;
      const parsed = JSON.parse(serialized);
      if (parsed?.version !== FOLDER_EXPANSION_VERSION || !Array.isArray(parsed.expanded)
        || parsed.expanded.some((token) => !OPAQUE_TOKEN.test(token))) {
        return null;
      }
      return [...new Set(parsed.expanded)];
    } catch (_) {
      return null;
    }
  }

  function saveFolderExpansion(storage, tokenIndex, expanded) {
    if (typeof storage?.setItem !== 'function') return false;
    try {
      cleanupLegacyFolderExpansion(storage);
      const key = folderPreferenceKey(tokenIndex);
      if (!key) return false;
      const identities = expanded instanceof Map ? expanded.values() : expanded;
      const tokens = [...identities]
        .map((identity) => tokenIndex.byIdentityKey.get(folderIdentityKey(identity)))
        .filter((token) => OPAQUE_TOKEN.test(token))
        .sort((left, right) => collator.compare(left, right));
      storage.setItem(key, JSON.stringify({
        version: FOLDER_EXPANSION_VERSION,
        expanded: tokens,
      }));
      return true;
    } catch (_) {
      return false;
    }
  }

  function createTreeExpansionState(storage) {
    let scope = null;
    let scopeKey = null;
    let tokenIndex = null;
    // Hydrate the saved set once per scope. Every list reload mints a fresh token
    // index object, so keying on object identity re-ran hydration on each refresh
    // and clobbered whatever the user had opened since.
    let hydrated = false;
    let expandableIds = [];
    // A surface's rows arrive after the scope switch, so the very first sync for
    // a scope usually has no folders yet. The first-visit default therefore stays
    // pending until a sync actually reports folders (or the user acts).
    let initialPending = true;
    // Folders this scope has already shown. A folder that appears later (a new
    // secret filed somewhere new) is not "collapsed by the user" — it inherits
    // the scope's first-visit rule so the row that just appeared stays visible.
    let knownKeys = new Set();
    const expanded = new Map();

    // A reconcile can land before the scope's token index arrives; the write is
    // owed until one does, otherwise a pruned folder stays in storage forever.
    let persistPending = false;
    const persist = () => {
      const wrote = Boolean(scope && tokenIndex
        && saveFolderExpansion(storage, tokenIndex, expanded));
      persistPending = !wrote;
      return wrote;
    };
    const applyInitial = (total, available) => {
      expanded.clear();
      const saved = tokenIndex ? loadFolderExpansion(storage, tokenIndex) : null;
      const initial = initialExpansion({ total, saved });
      const keys = initial === 'all'
        ? [...available.keys()]
        : (Array.isArray(initial)
          ? initial.map((token) => folderIdentityKey(tokenIndex?.byToken.get(token)))
          : []);
      for (const key of keys) {
        if (available.has(key)) expanded.set(key, available.get(key));
      }
      knownKeys = new Set(available.keys());
    };
    return Object.freeze({
      sync(nextScope, {
        total,
        expandableIds: nextExpandableIds,
        tokenIndex: nextTokenIndex = null,
      }) {
        const nextKey = JSON.stringify([
          String(nextScope?.backend || ''),
          String(nextScope?.vault || ''),
          String(nextScope?.surface || ''),
        ]);
        expandableIds = [...nextExpandableIds];
        const available = new Map(
          expandableIds.map((id) => [folderIdentityKey(id), id]),
        );
        const sameScope = nextKey === scopeKey;
        tokenIndex = nextTokenIndex;
        if (sameScope) {
          let reconciled = false;
          if (initialPending && available.size) {
            applyInitial(total, available);
            initialPending = false;
            hydrated = hydrated || Boolean(tokenIndex);
            return expanded;
          }
          if (tokenIndex && !hydrated) {
            const saved = loadFolderExpansion(storage, tokenIndex);
            if (saved !== null) {
              expanded.clear();
              for (const token of saved) {
                const identity = tokenIndex.byToken.get(token);
                const key = folderIdentityKey(identity);
                if (available.has(key)) expanded.set(key, available.get(key));
                else reconciled = true;
              }
            }
            hydrated = true;
            knownKeys = new Set(available.keys());
          }
          const expandsByDefault = initialExpansion({ total, saved: null }) === 'all';
          for (const [key, id] of available) {
            if (knownKeys.has(key)) continue;
            knownKeys.add(key);
            if (expandsByDefault) {
              expanded.set(key, id);
              reconciled = true;
            }
          }
          for (const key of [...expanded.keys()]) {
            if (!available.has(key)) {
              expanded.delete(key);
              reconciled = true;
            }
          }
          for (const key of [...knownKeys]) {
            if (!available.has(key)) knownKeys.delete(key);
          }
          if (reconciled || persistPending) persist();
          return expanded;
        }
        scope = { ...nextScope };
        scopeKey = nextKey;
        hydrated = Boolean(tokenIndex);
        applyInitial(total, available);
        initialPending = available.size === 0;
        return expanded;
      },
      toggle(id, value = !expanded.has(folderIdentityKey(id))) {
        initialPending = false;
        const key = folderIdentityKey(id);
        if (value) expanded.set(key, id);
        else expanded.delete(key);
        persist();
        return value;
      },
      expandAll() {
        initialPending = false;
        expanded.clear();
        for (const id of expandableIds) expanded.set(folderIdentityKey(id), id);
        persist();
        return expanded;
      },
      collapseAll() {
        initialPending = false;
        expanded.clear();
        persist();
        return expanded;
      },
      has(key) {
        return expanded.has(key);
      },
      snapshot() {
        return {
          expanded: [...expanded.values()].sort((left, right) => (
            collator.compare(left.path || '', right.path || '')
          )),
        };
      },
      expanded,
    });
  }

export { PROTECTED_MASK, formatDate, expirationDate, createProtectedState,
  typeCards, buildTypedDraft, groupSuggestions, conversionSummary,
  protectedDisplay, revealProtected, editProtected, hideProtected, loadProtected,
  sortedCopy, normalizeWidths, resizeAdjacentWidths, contentMode, contentRows,
  groupContentRows, normalizeFolderPath,
  FOLDER_UNFILED, folderIdentity, folderIdentityKey, treeItemKey,
  buildContentTree, treeFolderIdentities, treeFolderCount, treeForcedKeys,
  flattenContentTree, branchSelection, applyBranchSelection,
  initialExpansion, createFolderTokenIndex,
  folderPreferenceKey, loadFolderExpansion,
  saveFolderExpansion, createTreeExpansionState, filterSecrets, filterFiles,
  activeFilterChips };
