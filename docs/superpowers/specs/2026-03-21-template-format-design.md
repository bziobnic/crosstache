# `--format template` Output Format — Design Spec

## Overview

Implement the `--format template` output format, which applies per-row field substitution using a user-provided template string via the existing `--template` flag.

## Problem

`--format template` is accepted by the CLI parser but returns a runtime error: "Template output format is not yet supported." The `--template` global flag exists but is unused.

## Solution

Replace the `format_as_template` stub in `src/utils/format.rs` with simple `{{field_name}}` substitution. Field names come from the `Tabled` trait's `headers()`. Each row gets the template applied, producing one line per row.

## Usage

```
xv list --format template --template "export {{name}}={{value}}"
```

Output:
```
export DB_PASSWORD=s3cret
export API_KEY=abc123
```

## Implementation

### Template Syntax

- `{{field_name}}` — replaced with the column value for that row
- Whitespace inside braces is trimmed: `{{ name }}` works the same as `{{name}}`
- Field names may contain spaces and word characters: `{{Subscription ID}}` matches header "Subscription ID"
- Field matching is case-insensitive: `{{Name}}` and `{{name}}` both match column "Name"
- Unknown fields are left as-is (forgiving, like shell variable expansion)
- No logic, no loops, no conditionals — just field substitution

### Regex Pattern

`\{\{\s*([\w\s]+?)\s*\}\}` — captures field names containing word characters and spaces, with leading/trailing whitespace trimmed by the `\s*` anchors and the non-greedy `+?`.

Compile the regex once using `std::sync::LazyLock` (stable since Rust 1.80) at module level, not per-call.

### File Changes

**`src/utils/format.rs`:**
- Add `template: Option<String>` field to `TableFormatter`
- Update `TableFormatter::new()` signature to `new(format: OutputFormat, no_color: bool, template: Option<String>)`
- Change `format_as_template` signature: remove the `_template: &str` parameter, read from `self.template` instead
- Replace `format_as_template` stub with working implementation:
  1. If `self.template` is `None`, return error: "Template format requires --template flag with a format string"
  2. Get headers via `T::headers()`, build a case-insensitive name→index map (lowercase header → field index)
  3. For each row: get fields via `item.fields()`, use regex to find `{{...}}` placeholders, look up field index by lowercase name, replace with field value
  4. Join rows with newline
- Update both `format_table` dispatch sites (empty-data at ~line 115 AND non-empty at ~line 126) to call `self.format_as_template(data)` with no template parameter
- Add module-level `LazyLock<Regex>` for the template regex

**`src/config/settings.rs`:**
- Add `template: Option<String>` to `Config` (runtime-only, `#[serde(skip)]`, `#[tabled(skip)]`)

**`src/cli/commands.rs`:**
- In the `Cli::execute` method (around line 935 where `runtime_output_format` is set), also assign `config.template = self.template.clone()`
- Add warning when `--template` is provided without `--format template`: `output::warn("--template flag has no effect without --format template")`

**All callers of `TableFormatter::new()`:**
- Update every call site to pass the template parameter
- Call sites with access to `config`: pass `config.template.clone()`
- Call sites without `config` access (e.g., inside `SecretManager`, `VaultManager`): pass `None`. These internal formatters cannot use `--format template` — that's fine, template output is a CLI-level feature. If a user passes `--format template` to a command that uses an internal formatter, the formatter will return the "requires --template flag" error.

**`src/cli/file_ops.rs`:**
- Fix the broken `OutputFormat::Template` arm (~line 548) which currently passes an empty `Vec` regardless of data. It should pass the actual data through `format_table` like the other format arms.

### Validation

| Scenario | Behavior |
|----------|----------|
| `--format template` without `--template` | Error: "Template format requires --template flag with a format string. Example: --template '{{name}}: {{value}}'" |
| `--template` without `--format template` | Warning printed, flag ignored |
| `{{unknown_field}}` in template | Left as-is in output |
| Empty data | Empty string (no output) |
| No `{{...}}` in template string | Template printed verbatim once per row |

### Error Type

Uses existing `CrosstacheError::config()` for the missing-template validation error.

### Secret Values

Template output does not bypass any existing access controls. `xv list` does not include secret values by default — they only appear when `--include-values` is explicitly requested. Template output is equivalent to `--format json` in this regard.

## Testing

Unit tests in `format.rs` `#[cfg(test)]` module:
- Basic field substitution with known fields
- Case-insensitive field matching
- Multi-word field names (e.g., `{{Subscription ID}}`)
- Unknown fields left as-is
- Missing `--template` flag returns error
- Empty data returns empty string
- Multiple fields in one template
- Whitespace inside braces trimmed

## No New Dependencies

Uses `regex::Regex` (already in Cargo.toml) and `std::sync::LazyLock` (stable std).

## Out of Scope

- Conditional logic (`{{#if ...}}`)
- Loops/iteration (`{{#each ...}}`)
- Nested field access (`{{secret.name}}`)
- Escape sequences for literal `{{` output
