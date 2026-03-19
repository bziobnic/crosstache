# Design: `xv gen` — Password Generator Command

**Date**: 2026-03-17
**Status**: Approved

---

## Overview

Add a `gen` command to the `xv` CLI that generates a random password, copies it to the clipboard, and optionally saves it as a secret in the vault. This is a standalone password generator distinct from `rotate` (which mutates an existing secret). The default length of 15 is intentionally shorter than `rotate`'s 32 — `gen` targets human-memorable passwords, `rotate` targets high-entropy machine secrets.

---

## Command Interface

```
xv gen [OPTIONS]

Options:
  -l, --length <N>        Password length [default: 15, range: 6–100]
  -c, --charset <TYPE>    Character set [default: alphanumeric]
                          Values: alphanumeric, alphanumeric-symbols,
                                  numeric, uppercase, lowercase,
                                  hex, base64
      --save <NAME>       Save generated password as a secret in the vault
      --vault <VAULT>     Target vault for --save (overrides context/config default)
      --raw               Print to stdout instead of copying to clipboard
  -h, --help              Print help
```

**Note on charset values:** These map directly to existing `CharsetType` enum variants. The `alpha` value (letters only, A-Za-z) is intentionally omitted — use `uppercase` or `lowercase` for single-case alpha, or combine via `alphanumeric` and exclude digits in a future enhancement. If a combined mixed-case alpha-only charset is desired, a new `CharsetType::Alpha` variant would need to be added; that is out of scope for this feature.

### Default Behavior (no flags)

1. Generates a 15-character alphanumeric password
2. Copies to clipboard with auto-clear behavior matching `xv get` (respects `clipboard_timeout` config)
3. Prints: `Password copied to clipboard (auto-clears in 30s)`

### With `--save <name>`

- Stores the secret in the vault using the same vault resolution chain as `set` and `rotate`: `--vault` flag → active context → config default
- The `--vault` flag is supported on `gen` for consistency with other write commands
- The secret is saved first, then the output/clipboard step runs
- Secret `created_by` tag is populated automatically (same as `set`)
- Prints: `Secret 'mykey' saved and copied to clipboard`
- If vault save fails: warns the user and prints the generated value to stdout so it is never lost

### `--raw` + `--save` Combined Behavior

When both `--raw` and `--save` are provided:
- Saves the secret to the vault
- Prints the value to stdout (no clipboard involved)
- If vault save fails: still prints the value to stdout (since stdout was already the intended output channel), with a warning prefix

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

- **`Commands::Gen` variant** — added to the `Commands` enum in `src/cli/commands.rs` with fields: `length: usize`, `charset: Option<CharsetType>`, `save: Option<String>`, `vault: Option<String>`, `raw: bool`
- **`execute_gen_command()`** — new async function in `src/cli/commands.rs` orchestrating the execution flow
- **`gen_default_charset`** — new config key added to:
  - `src/config/settings.rs`: new field `gen_default_charset: CharsetType` on the `Config` struct, annotated `#[serde(default)]`; `CharsetType::Alphanumeric` is the default (via `Default` impl). This follows the same pattern as `azure_credential_priority: AzureCredentialType`. `CharsetType` must gain `#[derive(Serialize, Deserialize, Default)]` and a `FromStr` impl (matching variant names case-insensitively, e.g. `"alphanumeric-symbols"` → `AlphanumericSymbols`) to support TOML round-tripping and config-set parsing. `CharsetType` must also gain a `Display` impl (following `AzureCredentialType`) because `Config` derives `Tabled` — without it the build will fail. The field on `Config` should either include `#[tabled(rename = "Gen Default Charset")]` or `#[tabled(skip)]`
  - `execute_config_set()` in `src/cli/commands.rs`: new match arm to handle `"gen_default_charset"` (parse via `CharsetType::from_str`, store on `config.gen_default_charset`)
  - The known-keys error string in `execute_config_set()` (around line 4018): append `gen_default_charset` to the list so `xv config set gen_default_charset alphanumeric-symbols` works correctly

No new modules or files are required.

### Execution Flow

```
1. Resolve charset: --charset flag → config.gen_default_charset → Alphanumeric
2. Validate length in [6, 100] — fail fast if out of range
3. generate_random_value(length, resolved_charset)
4. If --save <name>:
     resolve vault (--vault flag → context → config default)
     store secret in vault
     on failure: warn + always print value to stdout (safe fallback regardless of --raw), return
5. If --raw: print value to stdout
   Else: copy_to_clipboard() + schedule_clipboard_clear()
6. Print success message:
     --save only:          "Secret 'mykey' saved and copied to clipboard"
     --save + --raw:       "Secret 'mykey' saved." (value already printed above)
     default (no --save):  "Password copied to clipboard (auto-clears in {N}s)" if clipboard_timeout > 0, else "Password copied to clipboard"
     --raw only:           (value already printed; no additional message needed)
```

**Note on `--raw + --save` failure path:** if the vault save fails, the fallback is always to print to stdout — regardless of whether `--raw` was set — since stdout is the safest recovery channel in all cases.

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
- Default clipboard path: smoke-level test verifying `copy_to_clipboard()` is called on success

### Integration Tests

- `--save <name>`: verifies the secret exists in the vault after the command runs, following the pattern of existing vault integration tests in `tests/`

### Testing Principles

- No mocking of `generate_random_value` — test real output against the expected charset
- Integration tests require live Azure credentials (consistent with existing test strategy)

---

## Out of Scope

- Group/tag/note metadata on `--save` (use `xv update` after creation if needed; `created_by` is auto-populated)
- Passphrase generation (word-based)
- Custom character set strings
- Mixed-case alpha-only charset (`alpha`) — requires a new `CharsetType::Alpha` variant, deferred
