# Design: Output Consistency & Confirmation Standardization

**Date:** 2026-03-06
**Theme:** UX Audit - Theme A
**Status:** Approved

## Problem

The codebase has three divergent output systems that use different emoji, different color handling, and different TTY awareness:

1. `DisplayUtils` in `format.rs` -- uses plain Unicode symbols with crossterm colors and `no_color` flag
2. `InteractivePrompt` in `interactive.rs` -- uses full-width emoji with no TTY/color awareness
3. Raw `println!`/`eprintln!` in `commands.rs` (77 occurrences) and `main.rs` -- uses varied emoji per command

Confirmations also use two different mechanisms: `InteractivePrompt::confirm()` (4 sites) and `rpassword::prompt_password()` with manual y/N parsing (5 sites).

## Design

### 1. New `utils/output.rs` Module

Free functions that auto-detect TTY via `std::io::IsTerminal`, cached with `std::sync::OnceLock`:

```
success(msg)  -- TTY: "checkmark {green msg}"     Pipe: "[ok] {msg}"
error(msg)    -- TTY: "x {red msg}"               Pipe: "[error] {msg}"  -> stderr
warn(msg)     -- TTY: "warning {yellow msg}"       Pipe: "[warn] {msg}"
info(msg)     -- TTY: "info {cyan msg}"            Pipe: "[info] {msg}"
hint(msg)     -- TTY: "bulb {dim msg}"             Pipe: "[hint] {msg}"
step(msg)     -- TTY: "play {bold msg}"            Pipe: ":: {msg}"
is_tty()      -- cached check on stdout
is_tty_stderr() -- cached check on stderr
```

Key properties:
- Free functions, no struct to pass around
- `OnceLock` for one-time TTY detection
- `error()` always writes to stderr, everything else to stdout
- Respects `NO_COLOR` env var and existing `--no-color` flag

### 2. Canonical Emoji Vocabulary

| Purpose     | TTY  | Pipe      | Replaces                                  |
|-------------|------|-----------|-------------------------------------------|
| Success     | (checkmark)  | `[ok]`    | Mixed check/checkmark/bare text           |
| Error       | (x mark)  | `[error]` | Mixed x variants                          |
| Warning     | (warning)  | `[warn]`  | Mixed warning variants                    |
| Info        | (info)  | `[info]`  | Mixed info variants                       |
| Hint        | (bulb)  | `[hint]`  | Ad-hoc bulb in some commands              |
| Step/action | (play)  | `::`      | lock/search/rotate/trash/clipboard/rocket |

All varied action emoji collapse into `step()`. The verb in the message provides context.

### 3. Confirmation Standardization

All 9 confirmation sites use `InteractivePrompt::confirm()` via `dialoguer`. `rpassword::prompt_password()` retained only for reading secret values (commands.rs:4038).

Migration sites (all in commands.rs):
- Line 5461: secret delete
- Line 5700: secret purge
- Line 7048: file delete
- Line 7721: file delete multiple
- Line 7960: group delete

No behavioral change: same default (No), same `--force` skip behavior.

### 4. Module Changes

**New:** `utils/output.rs` (~80 lines)

**Modified:**
- `utils/interactive.rs` -- remove message methods (.success, .error, .info, .warning, .step), keep input methods only. ProgressIndicator uses output::* internally.
- `utils/format.rs` -- remove DisplayUtils message methods (print_success, print_warning, print_error, print_info). Keep format_key_value_pairs, print_header, print_separator, print_banner. Remove _theme and no_color (move to output.rs).
- `cli/commands.rs` -- ~77 raw println!/eprintln! with emoji replaced with output::* calls. 5 rpassword confirmation sites migrated to InteractivePrompt::confirm().
- `main.rs` -- print_user_friendly_error() migrates to output::error().
- `config/init.rs` -- replace InteractivePrompt message calls with output::*.

**Unchanged:**
- TableFormatter and table output logic
- InteractivePrompt input methods (confirm, select, input_text)
- ProgressIndicator public API
- All command flag definitions in cli/mod.rs
- Secret/vault/blob manager internals

**Incidental fix:** Typo "Unimnplemented" at commands.rs:5943.
