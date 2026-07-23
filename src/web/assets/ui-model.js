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
    return value.split('/').map((segment) => segment.trim()).filter(Boolean).join('/');
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
        const id = parentPath ? `${parentPath}/${segment}` : segment;
        if (!siblings.has(segment)) {
          siblings.set(segment, {
            id,
            label: segment,
            directCount: 0,
            totalCount: 0,
            items: [],
            children: new Map(),
          });
        }
        const node = siblings.get(segment);
        node.totalCount++;
        if (id === path) {
          node.directCount++;
          node.items.push(item);
        }
        siblings = node.children;
        parentPath = id;
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
        id: '__unfiled__',
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
    return ['xv.ui.folder-expansion.v2', backend, vault, surface]
      .map((part, index) => index === 0 ? part : encodeURIComponent(String(part || '')))
      .join(':');
  }

  function loadFolderExpansion(storage, scope) {
    if (typeof storage?.getItem !== 'function') return null;
    try {
      const serialized = storage.getItem(folderPreferenceKey(scope));
      if (serialized === null) return null;
      const parsed = JSON.parse(serialized);
      if (!Array.isArray(parsed) || parsed.some((id) => typeof id !== 'string' || !id)) return null;
      return [...new Set(parsed)];
    } catch (_) {
      return null;
    }
  }

  function saveFolderExpansion(storage, scope, expanded) {
    if (typeof storage?.setItem !== 'function') return false;
    try {
      const ids = [...expanded]
        .filter((id) => typeof id === 'string' && id)
        .sort((left, right) => collator.compare(left, right));
      storage.setItem(folderPreferenceKey(scope), JSON.stringify(ids));
      return true;
    } catch (_) {
      return false;
    }
  }

  function createFolderNavigationState(storage) {
    let scope = null;
    let scopeKey = null;
    let selected = null;
    let expandableIds = [];
    const expanded = new Set();

    const persist = () => scope && saveFolderExpansion(storage, scope, expanded);
    return Object.freeze({
      sync(nextScope, { total, expandableIds: nextExpandableIds }) {
        const nextKey = folderPreferenceKey(nextScope);
        expandableIds = [...nextExpandableIds];
        if (nextKey === scopeKey) {
          const available = new Set(expandableIds);
          for (const id of [...expanded]) {
            if (!available.has(id)) expanded.delete(id);
          }
          return;
        }
        scope = { ...nextScope };
        scopeKey = nextKey;
        selected = null;
        expanded.clear();
        const saved = loadFolderExpansion(storage, scope);
        const initial = initialExpansion({ total, saved });
        const initialIds = initial === 'all' ? expandableIds : (Array.isArray(initial) ? initial : []);
        const available = new Set(expandableIds);
        for (const id of initialIds) {
          if (available.has(id)) expanded.add(id);
        }
      },
      select(id) {
        selected = id || null;
      },
      toggle(id, value = !expanded.has(id)) {
        if (value) expanded.add(id);
        else expanded.delete(id);
        persist();
      },
      expandAll() {
        expanded.clear();
        for (const id of expandableIds) expanded.add(id);
        persist();
      },
      collapseAll() {
        expanded.clear();
        persist();
      },
      snapshot() {
        return {
          selected,
          expanded: [...expanded].sort((left, right) => collator.compare(left, right)),
        };
      },
      expanded,
    });
  }

  function itemMatchesFolder(item, selected) {
    if (!selected) return true;
    const folder = normalizeFolderPath(item?.folder);
    if (selected === '__unfiled__') return folder === '';
    const target = normalizeFolderPath(selected);
    return folder === target || folder.startsWith(`${target}/`);
  }

  function treeCountMap(nodes, counts = new Map()) {
    for (const node of nodes) {
      counts.set(node.id, node.totalCount);
      treeCountMap(node.children, counts);
    }
    return counts;
  }

  function flattenFolderTree(nodes, expanded, level = 1, parentId = null, rows = []) {
    for (const node of nodes) {
      rows.push({ ...node, level, parentId });
      if (node.children.length && expanded.has(node.id)) {
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
    expanded,
    selected,
    focusedId,
    onSelect,
    onToggle,
    onFocus,
  }) {
    const tree = buildFolderTree(items);
    const visibleCounts = treeCountMap(buildFolderTree(visibleItems));
    const rows = [{
      id: '__all__',
      label: 'All items',
      level: 1,
      parentId: null,
      totalCount: items.length,
      children: [],
    }, ...flattenFolderTree(tree, expanded)];
    const visibleIds = rows.map((row) => row.id);
    let rovingId = visibleIds.includes(focusedId)
      ? focusedId
      : (visibleIds.includes(selected) ? selected : '__all__');
    const buttons = [];

    container.setAttribute('role', 'tree');
    const focusItem = (id) => {
      const button = buttons.find((candidate) => candidate.dataset.folderId === id);
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
      const visibleCount = row.id === '__all__'
        ? visibleItems.length
        : (visibleCounts.get(row.id) || 0);
      const countLabel = visibleCount === row.totalCount
        ? `${row.totalCount} ${row.totalCount === 1 ? 'item' : 'items'}`
        : `${visibleCount} visible of ${row.totalCount} total`;
      button.type = 'button';
      button.className = 'folder-tree-item';
      button.dataset.folderId = row.id;
      button.dataset.level = String(row.level);
      button.setAttribute('role', 'treeitem');
      button.setAttribute('aria-level', String(row.level));
      button.setAttribute('aria-selected', String(
        row.id === (selected || '__all__'),
      ));
      button.setAttribute('aria-label', `${row.label}, ${countLabel}`);
      button.tabIndex = row.id === rovingId ? 0 : -1;
      if (row.children.length) {
        button.setAttribute('aria-expanded', String(expanded.has(row.id)));
      }

      const disclosure = document.createElement('span');
      disclosure.className = 'folder-tree-disclosure';
      disclosure.textContent = row.children.length
        ? (expanded.has(row.id) ? '▾' : '▸')
        : '';
      disclosure.setAttribute('aria-hidden', 'true');
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
      button.onclick = () => onSelect?.(row.id === '__all__' ? null : row.id);
      button.onkeydown = (event) => {
        const index = buttons.indexOf(button);
        let destination = null;
        if (event.key === 'ArrowDown') destination = buttons[Math.min(buttons.length - 1, index + 1)];
        if (event.key === 'ArrowUp') destination = buttons[Math.max(0, index - 1)];
        if (event.key === 'Home') destination = buttons[0];
        if (event.key === 'End') destination = buttons.at(-1);
        if (event.key === 'ArrowRight' && row.children.length) {
          if (!expanded.has(row.id)) onToggle?.(row.id, true);
          else destination = buttons[index + 1];
        }
        if (event.key === 'ArrowLeft') {
          if (row.children.length && expanded.has(row.id)) onToggle?.(row.id, false);
          else if (row.parentId) destination = buttons.find(
            (candidate) => candidate.dataset.folderId === row.parentId,
          );
        }
        if (event.key === 'Enter' || event.key === ' ') {
          onSelect?.(row.id === '__all__' ? null : row.id);
        } else if (!destination
          && !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) {
          return;
        }
        event.preventDefault();
        if (destination) focusItem(destination.dataset.folderId);
      };
      buttons.push(button);
    }
    container.replaceChildren(...buttons);
    return Object.freeze({ visibleIds, focusedId: () => rovingId });
  }
export { PROTECTED_MASK, formatDate, expirationDate, createProtectedState,
  protectedDisplay, revealProtected, editProtected, hideProtected, loadProtected,
  sortedCopy, normalizeWidths, resizeAdjacentWidths, normalizeFolderPath,
  buildFolderTree, initialExpansion, folderPreferenceKey, loadFolderExpansion,
  saveFolderExpansion, createFolderNavigationState, itemMatchesFolder,
  flattenFolderTree, renderFolderTree };
