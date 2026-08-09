# Remove Azure CLI Installer Dependency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the generic crosstache installers from checking for or installing Azure CLI, because `xv` supports local, AWS, environment-credential, managed-identity, and OIDC operation without it.

**Architecture:** Treat Azure CLI as an optional Azure credential and interactive-provisioning tool, not an installation prerequisite. Exercise the Unix installer end to end with a hermetic fake release and an `az` spy, then make both Unix and PowerShell installers backend-neutral while preserving their existing download, signature, checksum, extraction, and PATH behavior.

**Tech Stack:** Rust integration tests, Bash, PowerShell, Markdown.

## Global Constraints

- Preserve Azure CLI authentication (`--credential-type cli`) as an optional supported mode.
- Preserve the separate `xfunction` provisioning installer, whose design intentionally orchestrates Azure CLI commands.
- Do not alter the user's existing `package-lock.json` modification.
- Installer downloads must remain signature- and checksum-verified.

---

### Task 1: Prove the generic Unix installer has no Azure CLI prerequisite

**Files:**
- Create: `tests/installer_tests.rs`

**Interfaces:**
- Consumes: `scripts/install.sh`, `XDG_BIN_HOME`, `PATH`, and the installer's existing release-download contract.
- Produces: `unix_installer_does_not_invoke_azure_cli`, an end-to-end regression test that fails if the installer executes `az`.

- [ ] **Step 1: Write the failing test**

Create a Unix-only Rust integration test that builds a tiny signed-release fixture, places fake `curl`, `minisign`, and `az` commands first on `PATH`, runs `bash scripts/install.sh v-test`, and asserts that the fake binary was installed while the `az` spy marker was not created. The fake `az` returns a valid `az version` response so the current installer completes instead of prompting; the marker is the observable regression.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test installer_tests unix_installer_does_not_invoke_azure_cli -- --exact --nocapture`

Expected: FAIL because the current installer calls the fake `az version` command and creates the marker.

- [ ] **Step 3: Keep the test fixture behavioral**

The test must execute the installer and assert process effects. It must not inspect installer source text for removed function names or messages.

### Task 2: Remove the mandatory Azure CLI work from both installers

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.ps1`

**Interfaces:**
- Consumes: the existing platform detection and verified release installation flows.
- Produces: non-interactive generic installation that never detects, prompts for, or installs Azure CLI.

- [ ] **Step 1: Remove the Unix Azure CLI prerequisite**

Delete `check_azure_cli`, `install_azure_cli`, and the Azure CLI check/prompt block in `main`. Change `show_usage` to recommend `xv init` and explain that backend-specific authentication is configured afterward.

- [ ] **Step 2: Run the Unix regression test**

Run: `cargo test --test installer_tests unix_installer_does_not_invoke_azure_cli -- --exact --nocapture`

Expected: PASS; the release fixture installs and the `az` marker remains absent.

- [ ] **Step 3: Remove the PowerShell Azure CLI prerequisite symmetrically**

Delete `Test-AzureCLI`, `Install-AzureCLI`, and the Azure CLI check/prompt block in `Install-crosstache`. Change `Show-Usage` to the same backend-neutral next step as the Unix installer.

- [ ] **Step 4: Validate installer syntax**

Run: `bash -n scripts/install.sh`

If `pwsh` is available, run: `pwsh -NoProfile -Command '$errors = $null; [void][System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path "scripts/install.ps1"), [ref]$null, [ref]$errors); if ($errors.Count) { $errors | ForEach-Object { Write-Error $_ }; exit 1 }'`

Expected: both scripts parse without errors.

### Task 3: Document the validated dependency boundary

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: the authentication modes already documented in the Authentication section.
- Produces: accurate installation guidance distinguishing optional Azure CLI auth from actual prerequisites.

- [ ] **Step 1: Clarify installation requirements**

Add a short note after Quick install: Azure CLI is not required to install or use crosstache with local, AWS, environment credentials, managed identity, or OIDC. It remains optional for `--credential-type cli` and for Azure discovery/provisioning in the interactive setup flow.

- [ ] **Step 2: Verify documentation and source references are coherent**

Run: `rg -n -S 'requires Azure CLI|Azure CLI \(az\) must be installed|crosstache will not work properly without Azure CLI' scripts README.md`

Expected: no false universal-prerequisite claims remain in the generic installers or README.

### Task 4: Full verification

**Files:**
- Verify only.

**Interfaces:**
- Consumes: all changes from Tasks 1-3.
- Produces: evidence that the installer dependency was removed without regressing the Rust project.

- [ ] **Step 1: Format and lint**

Run: `cargo fmt -- --check`

Run: `cargo clippy -- -D warnings`

Expected: both pass.

- [ ] **Step 2: Run the test suite**

Run: `cargo test`

Expected: PASS.

- [ ] **Step 3: Inspect the final diff and working tree**

Run: `git diff --check`

Run: `git status --short`

Expected: only the installer dependency change, its behavioral test, README clarification, and this plan are new; the pre-existing `package-lock.json` modification remains untouched.
