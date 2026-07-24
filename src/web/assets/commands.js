const collator = new Intl.Collator(undefined, {
  sensitivity: 'base',
  numeric: true,
});

const DEFAULT_COMMANDS = Object.freeze([
  Object.freeze({
    id: 'open-palette',
    label: 'Open command palette',
    shortcut: 'mod+k',
    surface: 'application',
    target: 'commands-open',
  }),
  Object.freeze({
    id: 'search-local',
    label: 'Search secrets',
    shortcut: '/',
    surface: 'current',
  }),
  Object.freeze({
    id: 'new-secret',
    label: 'New secret',
    shortcut: 'mod+n',
    surface: 'secrets',
    target: 'new-secret',
  }),
  Object.freeze({
    id: 'dismiss-topmost',
    label: 'Close current view or selection',
    shortcut: 'escape',
    surface: 'current',
  }),
]);

export function shouldHandleShortcut(event) {
  const target = event?.target;
  if (!target || target.isContentEditable) return false;
  return !['INPUT', 'TEXTAREA', 'SELECT'].includes(String(target.tagName || '').toUpperCase());
}

function named(value) {
  return typeof value === 'string' ? value : value?.name || '';
}

function normalizedScope(scope = {}) {
  return Object.freeze({
    alias: scope.alias || scope.workspace?.alias || '',
    backend: named(scope.backend),
    vault: named(scope.vault),
  });
}

function paletteMatch(values, query) {
  return values.some((value) => normalizeSearchText(value).includes(query));
}

export function createCommandRegistry() {
  let metadata = Object.freeze({
    secrets: Object.freeze([]),
    files: Object.freeze([]),
    folders: Object.freeze([]),
    scope: normalizedScope(),
  });

  function replaceMetadata({
    secrets = [],
    files = [],
    folders = [],
    scope = {},
  } = {}) {
    metadata = Object.freeze({
      secrets: Object.freeze(secrets.map((secret) => Object.freeze({
        name: secret?.original_name || secret?.name || '',
        folder: secret?.folder || '',
        groups: Array.isArray(secret?.groups)
          ? [...secret.groups]
          : (typeof secret?.groups === 'string' ? secret.groups : ''),
        type: secretType(secret),
      }))),
      files: Object.freeze(files.map((file) => Object.freeze({
        name: file?.name || '',
        folder: fileFolder(file),
        content_type: file?.content_type || '',
      }))),
      folders: Object.freeze(folders.map((folder) => Object.freeze({
        name: typeof folder === 'string' ? folder : (folder?.name || folder?.path || ''),
        surface: typeof folder === 'string' ? 'secrets' : (folder?.surface || 'secrets'),
      }))),
      scope: normalizedScope(scope),
    });
  }

  function search(query, { context } = {}) {
    const normalizedQuery = normalizeSearchText(query);
    const scope = metadata.scope.backend ? metadata.scope : normalizedScope(context);
    const results = [];
    for (const command of DEFAULT_COMMANDS) {
      if (normalizedQuery && !paletteMatch([command.label, command.id], normalizedQuery)) continue;
      results.push(Object.freeze({
        ...command,
        kind: 'command',
        name: command.label,
        scope,
      }));
    }
    if (!normalizedQuery) return results;
    for (const secret of metadata.secrets) {
      if (!paletteMatch([
        secret.name,
        secret.original_name,
        secret.folder,
        ...(Array.isArray(secret.groups) ? secret.groups : [secret.groups]),
        secret.record_type,
        secret.type,
      ], normalizedQuery)) continue;
      results.push(Object.freeze({
        kind: 'secret',
        name: secret.original_name || secret.name || '',
        folder: secret.folder || '',
        surface: 'secrets',
        scope,
      }));
    }
    for (const file of metadata.files) {
      if (!paletteMatch([file.name, file.folder, file.content_type], normalizedQuery)) continue;
      results.push(Object.freeze({
        kind: 'file',
        name: file.name || '',
        folder: file.folder || '',
        surface: 'files',
        scope,
      }));
    }
    for (const folder of metadata.folders) {
      if (!paletteMatch([folder.name, folder.path], normalizedQuery)) continue;
      results.push(Object.freeze({
        kind: 'folder',
        name: folder.name || folder.path || '',
        surface: folder.surface || 'secrets',
        scope,
      }));
    }
    for (const workspace of context?.workspace?.entries || []) {
      if (!paletteMatch([workspace.alias, workspace.backend, workspace.vault], normalizedQuery)) continue;
      results.push(Object.freeze({
        kind: 'workspace',
        name: workspace.alias,
        surface: 'context',
        scope: normalizedScope({
          alias: workspace.alias,
          backend: workspace.backend,
          vault: workspace.vault,
        }),
        contextChanging: workspace.alias !== context?.workspace?.alias,
      }));
    }
    return results;
  }

  return Object.freeze({
    commands: () => DEFAULT_COMMANDS,
    replaceMetadata,
    search,
    snapshot: () => Object.freeze({
      commandCount: DEFAULT_COMMANDS.length,
      secretCount: metadata.secrets.length,
      fileCount: metadata.files.length,
      folderCount: metadata.folders.length,
      scope: metadata.scope,
    }),
  });
}

