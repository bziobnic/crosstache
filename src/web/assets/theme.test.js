import assert from 'node:assert/strict';
import test from 'node:test';

import {
  CONTRAST_MIN,
  CUSTOM_COLOR_KEYS,
  HEX_PATTERN,
  PALETTE_NAMES,
  PALETTES,
  contrastRatio,
  deriveTokens,
  isValidCustomVariant,
  isValidHex,
  meetsContrast,
  mix,
  resolveTokens,
  validateCustomVariantContrast,
} from './theme.js';

test('HEX_PATTERN and isValidHex accept only strict six-digit hex', () => {
  for (const good of ['#000000', '#ffffff', '#AbC123', '#216446']) {
    assert.ok(HEX_PATTERN.test(good), good);
    assert.equal(isValidHex(good), true, good);
  }
  for (const bad of ['#fff', '000000', '#12345', '#1234567', 'red', 'rgb(0,0,0)', '#gggggg', '', null, undefined, 42]) {
    assert.equal(isValidHex(bad), false, String(bad));
  }
});

test('contrastRatio matches known WCAG reference values', () => {
  assert.equal(contrastRatio('#000000', '#ffffff'), 21);
  assert.equal(contrastRatio('#ffffff', '#ffffff'), 1);
  assert.equal(contrastRatio('#123456', '#654321'), contrastRatio('#654321', '#123456'));
});

test('meetsContrast enforces the WCAG AA 4.5 default threshold', () => {
  assert.equal(CONTRAST_MIN, 4.5);
  assert.equal(meetsContrast('#000000', '#ffffff'), true);
  assert.equal(meetsContrast('#777777', '#888888'), false);
  assert.equal(meetsContrast('#000000', '#ffffff', 21), true);
  assert.equal(meetsContrast('#000000', '#ffffff', 21.1), false);
});

test('contrast validation does not round a borderline failure up to 4.5', () => {
  const borderlineForeground = '#3eca1d';
  const borderlineBackground = '#0e457a';
  assert.ok(contrastRatio(borderlineForeground, borderlineBackground) < CONTRAST_MIN);

  const result = validateCustomVariantContrast({
    canvas: '#000000',
    surface: borderlineBackground,
    text: '#ffffff',
    accent: borderlineForeground,
    danger: '#ffffff',
  });
  assert.equal(result.valid, false);
  assert.ok(result.failures.some(({ pair }) => pair === 'accent-surface'));
});

test('every built-in palette resolves light and dark variants with valid strict hex core tokens', () => {
  assert.deepEqual([...PALETTE_NAMES].sort(), ['forest', 'high-contrast', 'nord', 'solarized'].sort());
  for (const name of PALETTE_NAMES) {
    const palette = PALETTES[name];
    for (const variant of ['light', 'dark']) {
      const core = palette[variant];
      assert.ok(core, `${name}/${variant} missing`);
      for (const key of CUSTOM_COLOR_KEYS) {
        assert.ok(isValidHex(core[key]), `${name}/${variant}/${key} = ${core[key]}`);
      }
    }
  }
});

test('Forest palette matches the exact current production colors', () => {
  assert.deepEqual(PALETTES.forest.light, {
    canvas: '#f3f1eb',
    surface: '#ffffff',
    text: '#18221c',
    accent: '#216446',
    danger: '#9f332e',
  });
  assert.deepEqual(PALETTES.forest.dark, {
    canvas: '#121814',
    surface: '#19211c',
    text: '#dce5df',
    accent: '#65c68e',
    danger: '#f18e85',
  });
});

test('built-in palettes satisfy the required contrast pairs in both variants', () => {
  for (const name of PALETTE_NAMES) {
    for (const variant of ['light', 'dark']) {
      const core = PALETTES[name][variant];
      const result = validateCustomVariantContrast(core);
      assert.equal(result.valid, true, `${name}/${variant}: ${JSON.stringify(result.failures)}`);
    }
  }
});

test('isValidCustomVariant enforces exact shape and strict hex, rejecting extra or missing keys', () => {
  const valid = { canvas: '#ffffff', surface: '#fafafa', text: '#000000', accent: '#123456', danger: '#654321' };
  assert.equal(isValidCustomVariant(valid), true);
  assert.equal(isValidCustomVariant({ ...valid, extra: '#000000' }), false);
  const { canvas, ...missingCanvas } = valid;
  assert.equal(isValidCustomVariant(missingCanvas), false);
  assert.equal(isValidCustomVariant({ ...valid, accent: 'not-a-hex' }), false);
  assert.equal(isValidCustomVariant({ ...valid, accent: 'rgb(0,0,0)' }), false);
  assert.equal(isValidCustomVariant(null), false);
  assert.equal(isValidCustomVariant('#ffffff'), false);
  assert.equal(isValidCustomVariant([]), false);
});

