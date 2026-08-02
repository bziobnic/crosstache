import { boundTimeout } from './preferences.js';
import {
  CUSTOM_COLOR_KEYS,
  PALETTES,
  PALETTE_NAMES,
  isValidHex,
  isValidCustomVariant,
  resolveTokens,
  validateCustomVariantContrast,
} from './theme.js';

const THEMES = new Set(['system', 'light', 'dark']);
const DENSITIES = new Set(['comfortable', 'compact']);
const VARIANTS = new Set(['light', 'dark']);
const PALETTE_VALUES = new Set([...PALETTE_NAMES, 'custom']);
const FOREST_CUSTOM_THEME = Object.freeze({
  light: Object.freeze({ ...PALETTES.forest.light }),
  dark: Object.freeze({ ...PALETTES.forest.dark }),
});
const DEFAULT_COLUMN_WIDTHS = Object.freeze({
  secrets: Object.freeze([28, 15, 14, 25, 18]),
  files: Object.freeze([42, 12, 24, 22]),
});
const TOKEN_CSS_VARS = Object.freeze({
  canvas: '--color-canvas',
  surface: '--color-surface',
  text: '--color-text',
  accent: '--color-accent',
  danger: '--color-danger',
  surfaceSubtle: '--color-surface-subtle',
  textMuted: '--color-text-muted',
  border: '--color-border',
  accentHover: '--color-accent-hover',
  accentQuiet: '--color-accent-quiet',
  accentText: '--color-accent-text',
  accentTextHover: '--color-accent-text-hover',
  dangerQuiet: '--color-danger-quiet',
  primaryForeground: '--color-primary-foreground',
  focusColor: '--color-focus',
  focusRing: '--focus-ring',
  shadowRaised: '--shadow-raised',
  railBg: '--rail-bg',
  railBorder: '--rail-border',
  railFg: '--rail-fg',
  railFgMuted: '--rail-fg-muted',
  railAccent: '--rail-accent',
  railAccentFg: '--rail-accent-fg',
  railHoverBg: '--rail-hover-bg',
  railConnectionOk: '--rail-connection-ok',
  railConnectionBad: '--rail-connection-bad',
  railErrorBg: '--rail-error-bg',
  railErrorBorder: '--rail-error-border',
  railErrorFg: '--rail-error-fg',
  railErrorAccent: '--rail-error-accent',
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

export { boundTimeout };

function cloneCustomTheme(customTheme) {
  return {
    light: { ...(customTheme?.light ?? FOREST_CUSTOM_THEME.light) },
    dark: { ...(customTheme?.dark ?? FOREST_CUSTOM_THEME.dark) },
  };
}

function applyPresentation(document, { theme, density, palette, customTheme, mediaQuery } = {}) {
  const root = document?.documentElement;
  if (!root) return;
  const resolvedTheme = THEMES.has(theme) ? theme : 'system';
  const resolvedPalette = PALETTE_VALUES.has(palette) ? palette : 'forest';
  const effective = effectiveTheme(resolvedTheme, mediaQuery);
  root.dataset.theme = resolvedTheme;
  root.dataset.effectiveTheme = effective;
  root.dataset.density = DENSITIES.has(density) ? density : 'comfortable';
  root.dataset.palette = resolvedPalette;
  root.style?.setProperty?.('color-scheme', effective);
  const tokens = resolveTokens(resolvedPalette, effective, customTheme);
  for (const [key, cssVar] of Object.entries(TOKEN_CSS_VARS)) {
    root.style?.setProperty?.(cssVar, tokens[key]);
  }
}

function setControlValue(control, value) {
  if (control) control.value = String(value);
}

function ensureTimeoutOption(document, control, value, { policyDerived = false } = {}) {
  if (!control || !document?.createElement) return;
  const stringValue = String(value);
  const exists = [...(control.querySelectorAll?.('option') ?? [])]
    .some((option) => option.value === stringValue);
  if (exists) return;
  const option = document.createElement('option');
  option.value = stringValue;
  option.textContent = `${stringValue} seconds (${policyDerived ? 'policy limit' : 'current'})`;
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
  const policyCopy = document?.getElementById?.('timeout-policy-copy');
  const palette = document?.getElementById?.('palette-select');
  const customFieldset = document?.getElementById?.('custom-theme-fieldset');
  const customVariantSelect = document?.getElementById?.('custom-variant-select');
  const customReset = document?.getElementById?.('custom-theme-reset');
  const customStatus = document?.getElementById?.('custom-theme-status');
  const customColorInputs = Object.fromEntries(
    CUSTOM_COLOR_KEYS.map((key) => [key, document?.getElementById?.(`custom-color-${key}`)]),
  );

  function announceCustom(message) {
    if (customStatus) customStatus.textContent = message;
  }

  function policyLimit() {
    const policy = resolve(securityPolicy);
    return nonNegativeInteger(
      typeof policy === 'object' ? policy?.clipboard_timeout_seconds : policy,
    );
  }

  function currentCustomTheme() {
    return cloneCustomTheme(preferences.get('custom_theme', FOREST_CUSTOM_THEME));
  }

  function currentVariant() {
    return VARIANTS.has(customVariantSelect?.value) ? customVariantSelect.value : 'light';
  }

  const customDraft = currentCustomTheme();
  const dirtyDraftVariants = new Set();

  function renderCustomInputs(variant) {
    const core = customDraft[variant] ?? FOREST_CUSTOM_THEME[variant];
    const shapeValid = isValidCustomVariant(core);
    const contrastResult = shapeValid
      ? validateCustomVariantContrast(core)
      : { valid: false, failures: [] };
    const invalidContrastKeys = new Set(contrastResult.failures
      .flatMap(({ pair }) => pair.split('-')));
    for (const key of CUSTOM_COLOR_KEYS) {
      const input = customColorInputs[key];
      if (!input) continue;
      input.value = core[key];
      input.ariaInvalid = isValidHex(core[key]) && !invalidContrastKeys.has(key) ? 'false' : 'true';
    }
  }

  function applyCurrentPresentation() {
    applyPresentation(document, {
      theme: preferences.get('theme', 'system'),
      density: preferences.get('density', 'comfortable'),
      palette: preferences.get('palette', 'forest'),
      customTheme: currentCustomTheme(),
      mediaQuery,
    });
  }

  function refresh() {
    const selectedTheme = preferences.get('theme', 'system');
    const selectedDensity = preferences.get('density', 'comfortable');
    const selectedPalette = PALETTE_VALUES.has(preferences.get('palette', 'forest'))
      ? preferences.get('palette', 'forest')
      : 'forest';
    const customTheme = currentCustomTheme();
    for (const variant of VARIANTS) {
      if (!dirtyDraftVariants.has(variant)) customDraft[variant] = { ...customTheme[variant] };
    }
    const requestedTimeout = nonNegativeInteger(
      preferences.get('exposure_timeout_seconds', 30),
    );
    const limit = policyLimit();
    const selectedTimeout = boundTimeout(requestedTimeout, limit);
    ensureTimeoutOption(document, timeout, selectedTimeout, {
      policyDerived: limit > 0 && requestedTimeout > limit,
    });
    setControlValue(theme, selectedTheme);
    setControlValue(density, selectedDensity);
    setControlValue(timeout, selectedTimeout);
    setControlValue(palette, selectedPalette);
    if (customFieldset) customFieldset.hidden = selectedPalette !== 'custom';
    setControlValue(customVariantSelect, currentVariant());
    renderCustomInputs(currentVariant());
    applyPresentation(document, {
      theme: selectedTheme,
      density: selectedDensity,
      palette: selectedPalette,
      customTheme,
      mediaQuery,
    });

    for (const option of timeout?.querySelectorAll?.('option') ?? []) {
      const value = nonNegativeInteger(option.value);
      option.disabled = limit > 0 && value > limit;
    }
    if (policyCopy) policyCopy.textContent = limit > 0
      ? `This app limits the timeout to ${limit} seconds. A saved 0-second timeout hides protected values immediately.`
      : 'No application maximum is configured. A saved 0-second timeout hides protected values immediately.';
  }

  const onTheme = () => {
    const value = THEMES.has(theme?.value) ? theme.value : 'system';
    preferences.set('theme', value);
    applyCurrentPresentation();
  };
  const onDensity = () => {
    const value = DENSITIES.has(density?.value) ? density.value : 'comfortable';
    preferences.set('density', value);
    applyCurrentPresentation();
  };
  const onTimeout = () => {
    const value = boundTimeout(timeout?.value, policyLimit());
    preferences.set('exposure_timeout_seconds', value);
    setControlValue(timeout, value);
    if (status) status.textContent = value > 0
      ? `Protected values hide after ${value} seconds.`
      : 'Protected values hide immediately.';
  };
  const onPalette = () => {
    const value = PALETTE_VALUES.has(palette?.value) ? palette.value : 'forest';
    preferences.set('palette', value);
    if (customFieldset) customFieldset.hidden = value !== 'custom';
    applyCurrentPresentation();
  };
  const onCustomVariant = () => {
    renderCustomInputs(currentVariant());
  };
  function onCustomColorChange(key) {
    return () => {
      const input = customColorInputs[key];
      const variant = currentVariant();
      customDraft[variant][key] = input?.value;
      dirtyDraftVariants.add(variant);
      const candidateVariant = { ...customDraft[variant] };
      if (candidateVariant[key] === '' || /^#[0-9a-fA-F]{0,5}$/.test(candidateVariant[key])) {
        if (input) input.ariaInvalid = 'true';
        announceCustom('');
        return;
      }
      const shapeValid = isValidCustomVariant(candidateVariant);
      const contrastResult = shapeValid
        ? validateCustomVariantContrast(candidateVariant)
        : { valid: false, failures: [] };
      const valid = shapeValid && contrastResult.valid;
      renderCustomInputs(variant);
      if (!valid) {
        announceCustom(shapeValid
          ? `Increase contrast to update the preview: ${contrastResult.failures
            .map((failure) => `${failure.pair.replace('-', ' vs ')} is ${failure.ratio.toFixed(2)}:1, needs 4.5:1`)
            .join('; ')}.`
          : 'Enter a six-digit hex color, for example #216446.');
        return;
      }
      const updated = { ...currentCustomTheme(), [variant]: candidateVariant };
      preferences.set('custom_theme', updated);
      if (preferences.get('palette', 'forest') === 'custom') {
        applyPresentation(document, {
          theme: preferences.get('theme', 'system'),
          density: preferences.get('density', 'comfortable'),
          palette: 'custom',
          customTheme: updated,
          mediaQuery,
        });
      }
      announceCustom('Custom color preview updated; persistence is pending.');
    };
  }
  const customColorHandlers = Object.fromEntries(
    CUSTOM_COLOR_KEYS.map((key) => [key, onCustomColorChange(key)]),
  );
  const onCustomReset = () => {
    const resetTheme = cloneCustomTheme(FOREST_CUSTOM_THEME);
    customDraft.light = { ...resetTheme.light };
    customDraft.dark = { ...resetTheme.dark };
    dirtyDraftVariants.clear();
    preferences.set('custom_theme', resetTheme);
    renderCustomInputs(currentVariant());
    if (preferences.get('palette', 'forest') === 'custom') {
      applyPresentation(document, {
        theme: preferences.get('theme', 'system'),
        density: preferences.get('density', 'comfortable'),
        palette: 'custom',
        customTheme: resetTheme,
        mediaQuery,
      });
    }
    announceCustom('Custom color preview reset to Forest defaults; persistence is pending.');
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
  palette?.addEventListener?.('change', onPalette);
  customVariantSelect?.addEventListener?.('change', onCustomVariant);
  for (const key of CUSTOM_COLOR_KEYS) {
    customColorInputs[key]?.addEventListener?.('input', customColorHandlers[key]);
  }
  customReset?.addEventListener?.('click', onCustomReset);
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
      palette?.removeEventListener?.('change', onPalette);
      customVariantSelect?.removeEventListener?.('change', onCustomVariant);
      for (const key of CUSTOM_COLOR_KEYS) {
        customColorInputs[key]?.removeEventListener?.('input', customColorHandlers[key]);
      }
      customReset?.removeEventListener?.('click', onCustomReset);
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
  const policyValue = safe.security?.clipboard_timeout_seconds;
  const hasPolicy = Number.isSafeInteger(policyValue) && policyValue >= 0;
  const policy = hasPolicy ? policyValue : null;
  const requestedTimeout = safe.preferences?.exposure_timeout_seconds;
  const hasRequestedTimeout = Number.isSafeInteger(requestedTimeout) && requestedTimeout >= 0;
  const effectiveTimeout = hasRequestedTimeout
    ? boundTimeout(requestedTimeout, policyValue)
    : null;
  const lines = [
    `Crosstache ${String(safe.version ?? 'unknown')}`,
    cleanLine('Config', safe.config_path ?? safe.configPath),
    cleanLine('Backend', safe.backend),
    cleanLine('Vault', safe.vault),
    cleanLine('Workspace', safe.workspace?.alias),
    cleanLine('Project', safe.project?.name),
    cleanLine('Environment', safe.environment?.name),
    cleanLine('Connection', safe.connection?.state),
    hasPolicy
      ? cleanLine('Security policy limit (seconds)', policy > 0 ? policy : 'none')
      : null,
    hasRequestedTimeout
      ? cleanLine('Effective protected-value timeout (seconds)', effectiveTimeout)
      : null,
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
  preferences,
  document = globalThis.document,
  clipboard = globalThis.navigator?.clipboard,
}) {
  const copy = document?.getElementById?.('help-copy-diagnostics');
  const status = document?.getElementById?.('help-copy-status');

  function currentContext() {
    return resolve(context) ?? {};
  }

  function diagnosticContext() {
    const current = currentContext();
    const preferenceSnapshot = preferences?.snapshot?.();
    return preferenceSnapshot
      ? { ...current, preferences: preferenceSnapshot }
      : current;
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
      try { await preferences?.load?.(); } catch (_) { /* preference client owns its safe error */ }
      await clipboard.writeText(buildHelpDiagnostics(diagnosticContext()));
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
