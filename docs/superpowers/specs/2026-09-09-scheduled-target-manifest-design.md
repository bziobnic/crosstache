# Scheduled rotation target manifest

> **Status:** Shipped — PR #444 (schema/resolver), PR #445
> (runner/drift/transaction), PR 3 (outcomes/status/native gates).
> Deviations from this document are recorded in `CHANGELOG.md` and in those
> pull requests.
> **Tasks:** A03-01 through A03-10, with REL02, REL03, REL04, REL05 and REL07 as delivery gates.
> **Golden outputs:** [`2026-09-09-scheduled-target-manifest-goldens.md`](2026-09-09-scheduled-target-manifest-goldens.md).

## Problem

`xv schedule install` currently writes a native scheduler entry that runs
`xv rotate --due --force`, optionally with `--vault`. It carries `HOME` and
`XDG_CONFIG_HOME`, but it does not persist the backend registry instance,
workspace alias, resolved real vault, project environment or installation
working directory. A later change to context, `.xv.toml`, environment variables
or config can therefore make an unattended run target a different account,
provider or vault. `xv schedule status` also reports the current config default
instead of the target encoded by the installed job.

The schedule must become a fail-closed replay of one resolved target. Before a
scheduled process mutates a secret, it must prove that the target still has the
identity approved at installation. Status must explain that target and the last
run without exposing credentials or secret data.

## Scope and invariants

This work keeps one per-user rotation schedule with the existing cadence and
fixed native scheduler identifiers. It does not introduce multiple named jobs,
a daemon, remote scheduling, or a generic workflow engine.

The following invariants are load-bearing:

1. Installation resolves one exact `(config, project context, workspace entry,
   backend instance, real vault)` tuple. An unresolved or ambiguous tuple is an
   error even when the current command would otherwise fall back at run time.
2. Native units contain an absolute executable and an absolute manifest path.
   They do not contain a vault name, credentials, tokens, secret names, secret
   values, backend settings or project settings.
3. The scheduled runner reads the recorded config file directly. Ambient
   `XV_BACKEND`, `XV_ENV`, context changes and its launch directory cannot select
   the mutation target.
4. Target drift is checked before backend construction or rotation. A drifted,
   missing, malformed or unsupported manifest refuses the sweep.
5. The manifest and result files are bounded, owner-private, no-follow and
   atomically replaced. They never store credential material or secret data.
6. A reinstall is the only operation that accepts a changed target. `status`
   diagnoses drift; it never repairs it. `--force` skips the human confirmation
   for installation and does not bypass drift checks.
7. `--print` is a read-only preview. Its manifest and unit examples are the
   exact values installation would write, apart from explicitly marked
   timestamps. It creates no directory, manifest, result, lock or native unit.

## Resolved design decisions

| Question | Decision |
|---|---|
| Where is target state stored? | In the per-user state directory, one `rotation-default` directory; scheduler units carry only its absolute manifest path. |
| Is the existing cache fingerprint sufficient? | No. Add a schedule-specific digest for the one selected registry backend instance. |
| Can installation leave the target to runtime defaults? | No. It must resolve one backend instance and real vault, and it requires a saved readable global config file. |
| How does `--vault` behave in a workspace? | It is an attached alias when a configured workspace exists; it is a raw vault only in the degenerate single-backend case. |
| Does `--print` write a candidate manifest? | No. It prints deterministic JSON with a timestamp placeholder and leaves the filesystem unchanged. |
| What happens on drift? | Run refuses before backend construction; only explicit reinstall accepts a changed target. |
| What happens after an in-place xv upgrade? | The same executable path with a new version is allowed with a warning; reinstall is recommended. A changed or missing path refuses. |
| Are legacy jobs auto-migrated? | No. Status labels them `legacy-unpinned`; explicit reinstall replaces them transactionally. |
| What does uninstall retain? | Last outcome, both lock inodes, log and incomplete rollback evidence; it removes recognized native artifacts and the active manifest. |
| How is next run calculated? | It is parsed from the native scheduler when available; xv never guesses it from cadence. |

## Files and ownership

The schedule owns a directory under the user's state directory:

| Platform | State root |
|---|---|
| Linux/macOS with `XDG_STATE_HOME` | `$XDG_STATE_HOME/xv/schedules/rotation-default/` |
| Linux/macOS fallback | `$HOME/.local/state/xv/schedules/rotation-default/` |
| Windows | `%LOCALAPPDATA%\\xv\\schedules\\rotation-default\\` |
| Test override | `XV_STATE_HOME/xv/schedules/rotation-default/` |

