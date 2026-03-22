# `--format template` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `--format template` output format with per-row `{{field_name}}` substitution.

**Architecture:** Add `template: Option<String>` to `TableFormatter` and `Config`. Replace the `format_as_template` stub with regex-based field substitution using headers from the `Tabled` trait. Wire `cli.template` through `Config` to all formatter call sites.

**Tech Stack:** `regex::Regex` (existing dep), `std::sync::LazyLock` (std), `tabled::Tabled` trait

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `src/utils/format.rs` | Modify | Template formatting logic + tests |
| `src/config/settings.rs` | Modify | Add `template` field to Config |
| `src/cli/commands.rs` | Modify | Wire `cli.template` to config, add warning |
| `src/cli/secret_ops.rs` | Modify | Update `TableFormatter::new` call sites |
| `src/cli/vault_ops.rs` | Modify | Update `TableFormatter::new` call sites |
| `src/cli/file_ops.rs` | Modify | Update call sites + fix broken Template arm |
| `src/vault/manager.rs` | Modify | Update `TableFormatter::new` call sites |
| `src/secret/manager.rs` | Modify | Update `TableFormatter::new` call sites |

---

### Task 1: Implement template formatting logic with tests

Add the template engine to `format.rs` and update `TableFormatter` to carry a `template` field. Write tests first.

**Files:**
- Modify: `src/utils/format.rs`

- [ ] **Step 1: Add the `LazyLock` regex and update `TableFormatter` struct + constructor**

At the top of `src/utils/format.rs`, add the regex import and lazy static. Then update the struct and constructor.

Add after the existing imports (after line 18):

```rust
use regex::Regex;
use std::sync::LazyLock;

/// Regex for template placeholders: {{field_name}} with optional whitespace.
/// Field names may contain word characters and spaces.
static TEMPLATE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{\s*([\w\s]+?)\s*\}\}").unwrap());
```

Update the `TableFormatter` struct (around line 86-90):

```rust
pub struct TableFormatter {
    _theme: ColorTheme,
    format: OutputFormat,
    no_color: bool,
    template: Option<String>,
}
```

Update `TableFormatter::new` (around line 94-100):

```rust
pub fn new(format: OutputFormat, no_color: bool, template: Option<String>) -> Self {
    Self {
        _theme: ColorTheme::default(),
        format: format.resolve_for_stdout(),
        no_color,
        template,
    }
}
```

- [ ] **Step 2: Replace the `format_as_template` stub**

Replace the existing `format_as_template` method (lines 189-194) with:

```rust
/// Format data using a template with {{field_name}} substitution
fn format_as_template<T: Tabled>(&self, data: &[T]) -> Result<String> {
    let template_str = self.template.as_deref().ok_or_else(|| {
        crate::error::CrosstacheError::config(
            "Template format requires --template flag with a format string. Example: --template '{{name}}: {{value}}'".to_string(),
        )
    })?;

    if data.is_empty() {
        return Ok(String::new());
    }

    // Build case-insensitive header → index map
    let headers = T::headers();
    let header_map: std::collections::HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.as_ref().to_lowercase(), i))
        .collect();

    // Apply template to each row
    let mut lines = Vec::with_capacity(data.len());
    for item in data {
        let fields = item.fields();
        let line = TEMPLATE_REGEX
            .replace_all(template_str, |caps: &regex::Captures| {
                let field_name = caps[1].trim().to_lowercase();
                if let Some(&idx) = header_map.get(&field_name) {
                    fields
                        .get(idx)
                        .map(|f| f.as_ref().to_string())
                        .unwrap_or_default()
                } else {
                    // Unknown field — leave placeholder as-is
                    caps[0].to_string()
                }
            })
            .to_string();
        lines.push(line);
    }

    Ok(lines.join("\n"))
}
```

- [ ] **Step 3: Update the `format_table` dispatch to remove the template parameter**

In `format_table` (around lines 103-129), change both Template arms:

Line ~115 (empty data path):
```rust
OutputFormat::Template => self.format_as_template(data),
```

Line ~126 (non-empty data path):
```rust
OutputFormat::Template => self.format_as_template(data),
```

(Remove the second `""` argument from both.)

