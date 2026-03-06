# README.md Accuracy Audit & Polish — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bring README.md fully up to date with the actual CLI, add missing command docs, and polish presentation.

**Architecture:** Edit the existing README.md in place, working section by section top-to-bottom. Each task targets one logical section. Verify each edit by re-reading the changed section before committing.

**Tech Stack:** Markdown only. Verification via `cargo run -- help <command>` for accuracy checks.

---

### Task 1: Create branch and fix Secrets section

**Files:**
- Modify: `README.md:70-87`

**Step 1: Create a new branch from main**

```bash
git checkout main
git checkout -b docs/readme-audit
```

**Step 2: Fix the Secrets code block**

Replace lines 74-87 with corrected content:
- Remove the non-existent `--group` usage implied in `set` (there is none to remove in this block, but the `set` examples are fine as-is)
- Add `find`/`search` after `get`
- Fix `delete` to show `--force` flag and `rm` alias
- Add `--force` to `purge`

The corrected Secrets block should be:

```bash
xv set "api-key"                          # Create (prompts for value)
xv set "api-key" --stdin < key.txt        # Create from stdin
xv set K1=val1 K2=val2 K3=@file.pem      # Bulk create
xv get "api-key"                          # Copy to clipboard (auto-clears)
xv get "api-key" --raw                    # Print to stdout
xv find                                   # Browse all secrets interactively
xv find "api"                             # Fuzzy search by name pattern
xv list                                   # List all secrets (alias: ls)
xv list --group production                # Filter by group
xv list --expiring 30d                    # Show secrets expiring soon
xv update "api-key" --group prod --note "Frontend key"
xv delete "api-key"                       # Soft-delete (alias: rm)
xv delete --group legacy --force          # Bulk delete by group
xv restore "api-key"                      # Restore soft-deleted
xv purge "api-key" --force                # Permanently delete
```

**Step 3: Verify the edit**

Read the modified section and compare against `cargo run -- help set`, `cargo run -- help get`, `cargo run -- help find`, `cargo run -- help list`, `cargo run -- help delete`, `cargo run -- help restore`, `cargo run -- help purge`.

**Step 4: Commit**

```bash
git add README.md
git commit -m "docs: fix secrets section — add find command, fix delete/purge flags"
```

---

### Task 2: Fix Secret History & Rotation section

**Files:**
- Modify: `README.md:132-140`

**Step 1: Replace the Secret History & Rotation code block**

The corrected block should be:

```bash
xv history "api-key"                      # Version history
xv rollback "api-key" --version 2         # Restore previous version (--version required)
xv rotate "api-key"                       # Generate new random value (32 chars)
xv rotate "api-key" --length 64 --charset alphanumeric
xv rotate "api-key" --charset hex         # Also: base64, numeric, uppercase, lowercase
xv rotate "api-key" --generator ./gen.sh  # Custom generator script
xv rotate "api-key" --show-value          # Display the generated value
```

**Step 2: Verify**

Compare against `cargo run -- help history`, `cargo run -- help rollback`, `cargo run -- help rotate`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: fix history/rotation section — correct rollback syntax, expand rotate options"
```

---

### Task 3: Fix Vault Management section

**Files:**
- Modify: `README.md:142-151`

**Step 1: Expand the Vault Management code block**

Add the missing subcommands (`restore`, `purge`, `update`, `share`):

```bash
xv vault create my-vault --resource-group my-rg --location eastus
xv vault list
xv vault info my-vault
xv vault delete my-vault
xv vault restore my-vault                 # Restore soft-deleted vault
xv vault purge my-vault                   # Permanently delete vault
xv vault update my-vault                  # Update vault properties
xv vault export my-vault --output secrets.json
xv vault import my-vault --input secrets.json --dry-run
xv vault share grant my-vault             # Vault-level access management
```

**Step 2: Verify**

Compare against `cargo run -- help vault`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add missing vault subcommands — restore, purge, update, share"
```

---

### Task 4: Fix Vault Context section

**Files:**
- Modify: `README.md:153-162`

**Step 1: Add `context clear` to the code block**

```bash
xv context use my-vault         # Switch active vault
xv cx use my-vault              # Alias
xv context show                 # Current context
xv context list                 # Recent contexts
xv context clear                # Clear current context
```

**Step 2: Verify**

