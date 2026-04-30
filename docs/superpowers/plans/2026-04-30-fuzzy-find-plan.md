# `xv find` Fuzzy Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the existing interactive `xv find` (dialoguer FuzzySelect) with a non-interactive ranked fuzzy search using the `nucleo` crate. Default search field is secret name; `--in <field>` opts in to folder/groups/note/tags. Output is table-like by default with score bars; `--names-only` makes it pipe-friendly. Adds `--names-only` to `xv ls` for the canonical `xv ls --names-only | fzf` workflow. Cuts v0.6.1-rc.1.

**Architecture:** New `src/utils/fuzzy.rs` module owns the pure scoring function (`score_matches(pattern, items, fields) -> Vec<Match>`) backed by `nucleo`. The `xv find` CLI handler in `src/cli/secret_ops.rs::execute_secret_find` is rewritten: replace the interactive selector with ranked output. `--all-vaults` iterates the user's vault list (existing cache); each vault's secrets are scored independently, results merged and re-ranked. `--names-only` overrides `--format` to ASCII-only one-line-per-name output regardless of TTY status. `xv ls --names-only` is a small parallel addition. No backend trait changes.

**Tech Stack:** Rust 2021, `nucleo = "0.5"` (pure-Rust SkimMatcherV2-style scoring; same matcher as Helix). `serde` for envelope types. No new heavy deps.

**Reference spec:** `docs/superpowers/specs/2026-04-29-strategic-improvements-phase-1-design.md` §3.3.

**Behavioral break notice:** The existing `xv find` is interactive (dialoguer::FuzzySelect → copies one secret to clipboard). After this plan it becomes non-interactive ranked output. The interactive picker is out of scope for this plan and is reserved for `xv pick` / TUI in v0.7.0. Release notes and `docs/find.md` must call this out so users running `xv find` in scripts know the behavior changed.

---

## File Structure

**Created:**

| Path | Responsibility |
|------|----------------|
| `src/utils/fuzzy.rs` | `FuzzyField` enum, `Match` struct, `score_matches(pattern, items, fields)`. Pure function; no I/O. |
| `docs/find.md` | User-facing reference for `xv find`: fields, flags, output, pipe-into-fzf canonical form. |

**Modified:**

| Path | Change |
|------|--------|
| `Cargo.toml` | Add `nucleo = "0.5"`. |
| `src/utils/mod.rs` | `pub mod fuzzy;`. |
| `src/cli/commands.rs` | Replace `Commands::Find` variant fields with the new flag set (`pattern`, `in`, `limit`, `min_score`, `names_only`, `all_vaults`). Add `--names-only` to `Commands::List`. |
| `src/cli/secret_ops.rs` | Rewrite `execute_secret_find` (drop `dialoguer::FuzzySelect`); add cross-vault iteration; add score-bar formatting. Add `--names-only` branch in `execute_secret_list`. |
| `docs/exit-codes.md` | Mention that `xv find` now falls under exit code `0` on found-or-empty (no special new code; document only). |
| `README.md` | Add a "Fuzzy search" subsection linking to `docs/find.md`, show `xv ls --names-only \| fzf` canonical example. |
| `docs/superpowers/specs/backend-trait-checklist.md` | Append two read-surface entries: `SecretManager::list_secrets`, `VaultManager::list_vaults`. (Create the file if it doesn't exist.) |
| `Cargo.toml` | Bump version to `0.6.1-rc.1` (Task 12). |

---

## Task 1: Add `nucleo` dependency

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock` (auto-updated by cargo)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, find the `[dependencies]` section. Add the line:

```toml
nucleo = "0.5"
```

Place it where it fits the section's existing convention (alphabetical preferred; otherwise append).

- [ ] **Step 2: Sanity build**

Run: `cargo build`
Expected: build succeeds. `Cargo.lock` picks up `nucleo` and its (small) transitive deps.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: add nucleo 0.5 for fuzzy ranking in xv find"
```

---

## Task 2: Create `utils::fuzzy` module with `score_matches` (name-only)

**Files:**
- Create: `src/utils/fuzzy.rs`
- Modify: `src/utils/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `src/utils/fuzzy.rs` with test scaffolding only:

```rust
//! Fuzzy ranking helpers backed by `nucleo`.
//!
//! Pure functions; no I/O. Used by `xv find` to rank secrets and
//! by future commands that want a "did you mean a list of these?"
//! ranked output.

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build minimal candidate items for tests.
    fn item(name: &str) -> CandidateItem {
        CandidateItem {
            name: name.to_string(),
            folder: None,
            groups: None,
            note: None,
            tags: vec![],
        }
    }

    #[test]
    fn ranks_close_match_first() {
        let items = vec![
            item("DB_PASSWORD"),
            item("API_TOKEN"),
            item("DB_HOSTNAME"),
        ];
        let matches = score_matches("dbpw", &items, &[FuzzyField::Name]);
        assert!(!matches.is_empty(), "must produce matches");
        assert_eq!(matches[0].item.name, "DB_PASSWORD");
    }

    #[test]
    fn empty_pattern_returns_all_with_score_zero() {
        let items = vec![item("FOO"), item("BAR")];
        let matches = score_matches("", &items, &[FuzzyField::Name]);
        assert_eq!(matches.len(), 2, "empty pattern returns all items");
        for m in &matches {
            assert_eq!(m.score, 0, "empty pattern → score 0 for every item");
        }
    }

    #[test]
    fn no_matches_returns_empty() {
        let items = vec![item("FOO"), item("BAR")];
        let matches = score_matches("xyzzy_nonexistent", &items, &[FuzzyField::Name]);
        assert!(matches.is_empty(), "no candidates match → empty");
    }

    #[test]
    fn ties_broken_by_alphabetical_name() {
        // Two identical patterns; nucleo will return equal scores; we
        // tie-break alphabetically by name.
        let items = vec![item("zebra"), item("alpha"), item("middle")];
        let matches = score_matches("a", &items, &[FuzzyField::Name]);
        // 'alpha' contains 'a' first (position 0); 'middle' has it later;
        // 'zebra' has it. nucleo's score may differ, but among equal scores,
        // alphabetical wins — this test asserts the tie-break works when
        // scores DO match (assert at least the order is deterministic).
        assert!(!matches.is_empty());
        // We don't assert exact ordering of dissimilar scores; we only
        // assert determinism: running again returns identical order.
        let again = score_matches("a", &items, &[FuzzyField::Name]);
        assert_eq!(
            matches.iter().map(|m| m.item.name.clone()).collect::<Vec<_>>(),
            again.iter().map(|m| m.item.name.clone()).collect::<Vec<_>>(),
            "scoring must be deterministic"
        );
    }

    #[test]
    fn name_only_does_not_match_folder_text() {
        let items = vec![CandidateItem {
            name: "FOO".to_string(),
            folder: Some("database".to_string()),
            groups: None,
            note: None,
            tags: vec![],
        }];
        // pattern matches the folder, but we asked for name-only → no match
        let matches = score_matches("data", &items, &[FuzzyField::Name]);
        assert!(matches.is_empty(), "name-only field selector ignores folder");
    }
}
```

Add to `src/utils/mod.rs`. Find the existing `pub mod ...;` declarations and add (alphabetically — `fuzzy` slots between `format` and `helpers`):

```rust
pub mod fuzzy;
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib utils::fuzzy`
Expected: compile error — `CandidateItem`, `FuzzyField`, `score_matches`, `Match` not defined.

- [ ] **Step 3: Implement the types and `score_matches`**

Add to `src/utils/fuzzy.rs` (above the `#[cfg(test)]` block):

