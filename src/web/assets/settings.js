const THEMES = new Set(['system', 'light', 'dark']);
const DENSITIES = new Set(['comfortable', 'compact']);
const DEFAULT_COLUMN_WIDTHS = Object.freeze({
  secrets: Object.freeze([28, 15, 14, 25, 18]),
  files: Object.freeze([42, 12, 24, 22]),
});

function nonNegativeInteger(value) {
  const number = Number(value);
  return Number.isSafeInteger(number) && number >= 0 ? number : 0;
}

function resolve(value) {
  return typeof value === 'function' ? value() : value;
}

export function effectiveTheme(preference, mediaQuery) {
  if (preference === 'light' || preference === 'dark') return preference;
  return mediaQuery?.matches ? 'dark' : 'light';
}

export function boundTimeout(requested, policy) {
  const timeout = nonNegativeInteger(requested);
  const limit = nonNegativeInteger(policy);
  return limit > 0 ? Math.min(timeout, limit) : timeout;
}

function applyPresentation(document, preference, density, mediaQuery) {
  const root = document?.documentElement;
  if (!root) return;
  const theme = THEMES.has(preference) ? preference : 'system';
  root.dataset.theme = theme;
  root.dataset.effectiveTheme = effectiveTheme(theme, mediaQuery);
  root.dataset.density = DENSITIES.has(density) ? density : 'comfortable';
}

function setControlValue(control, value) {
  if (control) control.value = String(value);
}

function ensureTimeoutOption(document, control, value) {
  if (!control || !document?.createElement) return;
  const stringValue = String(value);
  const exists = [...(control.querySelectorAll?.('option') ?? [])]
    .some((option) => option.value === stringValue);
  if (exists) return;
  const option = document.createElement('option');
  option.value = stringValue;
  option.textContent = `${stringValue} seconds (policy)`;
  control.append(option);
}

export function mountSettings({
  preferences,
  securityPolicy,
  document = globalThis.document,
  mediaQuery = globalThis.matchMedia?.('(prefers-color-scheme: dark)'),
}) {
  const theme = document?.getElementById?.('theme-select');
  const timeout = document?.getElementById?.('exposure-timeout-select');
  const density = document?.getElementById?.('density-select');
  const reset = document?.getElementById?.('layout-reset');
  const status = document?.getElementById?.('settings-live');

  function policyLimit() {
    const policy = resolve(securityPolicy);
    return nonNegativeInteger(
      typeof policy === 'object' ? policy?.clipboard_timeout_seconds : policy,
    );
  }

  function refresh() {
    const selectedTheme = preferences.get('theme', 'system');
    const selectedDensity = preferences.get('density', 'comfortable');
    const selectedTimeout = boundTimeout(
      preferences.get('exposure_timeout_seconds', 30),
      policyLimit(),
    );
    ensureTimeoutOption(document, timeout, selectedTimeout);
    setControlValue(theme, selectedTheme);
    setControlValue(density, selectedDensity);
    setControlValue(timeout, selectedTimeout);
    applyPresentation(document, selectedTheme, selectedDensity, mediaQuery);

    const limit = policyLimit();
    for (const option of timeout?.querySelectorAll?.('option') ?? []) {
      const value = nonNegativeInteger(option.value);
      option.disabled = limit > 0 && value > limit;
    }
  }

  const onTheme = () => {
    const value = THEMES.has(theme?.value) ? theme.value : 'system';
    preferences.set('theme', value);
    applyPresentation(document, value, preferences.get('density', 'comfortable'), mediaQuery);
  };
  const onDensity = () => {
    const value = DENSITIES.has(density?.value) ? density.value : 'comfortable';
    preferences.set('density', value);
    applyPresentation(document, preferences.get('theme', 'system'), value, mediaQuery);
  };
  const onTimeout = () => {
    const value = boundTimeout(timeout?.value, policyLimit());
    preferences.set('exposure_timeout_seconds', value);
    setControlValue(timeout, value);
    if (status) status.textContent = value > 0
      ? `Protected values hide after ${value} seconds.`
      : 'Protected values use the application security policy.';
  };
  const onReset = () => {
    const widths = {
      secrets: [...DEFAULT_COLUMN_WIDTHS.secrets],
      files: [...DEFAULT_COLUMN_WIDTHS.files],
    };
    preferences.set('density', 'comfortable');
    preferences.set('column_widths', widths);
    const CustomEventType = document?.defaultView?.CustomEvent ?? globalThis.CustomEvent;
    if (CustomEventType && document?.dispatchEvent) {
      document.dispatchEvent(new CustomEventType('xv:layout-reset', { detail: { columnWidths: widths } }));
    }
    refresh();
    if (status) status.textContent = 'Layout reset. Vault and folder state were kept.';
  };
  const onSystemTheme = () => {
    if (preferences.get('theme', 'system') === 'system') refresh();
  };

  theme?.addEventListener?.('change', onTheme);
  density?.addEventListener?.('change', onDensity);
  timeout?.addEventListener?.('change', onTimeout);
  reset?.addEventListener?.('click', onReset);
  mediaQuery?.addEventListener?.('change', onSystemTheme);

  const ready = Promise.resolve(preferences.load?.()).then(refresh);
  refresh();

  return Object.freeze({
    ready,
    refresh,
    destroy() {
      theme?.removeEventListener?.('change', onTheme);
      density?.removeEventListener?.('change', onDensity);
      timeout?.removeEventListener?.('change', onTimeout);
      reset?.removeEventListener?.('click', onReset);
      mediaQuery?.removeEventListener?.('change', onSystemTheme);
    },
  });
}

