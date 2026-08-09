import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import { PALETTES, contrastRatio, deriveTokens, mix } from './theme.js';

const html = await readFile(new URL('./index.html', import.meta.url), 'utf8');
const css = await readFile(new URL('./style.css', import.meta.url), 'utf8');

test('Settings renames the Theme control to Mode and keeps #theme-select', () => {
  const settingsSection = html.split('id="settings-dialog"')[1];
  assert.match(settingsSection, /for="theme-select"><span class="field-label">Mode<\/span>/);
  assert.doesNotMatch(settingsSection, />Theme<\/span>\s*<select id="theme-select"/);
});

test('Settings exposes a Color palette select with the five expected values', () => {
  const match = html.match(/<select id="palette-select">([\s\S]*?)<\/select>/);
  assert.ok(match, 'expected #palette-select to exist');
  const values = [...match[1].matchAll(/<option value="([^"]+)">/g)].map((m) => m[1]);
  assert.deepEqual(values, ['forest', 'nord', 'solarized', 'high-contrast', 'custom']);
});

test('Settings exposes an accessible custom-theme fieldset with variant, five color controls, status, and reset', () => {
  const match = html.match(/<fieldset id="custom-theme-fieldset"[^>]*>([\s\S]*?)<\/fieldset>/);
  assert.ok(match, 'expected #custom-theme-fieldset to exist');
  const fieldset = match[1];
  assert.match(fieldset, /<legend/);
  assert.match(fieldset, /id="custom-variant-select"/);
  assert.match(fieldset, /<option value="light">/);
  assert.match(fieldset, /<option value="dark">/);
  for (const key of ['canvas', 'surface', 'text', 'accent', 'danger']) {
    assert.match(fieldset, new RegExp(`for="custom-color-${key}"`), `missing label for custom-color-${key}`);
    assert.match(fieldset, new RegExp(`id="custom-color-${key}"`), `missing input custom-color-${key}`);
  }
  assert.match(fieldset, /id="custom-theme-reset"/);
});

test('style.css defines semantic rail custom properties consumed by the context rail', () => {
  for (const token of [
    '--rail-bg', '--rail-border', '--rail-fg', '--rail-fg-muted', '--rail-accent',
    '--rail-accent-fg', '--rail-hover-bg', '--rail-connection-ok', '--rail-connection-bad',
    '--rail-error-bg', '--rail-error-border', '--rail-error-fg', '--rail-error-accent',
  ]) {
    assert.match(css, new RegExp(`var\\(${token.replace(/[-[\]{}()*+?.,\\^$|#\s]/g, '\\$&')}\\)`), `${token} not consumed via var()`);
  }
});

test('the context-rail and related rules no longer hardcode the old Forest-only literal colors', () => {
  const railBlockStart = css.indexOf('.context-rail {');
  assert.ok(railBlockStart >= 0, 'expected a .context-rail rule');
  const railSection = css.slice(railBlockStart, css.indexOf('.app-header {'));
  for (const literal of ['#123426', '#365747', '#f4faf6', '#8bd2a8', '#557668', '#1b4534', '#88d4a5', '#f4a39a', '#5a2925', '#a95c54', '#46665a', '#acd1bc', '#cee3d6', '#bcd2c5', '#f0b8a8', '#ffe4dc']) {
    assert.ok(!railSection.includes(literal), `rail section still hardcodes ${literal}`);
  }
});

test('style.css collapses the old root/media/data-theme triplication into a single JS-driven token path', () => {
  assert.doesNotMatch(css, /:root\[data-theme="light"\]/);
  assert.doesNotMatch(css, /:root\[data-theme="dark"\]/);
});

test('palette-independent component rules do not retain Forest-tinted shadows or accents', () => {
  const themedComponentCss = css.slice(css.indexOf('.context-rail {'), css.indexOf('@media (prefers-color-scheme: dark)'));
  for (const literal of [
    'rgb(10 32 23 / 10%)',
    'rgb(33 100 70 / 24%)',
    'rgb(28 44 34 / 8%)',
    'rgb(24 34 28 / 5%)',
    'rgb(33 100 70 / 20%)',
    'rgb(18 30 22 / 16%)',
    'rgb(18 30 22 / 24%)',
    'rgb(18 30 22 / 32%)',
    'rgb(33 76 50 / 6%)',
    'rgb(24 34 28 / 18%)',
  ]) {
    assert.ok(!themedComponentCss.includes(literal), `component rules still hardcode ${literal}`);
  }
  assert.doesNotMatch(themedComponentCss, /\.utility-sheet[^}]*border-left:3px solid #8bd2a8/);
  assert.match(themedComponentCss, /\.eyebrow\s*\{[^}]*color:var\(--color-accent-text\)/);
});

test('pre-JS Forest light and dark fallbacks match accessible runtime tokens', () => {
  const rootBlock = css.match(/^:root \{([\s\S]*?)\n\}/)?.[1] ?? '';
  const darkBlock = css.match(/@media \(prefers-color-scheme: dark\) \{\s*:root \{([\s\S]*?)\n\s*\}/)?.[1] ?? '';
  const readToken = (block, name) => block.match(new RegExp(`${name}\\s*:\\s*([^;]+);`))?.[1].trim();
  const cssNames = {
    canvas: '--color-canvas', surface: '--color-surface', surfaceSubtle: '--color-surface-subtle',
    text: '--color-text', textMuted: '--color-text-muted', border: '--color-border',
    accent: '--color-accent', accentText: '--color-accent-text',
    accentTextHover: '--color-accent-text-hover', danger: '--color-danger', dangerQuiet: '--color-danger-quiet',
    primaryForeground: '--color-primary-foreground', railBg: '--rail-bg', railBorder: '--rail-border',
    railFg: '--rail-fg', railFgMuted: '--rail-fg-muted', railAccentFg: '--rail-accent-fg',
    focusColor: '--color-focus',
  };

  for (const [variant, block] of [['light', rootBlock], ['dark', darkBlock]]) {
    const core = PALETTES.forest[variant];
    const expected = { ...core, ...deriveTokens(core, variant) };
    for (const [key, cssName] of Object.entries(cssNames)) {
      assert.equal(readToken(block, cssName), expected[key], `${variant} ${cssName}`);
    }
    assert.ok(contrastRatio(readToken(block, '--color-text'), readToken(block, '--color-canvas')) >= 4.5);
    assert.ok(contrastRatio(readToken(block, '--color-text-muted'), readToken(block, '--color-surface-subtle')) >= 4.5);
    // Muted text also lands on the tinted composites style.css paints: the
    // accent wash on selected rows, the text wash on .tag, and both stacked.
    const surface = readToken(block, '--color-surface');
    const selectedRow = mix(surface, readToken(block, '--color-accent'), 0.12);
    const tag = mix(surface, readToken(block, '--color-text'), 0.06);
    for (const bg of [selectedRow, tag, mix(selectedRow, readToken(block, '--color-text'), 0.06)]) {
      assert.ok(contrastRatio(readToken(block, '--color-text-muted'), bg) >= 4.5, `${variant} muted on ${bg}`);
    }
    assert.ok(contrastRatio(readToken(block, '--color-primary-foreground'), readToken(block, '--color-accent')) >= 4.5);
    assert.ok(contrastRatio(readToken(block, '--rail-fg'), readToken(block, '--rail-bg')) >= 4.5);
    assert.ok(contrastRatio(readToken(block, '--rail-fg-muted'), readToken(block, '--rail-bg')) >= 4.5);
  }
});