```rust
use nucleo::{Config, Matcher, Utf32Str};

/// One row's worth of metadata that `score_matches` can search against.
/// Caller fills in whichever fields are populated; missing fields are skipped.
#[derive(Debug, Clone)]
pub struct CandidateItem {
    pub name: String,
    pub folder: Option<String>,
    pub groups: Option<String>,
    pub note: Option<String>,
    pub tags: Vec<String>,
}

/// Which field(s) of a `CandidateItem` to score the pattern against.
/// When multiple fields are given, the highest score across the listed
/// fields wins for that item; an item with no scoring field producing a
/// match is excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FuzzyField {
    Name,
    Folder,
    Groups,
    Note,
    Tags,
}

/// One scored result.
#[derive(Debug, Clone)]
pub struct Match<'a> {
    pub item: &'a CandidateItem,
    /// Raw nucleo score. Higher = better. `0` for the
    /// empty-pattern degenerate case (we still surface every item).
    pub score: u32,
}

/// Score every item against `pattern` using the requested fields.
///
/// Empty `pattern` returns every item with score `0`, in input order.
/// Otherwise: items that score against at least one of the requested
/// fields are kept; items with no matching field are dropped. Results
/// are sorted by score descending; ties are broken alphabetically by
/// `item.name` (case-insensitive).
pub fn score_matches<'a>(
    pattern: &str,
    items: &'a [CandidateItem],
    fields: &[FuzzyField],
) -> Vec<Match<'a>> {
    if pattern.is_empty() {
        return items.iter().map(|item| Match { item, score: 0 }).collect();
    }
    if items.is_empty() || fields.is_empty() {
        return Vec::new();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut pattern_buf = Vec::new();
    let pattern_utf32 = Utf32Str::new(pattern, &mut pattern_buf);

    let mut out: Vec<Match<'a>> = Vec::new();
    for item in items {
        let mut best: Option<u32> = None;
        for field in fields {
            let candidates: &[&str] = match field {
                FuzzyField::Name => &[item.name.as_str()],
                FuzzyField::Folder => match &item.folder {
                    Some(s) => std::slice::from_ref(&s.as_str()),
                    None => &[],
                },
                FuzzyField::Groups => match &item.groups {
                    Some(s) => std::slice::from_ref(&s.as_str()),
                    None => &[],
                },
                FuzzyField::Note => match &item.note {
                    Some(s) => std::slice::from_ref(&s.as_str()),
                    None => &[],
                },
                FuzzyField::Tags => {
                    // Score against each tag separately; keep the best.
                    let mut tag_best: Option<u32> = None;
                    for tag in &item.tags {
                        let mut hay_buf = Vec::new();
                        let hay = Utf32Str::new(tag.as_str(), &mut hay_buf);
                        if let Some(s) = matcher.fuzzy_match(hay, pattern_utf32) {
                            tag_best = Some(tag_best.map_or(s, |b| b.max(s)));
                        }
                    }
                    if let Some(s) = tag_best {
                        best = Some(best.map_or(s, |b| b.max(s)));
                    }
                    continue;
                }
            };
            for hay_str in candidates {
                let mut hay_buf = Vec::new();
                let hay = Utf32Str::new(hay_str, &mut hay_buf);
                if let Some(s) = matcher.fuzzy_match(hay, pattern_utf32) {
                    best = Some(best.map_or(s, |b| b.max(s)));
                }
            }
        }
        if let Some(score) = best {
            out.push(Match { item, score });
        }
    }

    // Sort: score desc, then name asc (case-insensitive) for tie-break.
    out.sort_by(|a, b| {
        b.score.cmp(&a.score).then_with(|| {
            a.item
                .name
                .to_lowercase()
                .cmp(&b.item.name.to_lowercase())
        })
    });
    out
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib utils::fuzzy`
Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/utils/fuzzy.rs src/utils/mod.rs
git commit -m "feat(utils): add fuzzy::score_matches nucleo-backed ranker

