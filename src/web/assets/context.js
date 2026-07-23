const SOURCE_LABELS = Object.freeze({
  cli: 'Command line',
  environment: 'Environment',
  'project-environment': 'Project environment',
  project: 'Project',
  'workspace-entry': 'Workspace entry',
  'global-config': 'Global configuration',
  'built-in': 'Built in',
});

const CAPABILITY_LABELS = Object.freeze({
  files: 'File storage',
  folders: 'Folders',
  groups: 'Groups',
  notes: 'Notes',
  expiry: 'Expiry',
  soft_delete: 'Trash',
  restore: 'Restore',
  purge: 'Permanent purge',
  scheduled_purge: 'Scheduled purge',
  versioning: 'Version history',
  rbac: 'Role-based access',
  audit: 'Audit history',
  rotation: 'Secret rotation',
  conversion: 'Record conversion',
  metadata: 'Metadata',
});

function named(value) {
  if (typeof value === 'string') return value;
  return value?.name || '';
}

function titleCase(value) {
  return String(value || '')
    .split(/[-_]/)
    .filter(Boolean)
    .map((part) => part[0].toUpperCase() + part.slice(1))
    .join(' ');
}

export function formatContextLine(context) {
  if (!context) return '';
  const scope = `${named(context.backend)} / ${named(context.vault)}`;
  const qualifiers = [named(context.project), named(context.environment)].filter(Boolean);
  return qualifiers.length ? `${scope} · ${qualifiers.join(' · ')}` : scope;
}

export function contextDetails(context) {
  const source = (key) => SOURCE_LABELS[context?.sources?.[key]]
    || titleCase(context?.sources?.[key] || 'built-in');
  const backend = named(context?.backend);
  const backendKind = named(context?.backend_kind);
  const values = [
    { label: 'Backend', value: backendKind ? `${backend} (${backendKind})` : backend, source: source('backend') },
    { label: 'Vault', value: named(context?.vault), source: source('vault') },
    { label: 'Workspace', value: context?.workspace?.alias || 'Default', source: source('workspace') },
    { label: 'Project', value: named(context?.project) || 'No project', source: source('project') },
    { label: 'Environment', value: named(context?.environment) || 'No environment', source: source('environment') },
  ];
  const limitations = Object.entries(CAPABILITY_LABELS)
    .filter(([key]) => context?.capabilities?.[key] === false)
    .map(([, label]) => `${label} unavailable`);
  const connectionState = titleCase(context?.connection?.state || 'unknown');
  return {
    values,
    connection: context?.connection?.message
      ? `${connectionState}: ${context.connection.message}`
      : connectionState,
    limitations,
    version: String(context?.version || ''),
  };
}

function errorCopy(error) {
  return {
    message: error?.message || 'Workspace activation could not be completed.',
    hint: error?.hint || 'The previous backend and vault remain active.',
  };
}

function apiRequest(api, method, path, body, options) {
  if (typeof api === 'function') return api(method, path, body, false, options);
  return api.request(method, path, body, false, options);
}

function isAbort(error) {
  return error?.name === 'AbortError';
}

