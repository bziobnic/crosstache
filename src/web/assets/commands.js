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

function isEditableTarget(target) {
  return Boolean(target?.isContentEditable)
    || ['INPUT', 'TEXTAREA', 'SELECT'].includes(String(target?.tagName || '').toUpperCase());
}

export function shortcutIntent(event, { allowOwnedEscape = false } = {}) {
  if (!event || event.defaultPrevented || event.repeat || event.isComposing || event.keyCode === 229
    || event.getModifierState?.('AltGraph')) return null;
  const key = event.key?.toLowerCase();
  const ownedEscape = allowOwnedEscape
    && key === 'escape'
    && Boolean(event.target?.closest?.('[role="dialog"]'));
  if (isEditableTarget(event.target) && !ownedEscape) return null;
  if (event.altKey || event.shiftKey) return null;
  const modifierCount = Number(Boolean(event.metaKey)) + Number(Boolean(event.ctrlKey));
  if (key === 'k' && modifierCount === 1) return 'open-palette';
  if (key === 'n' && modifierCount === 1) return 'new-secret';
  if (modifierCount !== 0) return null;
  if (key === '/') return 'search-local';
  if (key === 'escape') return 'dismiss-topmost';
  if (['arrowleft', 'arrowright', 'home', 'end'].includes(key)) return `tab-${key}`;
  return null;
}