test('validateCustomVariantContrast reports every failing required pair', () => {
  const lowContrast = { canvas: '#808080', surface: '#888888', text: '#7a7a7a', accent: '#8a8a8a', danger: '#8f8f8f' };
  const result = validateCustomVariantContrast(lowContrast);
  assert.equal(result.valid, false);
  assert.ok(result.failures.length > 0);
  for (const failure of result.failures) {
    assert.ok(['text-canvas', 'text-surface', 'accent-surface', 'danger-surface'].includes(failure.pair));
    assert.ok(failure.ratio < CONTRAST_MIN);
  }

  const highContrast = { canvas: '#ffffff', surface: '#ffffff', text: '#000000', accent: '#003399', danger: '#a30000' };
  const good = validateCustomVariantContrast(highContrast);
  assert.equal(good.valid, true);
  assert.deepEqual(good.failures, []);
});

test('deriveTokens produces a complete, valid-format secondary and rail token set', () => {
  for (const name of PALETTE_NAMES) {
    for (const variant of ['light', 'dark']) {
      const tokens = deriveTokens(PALETTES[name][variant], variant);
      for (const key of [
        'surfaceSubtle', 'textMuted', 'border', 'accentHover', 'accentQuiet',
        'accentText', 'accentTextHover',
        'dangerQuiet', 'primaryForeground', 'railBg', 'railBorder', 'railFg',
        'railFgMuted', 'railAccent', 'railAccentFg', 'railHoverBg',
        'railConnectionOk', 'railConnectionBad', 'railErrorBg', 'railErrorBorder',
        'railErrorFg', 'railErrorAccent', 'focusColor',
      ]) {
        assert.ok(isValidHex(tokens[key]), `${name}/${variant}/${key} = ${tokens[key]}`);
      }
      assert.equal(tokens.focusRing, `0 0 0 3px ${tokens.focusColor}`);
      assert.match(tokens.shadowRaised, /^0 14px 34px rgba\(\d+, \d+, \d+, [0-9.]+\)$/);
    }
  }
});