Pure function. Empty pattern returns every item with score 0; non-
empty pattern keeps items that match at least one requested field
and ranks by score desc, name asc on ties. Fields: Name (default
caller usage), Folder, Groups, Note, Tags.
"
```

---

## Task 3: Build `CandidateItem` from `SecretSummary`

**Files:**
- Modify: `src/utils/fuzzy.rs`

We need a small adapter so `xv find` can hand `SecretSummary` rows to the scorer without manual field extraction at every call site.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `src/utils/fuzzy.rs`:

```rust
    #[test]
    fn from_secret_summary_extracts_all_fields() {
        use crate::secret::manager::SecretSummary;
        let summary = SecretSummary {
            name: "DB_PASSWORD".to_string(),
            original_name: "DB_PASSWORD".to_string(),
            note: Some("primary db".to_string()),
            folder: Some("backend/database".to_string()),
            groups: Some("backend,prod".to_string()),
            updated_on: String::new(),
            enabled: true,
            content_type: String::new(),
        };
        let item = CandidateItem::from_secret_summary(&summary);
        // Prefer original_name over sanitized name (matches the user-typed form).
        assert_eq!(item.name, "DB_PASSWORD");
        assert_eq!(item.folder.as_deref(), Some("backend/database"));
        assert_eq!(item.groups.as_deref(), Some("backend,prod"));
        assert_eq!(item.note.as_deref(), Some("primary db"));
        // Tags come from groups (comma-separated). v0.6.1 has no separate
        // tags field on SecretSummary; we map groups to tags so `--in tags`
        // still works on existing data.
        assert!(item.tags.contains(&"backend".to_string()));
        assert!(item.tags.contains(&"prod".to_string()));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --lib utils::fuzzy::tests::from_secret_summary_extracts_all_fields`
Expected: compile error — `from_secret_summary` not defined on `CandidateItem`.

- [ ] **Step 3: Implement the adapter**

Add to `src/utils/fuzzy.rs` (in the `impl CandidateItem` block — create one if needed):

```rust
impl CandidateItem {
    /// Adapt a `SecretSummary` to a `CandidateItem`. Prefers
    /// `original_name` over the sanitized `name` since users search
    /// against what they typed, not against post-sanitization forms.
    /// Empty `original_name` falls back to `name`.
    pub fn from_secret_summary(s: &crate::secret::manager::SecretSummary) -> Self {
        let name = if s.original_name.is_empty() {
            s.name.clone()
        } else {
            s.original_name.clone()
        };
        let tags: Vec<String> = s
            .groups
            .as_deref()
            .map(|g| g.split(',').map(|t| t.trim().to_string()).collect())
            .unwrap_or_default();
        Self {
            name,
            folder: s.folder.clone(),
            groups: s.groups.clone(),
            note: s.note.clone(),
            tags,
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib utils::fuzzy`
Expected: 6 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/utils/fuzzy.rs
git commit -m "feat(utils): add CandidateItem::from_secret_summary adapter

Prefers original_name over sanitized name (matches what users typed).
Splits groups CSV into tags so '--in tags' works on existing data.
"
```

---

## Task 4: Replace `xv find` flags surface

**Files:**
- Modify: `src/cli/commands.rs`

This task only changes the CLI argument schema — implementation comes in Task 5. After this task the build is broken (the executor expects the old fields); we fix it next task.

- [ ] **Step 1: Replace the variant**

In `src/cli/commands.rs`, find the existing `Find` variant (around line 245):

```rust
    /// Interactively find and copy a secret by name pattern (alias: search)
    #[command(alias = "search")]
    Find {
        /// Search term — substring match, or prefix with trailing * (e.g. claude-*)
        /// Omit to browse all secrets interactively.
        term: Option<String>,
        /// Print value to stdout instead of copying to clipboard
        #[arg(short, long)]
        raw: bool,
    },
```

Replace with:

```rust
    /// Ranked fuzzy search over secrets (alias: search). Non-interactive;
    /// pipe the output through fzf or similar for an interactive picker.
    /// Default search field is the secret name; opt in to other fields
    /// via repeated `--in <field>`.
    #[command(alias = "search")]
    Find {
        /// Pattern to score every secret against. Omit to list all
        /// secrets unranked (score 0); flags still apply.
        pattern: Option<String>,

        /// Search additional fields alongside the name. Repeatable.
        /// Allowed: name, folder, groups, note, tags.
        #[arg(long = "in", value_name = "FIELD", num_args = 1..)]
        in_fields: Vec<String>,

        /// Maximum rows to print (default 50).
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Drop matches scoring below this fraction of the top match
        /// (0.0..=1.0). Default 0.3.
        #[arg(long, default_value_t = 0.3)]
        min_score: f32,

        /// Search every vault the caller has list rights on. Slow on
        /// cold cache. Mutually exclusive with vault-resolved context.
        #[arg(long)]
        all_vaults: bool,

        /// Print one name per line, no headers, no ANSI. Pipe-friendly.
        /// Overrides `--format` and disables auto-format-resolution to
        /// JSON when stdout is not a TTY.
        #[arg(long)]
        names_only: bool,
    },
```

- [ ] **Step 2: Verify the rest of the codebase fails to build (expected)**

Run: `cargo build 2>&1 | head -20`
Expected: compile error — the existing `Commands::Find { term, raw }` destructure in `Cli::execute` no longer matches; the existing `execute_secret_find_direct(term, raw, config)` calls reference the old fields.

This is intentional. Task 5 fixes the executor.

- [ ] **Step 3: Commit (broken build is OK at this checkpoint per the plan)**

```bash
git add src/cli/commands.rs
git commit -m "feat(cli): redefine Commands::Find flags for ranked fuzzy search

Replace interactive picker's term/raw fields with pattern/in/limit/
min-score/all-vaults/names-only. Build is intentionally broken at
this commit; Task 5 fixes the executor.
"
```

---

## Task 5: Rewrite `execute_secret_find` for ranked output

**Files:**
- Modify: `src/cli/secret_ops.rs`
- Modify: `src/cli/commands.rs` (call site)

Replace the interactive picker logic with non-interactive ranked output. Cross-vault and `--names-only` come in later tasks; this task handles the single-vault case with default formatting.

- [ ] **Step 1: Update the call-site in `commands.rs::Cli::execute`**

In `src/cli/commands.rs`, find:

```rust
            Commands::Find { term, raw } => {
                crate::cli::secret_ops::execute_secret_find_direct(term, raw, config).await
            }
```

Replace with:

```rust
            Commands::Find {
                pattern,
                in_fields,
                limit,
                min_score,
                all_vaults,
                names_only,
            } => {
                crate::cli::secret_ops::execute_secret_find_direct(
                    pattern,
                    in_fields,
                    limit,
                    min_score,
                    all_vaults,
                    names_only,
                    self.format,
                    config,
                )
                .await
            }
```

- [ ] **Step 2: Update `execute_secret_find_direct` signature**

In `src/cli/secret_ops.rs`, find `execute_secret_find_direct` (around line 971). Replace with:

```rust
pub(crate) async fn execute_secret_find_direct(
    pattern: Option<String>,
    in_fields: Vec<String>,
    limit: usize,
    min_score: f32,
    all_vaults: bool,
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    config: Config,
) -> Result<()> {
    let auth_provider = std::sync::Arc::new(
        DefaultAzureCredentialProvider::with_credential_priority(
            config.azure_credential_priority.clone(),
        )
        .map_err(|e| {
            CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
        })?,
    );
    let secret_manager = SecretManager::new(auth_provider, config.no_color);
    execute_secret_find(
        &secret_manager,
        pattern.as_deref(),
        in_fields,
        limit,
        min_score,
        all_vaults,
        names_only,
        format,
        &config,
    )
    .await
}
```

- [ ] **Step 3: Rewrite `execute_secret_find` body**

Find `execute_secret_find` (around line 989). Replace ENTIRELY with:

```rust
async fn execute_secret_find(
    secret_manager: &crate::secret::manager::SecretManager,
    pattern: Option<&str>,
    in_fields: Vec<String>,
    limit: usize,
    _all_vaults: bool, // wired in Task 8
    names_only: bool,
    format: crate::utils::format::OutputFormat,
    config: &Config,
) -> Result<()> {
    use crate::config::ContextManager;
    use crate::utils::fuzzy::{score_matches, CandidateItem, FuzzyField};

    let vault_name = config.resolve_vault_name(None).await?;

    let mut context_manager = ContextManager::load().await.unwrap_or_default();
    let _ = context_manager.update_usage(&vault_name).await;

    // Parse --in fields. Default: just Name. Always include Name even if
    // user supplied other fields (so a name match still counts).
    let mut fields: Vec<FuzzyField> = vec![FuzzyField::Name];
    for raw in &in_fields {
        let parsed = match raw.to_ascii_lowercase().as_str() {
            "name" => FuzzyField::Name,
            "folder" => FuzzyField::Folder,
            "groups" => FuzzyField::Groups,
            "note" => FuzzyField::Note,
            "tags" => FuzzyField::Tags,
            other => {
                return Err(CrosstacheError::invalid_argument(format!(
                    "unknown --in field: '{other}' (allowed: name, folder, groups, note, tags)"
                )));
            }
        };
        if !fields.contains(&parsed) {
            fields.push(parsed);
        }
    }

    // Fetch secrets (full list — pagination is on the display side).
    let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
    let all_secrets = secret_manager
        .secret_ops()
        .list_secrets(&vault_name, None)
        .await;
    progress.finish_clear();
    let all_secrets = all_secrets?;

    // Adapt to CandidateItems and score.
    let items: Vec<CandidateItem> = all_secrets
        .iter()
        .map(CandidateItem::from_secret_summary)
        .collect();
    let pattern_str = pattern.unwrap_or("");
    let mut matches = score_matches(pattern_str, &items, &fields);

    // Apply min_score (relative to the top score, so 0.3 means 30% of
    // top). Empty pattern → every score is 0; skip filtering.
    if !pattern_str.is_empty() && !matches.is_empty() {
        let top = matches[0].score as f32;
        if top > 0.0 {
            let cutoff = (top * min_score).ceil() as u32;
            matches.retain(|m| m.score >= cutoff);
        }
    }

    // Apply limit.
    matches.truncate(limit);

    // Render.
    if names_only {
        for m in &matches {
            println!("{}", m.item.name);
        }
        return Ok(());
    }

    // Otherwise, defer to format-aware rendering. Task 7 finishes the
    // table/json/yaml branches; for now, table-only minimal output.
    let resolved = format.resolve_for_stdout();
    use crate::utils::format::OutputFormat;
    if matches!(resolved, OutputFormat::Json | OutputFormat::Yaml) {
        let envelope: Vec<serde_json::Value> = matches
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.item.name,
                    "score": m.score,
                    "folder": m.item.folder,
                    "groups": m.item.groups,
                })
            })
            .collect();
        let rendered = match resolved {
            OutputFormat::Json => serde_json::to_string_pretty(&envelope).unwrap_or_default(),
            OutputFormat::Yaml => serde_yaml::to_string(&envelope).unwrap_or_default(),
            _ => unreachable!(),
        };
        println!("{rendered}");
        return Ok(());
    }

    // Plain/table fallback (Task 7 polishes the score-bar column).
    if matches.is_empty() {
        if let Some(p) = pattern {
            output::info(&format!("No secrets match '{p}' in vault '{vault_name}'"));
        } else {
            output::info(&format!("No secrets in vault '{vault_name}'"));
        }
        return Ok(());
    }
    println!("{:<40}  {:<8}  {:<24}  {}", "NAME", "SCORE", "FOLDER", "GROUPS");
    for m in &matches {
        let folder = m.item.folder.as_deref().unwrap_or("");
        let groups = m.item.groups.as_deref().unwrap_or("");
        println!("{:<40}  {:<8}  {:<24}  {}", m.item.name, m.score, folder, groups);
    }
    Ok(())
}
```

> **Note:** The function intentionally takes `_all_vaults: bool` (underscore-prefixed for now) so the signature is stable; Task 8 fills in the cross-vault traversal.

- [ ] **Step 4: Build and run unit tests**

Run: `cargo build`
Expected: clean.

Run: `cargo test --lib`
Expected: all PASS.

- [ ] **Step 5: Manual smoke test**

```bash
# Single vault, default pattern: should list every secret with score 0
xv find 2>&1 | head -10

# Pattern: should rank
xv find db 2>&1 | head -10

# JSON envelope
xv find db --format json 2>&1 | head -20
```

(Assumes a configured vault; the goal is to confirm no panic and reasonable output.)

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/secret_ops.rs
git commit -m "feat(cli): rewrite xv find as non-interactive ranked output

Replace dialoguer FuzzySelect picker with score_matches-driven
ranking. Default field is Name; users can opt in to additional
fields via --in. Output formats: table (default), json/yaml
(envelope), names-only (pipe-friendly). Cross-vault wiring lands
in Task 8.

BREAKING: 'xv find PATTERN' no longer copies the matched secret
to the clipboard. Use 'xv find PATTERN --names-only | fzf | xargs
xv get' or wait for v0.7.0's 'xv pick' interactive picker.
"
```

---

## Task 6: Add `--names-only` to `xv list` (canonical pipe form)

**Files:**
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/secret_ops.rs`

The spec calls out `xv ls --names-only | fzf` as the canonical interactive workflow. This task adds the flag.

- [ ] **Step 1: Add the flag to `Commands::List`**

In `src/cli/commands.rs::Commands::List` (around line 254), the variant currently has fields `group`, `all`, `expiring`, `expired`, `no_cache`. (Note: the in-flight pagination PR #146 adds `page`, `page_size`, `pager` but isn't merged at this plan's baseline; if it lands first, the merge will simply add another field alongside these — order doesn't matter for clap.) Add a new field:

```rust
        /// Print one name per line, no headers, no ANSI. Pipe-friendly.
        /// Overrides --format and disables auto-format-resolution.
        #[arg(long)]
        names_only: bool,
```

Place it adjacent to the other flags.

- [ ] **Step 2: Plumb through to the executor**

In the `Commands::List` arm of `Cli::execute`, add `names_only` to the destructure and pass it to `execute_secret_list_direct`. The current signature is roughly `(group, all, expiring, expired, no_cache, config)` — append `names_only: bool` before `config`. Update the implementation in `src/cli/secret_ops.rs::execute_secret_list_direct` and (downstream) `execute_secret_list` to accept and propagate the flag.

- [ ] **Step 3: Branch in the executor**

In `src/cli/secret_ops.rs::execute_secret_list_direct` (find via `grep -n "execute_secret_list_direct" src/cli/secret_ops.rs`), thread the `names_only` bool down to `execute_secret_list`. In the latter, add an early-exit branch:

```rust
    if names_only {
        for s in &filtered {
            let display = if s.original_name.is_empty() {
                &s.name
            } else {
                &s.original_name
            };
            println!("{display}");
        }
        return Ok(());
    }
```

Place it AFTER any filtering (group, expiring, expired) but BEFORE table rendering — names-only must respect the user's filters but ignore the format/pagination/headers.

- [ ] **Step 4: Run tests and smoke test**

Run: `cargo test --lib`
Expected: all PASS.

```bash
xv ls --names-only 2>&1 | head -5
xv ls --names-only --group backend 2>&1 | head -5
xv ls --names-only | head -3   # confirm no headers, no ANSI
```

- [ ] **Step 5: Commit**

```bash
git add src/cli/commands.rs src/cli/secret_ops.rs
git commit -m "feat(cli): add --names-only to xv ls for pipe-into-fzf

One name per line, no headers, no ANSI. Honors --group / --all /
--expiring / --expired filters. Overrides --format. Canonical
form: 'xv get \"\$(xv ls --names-only | fzf)\"'.
"
```

---

## Task 7: Score-bar column in `xv find` table output

**Files:**
- Modify: `src/cli/secret_ops.rs`
- Modify: `src/utils/fuzzy.rs` (small helper)

Polish the table format for `xv find`: replace the raw integer score column with a unicode bar that's faster to skim.

- [ ] **Step 1: Add `score_bar` helper**

Append to `src/utils/fuzzy.rs`:

```rust
/// Render a 10-cell unicode bar from a relative-score fraction in 0.0..=1.0.
/// Uses block characters; deterministic; no ANSI.
pub fn score_bar(fraction: f32) -> String {
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = (fraction * 10.0).round() as usize;
    let mut s = String::with_capacity(10);
    for i in 0..10 {
        s.push(if i < filled { '█' } else { '░' });
    }
    s
}

#[cfg(test)]
mod score_bar_tests {
    use super::*;

    #[test]
    fn full_bar() {
        assert_eq!(score_bar(1.0), "██████████");
    }
    #[test]
    fn empty_bar() {
        assert_eq!(score_bar(0.0), "░░░░░░░░░░");
    }
    #[test]
    fn half_bar() {
        // 0.5 * 10 = 5 → 5 filled
        assert_eq!(score_bar(0.5), "█████░░░░░");
    }
    #[test]
    fn clamps_above_one() {
        assert_eq!(score_bar(1.5), "██████████");
    }
    #[test]
    fn clamps_below_zero() {
        assert_eq!(score_bar(-0.5), "░░░░░░░░░░");
    }
}
```

- [ ] **Step 2: Use in the table render**

In `execute_secret_find` (Task 5), replace the table-rendering block:

```rust
    println!("{:<40}  {:<8}  {:<24}  {}", "NAME", "SCORE", "FOLDER", "GROUPS");
    for m in &matches {
        let folder = m.item.folder.as_deref().unwrap_or("");
        let groups = m.item.groups.as_deref().unwrap_or("");
        println!("{:<40}  {:<8}  {:<24}  {}", m.item.name, m.score, folder, groups);
    }
```

with:

```rust
    use crate::utils::fuzzy::score_bar;
    let top = matches.iter().map(|m| m.score).max().unwrap_or(1).max(1) as f32;
    println!("{:<40}  {:<10}  {:<24}  {}", "NAME", "SCORE", "FOLDER", "GROUPS");
    for m in &matches {
        let folder = m.item.folder.as_deref().unwrap_or("");
        let groups = m.item.groups.as_deref().unwrap_or("");
        let bar = score_bar(m.score as f32 / top);
        println!("{:<40}  {bar}  {:<24}  {}", m.item.name, folder, groups);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test --lib utils::fuzzy`
Expected: 11 tests PASS (6 prior + 5 new score_bar tests).

Run: `cargo build`
Expected: clean.

- [ ] **Step 4: Smoke test**

```bash
xv find db 2>&1 | head -10
```

Expected: a NAME / SCORE / FOLDER / GROUPS table where SCORE is a 10-cell unicode bar.

- [ ] **Step 5: Commit**

```bash
git add src/utils/fuzzy.rs src/cli/secret_ops.rs
git commit -m "feat(cli): score-bar column in xv find table output

Replaces raw integer score with a 10-cell unicode bar (relative to
the top score). Bar is deterministic and ANSI-free; safe for
copy-paste and screenshots. Clamps to [0,1].
"
```

---

## Task 8: Cross-vault search (`--all-vaults`)

**Files:**
- Modify: `src/cli/secret_ops.rs`

When `--all-vaults` is set, iterate every vault the caller has list rights on, score each vault's secrets independently, then merge and re-rank.

- [ ] **Step 1: Replace the `_all_vaults` placeholder with real logic**

In `execute_secret_find` (the one rewritten in Task 5), update the `_all_vaults` parameter to `all_vaults` (drop the underscore) and add a branch above the single-vault `secret_manager.secret_ops().list_secrets(...)` call:

```rust
    use crate::vault::manager::VaultManager;

    let items: Vec<CandidateItem> = if all_vaults {
        // Reach the vault list. Build a VaultManager from the same auth
        // provider already in scope (or via config — match Task 5's
        // construction).
        let auth_provider = std::sync::Arc::new(
            DefaultAzureCredentialProvider::with_credential_priority(
                config.azure_credential_priority.clone(),
            )
            .map_err(|e| {
                CrosstacheError::authentication(format!("Failed to create auth provider: {e}"))
            })?,
        );
        let vault_manager = VaultManager::new(
            auth_provider,
            config.subscription_id.clone(),
            config.no_color,
        )?;

        let vaults = vault_manager
            .vault_ops()
            .list_vaults(Some(&config.subscription_id), None)
            .await?;

        let progress = crate::utils::interactive::ProgressIndicator::new(&format!(
            "Searching {} vaults...",
            vaults.len()
        ));
        let mut combined: Vec<CandidateItem> = Vec::new();
        for v in &vaults {
            // Per-vault list — failures here are non-fatal; log + skip.
            match secret_manager
                .secret_ops()
                .list_secrets(&v.name, None)
                .await
            {
                Ok(secrets) => {
                    for s in &secrets {
                        let mut item = CandidateItem::from_secret_summary(s);
                        // Prefix the vault name into the displayed name so
                        // results are unambiguous: e.g. "myvault/SECRET".
                        item.name = format!("{}/{}", v.name, item.name);
                        combined.push(item);
                    }
                }
                Err(e) => {
                    tracing::debug!("list_secrets failed for vault {}: {e}", v.name);
                }
            }
        }
        progress.finish_clear();
        combined
    } else {
        // Single-vault path (existing logic from Task 5).
        let progress = crate::utils::interactive::ProgressIndicator::new("Loading secrets...");
        let all_secrets = secret_manager
            .secret_ops()
            .list_secrets(&vault_name, None)
            .await;
        progress.finish_clear();
        let all_secrets = all_secrets?;
        all_secrets
            .iter()
            .map(CandidateItem::from_secret_summary)
            .collect()
    };
```

This block REPLACES the existing single-path `let items: Vec<CandidateItem> = ...` from Task 5. Remove the now-unused single-path code.

- [ ] **Step 2: Build and test**

Run: `cargo build`
Expected: clean — `all_vaults` is now consumed.

Run: `cargo test --lib`
Expected: all PASS.

- [ ] **Step 3: Smoke test**

```bash
xv find db --all-vaults 2>&1 | head -10
```

Expected: progress indicator while loading; results include `vaultname/SECRET` style names. If the user has list rights on multiple vaults, expect entries from each.

- [ ] **Step 4: Commit**

```bash
git add src/cli/secret_ops.rs
git commit -m "feat(cli): implement xv find --all-vaults cross-vault search

Iterates every vault the caller has list rights on, prefixes
results with 'vaultname/SECRET' so the row is unambiguous. Per-
vault list failures degrade silently (debug log) so one missing
permission doesn't break the whole query.
"
```

---

## Task 9: Integration test — `xv find` exit codes & shape

**Files:**
- Modify: `tests/error_codes_tests.rs` (extend with find-specific tests)

Lock in the contract: bad `--in` value exits 2 (invalid argument); empty result is exit 0; JSON envelope shape matches spec.

- [ ] **Step 1: Append integration tests**

Append to `tests/error_codes_tests.rs`:

```rust
#[test]
fn find_unknown_in_field_exits_2() {
    let out = xv()
        .args(["find", "anything", "--in", "bogus_field"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn find_json_envelope_is_array_of_records() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    let out = xv()
        .args(["find", "db", "--vault", &vault, "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ok exit when vault reachable");
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be JSON");
    assert!(body.is_array(), "envelope is a top-level array");
    if let Some(first) = body.as_array().and_then(|a| a.first()) {
        assert!(first.get("name").is_some());
        assert!(first.get("score").is_some());
    }
}

#[test]
#[ignore = "requires XV_TEST_VAULT and credentials"]
fn ls_names_only_no_headers_no_ansi() {
    let vault = std::env::var("XV_TEST_VAULT").expect("XV_TEST_VAULT must be set");
    let out = xv()
        .args(["ls", "--names-only", "--vault", &vault])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // No ANSI escapes
    assert!(!stdout.contains('\x1b'), "names-only must be ANSI-free");
    // No "Name" header
    assert!(!stdout.lines().any(|l| l.trim() == "Name"));
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test --test error_codes_tests`
Expected: `find_unknown_in_field_exits_2` PASSES; the two `#[ignore]`'d tests are skipped.

Note: clap will return an error message about the unknown field via the explicit `CrosstacheError::invalid_argument` from Task 5, which exits 2 via the existing `exit_code()` mapping. If clap rejects it earlier (during arg parsing), the exit is also 2. Either way, the test passes.

- [ ] **Step 3: Commit**

```bash
git add tests/error_codes_tests.rs
git commit -m "test: integration tests for xv find exit codes and JSON shape

Active: bad --in field exits 2. Ignored (need XV_TEST_VAULT):
JSON envelope shape and 'ls --names-only' ANSI-freeness.
"
```

---

## Task 10: Stretch — dynamic shell completion (`xv __complete-secrets`)

**Files:**
- Modify: `src/cli/commands.rs`
- Modify: `src/cli/secret_ops.rs`

Per spec §3.3.3 this is **stretch**: drop if Tasks 1-9 took longer than expected. Adds a hidden subcommand that emits cached secret names one per line, designed to be called by generated bash/zsh/fish completion scripts.

- [ ] **Step 1: Add the hidden subcommand**

In `src/cli/commands.rs::Commands`, append (with `hide = true` so it doesn't show in help):

```rust
    /// (internal) Emit cached secret names for shell completion
    #[command(hide = true, name = "__complete-secrets")]
    CompleteSecrets,
```

- [ ] **Step 2: Wire the executor**

In `Cli::execute`, add a new arm that delegates to a small executor in `secret_ops.rs`:

```rust
            Commands::CompleteSecrets => {
                crate::cli::secret_ops::execute_complete_secrets(config).await
            }
```

- [ ] **Step 3: Implement `execute_complete_secrets`**

In `src/cli/secret_ops.rs`:

```rust
pub(crate) async fn execute_complete_secrets(config: Config) -> Result<()> {
    use crate::cache::{CacheKey, CacheManager};
    let vault_name = config.resolve_vault_name(None).await?;

    // Cache-only path. If cache is cold, exit silently — the user got
    // no completions, which is the right UX for a Tab press (no Azure
    // round-trip on every keystroke).
    let cache_manager = CacheManager::from_config(&config);
    if !cache_manager.is_enabled() {
        return Ok(());
    }
    let cache_key = CacheKey::SecretsList { vault_name: vault_name.clone() };
    if let Some(cached) = cache_manager.get::<Vec<crate::secret::manager::SecretSummary>>(&cache_key) {
        for s in &cached {
            let display = if s.original_name.is_empty() { &s.name } else { &s.original_name };
            println!("{display}");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: clean.

- [ ] **Step 5: Smoke test**

```bash
xv __complete-secrets 2>&1 | head -5
```

Expected: empty (cache cold) or one-name-per-line.

After running `xv ls` once to populate the cache:

```bash
xv ls > /dev/null && xv __complete-secrets | head -5
```

Expected: secret names from cache.

- [ ] **Step 6: Commit**

```bash
git add src/cli/commands.rs src/cli/secret_ops.rs
git commit -m "feat(cli): add hidden 'xv __complete-secrets' for shell tab-complete

Cache-only; emits one name per line. Designed to be called by
generated bash/zsh/fish completion scripts on Tab. Never touches
Azure on a key press — empty output on cold cache is the correct
UX. Hidden from --help.
"
```

> **If skipping this task:** record it in the v0.7.x backlog and skip directly to Task 11.

---

## Task 11: Docs — `docs/find.md`, README link, soft-commitment checklist

**Files:**
- Create: `docs/find.md`
- Modify: `README.md`
- Modify (or create): `docs/superpowers/specs/backend-trait-checklist.md`

- [ ] **Step 1: Create `docs/find.md`**

```markdown
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
```

- [ ] **Step 2: Add the README link**

Open `README.md`. Find the "Env profiles" subsection (added in Plan #2). Add a new subsection right after it:

```markdown
## Fuzzy search

For ranked search across secret names (and optionally folders, groups,
notes, tags), use `xv find <pattern>`. See [`docs/find.md`](docs/find.md)
for the full reference.

Pipe-into-fzf canonical form:

```bash
xv get "$(xv ls --names-only | fzf)"
```
```

- [ ] **Step 3: Append to backend-trait checklist**

Open `docs/superpowers/specs/backend-trait-checklist.md` (create if absent). Append:

```markdown
## v0.6.1 — `xv find`

- `SecretManager::list_secrets(vault_name, group_filter)` — used for the
  single-vault find path. Cacheable; current call ignores `group_filter`
  (always None).
- `VaultManager::vault_ops().list_vaults(subscription_id, resource_group)`
  — used by `--all-vaults`. Per-call; no cache.

These are read-only and align with the soft-commitment goal of keeping
the read surface small and well-known before phase 2.
```

- [ ] **Step 4: Commit**

```bash
git add docs/find.md README.md docs/superpowers/specs/backend-trait-checklist.md
git commit -m "docs: xv find reference + README link + trait-checklist entries

User-facing reference for the new ranked-search command, including
the breaking-change migration table from the old interactive picker.
README pointer alongside the existing scripting/env-profiles refs.
Soft-commitment checklist gets two new read-surface entries for
phase-2 trait planning.
"
```

---

## Task 12: Cut v0.6.1-rc.1

**Files:**
- Modify: `Cargo.toml` (version bump)
- Tag: git tag

- [ ] **Step 1: Run the full quality gate**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -W clippy::all 2>&1 | grep -E "warning:|error:" | head
cargo test
cargo test -- --test-threads=1
```

All must pass cleanly. If `cargo fmt --all -- --check` reports drift in files this branch did NOT modify, **leave them alone** (a separate fmt sweep can land on main if needed). If it reports drift in files we DID modify, run `cargo fmt --all` and inspect the diff.

If any quality gate fails, STOP and report BLOCKED.

- [ ] **Step 2: Bump the version**

In `Cargo.toml`:

```toml
version = "0.6.1-rc.1"
```

(Replace whatever current value `Cargo.toml` has — likely `0.6.0-rc.2` or `0.6.0`. Verify before bumping.)

Run: `cargo build` to refresh `Cargo.lock`.

- [ ] **Step 3: Commit & tag**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 0.6.1-rc.1"
git tag -a v0.6.1-rc.1 -m "v0.6.1-rc.1: ranked fuzzy xv find + xv ls --names-only"
```

- [ ] **Step 4: STOP — do NOT push**

Do NOT call `git push`. Do NOT call `gh pr create`. The plan explicitly stops at the local tag.

---

## Verification checklist (final, before declaring plan complete)

- [ ] `cargo test` — all green, including the active find integration test
- [ ] `cargo test -- --test-threads=1` — all green
- [ ] `cargo clippy --all-targets -- -W clippy::all` — no NEW warnings against `0.6.0` baseline
- [ ] `cargo fmt --all -- --check` — clean for branch-touched files
- [ ] Manual: `xv find db` ranks secrets; bar column renders with block characters
- [ ] Manual: `xv find` with no pattern lists every secret with empty bar (score 0 across the board; the score-bar normalizer `top.max(1)` keeps it from dividing by zero, and 0/1 = empty bar — that's the right "no ranking applied" signal)
- [ ] Manual: `xv find bogus_pattern_xxx` exits 0 with empty output
- [ ] Manual: `xv find x --in bogus` exits 2 with `xv-invalid-argument` error
- [ ] Manual: `xv find db --in folder` finds matches whose folder contains "db"
- [ ] Manual: `xv find db --names-only | fzf` works on a TTY
- [ ] Manual: `xv find db --names-only | head -3` works (no headers, no ANSI)
- [ ] Manual: `xv find db --format json | jq '.[0].name'` returns a string
- [ ] Manual: `xv ls --names-only` works with all existing filters (`--group`, `--all`, `--expiring`, `--expired`)
- [ ] Manual: `xv find db --all-vaults` shows `vaultname/SECRET` rows from multiple vaults (if access)
- [ ] Soft-commitment-checklist updated: `SecretManager::list_secrets` and `VaultManager::list_vaults` listed in `docs/superpowers/specs/backend-trait-checklist.md`

---

## Notes for the executing engineer

- **TDD discipline.** Each task starts with a failing test where reasonable. Some tasks (like Task 4 — flag schema) intentionally break the build at commit time; the next task fixes it. This is a deliberate small-step approach.
- **Commit per task.** Don't bundle. The trail matters for review and bisect.
- **Breaking change.** `xv find <pattern>` no longer copies to clipboard. The Task 5 commit message and `docs/find.md` migration section both call this out; release notes should too.
- **Stretch task.** Task 10 (`xv __complete-secrets`) ships if Tasks 1-9 land cleanly with time remaining; otherwise it gets recorded in the v0.7.x backlog and we ship rc.1 without it.
- **Read methods used (for the phase-2 trait checklist):**
  - `SecretManager::secret_ops().list_secrets()` (Task 5, Task 8)
  - `VaultManager::vault_ops().list_vaults()` (Task 8)
- **Coexistence with `xv find`'s old behavior.** If users want the old interactive picker behavior, the canonical replacement is `xv get "$(xv find <pattern> --names-only | fzf)"`. The `dialoguer` crate is no longer needed for `xv find`; do not remove it from `Cargo.toml` since other commands may still use it (verify via grep before any cleanup).