function surfaceLabel(surface) {
  return {
    application: 'Application',
    current: 'Current view',
    secrets: 'Secrets',
    files: 'Files',
    context: 'Context',
  }[surface] || surface;
}

function sameScope(result, context) {
  return result.scope?.alias === (context?.workspace?.alias || '')
    && result.scope?.backend === named(context?.backend)
    && result.scope?.vault === named(context?.vault);
}

function activeSurface(document) {
  return document.querySelector?.('[role="tab"][aria-selected="true"]')?.id?.replace('tab-', '')
    || 'secrets';
}

export function mountCommandPalette({
  registry,
  store,
  guardNavigation,
  activateContext,
  dialogs,
  document = globalThis.document,
} = {}) {
  const byId = (id) => document.getElementById(id);
  const dialog = byId('commands-dialog');
  const opener = byId('commands-open');
  const closer = byId('commands-close');
  const query = byId('commands-query');
  const list = byId('commands-results');
  const empty = byId('commands-empty');
  let results = [];
  let activeIndex = 0;

  function close() {
    query.value = '';
    if (dialogs) dialogs.closeModal(dialog);
    else dialog.hidden = true;
  }

  function currentContext() {
    return store.snapshot().context;
  }

  async function showSurface(surface) {
    if (!['secrets', 'files', 'trash'].includes(surface)) return true;
    const tab = byId(`tab-${surface}`);
    if (!tab || tab.hidden || tab.disabled) return false;
    await tab.onclick?.();
    return tab.getAttribute('aria-selected') === 'true';
  }

  function clearSurfaceDiscovery(surface) {
    const singular = surface === 'files' ? 'file' : 'secret';
    const search = byId(surface === 'files' ? 'file-search' : 'search');
    if (search?.value) {
      search.value = '';
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    const clearFilters = byId(`${singular}-filters-clear`);
    if (clearFilters && !clearFilters.hidden) clearFilters.click();
    const allItems = [...document.querySelectorAll?.(`#${surface}-folder-tree .folder-tree-item`) || []]
      .find((item) => item.querySelector('.folder-tree-label')?.textContent === 'All items');
    allItems?.click();
  }

  function focusLocalSearch() {
    const surface = activeSurface(document);
    const search = byId(surface === 'files' ? 'file-search' : 'search');
    search?.focus();
  }

  async function activate(result) {
    const before = store.snapshot();
    if (before.contextSwitchPending || before.savePending || before.scopedMutationPending) return false;
    if (result.contextChanging) {
      close();
      if (!(await guardNavigation(result))) return false;
      return Boolean(await activateContext?.(result.scope.alias, { skipGuard: true }));
    }
    if (result.kind !== 'workspace' && result.kind !== 'command' && !sameScope(result, before.context)) {
      render();
      return false;
    }
    close();
    if (result.kind === 'command') {
      if (result.id === 'search-local') {
        focusLocalSearch();
        return true;
      }
      if (result.id === 'new-secret') {
        if (!(await showSurface('secrets'))) return false;
        byId('new-secret')?.click();
        return true;
      }
      return result.id === 'dismiss-topmost';
    }
    if (result.kind === 'workspace') return true;
    if (!(await showSurface(result.surface))) return false;
    clearSurfaceDiscovery(result.surface);
    if (result.kind === 'folder') {
      byId(`${result.surface}-folders-expand-all`)?.click();
      const folder = [...document.querySelectorAll?.(`#${result.surface}-folder-tree .folder-tree-item`) || []]
        .find((item) => item.querySelector('.folder-tree-label')?.textContent === result.name);
      folder?.click();
      folder?.focus();
      return Boolean(folder);
    }
    const search = byId(result.surface === 'files' ? 'file-search' : 'search');
    if (search) {
      search.value = result.name;
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    if (result.kind === 'secret') {
      const action = [...document.querySelectorAll?.('#secrets-table .row-action') || []]
        .find((button) => button.textContent.trim() === result.name);
      action?.click();
      return Boolean(action);
    }
    if (result.kind === 'file') {
      const link = [...document.querySelectorAll?.('#files-table .file-link') || []]
        .find((candidate) => candidate.textContent.trim() === result.name);
      link?.focus();
      return Boolean(link);
    }
    return false;
  }

  function setActive(index) {
    const options = [...list.querySelectorAll('[role="option"]')];
    if (!options.length) return;
    activeIndex = (index + options.length) % options.length;
    for (const [optionIndex, option] of options.entries()) {
      option.setAttribute('aria-selected', String(optionIndex === activeIndex));
    }
    query.setAttribute('aria-activedescendant', options[activeIndex].id);
    options[activeIndex].scrollIntoView?.({ block: 'nearest' });
  }

  function render() {
    const surface = activeSurface(document);
    results = registry.search(query.value, { context: currentContext() }).map((result) => (
      result.id === 'search-local'
        ? { ...result, name: surface === 'files' ? 'Search files' : 'Search secrets' }
        : result
    ));
    const options = results.map((result, index) => {
      const option = document.createElement('button');
      option.type = 'button';
      option.className = 'command-result';
      option.id = `command-result-${index}`;
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', 'false');
      const title = document.createElement('strong');
      title.textContent = result.name;
      const scope = document.createElement('small');
      scope.textContent = `${surfaceLabel(result.surface)} · ${result.scope.backend} / ${result.scope.vault}`;
      option.append(title, scope);
      option.onclick = () => { void activate(result); };
      return option;
    });
    list.replaceChildren(...options);
    empty.hidden = options.length > 0;
    activeIndex = 0;
    if (options.length) setActive(0);
    else query.removeAttribute('aria-activedescendant');
  }

  function open() {
    const snapshot = store.snapshot();
    if (!snapshot.context
      || snapshot.contextSwitchPending
      || snapshot.savePending
      || snapshot.scopedMutationPending) {
      return false;
    }
    const top = dialogs?.topModal?.();
    if (top && top !== byId('drawer')) return false;
    query.value = '';
    render();
    if (dialogs) {
      dialogs.openModal(dialog, {
        initialFocus: query,
        invoker: document.activeElement || opener,
        onEscape: close,
      });
    } else {
      dialog.hidden = false;
      query.focus();
    }
    return true;
  }

  query.oninput = render;
  query.onkeydown = (event) => {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      setActive(activeIndex + 1);
    } else if (event.key === 'ArrowUp') {
      event.preventDefault();
      setActive(activeIndex - 1);
    } else if (event.key === 'Enter' && results[activeIndex]) {
      event.preventDefault();
      void activate(results[activeIndex]);
    }
  };
  opener.onclick = open;
  closer.onclick = close;

  function dismissSelection() {
    const surface = activeSurface(document);
    const cancel = byId(surface === 'files' ? 'cancel-file-selection' : 'cancel-secret-selection');
    if (!cancel?.closest?.('.bulk-toolbar')?.hidden && !cancel.disabled) {
      cancel.click();
      return true;
    }
    const notice = byId('action-notice');
    if (notice && !notice.hidden) {
      byId('dismiss-action-notice')?.click();
      return true;
    }
    return false;
  }

  document.addEventListener?.('keydown', (event) => {
    if (event.defaultPrevented) return;
    const key = event.key?.toLowerCase();
    const modifier = event.metaKey || event.ctrlKey;
    if (key === 'escape') {
      if (dialogs?.topModal?.()) return;
      if (dismissSelection()) event.preventDefault();
      return;
    }
    if (modifier && key === 'k') {
      event.preventDefault();
      open();
      return;
    }
    if (!shouldHandleShortcut(event)) return;
    if (!modifier && key === '/') {
      event.preventDefault();
      focusLocalSearch();
    } else if (modifier && key === 'n') {
      event.preventDefault();
      const result = registry.commands().find(({ id }) => id === 'new-secret');
      void activate({ ...result, kind: 'command', scope: normalizedScope(currentContext()) });
    }
  });

  return Object.freeze({ open, close, render, activate });
}

function normalizeSearchText(value) {
  return typeof value === 'string'
    ? value.normalize('NFKC').toLocaleLowerCase().trim()
    : '';
}

function normalizedList(value) {
  const values = Array.isArray(value)
    ? value
    : (typeof value === 'string' ? value.split(',') : []);
  return values.map(normalizeSearchText).filter(Boolean);
}

function fileFolder(file) {
  if (typeof file?.folder === 'string') return file.folder;
  const name = typeof file?.name === 'string' ? file.name : '';
  const separator = name.lastIndexOf('/');
  return separator < 0 ? '' : name.slice(0, separator);
}

function secretType(secret) {
  return secret?.tags?.['xv-type']
    || secret?.record_type
    || secret?.type
    || (secret?.content_type === 'application/vnd.xv.record' ? 'record' : 'plain');
}

function entry(surface, sourceIndex, name, folder, terms, searchName = name) {
  return Object.freeze({
    surface,
    sourceIndex,
    name: typeof name === 'string' ? name : '',
    folder: typeof folder === 'string' ? folder : '',
    normalizedName: normalizeSearchText(searchName),
    normalizedFolder: normalizeSearchText(folder),
    normalizedTerms: Object.freeze(terms.flatMap(normalizedList)),
  });
}

export function buildMetadataIndex({ secrets = [], files = [], folders = [] } = {}) {
  const entries = [
    ...secrets.map((secret, sourceIndex) => entry(
      'secrets',
      sourceIndex,
      secret?.original_name || secret?.name,
      secret?.folder,
      [secret?.groups, secretType(secret)],
    )),
    ...files.map((file, sourceIndex) => entry(
      'files',
      sourceIndex,
      file?.name,
      fileFolder(file),
      [file?.content_type],
      file?.name?.split('/').at(-1),
    )),
    ...folders.map((folder, sourceIndex) => entry(
      'folders',
      sourceIndex,
      folder,
      folder,
      [],
    )),
  ];
  return Object.freeze({ entries: Object.freeze(entries) });
}

function matchRank(entryValue, query) {
  if (entryValue === query) return 0;
  if (entryValue.startsWith(query)) return 1;
  if (entryValue.split(/[^\p{Letter}\p{Number}]+/u).some((word) => word.startsWith(query))) {
    return 2;
  }
  return entryValue.includes(query) ? 3 : null;
}

export function searchIndex(index, query) {
  const normalizedQuery = normalizeSearchText(query);
  if (!normalizedQuery) return [];
  const ranked = [];
  for (const indexed of index?.entries || []) {
    let score = matchRank(indexed.normalizedName, normalizedQuery);
    if (score === null && indexed.normalizedTerms.some((term) => term.includes(normalizedQuery))) {
      score = 4;
    }
    if (score === null && indexed.normalizedFolder.includes(normalizedQuery)) score = 5;
    if (score !== null) ranked.push({ indexed, score });
  }
  ranked.sort((left, right) => (
    left.score - right.score
      || collator.compare(left.indexed.name, right.indexed.name)
      || collator.compare(left.indexed.surface, right.indexed.surface)
      || left.indexed.sourceIndex - right.indexed.sourceIndex
  ));
  return ranked.map(({ indexed }) => indexed);
}

export { normalizeSearchText };
