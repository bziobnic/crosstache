export const HEX_PATTERN = /^#[0-9a-fA-F]{6}$/;
export const CONTRAST_MIN = 4.5;
export const CUSTOM_COLOR_KEYS = Object.freeze(['canvas', 'surface', 'text', 'accent', 'danger']);
export const PALETTE_NAMES = Object.freeze(['forest', 'nord', 'solarized', 'high-contrast']);

const REQUIRED_CONTRAST_PAIRS = Object.freeze([
  ['text', 'canvas'],
  ['text', 'surface'],
  ['accent', 'surface'],
  ['danger', 'surface'],
]);

export function isValidHex(value) {
  return typeof value === 'string' && HEX_PATTERN.test(value);
}

export function hexToRgb(hex) {
  const value = hex.slice(1);
  return {
    r: parseInt(value.slice(0, 2), 16),
    g: parseInt(value.slice(2, 4), 16),
    b: parseInt(value.slice(4, 6), 16),
  };
}

function componentToHex(component) {
  return Math.max(0, Math.min(255, Math.round(component))).toString(16).padStart(2, '0');
}

export function rgbToHex({ r, g, b }) {
  return `#${componentToHex(r)}${componentToHex(g)}${componentToHex(b)}`;
}

export function mix(hexA, hexB, amount) {
  const a = hexToRgb(hexA);
  const b = hexToRgb(hexB);
  const t = Math.max(0, Math.min(1, amount));
  return rgbToHex({
    r: a.r * (1 - t) + b.r * t,
    g: a.g * (1 - t) + b.g * t,
    b: a.b * (1 - t) + b.b * t,
  });
}