test('deriveTokens picks a readable primary foreground against the accent for bright and dark accents', () => {
  const brightAccent = deriveTokens({ canvas: '#ffffff', surface: '#ffffff', text: '#000000', accent: '#f5e642', danger: '#a30000' }, 'light');
  assert.ok(meetsContrast(brightAccent.primaryForeground, '#f5e642', 3), 'bright accent should pick a dark foreground');

  const darkAccent = deriveTokens({ canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#0a1a33', danger: '#a30000' }, 'dark');
  assert.ok(meetsContrast(darkAccent.primaryForeground, '#0a1a33', 3), 'dark accent should pick a light foreground');
});

test('medium accepted accents choose true black when the near-black brand tone is not readable', () => {
  const core = { canvas: '#000000', surface: '#000000', text: '#ffffff', accent: '#777777', danger: '#ff6666' };
  assert.equal(validateCustomVariantContrast(core).valid, true);
  const tokens = deriveTokens(core, 'dark');
  assert.equal(tokens.primaryForeground, '#000000');
  assert.equal(tokens.railAccentFg, '#000000');
  assert.ok(meetsContrast(tokens.primaryForeground, core.accent));
  assert.ok(meetsContrast(tokens.railAccentFg, tokens.railAccent));
});

test('accepted accents derive readable text colors for canvas and component surfaces', () => {
  const core = {
    canvas: '#767676', surface: '#ffffff', text: '#000000', accent: '#767676', danger: '#000000',
  };
  assert.equal(validateCustomVariantContrast(core).valid, true);
  const tokens = deriveTokens(core, 'light');
  assert.ok(meetsContrast(tokens.accentText, core.canvas));
  assert.ok(meetsContrast(tokens.accentText, core.surface));
  assert.notEqual(tokens.accentText, core.accent);
});

test('deriveTokens is deterministic: same input always produces the same output', () => {
  const a = deriveTokens(PALETTES.nord.dark, 'dark');
  const b = deriveTokens(PALETTES.nord.dark, 'dark');
  assert.deepEqual(a, b);
});

test('resolveTokens merges core and derived tokens for a built-in palette', () => {
  const tokens = resolveTokens('nord', 'light', null);
  assert.equal(tokens.canvas, PALETTES.nord.light.canvas);
  assert.equal(tokens.accent, PALETTES.nord.light.accent);
  assert.ok(isValidHex(tokens.railBg));
});

test('resolveTokens uses the supplied custom theme when palette is custom', () => {
  const customTheme = {
    light: { canvas: '#ffffff', surface: '#fafafa', text: '#000000', accent: '#003399', danger: '#a30000' },
    dark: { canvas: '#000000', surface: '#0a0a0a', text: '#ffffff', accent: '#3399ff', danger: '#ff5555' },
  };
  const light = resolveTokens('custom', 'light', customTheme);
  assert.equal(light.canvas, '#ffffff');
  assert.equal(light.accent, '#003399');
  const dark = resolveTokens('custom', 'dark', customTheme);
  assert.equal(dark.canvas, '#000000');
  assert.equal(dark.accent, '#3399ff');
});

test('textMuted stays readable on the translucent .app-header/.tab-list composite background', () => {
  for (const name of PALETTE_NAMES) {
    for (const variant of ['light', 'dark']) {
      const core = PALETTES[name][variant];
      const tokens = deriveTokens(core, variant);
      const tabListBg = mix(mix(core.canvas, core.surface, 0.92), core.text, 0.04);
      assert.ok(
        meetsContrast(tokens.textMuted, tabListBg),
        `${name}/${variant}: ${tokens.textMuted} vs ${tabListBg} = ${contrastRatio(tokens.textMuted, tabListBg)}`,
      );
    }
  }
});

test('resolveTokens falls back to Forest defaults for an unknown palette name', () => {
  const tokens = resolveTokens('not-a-real-palette', 'light', null);
  assert.equal(tokens.canvas, PALETTES.forest.light.canvas);
});

test('every derived foreground token meets 4.5:1 against every background it is rendered on', () => {
  for (const name of PALETTE_NAMES) {
    for (const variant of ['light', 'dark']) {
      const core = PALETTES[name][variant];
      const tokens = deriveTokens(core, variant);
      const pairs = [
        ['textMuted', tokens.textMuted, core.canvas],
        ['textMuted', tokens.textMuted, core.surface],
        ['primaryForeground', tokens.primaryForeground, core.accent],
        ['railFg', tokens.railFg, tokens.railBg],
        ['railFgMuted', tokens.railFgMuted, tokens.railBg],
        ['railAccentFg', tokens.railAccentFg, tokens.railAccent],
        ['railErrorFg', tokens.railErrorFg, tokens.railErrorBg],
        ['railConnectionBad (button text)', tokens.railConnectionBad, tokens.railBg],
      ];
      for (const [label, fg, bg] of pairs) {
        assert.ok(
          meetsContrast(fg, bg),
          `${name}/${variant} ${label}: ${fg} vs ${bg} = ${contrastRatio(fg, bg)}`,
        );
      }
    }
  }
});

test('derived text, danger, borders, and focus stay accessible for every valid core palette', () => {
  const adversarial = [
    { canvas: '#fbc98d', surface: '#14fc45', text: '#255b2d', accent: '#7e3072', danger: '#8f0ab3' },
    { canvas: '#4101ed', surface: '#005126', text: '#c2e5a3', accent: '#96f595', danger: '#2ad615' },
    { canvas: '#969a52', surface: '#77e7f4', text: '#0c3103', accent: '#990691', danger: '#6a563e' },
    { canvas: '#532b47', surface: '#2b1fbb', text: '#d2b78b', accent: '#47d49f', danger: '#b1fc1c' },
  ];
  const cores = [
    ...PALETTE_NAMES.flatMap((name) => ['light', 'dark'].map((variant) => ({
      label: `${name}/${variant}`,
      variant,
      core: PALETTES[name][variant],
    }))),
    ...adversarial.map((core, index) => ({ label: `adversarial-${index}`, variant: 'light', core })),
  ];

  for (const { label, variant, core } of cores) {
    assert.equal(validateCustomVariantContrast(core).valid, true, `${label} must be accepted by core validation`);
    const tokens = deriveTokens(core, variant);
    const checks = [
      ['muted text on canvas', tokens.textMuted, core.canvas, 4.5],
      ['muted text on surface', tokens.textMuted, core.surface, 4.5],
      ['muted text on subtle surface', tokens.textMuted, tokens.surfaceSubtle, 4.5],
      ['danger text on quiet danger surface', core.danger, tokens.dangerQuiet, 4.5],
      ['border on surface', tokens.border, core.surface, 3],
      ['border on canvas', tokens.border, core.canvas, 3],
      ['border on subtle surface', tokens.border, tokens.surfaceSubtle, 3],
      ['primary text on hover background', tokens.primaryForeground, tokens.accentHover, 4.5],
      ['text on quiet accent surface', core.text, tokens.accentQuiet, 4.5],
      ['accent on quiet accent surface', core.accent, tokens.accentQuiet, 4.5],
      ['accent text on canvas', tokens.accentText, core.canvas, 4.5],
      ['accent text on surface', tokens.accentText, core.surface, 4.5],
      ['accent text on subtle surface', tokens.accentText, tokens.surfaceSubtle, 4.5],
      ['accent text on quiet accent surface', tokens.accentText, tokens.accentQuiet, 4.5],
      ['accent hover text on canvas', tokens.accentTextHover, core.canvas, 4.5],
      ['accent hover text on surface', tokens.accentTextHover, core.surface, 4.5],
      ['accent hover text on subtle surface', tokens.accentTextHover, tokens.surfaceSubtle, 4.5],
      ['rail border on rail', tokens.railBorder, tokens.railBg, 3],
      ['rail text on hover background', tokens.railFg, tokens.railHoverBg, 4.5],
      ['error border on error surface', tokens.railErrorBorder, tokens.railErrorBg, 3],
      ['focus on canvas', tokens.focusColor, core.canvas, 3],
      ['focus on surface', tokens.focusColor, core.surface, 3],
    ];
    for (const [description, foreground, background, minimum] of checks) {
      assert.ok(
        foreground && meetsContrast(foreground, background, minimum),
        `${label} ${description}: ${foreground ?? 'missing'} vs ${background}`,
      );
    }
  }
});
