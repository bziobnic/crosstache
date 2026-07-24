'use strict';

const CONFIG_FALLBACK = '~/.config/xv/xv.conf';
const BACKEND_LABELS = {
  local: 'Local',
  azure: 'Azure',
  aws: 'AWS',
  unknown: 'Unknown backend',
};

const escapeHtml = (value = '') =>
  String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;');

const plainText = (html) =>
  html
    .replaceAll(/<[^>]+>/g, ' ')
    .replaceAll('&lt;', '<')
    .replaceAll('&gt;', '>')
    .replaceAll('&quot;', '"')
    .replaceAll('&#039;', "'")
    .replaceAll('&amp;', '&')
    .replaceAll(/\s+/g, ' ')
    .trim();

const createView = (html, selectors = []) => ({
  html,
  textContent: plainText(html),
  mount(root) {
    const parsed = new DOMParser().parseFromString(html, 'text/html');
    root.replaceChildren(...parsed.body.childNodes);
  },
  querySelector(selector) {
    return selectors.includes(selector) ? { selector } : null;
  },
});

const phase = (eyebrow, title, detail, backend = '') => {
  const label = backend ? BACKEND_LABELS[backend] ?? escapeHtml(backend) : '';
  const html = `
    <section class="phase" aria-labelledby="phase-title">
      <p class="eyebrow">${escapeHtml(eyebrow)}</p>
      <h1 id="phase-title" tabindex="-1">${escapeHtml(title)}</h1>
      <p class="lede">${escapeHtml(detail)}</p>
      ${label ? `<p class="backend-chip">${escapeHtml(label)}</p>` : ''}
      <div class="progress" aria-label="${escapeHtml(title)}"><span></span></div>
    </section>`;
  return createView(html);
};

const setupRequired = (state) => {
  const path = escapeHtml(state.config_path || CONFIG_FALLBACK);
  const choices = [
    ['local', 'Create local vault', 'Keep encrypted data and key material on this device.'],
    ['azure', 'Connect Azure', 'Use your existing Azure CLI session and Key Vault scope.'],
    ['aws', 'Connect AWS', 'Use your AWS profile or default credential chain.'],
    ['advanced', 'Advanced configuration', 'Open the config file or continue from the command line.'],
  ];
  const buttons = choices
    .map(
      ([kind, title, detail]) => `
        <button class="choice ${kind === 'advanced' ? 'choice-secondary' : ''}"
                type="button" data-action="choose-backend" data-backend="${kind}">
          <span>${title}</span><small>${detail}</small>
        </button>`,
    )
    .join('');
  const selectors = [
    '[data-action="choose-backend"]',
    ...choices.map(([, ,], index) => `[data-choice="${index}"]`),
  ];
  return createView(
    `<section aria-labelledby="phase-title">
      <p class="eyebrow">Setup required</p>
      <h1 id="phase-title" tabindex="-1">Choose where Crosstache stores secrets</h1>
      <p class="lede">Crosstache only asks for non-secret connection details. Credentials stay with your provider tools.</p>
      <div class="choice-grid">${buttons}</div>
      <p class="config-footnote">Config file <code>${path}</code></p>
    </section>`,
    selectors,
  );
};

const FIELD_SETS = {
  local: [
    ['store_path', 'Store path', 'Folder for the encrypted local store.', true],
    ['key_file', 'Key file', 'Path to the local encryption key file.', true],
    ['vault', 'Vault', 'The vault name to open after setup.', true],
  ],
  azure: [
    ['subscription_id', 'Subscription ID', 'Azure subscription containing the vault.', true],
    ['tenant_id', 'Tenant ID', 'Microsoft Entra tenant used by your CLI session.', true],
    ['vault', 'Vault', 'Azure Key Vault name.', true],
    ['resource_group', 'Resource group', 'Resource group containing the vault.', true],
    ['location', 'Location', 'Azure region for this vault.', true],
  ],
  aws: [
    ['region', 'Region', 'AWS region containing your secrets.', true],
    ['profile', 'Profile (optional)', 'Named AWS profile; leave blank for the default chain.', false],
    ['vault_prefix', 'Vault prefix', 'Prefix used to group Crosstache secrets.', true],
  ],
};

