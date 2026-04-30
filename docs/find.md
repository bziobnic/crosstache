# `xv find` — Ranked Fuzzy Search

`xv find <pattern>` ranks every secret in the active vault against the
pattern using nucleo (the same fuzzy matcher as Helix). Output is
non-interactive and pipe-friendly.

## Usage

```bash
xv find <pattern> [--in <field>]... [--limit N] [--min-score F]
                  [--all-vaults] [--names-only]
```

- **`<pattern>`** — fuzzy pattern. Omit to list every secret with score 0.
- **`--in <field>`** — search additional fields beyond the name. Repeatable. Allowed: `name`, `folder`, `groups`, `note`, `tags`. Default: `name`.
- **`--limit N`** — max rows (default 50).
- **`--min-score F`** — drop matches scoring below `F` × top match (0.0..=1.0; default 0.3).
- **`--all-vaults`** — search every vault you can list. Slow on cold cache.
- **`--names-only`** — one name per line, no headers, no ANSI. Pipe-friendly. Overrides `--format`.

## Output

Default: a NAME / SCORE / FOLDER / GROUPS table where SCORE is a 10-cell
unicode bar relative to the top match.

`--format json` / `--format yaml`: an array of `{name, score, folder, groups}` records on stdout.

`--names-only`: one name per line, ANSI-free, suitable for piping.

## Pipe into fzf

```bash
xv get "$(xv ls --names-only | fzf)"
xv get "$(xv find db --names-only | fzf)"
```

## Migrating from the old `xv find`

Before v0.6.1, `xv find <pattern>` opened an interactive picker via
dialoguer and copied the chosen secret to the clipboard. v0.6.1
replaces that with non-interactive ranked output. The interactive
picker is reserved for `xv pick` in v0.7.0 (TUI feature).

Current equivalents:

| Old | New |
|-----|-----|
| `xv find db` (interactive) | `xv get "$(xv find db --names-only \| fzf)"` |
| `xv find db --raw` | `xv find db --names-only \| head -1 \| xargs -I{} xv get {} --raw` |