- [ ] **Step 4: Update existing tests to pass `None` for template**

In the `#[cfg(test)]` module at the bottom of `format.rs`, update the existing test constructors:

Line ~363: `let formatter = TableFormatter::new(OutputFormat::Table, true, None);`
Line ~371: `let formatter = TableFormatter::new(OutputFormat::Json, true, None);`

- [ ] **Step 5: Add template format tests**

Add these tests to the existing `#[cfg(test)] mod tests` block in `format.rs`:

```rust
#[test]
fn test_template_basic_substitution() {
    let data = vec![
        TestData {
            name: "secret1".to_string(),
            value: "abc123".to_string(),
            status: "active".to_string(),
        },
        TestData {
            name: "secret2".to_string(),
            value: "xyz789".to_string(),
            status: "inactive".to_string(),
        },
    ];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("export {{Name}}={{Value}}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "export secret1=abc123\nexport secret2=xyz789");
}

#[test]
fn test_template_case_insensitive() {
    let data = vec![TestData {
        name: "mykey".to_string(),
        value: "myval".to_string(),
        status: "active".to_string(),
    }];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{name}} {{NAME}} {{Name}}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "mykey mykey mykey");
}

#[test]
fn test_template_unknown_field_left_as_is() {
    let data = vec![TestData {
        name: "key".to_string(),
        value: "val".to_string(),
        status: "ok".to_string(),
    }];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{Name}}: {{nonexistent}}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "key: {{nonexistent}}");
}

#[test]
fn test_template_missing_template_flag_errors() {
    let data = vec![TestData {
        name: "key".to_string(),
        value: "val".to_string(),
        status: "ok".to_string(),
    }];

    let formatter = TableFormatter::new(OutputFormat::Template, true, None);
    let result = formatter.format_table(&data);
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("--template"),
        "Error should mention --template flag"
    );
}

#[test]
fn test_template_empty_data_returns_empty() {
    let data: Vec<TestData> = vec![];
    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{Name}}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "");
}

#[test]
fn test_template_whitespace_in_braces() {
    let data = vec![TestData {
        name: "key".to_string(),
        value: "val".to_string(),
        status: "ok".to_string(),
    }];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{ Name }} = {{  Value  }}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "key = val");
}

#[test]
fn test_template_multiple_fields() {
    let data = vec![TestData {
        name: "db_pass".to_string(),
        value: "secret".to_string(),
        status: "active".to_string(),
    }];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{Name}}={{Value}} ({{Status}})".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "db_pass=secret (active)");
}
#[test]
fn test_template_multi_word_field_name() {
    // TestData uses #[tabled(rename = "Name")] etc. which are single-word.
    // To test multi-word headers, we need a struct with multi-word renames.
    #[derive(Tabled, Serialize)]
    struct MultiWordData {
        #[tabled(rename = "Secret Name")]
        secret_name: String,
        #[tabled(rename = "Created By")]
        created_by: String,
    }

    let data = vec![MultiWordData {
        secret_name: "api-key".to_string(),
        created_by: "admin".to_string(),
    }];

    let formatter = TableFormatter::new(
        OutputFormat::Template,
        true,
        Some("{{Secret Name}} by {{Created By}}".to_string()),
    );
    let result = formatter.format_table(&data).unwrap();
    assert_eq!(result, "api-key by admin");
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test --lib format -- --nocapture`
Expected: All tests PASS (existing + 7 new)

- [ ] **Step 7: Commit**

```bash
git add src/utils/format.rs
git commit -m "feat: implement --format template with field substitution"
```

---

### Task 2: Wire template through Config and update all call sites

Add `template` to Config, wire `cli.template` to it, update all `TableFormatter::new` call sites, and fix the broken `file_ops.rs` Template arm.

**Files:**
- Modify: `src/config/settings.rs`
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/secret_ops.rs`
- Modify: `src/cli/vault_ops.rs`
- Modify: `src/cli/file_ops.rs`
- Modify: `src/vault/manager.rs`
- Modify: `src/secret/manager.rs`

- [ ] **Step 1: Add `template` field to Config**

In `src/config/settings.rs`, add after `runtime_output_format` (around line 105):

```rust
    /// Custom template string for `--format template` (set in `Cli::execute`, not persisted).
    #[serde(skip)]
    #[tabled(skip)]
    pub template: Option<String>,