export function shouldHandleShortcut(event, options) {
  return Boolean(shortcutIntent(event, options));
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
  let operationGeneration = 0;
  let metadataSignature = '';
  let metadata = Object.freeze({
    secrets: Object.freeze([]),
    files: Object.freeze([]),
    folders: Object.freeze([]),
    scope: normalizedScope(),
    contextGeneration: '',
  });

  function replaceMetadata({
    secrets = [],
    files = [],
    folders = [],
    scope = {},
    contextGeneration = '',
    dataGeneration = '',
  } = {}) {
    const nextMetadata = {
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
      contextGeneration: String(contextGeneration || scope?.version || ''),
      dataGeneration: String(dataGeneration),
    };
    const nextSignature = JSON.stringify(nextMetadata);
    if (nextSignature === metadataSignature) return operationGeneration;
    metadataSignature = nextSignature;
    operationGeneration++;
    metadata = Object.freeze(nextMetadata);
    return operationGeneration;
  }

  function search(query, { context } = {}) {
    const normalizedQuery = normalizeSearchText(query);
    const scope = metadata.scope.backend ? metadata.scope : normalizedScope(context);
    const contextGeneration = metadata.contextGeneration || String(context?.version || '');
    const result = (values) => {
      const target = Object.freeze({
        alias: values.scope.alias,
        backend: values.scope.backend,
        vault: values.scope.vault,
        surface: values.surface,
        item: values.item || values.name || values.id || '',
      });
      return Object.freeze({
        ...values,
        scope: values.scope,
        target,
        sourceScope: normalizedScope(context),
        operationGeneration,
        contextGeneration,
      });
    };
    const results = [];
    for (const command of DEFAULT_COMMANDS) {
      if (command.id === 'open-palette') continue;
      if (normalizedQuery && !paletteMatch([command.label, command.id], normalizedQuery)) continue;
      results.push(result({
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
      results.push(result({
        kind: 'secret',
        name: secret.original_name || secret.name || '',
        folder: secret.folder || '',
        surface: 'secrets',
        scope,
      }));
    }
    for (const file of metadata.files) {
      if (!paletteMatch([file.name, file.folder, file.content_type], normalizedQuery)) continue;
      results.push(result({
        kind: 'file',
        name: file.name || '',
        folder: file.folder || '',
        surface: 'files',
        scope,
      }));
    }
    for (const folder of metadata.folders) {
      if (!paletteMatch([folder.name, folder.path], normalizedQuery)) continue;
      results.push(result({
        kind: 'folder',
        name: folder.name || folder.path || '',
        surface: folder.surface || 'secrets',
        scope,
      }));
    }
    for (const workspace of context?.workspace?.entries || []) {
      if (!paletteMatch([workspace.alias, workspace.backend, workspace.vault], normalizedQuery)) continue;
      const destination = normalizedScope({
        alias: workspace.alias,
        backend: workspace.backend,
        vault: workspace.vault,
      });
      results.push(result({
        kind: 'workspace',
        name: workspace.alias,
        surface: 'context',
        scope: destination,
        contextChanging: workspace.alias !== context?.workspace?.alias,
      }));
    }
    return results;
  }

  return Object.freeze({
    commands: () => DEFAULT_COMMANDS,
    replaceMetadata,
    search,
    isCurrent(result, context) {
      if (!result || result.operationGeneration !== operationGeneration
        || result.contextGeneration !== String(context?.version || '')) return false;
      if (result.kind === 'workspace') {
        const source = result.sourceScope;
        return source.alias === (context?.workspace?.alias || '')
          && source.backend === named(context?.backend)
          && source.vault === named(context?.vault);
      }
      return sameScope(result, context);
    },
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
    query.setAttribute('aria-expanded', 'false');
    query.removeAttribute('aria-activedescendant');
    results = [];
    list.replaceChildren();
    if (dialogs) dialogs.closeModal(dialog);
    else dialog.hidden = true;
  }

  function currentContext() {
    return store.snapshot().context;
  }

  function activationStillCurrent(result) {
    const snapshot = store.snapshot();
    return !snapshot.contextSwitchPending
      && !snapshot.savePending
      && !snapshot.scopedMutationPending
      && registry.isCurrent(result, snapshot.context);
  }

  async function showSurface(surface) {
    if (!['secrets', 'files', 'trash'].includes(surface)) return true;
    const tab = byId(`tab-${surface}`);
    if (!tab || tab.hidden || tab.disabled) return false;
    await tab.onclick?.();
    return tab.getAttribute('aria-selected') === 'true';
  }

  function clearSurfaceDiscovery(surface, result) {
    if (!activationStillCurrent(result)) return false;
    const singular = surface === 'files' ? 'file' : 'secret';
    const search = byId(surface === 'files' ? 'file-search' : 'search');
    if (search?.value) {
      if (!activationStillCurrent(result)) return false;
      search.value = '';
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    const clearFilters = byId(`${singular}-filters-clear`);
    if (clearFilters && !clearFilters.hidden) {
      if (!activationStillCurrent(result)) return false;
      clearFilters.click();
    }
    const allItems = [...document.querySelectorAll?.(`#${surface}-folder-tree .folder-tree-item`) || []]
      .find((item) => item.querySelector('.folder-tree-label')?.textContent === 'All items');
    if (allItems) {
      if (!activationStillCurrent(result)) return false;
      allItems.click();
    }
    return true;
  }

  function focusLocalSearch() {
    const surface = activeSurface(document);
    const search = byId(surface === 'files' ? 'file-search' : 'search');
    search?.focus();
  }

  async function activate(result) {
    const before = store.snapshot();
    if (before.contextSwitchPending || before.savePending || before.scopedMutationPending) return false;
    if (!activationStillCurrent(result)) return false;
    if (result.contextChanging) {
      close();
      if (!(await guardNavigation(result))) return false;
      if (!activationStillCurrent(result)) return false;
      return Boolean(await activateContext?.(result.target, { skipGuard: true }));
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
        if (!activationStillCurrent(result)) return false;
        byId('new-secret')?.click();
        return true;
      }
      return result.id === 'dismiss-topmost';
    }
    if (result.kind === 'workspace') return true;
    if (!(await showSurface(result.surface))) return false;
    if (!activationStillCurrent(result)) return false;
    if (!clearSurfaceDiscovery(result.surface, result)) return false;
    if (result.kind === 'folder') {
      if (!activationStillCurrent(result)) return false;
      byId(`${result.surface}-folders-expand-all`)?.click();
      const folder = [...document.querySelectorAll?.(`#${result.surface}-folder-tree .folder-tree-item`) || []]
        .find((item) => item.querySelector('.folder-tree-label')?.textContent === result.name);
      if (!activationStillCurrent(result)) return false;
      folder?.click();
      folder?.focus();
      return Boolean(folder);
    }
    const search = byId(result.surface === 'files' ? 'file-search' : 'search');
    if (search) {
      if (!activationStillCurrent(result)) return false;
      search.value = result.name;
      search.dispatchEvent(new Event('input', { bubbles: true }));
    }
    if (result.kind === 'secret') {
      const action = [...document.querySelectorAll?.('#secrets-table .row-action') || []]
        .find((button) => button.textContent.trim() === result.name);
      if (!activationStillCurrent(result)) return false;
      action?.click();
      return Boolean(action);
    }
    if (result.kind === 'file') {
      const link = [...document.querySelectorAll?.('#files-table .file-link') || []]
        .find((candidate) => candidate.textContent.trim() === result.name);
      if (!activationStillCurrent(result)) return false;
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
    results = registry.search(query.value, { context: currentContext() })
      .filter((result) => !(surface === 'trash' && result.id === 'search-local'));
    const options = results.map((result, index) => {
      const option = document.createElement('button');
      option.type = 'button';
      option.className = 'command-result';
      option.id = `command-result-${index}`;
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', 'false');
      option.tabIndex = -1;
      const title = document.createElement('strong');
      title.textContent = result.id === 'search-local' && surface === 'files'
        ? 'Search files'
        : result.name;
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
    query.setAttribute('aria-expanded', 'true');
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
    } else if (event.key === 'Home') {
      event.preventDefault();
      setActive(0);
    } else if (event.key === 'End') {
      event.preventDefault();
      setActive(results.length - 1);
    } else if (event.key === 'Enter' && results[activeIndex]) {
      event.preventDefault();
      void activate(results[activeIndex]);
    }
  };
  opener.onclick = open;
  closer.onclick = close;

  function dismissTopmostTransient() {
    const notice = byId('action-notice');
    if (notice && !notice.hidden) {
      byId('dismiss-action-notice')?.click();
      return true;
    }
    const surface = activeSurface(document);
    const cancel = byId(surface === 'files' ? 'cancel-file-selection' : 'cancel-secret-selection');
    if (!cancel?.closest?.('.bulk-toolbar')?.hidden && !cancel.disabled) {
      cancel.click();
      return true;
    }
    return false;
  }

  document.addEventListener?.('keydown', (event) => {
    const intent = shortcutIntent(event);
    if (!intent) return;
    if (intent === 'dismiss-topmost') {
      if (dialogs?.topModal?.()) return;
      if (dismissTopmostTransient()) event.preventDefault();
    } else if (intent === 'open-palette') {
      event.preventDefault();
      open();
    } else if (intent === 'search-local') {
      if (activeSurface(document) === 'trash') return;
      event.preventDefault();
      focusLocalSearch();
    } else if (intent === 'new-secret') {
      event.preventDefault();
      const result = registry.search('new secret', { context: currentContext() })
        .find(({ id }) => id === 'new-secret');
      if (result) void activate(result);
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