function srgbToLinear(channel) {
  const value = channel / 255;
  return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

export function relativeLuminance(hex) {
  const { r, g, b } = hexToRgb(hex);
  return 0.2126 * srgbToLinear(r) + 0.7152 * srgbToLinear(g) + 0.0722 * srgbToLinear(b);
}

export function contrastRatio(hexA, hexB) {
  const lumA = relativeLuminance(hexA);
  const lumB = relativeLuminance(hexB);
  const lighter = Math.max(lumA, lumB);
  const darker = Math.min(lumA, lumB);
  return (lighter + 0.05) / (darker + 0.05);
}

export function meetsContrast(hexA, hexB, minRatio = CONTRAST_MIN) {
  return contrastRatio(hexA, hexB) >= minRatio;
}

function contrastingForeground(bgHex) {
  const white = '#ffffff';
  const black = '#000000';
  return contrastRatio(bgHex, white) >= contrastRatio(bgHex, black) ? white : black;
}

const CONTRAST_SEARCH_STEPS = 256;

// Finds the most muted (largest mix-toward-`driftTowards`) tone that still
// meets minRatio against every background. `base` at weight 0 must already
// pass every background, which callers guarantee via validated core tokens.
function findMutedTone(base, driftTowards, backgrounds, minRatio = CONTRAST_MIN) {
  for (let step = CONTRAST_SEARCH_STEPS; step >= 0; step -= 1) {
    const candidate = mix(base, driftTowards, step / CONTRAST_SEARCH_STEPS);
    if (backgrounds.every((bg) => contrastRatio(candidate, bg) >= minRatio)) return candidate;
  }
  return base;
}

// Finds the least aggressive mix-toward-`driftTowards` tone that meets
// minRatio against every background. Falls back to `driftTowards` itself.
function findAccessibleTone(base, driftTowards, backgrounds, minRatio = CONTRAST_MIN) {
  for (let step = 0; step <= CONTRAST_SEARCH_STEPS; step += 1) {
    const candidate = mix(base, driftTowards, step / CONTRAST_SEARCH_STEPS);
    if (backgrounds.every((bg) => contrastRatio(candidate, bg) >= minRatio)) return candidate;
  }
  return driftTowards;
}

// Starts from a background already known to support the foreground(s), then
// moves toward the desired tint only while every required contrast holds.
function findAccessibleBackground(base, desired, foregrounds, minRatio = CONTRAST_MIN) {
  for (let step = CONTRAST_SEARCH_STEPS; step >= 0; step -= 1) {
    const candidate = mix(base, desired, step / CONTRAST_SEARCH_STEPS);
    if (foregrounds.every((fg) => contrastRatio(fg, candidate) >= minRatio)) return candidate;
  }
  return base;
}

export const PALETTES = Object.freeze({
  forest: Object.freeze({
    light: Object.freeze({
      canvas: '#f3f1eb',
      surface: '#ffffff',
      text: '#18221c',
      accent: '#216446',
      danger: '#9f332e',
    }),
    dark: Object.freeze({
      canvas: '#121814',
      surface: '#19211c',
      text: '#dce5df',
      accent: '#65c68e',
      danger: '#f18e85',
    }),
  }),
  nord: Object.freeze({
    light: Object.freeze({
      canvas: '#eceff4',
      surface: '#fbfcfe',
      text: '#2e3440',
      accent: '#34506e',
      danger: '#96323c',
    }),
    dark: Object.freeze({
      canvas: '#2e3440',
      surface: '#3b4252',
      text: '#eceff4',
      accent: '#88c0d0',
      danger: '#e49da4',
    }),
  }),
  solarized: Object.freeze({
    light: Object.freeze({
      canvas: '#fdf6e3',
      surface: '#fefbef',
      text: '#073642',
      accent: '#1b6ea8',
      danger: '#b8221f',
    }),
    dark: Object.freeze({
      canvas: '#002b36',
      surface: '#073642',
      text: '#eee8d5',
      accent: '#5bc3ba',
      danger: '#ec847d',
    }),
  }),
  'high-contrast': Object.freeze({
    light: Object.freeze({
      canvas: '#ffffff',
      surface: '#ffffff',
      text: '#000000',
      accent: '#0033cc',
      danger: '#b30000',
    }),
    dark: Object.freeze({
      canvas: '#000000',
      surface: '#000000',
      text: '#ffffff',
      accent: '#4d94ff',
      danger: '#ff6666',
    }),
  }),
});

export function isValidCustomVariant(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const keys = Object.keys(value);
  if (keys.length !== CUSTOM_COLOR_KEYS.length) return false;
  return CUSTOM_COLOR_KEYS.every((key) => isValidHex(value[key]));
}

export function validateCustomVariantContrast(core) {
  const failures = [];
  for (const [foreground, background] of REQUIRED_CONTRAST_PAIRS) {
    const ratio = contrastRatio(core[foreground], core[background]);
    if (ratio < CONTRAST_MIN) {
      failures.push({ pair: `${foreground}-${background}`, ratio, required: CONTRAST_MIN });
    }
  }
  return { valid: failures.length === 0, failures };
}

export function deriveTokens(core, variant) {
  const surfaceSubtle = findAccessibleBackground(
    core.surface,
    mix(core.surface, core.canvas, 0.5),
    [core.text],
  );
  // Matches the translucent .app-header/.tab-list composite in style.css
  // (surface 92% over canvas, then text 4% over that) so muted text stays
  // readable on that rendered background too, not just the flat tokens.
  const tabListBg = mix(mix(core.canvas, core.surface, 0.92), core.text, 0.04);
  const textMuted = findMutedTone(
    core.text,
    core.surface,
    [core.canvas, core.surface, surfaceSubtle, tabListBg],
  );
  const border = findAccessibleTone(
    core.surface,
    core.text,
    [core.canvas, core.surface, surfaceSubtle],
    3,
  );
  const primaryForeground = contrastingForeground(core.accent);
  const desiredAccentHover = variant === 'dark'
    ? mix(core.accent, '#ffffff', 0.18)
    : mix(core.accent, '#000000', 0.18);
  const accentHover = findAccessibleBackground(
    core.accent,
    desiredAccentHover,
    [primaryForeground],
  );
  const accentQuiet = findAccessibleBackground(
    core.surface,
    mix(core.accent, core.surface, 0.92),
    [core.text, core.accent],
  );
  const accentTextBackgrounds = [core.canvas, core.surface, surfaceSubtle, accentQuiet];
  const accentText = findAccessibleTone(
    core.accent,
    core.text,
    accentTextBackgrounds,
  );
  const accentTextHover = findAccessibleBackground(
    accentText,
    mix(accentText, core.text, 0.18),
    accentTextBackgrounds,
  );
  const dangerQuiet = findAccessibleBackground(
    core.surface,
    mix(core.surface, core.danger, 0.06),
    [core.danger],
  );
  const railBg = mix(core.accent, '#000000', 0.55);
  const railFg = contrastingForeground(railBg);
  const railBorder = findAccessibleTone(railBg, railFg, [railBg], 3);
  const railFgMuted = findMutedTone(railFg, railBg, [railBg]);
  const railAccent = core.accent;
  const railAccentFg = contrastingForeground(railAccent);
  const railHoverBg = findAccessibleBackground(
    railBg,
    mix(railBg, '#ffffff', 0.1),
    [railFg],
  );
  const railConnectionOk = mix(core.accent, '#ffffff', 0.35);
  const railConnectionBad = findAccessibleTone(core.danger, '#ffffff', [railBg]);
  const railErrorBg = mix(core.danger, '#000000', 0.55);
  const railErrorFg = contrastingForeground(railErrorBg);
  const railErrorBorder = findAccessibleTone(railErrorBg, railErrorFg, [railErrorBg], 3);
  const railErrorAccent = mix(core.danger, '#ffffff', 0.35);
  const shadowTint = hexToRgb(mix(core.text, '#000000', 0.4));
  const focusColor = findAccessibleTone(core.accent, core.text, [core.canvas, core.surface], 3);

  return {
    surfaceSubtle,
    textMuted,
    border,
    accentHover,
    accentQuiet,
    accentText,
    accentTextHover,
    dangerQuiet,
    primaryForeground,
    railBg,
    railBorder,
    railFg,
    railFgMuted,
    railAccent,
    railAccentFg,
    railHoverBg,
    railConnectionOk,
    railConnectionBad,
    railErrorBg,
    railErrorBorder,
    railErrorFg,
    railErrorAccent,
    focusColor,
    focusRing: `0 0 0 3px ${focusColor}`,
    shadowRaised: `0 14px 34px rgba(${shadowTint.r}, ${shadowTint.g}, ${shadowTint.b}, ${variant === 'dark' ? 0.28 : 0.1})`,
  };
}

export function resolveTokens(paletteName, variantKind, customTheme) {
  const variant = variantKind === 'dark' ? 'dark' : 'light';
  let core;
  if (paletteName === 'custom' && isValidCustomVariant(customTheme?.[variant])) {
    core = customTheme[variant];
  } else {
    core = (PALETTES[paletteName] ?? PALETTES.forest)[variant];
  }
  return { ...core, ...deriveTokens(core, variant) };
}