`XV_STATE_HOME` is a test and embedding override honored on every platform. It
must be documented as an internal override, not a normal target-selection input.
An empty override is ignored. On Windows, `dirs::data_local_dir()` supplies the
fallback and failure to determine it is an actionable configuration error.

The directory contains:

| File | Owner | Lifetime |
|---|---|---|
| `manifest.json` | install/reinstall | Removed by uninstall |
| `last-run.json` | scheduled runner | Retained by uninstall and reinstall |
| `run.lock` | scheduled runner | Persistent lock inode; retained |
| `install.lock` | install/reinstall/uninstall | Persistent lock inode; retained |
| `recovery/` | install rollback | Created only when rollback is incomplete; retained |

The native unit files and Task Scheduler task remain the files/entry already
owned by `xv`: `com.crosstache.xv-rotate.plist`, `xv-rotate.service`,
`xv-rotate.timer`, and `crosstache-xv-rotate`. Uninstall may remove only these
fixed owned artifacts and `manifest.json`. It retains `last-run.json`,
both lock files, `recovery/`, `rotate.log`, unrelated files in the state directory, and any
similarly named user jobs. If the schedule directory becomes empty, it may be
removed; otherwise it remains.

`create_private_dir`, `atomic_write_file_no_follow(..., true)` and
`write_private_file_no_follow_create_new` from `src/utils/helpers.rs` are the
required storage primitives. Reads reject a symlink in any existing path
component and cap each JSON file at 64 KiB before parsing. Unix directories are
`0700` and files are `0600`; Windows uses the helpers' owner-and-SYSTEM DACL.

## Manifest schema

`manifest.json` uses a tagged, versioned JSON schema. Deserialization denies
unknown fields. Version 1 is:

```json
{
  "schema_version": 1,
  "schedule_id": "rotation-default",
  "installed_at": "2026-09-09T15:04:05Z",
  "cadence": { "kind": "daily", "hour": 3, "minute": 0 },
  "execution": {
    "binary_path": "/opt/homebrew/bin/xv",
    "installed_version": "0.39.0",
    "working_directory": "/Users/alice/work/service",
    "log_path": "/Users/alice/.local/state/xv/rotate.log"
  },
  "target": {
    "config_path": "/Users/alice/.config/xv/xv.conf",
    "config_digest": "sha256:9c1e...",
    "project_path": "/Users/alice/work/service/.xv.toml",
    "project_digest": "sha256:83ad...",
    "environment": "production",
    "context_path": null,
    "context_digest": null,
    "workspace_source": "project",
    "workspace_alias": "payments",
    "backend_name": "aws-prod",
    "backend_kind": "aws",
    "backend_identity": "sha256:55db...",
    "vault": "payments-production",
    "vault_selection": "explicit"
  }
}
```

`project_path`, `project_digest`, `environment`, `context_path`,
`context_digest` and `workspace_alias` are null when the corresponding
resolution layer did not participate. `workspace_source` is `project`,
`context`, or `degenerate`; a degenerate target may still carry a context
path/digest when its current vault came from context. `vault_selection` is
`explicit` when install was given `--vault` and `implicit` when the vault came
out of the resolution chain; only an implicit degenerate target is re-derived
during drift validation, which is what makes "default changed for an implicit
install" refuse. Paths must be
absolute, lexically normalized and valid for the host. Existing paths are also
canonicalized at installation; the canonical value is stored. `log_path` may
name a file that does not exist yet, but its nearest existing ancestor must
resolve without symlinks.

`config_digest` and `project_digest` are SHA-256 hashes of the exact bytes read
during resolution. They contain no source bytes. `backend_identity` is a
schedule-specific SHA-256 digest over canonical JSON containing only the
selected registry entry's identity-bearing fields:

| Backend kind | Identity fields |
|---|---|
| Azure | registry name, tenant ID, subscription ID, credential-priority mode |
| AWS | registry name, region, profile name, endpoint URL |
| Local | registry name, canonical resolved store path |

Missing optional fields serialize as null and object keys are fixed in the
order above. Domain-separate the digest with `xv-schedule-backend-v1\n`. Do not
reuse `cache::config_fingerprint`: that digest is deliberately config-wide and
does not fully distinguish selected named backend entries.