const SAFE_ERROR_LIMITS = {
  code: 128,
  operation: 128,
  backend: 128,
  vault: 512,
  message: 1_024,
  hint: 1_024,
  diagnostics: 16_384,
};
const GENERIC_SAFE_ERROR = Object.freeze({
  code: 'xv-command-failed',
  operation: 'desktop-command',
  backend: 'unknown',
  vault: '',
  message: 'Crosstache could not complete this operation.',
  hint: 'Try again or open the configuration file.',
  diagnostics: 'No safe diagnostics are available.',
});

const FORM_COPY = {
  local: ['Create local vault', 'Local storage stays on this device. No provider login is needed.'],
  azure: ['Connect Azure', 'Crosstache uses your Azure CLI session; it never collects a client secret.'],
  aws: ['Connect AWS', 'Crosstache uses your AWS credential chain; access keys are never entered here.'],
};

const renderField = ([name, label, hint, required]) => `
  <div class="field" data-field="${name}">
    <label for="${name}">${label}</label>
    <input id="${name}" name="${name}" type="text"
      ${required ? 'required aria-required="true"' : ''}
      aria-describedby="${name}-hint ${name}-error" autocomplete="off">
    <p class="field-hint" id="${name}-hint">${hint}</p>
    <p class="field-error" id="${name}-error" aria-live="polite"></p>
  </div>`;

const renderAdvanced = (configPath) =>
  createView(
    `<section aria-labelledby="phase-title">
      <button class="back" type="button" data-action="choose-backend">← Back to providers</button>
      <p class="eyebrow">Advanced configuration</p>
      <h1 id="phase-title" tabindex="-1">Continue with your existing tools</h1>
      <p class="lede">Edit the exact configuration file or initialize a backend from a terminal.</p>
      <div class="path-block">
        <span>Configuration file</span>
        <code>${escapeHtml(configPath || CONFIG_FALLBACK)}</code>
        <button type="button" class="button secondary" data-action="open-config">Open config</button>
      </div>
      <div class="command-list" aria-label="Command line equivalents">
        <p><span>Initialize any backend</span><code>xv init</code></p>
        <p><span>Refresh Azure credentials</span><code>az login</code></p>
        <p><span>Refresh AWS SSO credentials</span><code>aws sso login</code></p>
      </div>
      <p class="action-status" data-action-status role="alert" aria-live="polite"></p>
    </section>`,
    ['[data-action="choose-backend"]', '[data-action="open-config"]'],
  );

export function renderSetupForm(kind, { configPath } = {}) {
  if (kind === 'advanced') return renderAdvanced(configPath);
  const fields = FIELD_SETS[kind];
  if (!fields) return setupRequired({ config_path: configPath });
  const [title, description] = FORM_COPY[kind];
  const selectors = [
    ...fields.map(([name]) => `[name="${name}"]`),
    '[data-action="choose-backend"]',
    '[data-action="preview"]',
    '[data-action="apply"]',
  ];
  return createView(
    `<section aria-labelledby="phase-title">
      <button class="back" type="button" data-action="choose-backend">← Back to providers</button>
      <p class="eyebrow">${BACKEND_LABELS[kind]} setup</p>
      <h1 id="phase-title" tabindex="-1">${title}</h1>
      <p class="lede">${description}</p>
      <form data-setup-kind="${kind}" novalidate>
        <div class="form-fields">${fields.map(renderField).join('')}</div>
        <div class="form-status" data-form-status role="alert" aria-live="assertive"></div>
        <section class="preview-card" data-preview hidden aria-live="polite"></section>
        <div class="form-actions">
          <button class="button primary" type="submit" data-action="preview">Preview setup</button>
          <button class="button primary" type="button" data-action="apply" hidden disabled>Apply setup</button>
        </div>
      </form>
    </section>`,
    selectors,
  );
}

