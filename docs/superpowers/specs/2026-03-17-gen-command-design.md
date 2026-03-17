# Design: `xv gen` — Password Generator Command

**Date**: 2026-03-17
**Status**: Approved

---

## Overview

Add a `gen` command to the `xv` CLI that generates a random password, copies it to the clipboard, and optionally saves it as a secret in the vault. This is a standalone password generator distinct from `rotate` (which mutates an existing secret).

---

## Command Interface

```
xv gen [OPTIONS]

Options:
  -l, --length <N>        Password length [default: 15, range: 6–100]
  -c, --charset <TYPE>    Character set [default: alphanumeric]
                          Values: alphanumeric, alphanumeric-symbols,
                                  alpha, numeric, uppercase, lowercase,
                                  hex, base64
      --save <NAME>       Save generated password as a secret in the vault
      --raw               Print to stdout instead of copying to clipboard
  -h, --help              Print help
```

### Default Behavior (no flags)

1. Generates a 15-character alphanumeric password
2. Copies to clipboard with auto-clear behavior matching `xv get` (respects `clipboard_timeout` config)
3. Prints: `Password copied to clipboard (auto-clears in 30s)`

### With `--save <name>`

- Stores the secret in the vault, then copies to clipboard
- Prints: `Secret 'mykey' saved and copied to clipboard`
- If vault save fails: warns the user and prints the generated value to stdout so it is never lost

### Config Key

`gen_default_charset` — overrides the `alphanumeric` default charset. Accepts the same values as `--charset`. Allows users to set a personal default (e.g., always use `alphanumeric-symbols`).

---

## Architecture

### Reused Existing Code (no changes needed)

| Component | Location | Used by |
|-----------|----------|---------|
| `CharsetType` enum + `generate_random_value()` | `src/cli/commands.rs` | `rotate` |
| `copy_to_clipboard()` + `schedule_clipboard_clear()` | `src/cli/commands.rs` | `get` |
| Secret set logic | `src/cli/commands.rs` | `set`, `rotate` |

### New Code

- **`Commands::Gen` variant** — added to the `Commands` enum in `src/cli/commands.rs` with fields: `length: u8`, `charset: Option<CharsetType>`, `save: Option<String>`, `raw: bool`
- **`execute_gen_command()`** — new async function in `src/cli/commands.rs` orchestrating the execution flow
- **`gen_default_charset`** — new optional config key in `src/config/settings.rs` and its parser

No new modules or files are required.

### Execution Flow

```
1. Resolve charset: --charset flag → gen_default_charset config → Alphanumeric
2. Validate length in [6, 100] — fail fast if out of range
3. generate_random_value(length, resolved_charset)
4. If --save <name>: store secret in vault (warn + print value on failure)
5. If --raw: print value to stdout
   Else: copy_to_clipboard() + schedule_clipboard_clear()
6. Print success message
```

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Length out of range | Fail with: `Error: length must be between 6 and 100` |
| Clipboard failure | Warn and fall back to printing value to stdout |
| `--save` vault failure | Warn, then print generated value to stdout so it is not lost |
| Invalid `gen_default_charset` in config | Fail with a clear message pointing to the config key |

---

## Testing

### Unit Tests

- Default invocation produces a 15-character alphanumeric string
- `--length` boundary validation: 5 → rejected, 6 → accepted, 100 → accepted, 101 → rejected
- Each `--charset` value produces only characters from the expected set
- `--raw` prints value to stdout instead of copying to clipboard

### Integration Tests

- `--save <name>`: verifies the secret exists in the vault after the command runs, following the pattern of existing vault integration tests in `tests/`

### Testing Principles

- No mocking of `generate_random_value` — test real output against the expected charset
- Integration tests require live Azure credentials (consistent with existing test strategy)

---

## Out of Scope

- Group/tag/note metadata on `--save` (use `xv update` after creation if needed)
- Passphrase generation (word-based)
- Custom character set strings