```

In the `Default` impl (around line 152), add after `runtime_output_format: OutputFormat::Auto,`:

```rust
            template: None,
```

Also add in both test config string blocks if they construct Config manually — or the `#[serde(skip)]` will handle it.

- [ ] **Step 2: Wire `cli.template` in `Cli::execute` and add warning**

In `src/cli/commands.rs`, in the `Cli::execute` method (around line 935), after the line `config.output_json = matches!(resolved, OutputFormat::Json);`, add:

```rust
        // Wire template string
        config.template = self.template.clone();

        // Warn if --template given without --format template
        if config.template.is_some() && resolved != OutputFormat::Template {
            crate::utils::output::warn(
                "--template flag has no effect without --format template",
            );
        }
```

- [ ] **Step 3: Update `TableFormatter::new` call sites with `config` access**

These call sites have access to `config` and should pass `config.template.clone()`:

**`src/cli/secret_ops.rs`:**
- Line 151: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 157: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 2014: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 2027: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 2634: `crate::utils::format::TableFormatter::new(crate::utils::format::OutputFormat::Table, config.no_color, None)` (hardcoded Table format — no template needed)

**`src/cli/vault_ops.rs`:**
- Line 224: `TableFormatter::new(output_format, config.no_color, config.template.clone())`
- Line 885: `crate::utils::format::TableFormatter::new(output_format, config.no_color, config.template.clone())`

**`src/cli/file_ops.rs`:**
- Line 504: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 528: `TableFormatter::new(fmt, config.no_color, config.template.clone())`
- Line 549: `TableFormatter::new(fmt, config.no_color, config.template.clone())`

- [ ] **Step 4: Update `TableFormatter::new` call sites without `config` access**

These internal callers pass `None`:

**`src/vault/manager.rs`:**
- Line 116: `TableFormatter::new(output_format, self.no_color, None)`
- Line 274: `TableFormatter::new(output_format, self.no_color, None)`
- Line 436: `TableFormatter::new(OutputFormat::Table, self.no_color, None)`

**`src/secret/manager.rs`:**
- Line 1610: `TableFormatter::new(OutputFormat::Table, self.no_color, None)`
- Line 1733: `TableFormatter::new(output_format, self.no_color, None)`
- Line 1783: `TableFormatter::new(output_format.clone(), self.no_color, None)`

- [ ] **Step 5: Fix the broken `file_ops.rs` Template arm**

In `src/cli/file_ops.rs` (lines 548-552), replace:

```rust
        OutputFormat::Template => {
            let formatter = TableFormatter::new(fmt, config.no_color);
            let empty: Vec<ListItem> = vec![];
            println!("{}", formatter.format_table(&empty)?);
        }
```

With:

```rust
        OutputFormat::Template => {
            let display_items: Vec<ListItem> = items
                .iter()
                .map(|item| match item {
                    BlobListItem::Directory { name, .. } => ListItem {
                        name: name.clone(),
                        size: "<DIR>".to_string(),
                        content_type: "-".to_string(),
                        modified: "-".to_string(),
                        groups: "-".to_string(),
                    },
                    BlobListItem::File(file) => ListItem {
                        name: file.name.clone(),
                        size: format_size(file.size),
                        content_type: file.content_type.clone(),
                        modified: file.last_modified.format("%Y-%m-%d %H:%M:%S").to_string(),
                        groups: file.groups.join(", "),
                    },
                })
                .collect();
            let formatter = TableFormatter::new(fmt, config.no_color, config.template.clone());
            println!("{}", formatter.format_table(&display_items)?);
        }
```

- [ ] **Step 6: Run all tests and clippy**

Run: `cargo test --lib`
Expected: All tests PASS

Run: `cargo clippy --all-targets`
Expected: No new warnings

- [ ] **Step 7: Commit**

```bash
git add src/config/settings.rs src/cli/commands.rs src/cli/secret_ops.rs src/cli/vault_ops.rs src/cli/file_ops.rs src/vault/manager.rs src/secret/manager.rs
git commit -m "feat: wire --template through Config to all formatter call sites"
```