export function renderRecovery(error = {}) {
  const value = (key, fallback = 'Not available') => escapeHtml(error[key] || fallback);
  return createView(
    `<section class="recovery" aria-labelledby="phase-title">
      <p class="eyebrow danger">Recovery</p>
      <div class="recovery-banner" role="alert">
        <h1 id="phase-title" tabindex="-1">Crosstache could not open this vault</h1>
        <p>${value('message')}</p>
        <p class="recovery-hint">${value('hint', 'Review the safe details below, then choose a recovery action.')}</p>
      </div>
      <dl class="evidence">
        <div><dt>Code</dt><dd><code>${value('code')}</code></dd></div>
        <div><dt>Operation</dt><dd>${value('operation')}</dd></div>
        <div><dt>Backend</dt><dd>${value('backend')}</dd></div>
        <div><dt>Vault</dt><dd>${value('vault')}</dd></div>
      </dl>
      <details>
        <summary>Safe diagnostics</summary>
        <pre>${value('diagnostics')}</pre>
      </details>
      <div class="recovery-actions">
        <button class="button primary" type="button" data-action="retry">Retry</button>
        <button class="button secondary" type="button" data-action="choose-backend">Choose backend</button>
        <button class="button secondary" type="button" data-action="open-config">Open config</button>
        <button class="button secondary" type="button" data-action="copy-diagnostics">Copy diagnostics</button>
        <button class="button quiet" type="button" data-action="show-cli">Show CLI</button>
      </div>
      <div class="cli-help" data-cli-help hidden>
        <p><code>xv init</code></p><p><code>az login</code></p><p><code>aws sso login</code></p>
      </div>
      <p class="action-status" data-action-status aria-live="polite"></p>
    </section>`,
    [
      '[data-action="retry"]',
      '[data-action="choose-backend"]',
      '[data-action="open-config"]',
      '[data-action="copy-diagnostics"]',
      '[data-action="show-cli"]',
    ],
  );
}

export function renderStartupState(state = {}) {
  switch (state.kind) {
    case 'loading-configuration':
      return phase('Starting Crosstache', 'Loading configuration', 'Finding your active backend and vault.');
    case 'connecting':
      return phase(
        'Checking connection',
        `Connecting to ${state.vault || 'your vault'}`,
        `Verifying access with ${BACKEND_LABELS[state.backend] ?? state.backend ?? 'the selected backend'}.`,
        state.backend,
      );
    case 'setup-required':
      return setupRequired(state);
    case 'recoverable-failure':
    case 'recovery':
      return renderRecovery(state.error || state);
    case 'ready':
      return phase('Connection verified', 'Opening your vault', 'Crosstache is ready.');
    default:
      return phase('Starting Crosstache', 'Loading configuration', 'Finding your active backend and vault.');
  }
}

export function validateSetup(kind, values) {
  const errors = {};
  for (const [name, label, , required] of FIELD_SETS[kind] || []) {
    if (required && !String(values[name] || '').trim()) {
      errors[name] = `${label} is required.`;
    }
  }
  return errors;
}

const requestFor = (kind, values = {}) => {
  const fields = FIELD_SETS[kind];
  if (!fields) throw new TypeError('Unsupported setup backend.');
  const clean = Object.fromEntries(
    fields.map(([name]) => [name, String(values?.[name] ?? '').trim()]),
  );
  if (kind === 'aws' && !clean.profile) clean.profile = null;
  return Object.freeze({ backend: kind, ...clean });
};