Compare against `cargo run -- help context`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add context clear subcommand"
```

---

### Task 5: Fix Cross-Vault Operations section

**Files:**
- Modify: `README.md:175-180`

**Step 1: Add `diff` and expand `copy`/`move` with `--new-name`**

```bash
xv diff vault-a vault-b                   # Compare secrets between vaults
xv diff vault-a vault-b --show-values     # Include values in comparison
xv copy "api-key" --from vault-a --to vault-b
xv copy "api-key" --from vault-a --to vault-b --new-name "api-key-v2"
xv move "api-key" --from vault-a --to vault-b
```

**Step 2: Verify**

Compare against `cargo run -- help diff`, `cargo run -- help copy`, `cargo run -- help move`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add diff command and --new-name flag to cross-vault ops"
```

---

### Task 6: Fix Identity & Auditing section

**Files:**
- Modify: `README.md:195-201`

**Step 1: Expand audit options and add info command**

```bash
xv whoami                                 # Show authenticated identity
xv info my-vault                          # Resource info (vault, secret, or file)
xv audit "api-key"                        # Access/change history
xv audit --vault my-vault                 # Vault-wide activity
xv audit --vault my-vault --days 7        # Last 7 days only
xv audit "api-key" --operation get        # Filter by operation type
```

**Step 2: Verify**

Compare against `cargo run -- help whoami`, `cargo run -- help info`, `cargo run -- help audit`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: expand audit options, add info command"
```

---

### Task 7: Fix Configuration section

**Files:**
- Modify: `README.md:212-218`

**Step 1: Add `config path` to the Setup block**

```bash
xv init                                   # Interactive setup
xv config show                            # View current config
xv config set default_vault my-vault      # Set a value
xv config path                            # Show config file location
```

**Step 2: Verify**

Compare against `cargo run -- help config`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add config path subcommand"
```

---

### Task 8: Fix Output Formats section

**Files:**
- Modify: `README.md:254-261`

**Step 1: Expand Output Formats to show all formats and global nature**

Replace the section with:

```markdown
## Output Formats

Most commands support a global `--format` flag:

\```bash
xv list                         # Table (default)
xv list --format json           # JSON
xv list --format yaml           # YAML
xv list --format csv            # CSV
xv list --format plain          # Simple text
xv list --columns name,groups   # Select specific columns
xv get "key" --raw              # Raw value (for scripting)
\```

Available formats: `table`, `json`, `yaml`, `csv`, `plain`, `raw`, `template`.
```

**Step 2: Verify**

Compare against `cargo run -- --help --show-options` for global flags.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: expand output formats — add csv, plain, template, columns"
```

---

### Task 9: Add Utilities section

**Files:**
- Modify: `README.md` — insert new section before "Configuration" (before line 203)

**Step 1: Add new Utilities section**

Insert after the Identity & Auditing section:

```markdown
### Utilities

```bash
xv parse "Server=db;User=admin;Pass=secret"   # Parse connection strings
xv version                                     # Detailed build info
xv completion bash                             # Generate shell completions
xv completion zsh > ~/.zfunc/_xv               # Install zsh completions
```
```

**Step 2: Verify**

Compare against `cargo run -- help parse`, `cargo run -- help version`, `cargo run -- help completion`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add utilities section — parse, version, completion commands"
```

---

### Task 10: Add Global Options section

**Files:**
- Modify: `README.md` — insert new section after "Output Formats"

**Step 1: Add Global Options section**

Insert after the Output Formats section:

```markdown
## Global Options

These flags work with any command:

| Flag | Purpose |
|------|---------|
| `--format <FORMAT>` | Output format (`table`, `json`, `yaml`, `csv`, `plain`, `raw`, `template`) |
| `--columns <COLS>` | Select specific columns for table output (comma-separated) |
| `--credential-type <TYPE>` | Azure credential type (`cli`, `managed_identity`, `environment`, `default`) |
| `--debug` | Enable debug logging |
```

**Step 2: Verify**

Compare against `cargo run -- --help --show-options`.

**Step 3: Commit**

```bash
git add README.md
git commit -m "docs: add global options reference table"
```

---

### Task 11: Final review and polish pass

**Files:**
- Modify: `README.md`

**Step 1: Read the full README top to bottom**

Check for:
- Consistent formatting (alignment in code blocks, spacing)
- Grammar and typos
- Logical flow between sections
- Any remaining inaccuracies

**Step 2: Make any final polish edits**

Fix any issues found in the review pass.

**Step 3: Final commit**

```bash
git add README.md
git commit -m "docs: final polish pass on README"
```