function cleanLine(label, value) {
  const text = String(value ?? '').replaceAll('\r', ' ').replaceAll('\n', ' ').trim();
  return text ? `${label}: ${text}` : null;
}

export function buildHelpDiagnostics(context) {
  const safe = context && typeof context === 'object' ? context : {};
  const capabilities = safe.capabilities && typeof safe.capabilities === 'object'
    ? safe.capabilities
    : {};
  const lines = [
    `Crosstache ${String(safe.version ?? 'unknown')}`,
    cleanLine('Config', safe.config_path ?? safe.configPath),
    cleanLine('Backend', safe.backend),
    cleanLine('Vault', safe.vault),
    cleanLine('Workspace', safe.workspace?.alias),
    cleanLine('Project', safe.project?.name),
    cleanLine('Environment', safe.environment?.name),
    cleanLine('Connection', safe.connection?.state),
    cleanLine('Protected value timeout', safe.security?.clipboard_timeout_seconds),
    cleanLine(
      'Capabilities',
      ['files', 'trash', 'restore', 'purge']
        .filter((key) => capabilities[key] === true)
        .join(', '),
    ),
  ];
  return `${lines.filter(Boolean).join('\n')}\n`;
}

function setText(document, id, value) {
  const element = document?.getElementById?.(id);
  if (element) element.textContent = value;
}

function capabilityCopy(context) {
  const capabilities = context?.capabilities ?? {};
  const availability = (key) => capabilities[key] === true ? 'Available' : 'Unavailable';
  return [
    `Files: ${availability('files')}.`,
    `Trash: ${availability('trash')}; restore: ${availability('restore')}; permanent purge: ${availability('purge')}.`,
    `Atomic rename: ${availability('atomic_rename')}; protected conversion: ${availability('conditional_conversion')}.`,
  ].join(' ');
}

export function mountHelp({
  context,
  document = globalThis.document,
  clipboard = globalThis.navigator?.clipboard,
}) {
  const copy = document?.getElementById?.('help-copy-diagnostics');
  const status = document?.getElementById?.('help-copy-status');

  function currentContext() {
    return resolve(context) ?? {};
  }

  function refresh() {
    const current = currentContext();
    setText(document, 'help-context-summary',
      `${current.backend ?? 'Unknown backend'} · ${current.vault ?? 'Unknown vault'}`);
    setText(document, 'help-capabilities', capabilityCopy(current));
    setText(document, 'help-config-path', current.config_path ?? current.configPath ?? 'Unavailable');
    setText(document, 'help-version', current.version ?? 'Unknown');
  }

  const onCopy = async () => {
    try {
      if (typeof clipboard?.writeText !== 'function') throw new Error('Clipboard unavailable');
      await clipboard.writeText(buildHelpDiagnostics(currentContext()));
      if (status) status.textContent = 'Diagnostics copied.';
    } catch (_) {
      if (status) status.textContent = 'Diagnostics could not be copied.';
    }
  };

  copy?.addEventListener?.('click', onCopy);
  refresh();
  return Object.freeze({
    refresh,
    destroy: () => copy?.removeEventListener?.('click', onCopy),
  });
}