const safeError = (error) => {
  if (!error || typeof error !== 'object' || Array.isArray(error)) {
    return GENERIC_SAFE_ERROR;
  }
  const expected = Object.keys(SAFE_ERROR_LIMITS);
  const actual = Object.keys(error);
  if (
    actual.length !== expected.length
    || !expected.every((field) => (
      Object.hasOwn(error, field)
      && typeof error[field] === 'string'
      && error[field].length <= SAFE_ERROR_LIMITS[field]
    ))
    || !error.code.startsWith('xv-')
    || !error.operation
    || !error.backend
    || !error.message
    || !error.hint
    || !error.diagnostics
  ) {
    return GENERIC_SAFE_ERROR;
  }
  return Object.freeze(Object.fromEntries(expected.map((field) => [field, error[field]])));
};

export function createStartupWorkflow({ invoke, listen, onRender }) {
  let selectedKind = null;
  let configPath = CONFIG_FALLBACK;
  let previewedRequest = null;
  let setupEpoch = 0;
  let snapshotEpoch = 0;
  let stopListening = null;
  const render = (view) => {
    onRender(view);
    return view;
  };
  const renderState = (state) => {
    if (state?.config_path) configPath = state.config_path;
    return render(renderStartupState(state));
  };

  return {
    async start() {
      renderState({ kind: 'loading-configuration' });
      const requestEpoch = ++snapshotEpoch;
      try {
        stopListening = await listen('xv://startup-state', ({ payload }) => {
          snapshotEpoch += 1;
          renderState(payload);
        });
        const state = await invoke('startup_status');
        if (requestEpoch !== snapshotEpoch) return { stale: true };
        return renderState(state);
      } catch (error) {
        if (requestEpoch !== snapshotEpoch) return { stale: true };
        return render(renderRecovery(safeError(error)));
      }
    },
    stop() {
      stopListening?.();
    },
    selectBackend(kind) {
      selectedKind = kind;
      previewedRequest = null;
      setupEpoch += 1;
      return render(renderSetupForm(kind, { configPath }));
    },
    invalidatePreview() {
      previewedRequest = null;
      setupEpoch += 1;
    },
    async preview(kind, values) {
      if (!FIELD_SETS[kind]) {
        return { ok: false, error: GENERIC_SAFE_ERROR };
      }
      const errors = validateSetup(kind, values);
      if (Object.keys(errors).length) return { ok: false, errors };
      if (selectedKind !== kind) {
        selectedKind = kind;
        setupEpoch += 1;
      }
      const generation = ++setupEpoch;
      previewedRequest = null;
      const request = requestFor(kind, values);
      try {
        const preview = await invoke('preview_setup', { request });
        if (generation !== setupEpoch || selectedKind !== kind) {
          return { ok: false, stale: true };
        }
        previewedRequest = { generation, kind, request };
        return { ok: true, preview, request, generation };
      } catch (error) {
        if (generation !== setupEpoch || selectedKind !== kind) {
          return { ok: false, stale: true };
        }
        return { ok: false, error: safeError(error) };
      }
    },
    async apply() {
      const preview = previewedRequest;
      if (
        !preview
        || preview.generation !== setupEpoch
        || preview.kind !== selectedKind
      ) {
        return { ok: false, error: GENERIC_SAFE_ERROR };
      }
      previewedRequest = null;
      try {
        const result = await invoke('apply_setup', { request: preview.request });
        if (preview.generation !== setupEpoch || preview.kind !== selectedKind) {
          return { ok: false, stale: true };
        }
        return { ok: true, result };
      } catch (error) {
        if (preview.generation !== setupEpoch || preview.kind !== selectedKind) {
          return { ok: false, stale: true };
        }
        previewedRequest = preview;
        return { ok: false, error: safeError(error) };
      }
    },
    async retry() {
      renderState({ kind: 'loading-configuration' });
      try {
        return renderState(await invoke('retry_startup'));
      } catch (error) {
        return render(renderRecovery(safeError(error)));
      }
    },
    chooseBackend() {
      selectedKind = null;
      previewedRequest = null;
      setupEpoch += 1;
      return renderState({ kind: 'setup-required', config_path: configPath });
    },
    async openConfig() {
      return invoke('open_config');
    },
    async copyDiagnostics() {
      return invoke('copy_diagnostics');
    },
  };
}