The manifest stores names and routing metadata that already appear in config or
CLI output. It must never serialize access keys, session tokens, client secrets,
credential file contents, environment values, local age identities, secret
names or secret values.

## Canonical target resolution

Add `schedule::target::resolve_install_target`. It accepts the loaded config,
the exact global config path and bytes, the install working directory, and the
optional `--vault`. It uses the existing project and workspace resolvers once,
then returns a `ResolvedScheduleTarget` containing the full manifest target and
the resolved workspace entry.

Resolution follows these rules:

1. Read and canonicalize `Config::get_config_path()`. The file must exist and be
   readable: an unattended schedule cannot safely replay an environment-only or
   implicit configuration. Users must save the tested configuration before
   installing a schedule.
2. Capture the canonical current directory. Resolve `.xv.toml` from that exact
   directory, including `.xv.boundary` and `XV_NO_PARENT_CONFIG` behavior.
3. Capture the selected environment name returned by `project::resolve_env`.
   Installation resolves `XV_ENV`/`--env` now; the runner replays the recorded
   environment and does not consult either ambient source.
4. Read the exact context file when context participates and record its
   canonical path and digest. Resolve the active workspace once. In a configured
   workspace, `--vault` must name an attached alias; otherwise select its
   default entry. With a degenerate workspace, an explicit `--vault` is a raw
   vault paired with the effective backend; without it, use the degenerate
   default. Record a null alias for the degenerate case.
5. Record the entry's registry backend name, derive its backend kind and
   identity from the selected built-in or `named_backends` entry, and record the
   entry's real vault. Unknown registry names and unavailable compile-time
   backends fail installation.
6. Materialize only the selected backend and perform the existing read-only
   connection/vault verification. No store, vault, key or credential is
   provisioned by preview or install.

An explicit `--vault` that does not match an attached alias in a configured
workspace is rejected with the available aliases. Raw vault names remain valid
when no configured workspace is attached. This removes the ambiguity where the
same text may mean an alias in one configured workspace and a raw vault in
another directory.

## Runner interface

Add a hidden clap variant:

```text
xv schedule run --manifest <absolute-path>
```

It is private scheduler plumbing and is omitted from normal `schedule --help`.
It requires an absolute path equal to the owned `manifest.json` path for the
current user. The native launchd, systemd and Task Scheduler renderers invoke
only this command. `HOME` remains in launchd/systemd units for credential and
platform-library compatibility; target selection never depends on it.

Main dispatch must special-case `ScheduleCommands::Run` before normal ambient
backend construction. The runner loads the bounded manifest, reads the exact
recorded config with a new `config::load_config_file_at(path)` function that
does not apply `XV_*` selection overrides, replays the recorded project
environment against the recorded working directory, and validates drift. Only
then does it construct a lazy `BackendRegistry`, materialize the recorded
backend name, and call a result-returning due-rotation service extracted from
`cli::secret_ops::execute_rotate_due`.

The public `xv rotate --due` path continues to use ambient CLI resolution and
render its existing messages. The extracted service returns counts and typed
per-secret failures so the schedule runner can persist an outcome without
capturing stdout or parsing error strings.

## Drift policy

Validation recomputes the target from recorded inputs and compares it with the
manifest before backend construction. The result is `Valid`, `Warning`, or
`Refuse`:

| Change | Result | Reason/action |
|---|---|---|
| Manifest missing, malformed, oversized, symlinked or unknown version | Refuse | Reinstall with the current binary |
| Config missing, path changed, bytes changed or now exists after absent install | Refuse | Review config and reinstall |
| Project file missing, path changed, bytes changed, or environment removed | Refuse | Review project selection and reinstall |
| Participating context file missing, path changed or bytes changed | Refuse | Review personal context/workspace and reinstall |
| Workspace alias removed/remapped or default changed for an implicit install | Refuse | Review workspace and reinstall |
| Backend registry entry missing, kind changed or identity digest changed | Refuse | Review account/provider and reinstall |
| Real vault changed or selected vault no longer verifies | Refuse | Review vault and reinstall |
| Recorded working directory missing or canonicalizes differently | Refuse | Reinstall from a stable directory |
| Recorded executable missing or is not a regular executable | Refuse | Repair the install or reinstall |
| Unit invokes a different manifest or executable path | Refuse in status | Reinstall to replace the unit |
| Binary at the same canonical path reports a different xv version | Warning; run allowed | Normal in-place upgrade; status recommends reinstall so rendered units/schema are refreshed |
| Cadence or log path differs between unit and manifest | Refuse in status | Reinstall |
| Ambient cwd, `XV_BACKEND`, `XV_ENV`, context or config-home differs | Ignore | The runner uses recorded inputs |

