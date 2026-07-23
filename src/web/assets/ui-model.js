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

  function normalizeFolderPath(value) {
    if (typeof value !== 'string') return '';
    return value.split('/').filter((segment) => segment !== '').join('/');
  }

  function opaqueHash(value) {
    let hash = 0xcbf29ce484222325n;
    for (const character of String(value)) {
      hash ^= BigInt(character.codePointAt(0));
      hash = BigInt.asUintN(64, hash * 0x100000001b3n);
    }
    return hash.toString(16).padStart(16, '0');
  }

  const FOLDER_ALL = Object.freeze({ kind: 'all', opaque: 'a' });
  const FOLDER_UNFILED = Object.freeze({ kind: 'unfiled', opaque: 'u' });

  function folderIdentity(path) {
    const normalized = normalizeFolderPath(path);
    if (normalized === '') return FOLDER_UNFILED;
    return Object.freeze({
      kind: 'folder',
      path: normalized,
      opaque: `f-${opaqueHash(normalized)}`,
    });
  }

  function folderIdentityKey(identity) {
    if (identity?.kind === 'all') return FOLDER_ALL.opaque;
    if (identity?.kind === 'unfiled') return FOLDER_UNFILED.opaque;
    if (identity?.kind === 'folder' && typeof identity.opaque === 'string') return identity.opaque;
    return '';
  }

  function sameFolderIdentity(left, right) {
    if (left?.kind !== right?.kind) return false;
    if (left?.kind === 'folder') return left.path === right.path;
    return left?.kind === 'all' || left?.kind === 'unfiled';
  }

  function buildFolderTree(items) {
    const roots = new Map();
    const unfiledItems = [];

    for (const item of items || []) {
      const path = normalizeFolderPath(item?.folder);
      if (!path) {
        unfiledItems.push(item);
        continue;
      }
      let siblings = roots;
      let parentPath = '';
      for (const segment of path.split('/')) {
        const folderPath = parentPath ? `${parentPath}/${segment}` : segment;
        if (!siblings.has(segment)) {
          siblings.set(segment, {
            id: folderIdentity(folderPath),
            label: segment,
            directCount: 0,
            totalCount: 0,
            items: [],
            children: new Map(),
          });
        }
        const node = siblings.get(segment);
        node.totalCount++;
        if (folderPath === path) {
          node.directCount++;
          node.items.push(item);
        }
        siblings = node.children;
        parentPath = folderPath;
      }
    }

    const finalize = (nodes) => [...nodes.values()]
      .sort((left, right) => collator.compare(left.label, right.label))
      .map((node) => ({
        ...node,
        children: finalize(node.children),
      }));

    const tree = finalize(roots);
    if (unfiledItems.length) {
      tree.unshift({
        id: FOLDER_UNFILED,
        label: 'Unfiled',
        directCount: unfiledItems.length,
        totalCount: unfiledItems.length,
        items: [...unfiledItems],
        children: [],
      });
    }
    return tree;
  }

  function initialExpansion({ total, saved }) {
    if (Array.isArray(saved)) return [...saved];
    return Number(total) <= 50 ? 'all' : 'collapsed';
  }

  function folderPreferenceKey({ backend, vault, surface }) {
    const scope = JSON.stringify([
      String(backend || ''),
      String(vault || ''),
      String(surface || ''),
    ]);
    return `xv.ui.folder-expansion.v3:${opaqueHash(scope)}`;
  }

  function cleanupLegacyFolderExpansion(storage, scope) {
    if (typeof storage?.removeItem !== 'function') return;
    const legacyScoped = ['xv.ui.folder-expansion.v2', scope?.backend, scope?.vault, scope?.surface]
      .map((part, index) => index === 0 ? part : encodeURIComponent(String(part || '')))
      .join(':');
    const legacyKeys = new Set([
      legacyScoped,
      'xv.ui.folder-expansion.v1',
      'folder_expansion',
    ]);
    if (Number.isInteger(storage.length) && typeof storage.key === 'function') {
      for (let index = 0; index < storage.length; index++) {
        const key = storage.key(index);
        if (key?.startsWith('xv.ui.folder-expansion.v2:')) legacyKeys.add(key);
      }
    }
    for (const key of legacyKeys) {
      try { storage.removeItem(key); } catch (_) { /* storage cleanup is best effort */ }
    }
  }

  function loadFolderExpansion(storage, scope) {
    if (typeof storage?.getItem !== 'function') return null;
    try {
      cleanupLegacyFolderExpansion(storage, scope);
      const serialized = storage.getItem(folderPreferenceKey(scope));
      if (serialized === null) return null;
      const parsed = JSON.parse(serialized);
      if (parsed?.version !== 3 || !Array.isArray(parsed.expanded)
        || parsed.expanded.some((id) => typeof id !== 'string' || !/^f-[a-f0-9]{16}$/.test(id))) {
        return null;
      }
      return [...new Set(parsed.expanded)];
    } catch (_) {
      return null;
    }
  }

  function saveFolderExpansion(storage, scope, expanded) {
    if (typeof storage?.setItem !== 'function') return false;
    try {
      cleanupLegacyFolderExpansion(storage, scope);
      const identities = expanded instanceof Map ? expanded.values() : expanded;
      const ids = [...identities]
        .map(folderIdentityKey)
        .filter((id) => /^f-[a-f0-9]{16}$/.test(id))
        .sort((left, right) => collator.compare(left, right));
      storage.setItem(folderPreferenceKey(scope), JSON.stringify({
        version: 3,
        expanded: ids,
      }));
      return true;
    } catch (_) {
      return false;
    }
  }

  function createFolderNavigationState(storage) {
    let scope = null;
    let scopeKey = null;
    let selected = FOLDER_ALL;
    let folderIds = [];
    let expandableIds = [];
    const expanded = new Map();

    const persist = () => scope && saveFolderExpansion(storage, scope, expanded);
    return Object.freeze({
      sync(nextScope, {
        total,
        folderIds: nextFolderIds = null,
        expandableIds: nextExpandableIds,
      }) {
        const nextKey = folderPreferenceKey(nextScope);
        expandableIds = [...nextExpandableIds];
        folderIds = [...(nextFolderIds || nextExpandableIds)];
        const availableFolders = new Map(
          folderIds.map((id) => [folderIdentityKey(id), id]),
        );
        const availableExpandable = new Map(
          expandableIds.map((id) => [folderIdentityKey(id), id]),
        );
        if (nextKey === scopeKey) {
          for (const key of [...expanded.keys()]) {
            if (!availableExpandable.has(key)) expanded.delete(key);
          }
          if (!sameFolderIdentity(selected, FOLDER_ALL)
            && !availableFolders.has(folderIdentityKey(selected))) {
            selected = FOLDER_ALL;
          }
          return selected;
        }
        scope = { ...nextScope };
        scopeKey = nextKey;
        selected = FOLDER_ALL;
        expanded.clear();
        const saved = loadFolderExpansion(storage, scope);
        const initial = initialExpansion({ total, saved });
        const initialKeys = initial === 'all'
          ? [...availableExpandable.keys()]
          : (Array.isArray(initial) ? initial : []);
        for (const key of initialKeys) {
          if (availableExpandable.has(key)) expanded.set(key, availableExpandable.get(key));
        }
        return selected;
      },
      select(id) {
        selected = id?.kind ? id : FOLDER_ALL;
        return selected;
      },
      toggle(id, value = !expanded.has(folderIdentityKey(id))) {
        const key = folderIdentityKey(id);
        if (value) expanded.set(key, id);
        else {
          expanded.delete(key);
          if (selected?.kind === 'folder' && id?.kind === 'folder'
            && selected.path.startsWith(`${id.path}/`)) {
            selected = id;
          }
        }
        persist();
        return selected;
      },
      expandAll() {
        expanded.clear();
        for (const id of expandableIds) expanded.set(folderIdentityKey(id), id);
        persist();
        return selected;
      },
      collapseAll() {
        expanded.clear();
        selected = FOLDER_ALL;
        persist();
        return selected;
      },
      snapshot() {
        return {
          selected,
          expanded: [...expanded.values()].sort((left, right) => (
            collator.compare(left.path || '', right.path || '')
          )),
        };
      },
      expanded,
    });
  }

  function itemMatchesFolder(item, selected) {
    if (!selected || selected.kind === 'all') return true;
    const folder = normalizeFolderPath(item?.folder);
    if (selected.kind === 'unfiled') return folder === '';
    if (selected.kind !== 'folder') return false;
    const target = selected.path;
    return folder === target || folder.startsWith(`${target}/`);
  }

  function treeCountMap(nodes, counts = new Map()) {
    for (const node of nodes) {
      counts.set(folderIdentityKey(node.id), node.totalCount);
      treeCountMap(node.children, counts);
    }
    return counts;
  }

  function buildFolderViewModel(items, visibleItems = items, {
    buildTree = buildFolderTree,
  } = {}) {
    const tree = buildTree(items);
    const visibleTree = buildTree(visibleItems);
    const folderIds = [];
    const expandableIds = [];
    let folderCount = 0;
    const visit = (nodes) => {
      for (const node of nodes) {
        folderIds.push(node.id);
        if (node.id.kind === 'folder') folderCount++;
        if (node.children.length) expandableIds.push(node.id);
        visit(node.children);
      }
    };
    visit(tree);
    return Object.freeze({
      tree,
      visibleCounts: treeCountMap(visibleTree),
      folderIds: Object.freeze(folderIds),
      expandableIds: Object.freeze(expandableIds),
      folderCount,
      totalCount: items.length,
      visibleCount: visibleItems.length,
    });
  }

  function flattenFolderTree(nodes, expanded, level = 1, parentId = null, rows = []) {
    for (const node of nodes) {
      rows.push({ ...node, level, parentId });
      if (node.children.length && expanded.has(folderIdentityKey(node.id))) {
        flattenFolderTree(node.children, expanded, level + 1, node.id, rows);
      }
    }
    return rows;
  }

  function renderFolderTree({
    document,
    container,
    items,
    visibleItems = items,
    viewModel = null,
    expanded,
    selected,
    focusedId,
    onSelect,
    onToggle,
    onFocus,
  }) {
    const folderView = viewModel || buildFolderViewModel(items, visibleItems);
    const tree = folderView.tree;
    const visibleCounts = folderView.visibleCounts;
    const rows = [{
      id: FOLDER_ALL,
      label: 'All items',
      level: 1,
      parentId: null,
      totalCount: folderView.totalCount,
      children: [],
    }, ...flattenFolderTree(tree, expanded)];
    const visibleIds = rows.map((row) => row.id);
    const visibleKeys = new Set(visibleIds.map(folderIdentityKey));
    const effectiveSelected = visibleKeys.has(folderIdentityKey(selected))
      ? selected
      : FOLDER_ALL;
    let rovingId = visibleKeys.has(folderIdentityKey(focusedId))
      ? focusedId
      : effectiveSelected;
    const buttons = [];
    const hadTreeFocus = Boolean(container.contains?.(document.activeElement));
    const previousFocusKey = hadTreeFocus
      ? document.activeElement?.dataset?.folderId
      : '';

    container.setAttribute('role', 'tree');
    const focusItem = (id) => {
      const key = folderIdentityKey(id);
      const button = buttons.find((candidate) => candidate.dataset.folderId === key);
      if (!button) return;
      rovingId = id;
      for (const candidate of buttons) {
        candidate.tabIndex = candidate === button ? 0 : -1;
      }
      button.focus();
      onFocus?.(id);
    };

    for (const row of rows) {
      const button = document.createElement('button');
      const rowKey = folderIdentityKey(row.id);
      const visibleCount = row.id.kind === 'all'
        ? folderView.visibleCount
        : (visibleCounts.get(rowKey) || 0);
      const countLabel = visibleCount === row.totalCount
        ? `${row.totalCount} ${row.totalCount === 1 ? 'item' : 'items'}`
        : `${visibleCount} visible of ${row.totalCount} total`;
      button.type = 'button';
      button.className = 'folder-tree-item';
      button.dataset.folderId = rowKey;
      button.dataset.level = String(row.level);
      button.style.setProperty('--folder-depth', String(row.level - 1));
      button.setAttribute('role', 'treeitem');
      button.setAttribute('aria-level', String(row.level));
      button.setAttribute('aria-selected', String(
        sameFolderIdentity(row.id, effectiveSelected),
      ));
      button.setAttribute('aria-label', `${row.label}, ${countLabel}`);
      button.tabIndex = sameFolderIdentity(row.id, rovingId) ? 0 : -1;
      if (row.children.length) {
        button.setAttribute('aria-expanded', String(expanded.has(rowKey)));
      }

      const disclosure = document.createElement('span');
      disclosure.className = 'folder-tree-disclosure';
      disclosure.textContent = row.children.length
        ? (expanded.has(rowKey) ? '▾' : '▸')
        : '';
      disclosure.setAttribute('aria-hidden', 'true');
      if (row.children.length) {
        disclosure.onclick = (event) => {
          event.preventDefault();
          event.stopPropagation();
          onToggle?.(row.id, !expanded.has(rowKey));
          const replacement = [...container.querySelectorAll('[role="treeitem"]')]
            .find((candidate) => candidate.dataset.folderId === rowKey);
          replacement?.focus();
        };
      }
      const label = document.createElement('span');
      label.className = 'folder-tree-label';
      label.textContent = row.label;
      const count = document.createElement('span');
      count.className = 'folder-tree-count';
      count.textContent = countLabel;
      button.append(disclosure, label, count);
      button.onfocus = () => {
        rovingId = row.id;
        for (const candidate of buttons) {
          candidate.tabIndex = candidate === button ? 0 : -1;
        }
        onFocus?.(row.id);
      };
      button.onclick = () => {
        if (onSelect?.(row.id) === false) return;
        const replacement = [...container.querySelectorAll('[role="treeitem"]')]
          .find((candidate) => candidate.dataset.folderId === rowKey);
        replacement?.focus();
      };
      button.onkeydown = (event) => {
        const index = buttons.indexOf(button);
        let destination = null;
        if (event.key === 'ArrowDown') destination = buttons[Math.min(buttons.length - 1, index + 1)];
        if (event.key === 'ArrowUp') destination = buttons[Math.max(0, index - 1)];
        if (event.key === 'Home') destination = buttons[0];
        if (event.key === 'End') destination = buttons.at(-1);
        if (event.key === 'ArrowRight' && row.children.length) {
          if (!expanded.has(rowKey)) onToggle?.(row.id, true);
          else destination = buttons[index + 1];
        }
        if (event.key === 'ArrowLeft') {
          if (row.children.length && expanded.has(rowKey)) onToggle?.(row.id, false);
          else if (row.parentId) destination = buttons.find(
            (candidate) => candidate.dataset.folderId === folderIdentityKey(row.parentId),
          );
        }
        if (event.key === 'Enter' || event.key === ' ') {
          onSelect?.(row.id);
        } else if (!destination
          && !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) {
          return;
        }
        event.preventDefault();
        if (destination) {
          const destinationRow = rows.find(
            (candidate) => folderIdentityKey(candidate.id) === destination.dataset.folderId,
          );
          if (destinationRow) focusItem(destinationRow.id);
        } else if (event.key === 'Enter' || event.key === ' ') {
          const replacement = [...container.querySelectorAll('[role="treeitem"]')]
            .find((candidate) => candidate.dataset.folderId === rowKey);
          replacement?.focus();
        }
      };
      buttons.push(button);
    }
    container.replaceChildren(...buttons);
    if (hadTreeFocus) {
      const replacement = rows.find((row) => folderIdentityKey(row.id) === previousFocusKey);
      focusItem(replacement?.id || rovingId);
    }
    return Object.freeze({ visibleIds, focusedId: () => rovingId });
  }
export { PROTECTED_MASK, formatDate, expirationDate, createProtectedState,
  protectedDisplay, revealProtected, editProtected, hideProtected, loadProtected,
  sortedCopy, normalizeWidths, resizeAdjacentWidths, normalizeFolderPath,
  FOLDER_ALL, FOLDER_UNFILED, folderIdentity, folderIdentityKey, sameFolderIdentity,
  buildFolderTree, buildFolderViewModel, initialExpansion, folderPreferenceKey, loadFolderExpansion,
  saveFolderExpansion, createFolderNavigationState, itemMatchesFolder,
  flattenFolderTree, renderFolderTree };