const collectValues = (form) =>
  Object.fromEntries(new FormData(form).entries());

export const mountBrowser = () => {
  const root = document.querySelector('#app');
  const internals = globalThis.__TAURI_INTERNALS__;
  const globalTauri = globalThis.__TAURI__;
  const invoke = globalTauri?.core?.invoke
    ? (command, args) => globalTauri.core.invoke(command, args)
    : internals?.invoke
      ? (command, args) => internals.invoke(command, args)
      : null;
  if (!root || !invoke) return null;

  const listen = async (event, handler) => {
    if (globalTauri?.event?.listen) return globalTauri.event.listen(event, handler);
    const callbackId = internals.transformCallback(handler);
    const eventId = await internals.invoke('plugin:event|listen', {
      event,
      target: { kind: 'Any' },
      handler: callbackId,
    });
    return () => {
      globalThis.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener?.(event, eventId);
      return internals.invoke('plugin:event|unlisten', { event, eventId });
    };
  };
  const emit = (event, payload = null) => {
    if (globalTauri?.event?.emit) return globalTauri.event.emit(event, payload);
    return internals.invoke('plugin:event|emit', { event, payload });
  };
  const workflow = createStartupWorkflow({
    invoke,
    listen,
    onRender(view) {
      view.mount(root);
      root.querySelector('h1')?.focus();
    },
  });
  let closeDialog = null;
  const requestWindowClose = () => {
    const dirtyForm = root.querySelector('form[data-dirty="true"]');
    if (!dirtyForm) return emit('xv://window-close-approved');
    if (closeDialog?.isConnected) {
      closeDialog.querySelector('[data-close-action="keep"]')?.focus();
      return null;
    }

    const returnFocus = dirtyForm.querySelector('input, button');
    const dialog = document.createElement('dialog');
    dialog.className = 'close-confirmation';
    dialog.setAttribute('role', 'alertdialog');
    dialog.setAttribute('aria-labelledby', 'close-confirmation-title');
    const title = document.createElement('h2');
    title.id = 'close-confirmation-title';
    title.textContent = 'Discard setup changes?';
    const description = document.createElement('p');
    description.textContent = 'Your unfinished setup values will be lost.';
    const actions = document.createElement('div');
    actions.className = 'close-confirmation-actions';
    const keep = document.createElement('button');
    keep.type = 'button';
    keep.dataset.closeAction = 'keep';
    keep.textContent = 'Keep editing';
    const discard = document.createElement('button');
    discard.type = 'button';
    discard.className = 'danger-action';
    discard.textContent = 'Discard and close';
    const dismiss = () => {
      dialog.close();
      dialog.remove();
      closeDialog = null;
    };
    keep.addEventListener('click', () => {
      dismiss();
      returnFocus?.focus();
    });
    discard.addEventListener('click', () => {
      dismiss();
      emit('xv://window-close-approved');
    });
    dialog.addEventListener('cancel', (event) => {
      event.preventDefault();
      keep.click();
    });
    actions.append(keep, discard);
    dialog.append(title, description, actions);
    document.body.append(dialog);
    closeDialog = dialog;
    dialog.showModal();
    keep.focus();
    return null;
  };
  const stopCloseListener = listen(
    'xv://window-close-requested',
    requestWindowClose,
  ).catch(() => null);

  const showFormError = (form, error) => {
    form.querySelector('[data-form-status]').textContent =
      error?.message || 'Crosstache could not complete this setup step.';
  };
  const runAction = async (action, successMessage = '') => {
    const status = root.querySelector('[data-action-status]');
    if (status) status.textContent = '';
    try {
      await action();
      if (status) status.textContent = successMessage;
    } catch (error) {
      if (status) status.textContent = safeError(error).message;
    }
  };
  const clearPreview = (form) => {
    workflow.invalidatePreview();
    const preview = form.querySelector('[data-preview]');
    const apply = form.querySelector('[data-action="apply"]');
    preview.hidden = true;
    preview.replaceChildren();
    apply.hidden = true;
    apply.disabled = true;
  };
  const showValidation = (form, errors) => {
    for (const input of form.elements) {
      if (!input.name) continue;
      const message = errors[input.name] || '';
      const error = form.querySelector(`#${CSS.escape(input.name)}-error`);
      if (!error) continue;
      input.setAttribute('aria-invalid', String(Boolean(message)));
      error.textContent = message;
    }
    form.querySelector('[aria-invalid="true"]')?.focus();
  };

  root.addEventListener('input', ({ target }) => {
    const form = target.closest('form');
    if (!form) return;
    form.dataset.dirty = 'true';
    clearPreview(form);
    target.setAttribute('aria-invalid', 'false');
    const fieldError = target.name
      ? form.querySelector(`#${CSS.escape(target.name)}-error`)
      : null;
    if (fieldError) fieldError.textContent = '';
    const formStatus = form.querySelector('[data-form-status]');
    if (formStatus) formStatus.textContent = '';
  });
  root.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.target;
    showValidation(form, {});
    const result = await workflow.preview(form.dataset.setupKind, collectValues(form));
    if (result.stale) return;
    if (!result.ok) {
      if (result.errors) showValidation(form, result.errors);
      if (result.error) showFormError(form, result.error);
      return;
    }
    const preview = form.querySelector('[data-preview]');
    const title = document.createElement('h2');
    title.textContent = 'Ready to apply';
    const detail = document.createElement('p');
    const scope = [result.preview?.backend, result.preview?.vault].filter(Boolean).join(' · ');
    detail.textContent = scope || 'Configuration is valid and ready to write.';
    preview.replaceChildren(title, detail);
    preview.hidden = false;
    const apply = form.querySelector('[data-action="apply"]');
    apply.hidden = false;
    apply.disabled = false;
    apply.focus();
  });
  root.addEventListener('click', async ({ target }) => {
    const button = target.closest('[data-action]');
    if (!button) return;
    const action = button.dataset.action;
    if (action === 'choose-backend') {
      const backend = button.dataset.backend;
      backend ? workflow.selectBackend(backend) : workflow.chooseBackend();
    } else if (action === 'apply') {
      const form = button.closest('form');
      button.disabled = true;
      button.textContent = 'Applying…';
      await Promise.resolve(emit('xv://save-pending-changed', true)).catch(() => {});
      let result;
      try {
        result = await workflow.apply();
      } finally {
        await Promise.resolve(emit('xv://save-pending-changed', false)).catch(() => {});
      }
      if (result.stale) {
        button.disabled = false;
        button.textContent = 'Apply setup';
        return;
      }
      if (!result.ok) {
        button.disabled = false;
        button.textContent = 'Apply setup';
        showFormError(form, result.error);
      }
    } else if (action === 'retry') {
      await workflow.retry();
    } else if (action === 'open-config') {
      await runAction(() => workflow.openConfig(), 'Configuration file opened.');
    } else if (action === 'copy-diagnostics') {
      await runAction(() => workflow.copyDiagnostics(), 'Safe diagnostics copied.');
    } else if (action === 'show-cli') {
      const help = root.querySelector('[data-cli-help]');
      help.hidden = !help.hidden;
      button.textContent = help.hidden ? 'Show CLI' : 'Hide CLI';
    }
  });
  window.addEventListener('beforeunload', () => {
    workflow.stop();
    stopCloseListener.then((stop) => stop?.());
  }, { once: true });
  workflow.start().catch((error) => root.replaceChildren(document.createTextNode(safeError(error).message)));
  return workflow;
};

if (typeof document !== 'undefined') mountBrowser();