Any config/project byte change is intentionally conservative even when the
resolved tuple appears unchanged. Scheduled secret mutation should require a
fresh acknowledgement after configuration changes. `status` lists every drift
reason in deterministic field order. A scheduled run records one redacted
`target_drift` failure and exits with the existing configuration-error exit code.

## Install, reinstall and recovery

Installation performs these steps under an exclusive `install.lock` in the
schedule directory:

1. Resolve and verify the target without mutation.
2. Render the manifest and all native units fully in memory.
3. If owned artifacts already exist, read and validate their exact bytes into
   memory. A foreign/symlinked artifact is refused rather than adopted.
4. Atomically publish `manifest.json` privately.
5. Install/register the rendered native unit(s).
6. Query the scheduler and verify that the owned entry is installed and points
   to the new executable and manifest.

If steps 5 or 6 fail, restore the prior manifest and native unit bytes and
re-register the prior scheduler state. If no prior schedule existed, remove the
new manifest and owned unit files. If rollback also fails, return both errors
and leave the saved prior bytes as owner-private `recovery/*.json` or unit
snapshots under `recovery/<UTC-basic>-<artifact-name>`; the error tells the user
to run `xv schedule status` and reinstall. Recovery filenames are generated by
xv, use no user-controlled path components, and are created owner-private. No
credential or secret data enters recovery snapshots.

Reinstall uses the same flow and is the explicit acknowledgement for target
drift. It preserves `last-run.json`, `run.lock` and the log. It updates
`installed_at` and the recorded binary version.

### Legacy schedules

A native owned unit with no manifest is `legacy-unpinned`. `status` reports its
actual command and warns that it cannot prove the account/backend target. It
does not fabricate a manifest or claim the schedule is healthy. There is no
automatic `migrate` command: the supported migration is an explicit
`xv schedule install ...` (plus `--force` for noninteractive use), which shows
the newly resolved target and atomically replaces the legacy unit. `uninstall`
removes a recognized legacy owned unit as it does today.

A manifest with no native unit is `orphaned-manifest`. Status explains it and
reinstall repairs it. Uninstall removes the orphaned manifest. Foreign content
at an owned path is reported and retained.

## `--print` contract and golden example

`xv schedule install --print` performs target resolution and read-only backend
verification, then prints:

1. scheduler, cadence, target summary, drift-sensitive input paths, command and
   log path;
2. a `manifest.json` block with `installed_at` set to
   `"<set-at-install>"`;
3. the native unit block(s), whose command is `schedule run --manifest ...`;
4. for Windows, the exact `schtasks` creation invocation.

It performs no filesystem writes and makes no scheduler calls. A representative
abbreviated human output is below. The normative complete examples are in the
linked golden-output artifact.

```text
# scheduler: systemd user timer
# schedule:  daily at 03:00
# target:    payments -> aws-prod/payments-production
# config:    /home/alice/.config/xv/xv.conf
# project:   /home/alice/work/service/.xv.toml (environment production)
# command:   /home/alice/bin/xv schedule run --manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
# log:       /home/alice/.local/state/xv/rotate.log

# --- manifest.json (preview; installed_at is assigned during install) ---
{ ... }

# --- /home/alice/.config/systemd/user/xv-rotate.service ---
[Unit]
Description=xv due-secret rotation
...
```

Preview errors if the target cannot be pinned. It never falls back to printing
an ambient `rotate --due` command.

## Run outcome and concurrency

Before validation, `schedule run` acquires `run.lock` with `fs2::FileExt` using
a nonblocking exclusive lock. If another run holds it, the second process exits
successfully after logging `already_running`; it does not overwrite the active
run's outcome or start a second sweep.

After acquiring the lock, the runner atomically writes a `running` outcome.
Every normal return path replaces it with a terminal outcome. A process killed
between those writes leaves `running`; status reports `interrupted` when the
lock can be acquired, which proves no runner still owns that run.

`last-run.json` version 1 is:

