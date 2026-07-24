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

const requestFor = (kind, values) => {
  const clean = Object.fromEntries(
    Object.entries(values).map(([name, value]) => [name, String(value).trim()]),
  );
  if (kind === 'aws' && !clean.profile) clean.profile = null;
  return { backend: kind, ...clean };
};

const safeError = (error) =>
  error && typeof error === 'object'
    ? error
    : {
        code: 'xv-command-failed',
        operation: 'desktop-command',
        message: String(error || 'The operation failed.'),
        hint: 'Try again or open the configuration file.',
        diagnostics: 'No additional safe diagnostics were provided.',
      };

export function createStartupWorkflow({ invoke, listen, onRender }) {
  let selectedKind = null;
  let configPath = CONFIG_FALLBACK;
  let previewedRequest = null;
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
      try {
        stopListening = await listen('xv://startup-state', ({ payload }) => renderState(payload));
        return renderState(await invoke('startup_status'));
      } catch (error) {
        return render(renderRecovery(safeError(error)));
      }
    },
    stop() {
      stopListening?.();
    },
    selectBackend(kind) {
      selectedKind = kind;
      previewedRequest = null;
      return render(renderSetupForm(kind, { configPath }));
    },
    invalidatePreview() {
      previewedRequest = null;
    },
    async preview(kind, values) {
      const errors = validateSetup(kind, values);
      if (Object.keys(errors).length) return { ok: false, errors };
      const request = requestFor(kind, values);
      try {
        const preview = await invoke('preview_setup', { request });
        selectedKind = kind;
        previewedRequest = request;
        return { ok: true, preview, request };
      } catch (error) {
        return { ok: false, error: safeError(error) };
      }
    },
    async apply() {
      if (!previewedRequest) {
        return { ok: false, error: { message: 'Preview setup before applying changes.' } };
      }
      const request = previewedRequest;
      previewedRequest = null;
      try {
        return { ok: true, result: await invoke('apply_setup', { request }) };
      } catch (error) {
        previewedRequest = request;
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

const mountBrowser = () => {
  const root = document.querySelector('#app');
  const internals = globalThis.__TAURI_INTERNALS__;
  if (!root || !internals?.invoke) return;

  const listen = async (event, handler) => {
    if (globalThis.__TAURI__?.event?.listen) return globalThis.__TAURI__.event.listen(event, handler);
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
  const workflow = createStartupWorkflow({
    invoke: (command, args) => internals.invoke(command, args),
    listen,
    onRender(view) {
      view.mount(root);
      root.querySelector('h1')?.focus();
    },
  });

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
      input.setAttribute('aria-invalid', String(Boolean(message)));
      form.querySelector(`#${CSS.escape(input.name)}-error`).textContent = message;
    }
    form.querySelector('[aria-invalid="true"]')?.focus();
  };

  root.addEventListener('input', ({ target }) => {
    const form = target.closest('form');
    if (!form) return;
    clearPreview(form);
    target.setAttribute('aria-invalid', 'false');
    form.querySelector(`#${CSS.escape(target.name)}-error`).textContent = '';
    form.querySelector('[data-form-status]').textContent = '';
  });
  root.addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.target;
    showValidation(form, {});
    const result = await workflow.preview(form.dataset.setupKind, collectValues(form));
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
      const result = await workflow.apply();
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
  window.addEventListener('beforeunload', () => workflow.stop(), { once: true });
  workflow.start().catch((error) => root.replaceChildren(document.createTextNode(safeError(error).message)));
};

if (typeof document !== 'undefined') mountBrowser();