export function mountContextRail({
  store,
  api,
  guardNavigation,
  document = globalThis.document,
}) {
  const byId = (id) => document.getElementById(id);
  const selector = byId('workspace-select');
  let generation = 0;
  let controller = null;

  function renderDetails(details) {
    const list = byId('context-details-list');
    if (!list) return;
    const rows = [];
    for (const item of details.values) {
      const term = document.createElement('dt');
      term.textContent = item.label;
      const description = document.createElement('dd');
      description.textContent = `${item.value} — ${item.source}`;
      rows.push(term, description);
    }
    list.replaceChildren(...rows);
  }

  function renderSelector(context, pending) {
    if (!selector) return;
    const entries = context?.workspace?.entries || [];
    const options = entries.map((entry) => {
      const option = document.createElement('option');
      option.value = entry.alias;
      option.textContent = `${entry.alias} — ${entry.backend} / ${entry.vault}`;
      return option;
    });
    selector.replaceChildren(...options);
    selector.value = context?.workspace?.alias || entries.find((entry) => entry.default)?.alias || '';
    selector.disabled = Boolean(pending);
  }

  function render(snapshot) {
    const error = byId('context-error');
    if (error) {
      error.hidden = !snapshot.contextError;
      const message = byId('context-error-message');
      if (message) message.textContent = snapshot.contextError
        ? `${snapshot.contextError.message} ${snapshot.contextError.hint}`.trim()
        : '';
    }
    const context = snapshot.context;
    if (!context) return;
    const details = contextDetails(context);
    const line = formatContextLine(context);
    if (byId('context-line')) byId('context-line').textContent = line;
    if (byId('context-backend-kind')) byId('context-backend-kind').textContent = titleCase(named(context.backend_kind));
    if (byId('context-connection')) {
      byId('context-connection').textContent = details.connection;
      byId('context-connection').dataset.state = context.connection?.state || 'unknown';
    }
    if (byId('context-capabilities')) {
      byId('context-capabilities').textContent = details.limitations.length
        ? details.limitations.join(' · ')
        : 'All attached backend capabilities available';
    }
    if (byId('context-version')) byId('context-version').textContent = `v${details.version}`;
    renderDetails(details);
    renderSelector(context, snapshot.contextSwitchPending || snapshot.savePending);
    for (const surface of document.querySelectorAll?.('[data-context-copy]') || []) {
      surface.textContent = line;
    }
  }

  const unsubscribe = store.subscribe(render);

  async function switchTo(alias) {
    const before = store.snapshot();
    if (!before.context || before.savePending) {
      render(before);
      return false;
    }
    if (!(await guardNavigation())) {
      render(store.snapshot());
      return false;
    }

    const guarded = store.snapshot();
    if (guarded.savePending) {
      render(guarded);
      return false;
    }
    const entry = guarded.context.workspace?.entries?.find((candidate) => candidate.alias === alias);
    if (!entry) {
      store.dispatch({
        type: 'context/switch-failed',
        error: errorCopy(new Error('Workspace activation target is unavailable.')),
      });
      return false;
    }

    const requestGeneration = ++generation;
    controller?.abort();
    controller = new AbortController();
    store.dispatch({ type: 'context/switch-started', alias });
    try {
      const request = {
        alias: entry.alias,
        backend: entry.backend,
        vault: entry.vault,
      };
      const resolved = await apiRequest(
        api,
        'POST',
        '/api/context/activate',
        request,
        { signal: controller.signal },
      );
      if (requestGeneration !== generation) return false;
      if (!resolved?.context) {
        throw new Error('Workspace activation did not return display-safe context.');
      }
      const activated = await apiRequest(
        api,
        'POST',
        '/api/workspaces/activate',
        request,
        { signal: controller.signal },
      );
      if (requestGeneration !== generation) return false;
      if (!Array.isArray(activated?.secrets)) {
        throw new Error('Workspace activation did not return both context and its initial secret list.');
      }
      store.dispatch({
        type: 'context/switch-succeeded',
        context: resolved.context,
        secrets: activated.secrets,
      });
      return true;
    } catch (error) {
      if (requestGeneration !== generation || isAbort(error)) return false;
      store.dispatch({ type: 'context/switch-failed', error: errorCopy(error) });
      return false;
    }
  }

  const onChange = () => { void switchTo(selector.value); };
  selector?.addEventListener?.('change', onChange);

  const ready = apiRequest(api, 'GET', '/api/context')
    .then((context) => {
      store.dispatch({ type: 'context/loaded', context });
      return context;
    })
    .catch((error) => {
      store.dispatch({ type: 'context/load-failed', error: errorCopy(error) });
      throw error;
    });

  return Object.freeze({
    ready,
    switchTo,
    destroy() {
      generation++;
      controller?.abort();
      selector?.removeEventListener?.('change', onChange);
      unsubscribe();
    },
  });
}