```json
{
  "schema_version": 1,
  "schedule_id": "rotation-default",
  "manifest_digest": "sha256:4d38...",
  "started_at": "2026-09-10T03:00:00Z",
  "finished_at": "2026-09-10T03:00:02Z",
  "state": "success",
  "exit_code": 0,
  "summary": {
    "policy_managed": 12,
    "due": 2,
    "rotated": 2,
    "failed": 0
  },
  "diagnostic": null
}
```

`state` is `running`, `success`, `partial_failure`, `failed`, or
`refused_drift`. `finished_at`, `exit_code`, `summary` and `diagnostic` are null
while running. A diagnostic contains only a stable code and a sanitized message
limited to 1024 Unicode scalar values. It may name manifest fields and paths,
but never secret names, values, backend error bodies, tokens or command output.
Per-secret failures contribute only to counts. The normal log may retain the
existing user-facing rotation diagnostics under its existing security model.

`manifest_digest` binds an outcome to the installation that produced it.
Status labels a retained outcome from a different manifest `previous install`.
Uninstall retains history; a later reinstall does not present it as the current
installation until a new run completes.

## Status contract

`xv schedule status` is read-only and reports these independent dimensions:

- scheduler state: `installed`, `absent`, `unknown`, or `error`;
- ownership state: `managed`, `legacy-unpinned`, `orphaned-manifest`, or
  `foreign`;
- intended target: workspace alias, registry backend name/kind, real vault,
  config path, project path/environment and install cwd;
- drift state: `valid`, `warning`, or `refused`, with all reasons;
- executable: recorded path/version and current version;
- last run: start/end, state, exit code and aggregate counts, or `never`;
- next run: scheduler-reported time when the platform exposes a parseable value,
  otherwise `unknown (scheduler did not report a next run)`.

Scheduler command failure is distinct from absence. Existing lifecycle methods
must stop collapsing command failure into `installed: false`. Platform parsers
consume `launchctl print`, `systemctl --user show ... --property=NextElapseUSecRealtime`
and `schtasks /Query /TN ... /V /FO LIST`; fixture tests pin their behavior.
Locale-dependent or absent next-run values yield `unknown`, never a guessed
time. Status does not contact the secrets provider; vault verification belongs
to install and run.

## Verification strategy

Pure unit tests cover all schemas, bounded/no-follow storage, backend identity
fixtures, target comparison, drift ordering, recovery transitions, renderer
quoting and status parsers. Existing renderer tests continue to exercise all
three platforms on every host.

`tests/schedule_cli_tests.rs` uses isolated local configurations to cover:

- two named local backends with the same real vault name;
- a workspace alias mapped to a different real vault;
- project environment selection and a path containing spaces;
- invocation from a changed cwd and with conflicting `XV_BACKEND`/`XV_ENV`;
- missing config, project, cwd, executable and backend entry;
- `--print` leaving the entire temp tree byte-for-byte unchanged;
- legacy, orphaned and retained-history status output;
- one real `schedule run` rotating only due secrets in the pinned local target.

CI keeps native registration out of ordinary developer tests. A dedicated
workflow validates generated artifacts on `ubuntu-latest`, `macos-latest` and
`windows-latest`: `systemd-analyze verify`, `plutil -lint`, and an ephemeral
Task Scheduler create/query/delete round trip with a far-future trigger. The
workflow uses a unique task name suffix for the round trip and deletes it in an
`always()` cleanup step. Native registration tests never execute rotation.

## Delivery split and task mapping

### PR 1: manifest and canonical resolver

A03-01, A03-02 and A03-03. Add the schema, private store, state paths,
schedule-specific backend identity, exact target resolver and read-only preview.
The existing installed command continues to be used until PR 2; PR 1 must not
claim installed jobs are protected yet.

### PR 2: manifest runner, drift and upgrade/recovery

A03-04, A03-05, A03-06 and the owned-artifact/binary-upgrade portion of
A03-09. Switch all native units to the manifest runner, refuse drift before
rotation, implement transactional reinstall/rollback, diagnose legacy units and
define same-path binary upgrades.

### PR 3: outcomes, status and platform verification

A03-07, A03-08, A03-10 and the historical-result-retention portion of A03-09.
Persist bounded outcomes, prevent overlapping runs, expand status, add target
isolation scenarios and native artifact validation. This placement resolves the
dependency between result retention and the outcome schema.

Every PR must update user/operator documentation for behavior it makes real,
run formatting, clippy and the relevant test suites, and receive clean
current-head CI and Bugbot review before merge. The next PR starts from the
preceding merged `main`.
