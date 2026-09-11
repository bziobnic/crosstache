# `xv rotate` — rotation and rotation policies

Two distinct things share the word "rotation":

1. **Replacing a value** — `xv rotate NAME` generates a new value and writes it
   as a new version. Works on every backend.
2. **Knowing *when* to replace it** — a rotation *policy* stored with the secret,
   a command that acts on everything due, and `xv schedule` to run that command
   on a cadence via the OS scheduler. All on every backend.

Plus one backend-specific mechanism: `xv rotate --native` hands the whole job to
AWS Secrets Manager's rotation Lambda.

- [Replacing a value](#replacing-a-value)
- [Rotation policies](#rotation-policies)
- [Acting on due secrets](#acting-on-due-secrets)
- [`xv schedule` — run the sweep automatically](#xv-schedule--run-the-sweep-automatically)
  - [Manifest target fields](#manifest-target-fields)
  - [Concurrency: overlapping runs](#concurrency-overlapping-runs)
  - [Status at a glance](#status-at-a-glance)
  - [Exit codes](#exit-codes)
- [Where the schedule actually lives](#where-the-schedule-actually-lives)
- [Native rotation (AWS)](#native-rotation-aws)
- [What rotation does not do](#what-rotation-does-not-do)

---

## Replacing a value

```bash
xv rotate API_KEY                            # new 32-char alphanumeric value
xv rotate API_KEY --length 64
xv rotate API_KEY --charset hex              # hex / base64 / numeric / alphanumeric-symbols / …
xv rotate API_KEY --generator ./mygen.sh     # custom generator (must be owned by you, mode 0700)
xv rotate API_KEY --show-value               # echo the new value (otherwise silent)
xv rotate API_KEY --force                    # skip the confirmation
```

Metadata is preserved: tags, groups, note, folder, expiry, content type. On a
typed record, the generated value becomes the new **primary field** rather than
replacing the envelope.

Every rotation stamps `xv:rotated_at`, so if the secret has a policy its clock
restarts from the rotation that actually happened.

---

## Rotation policies

A policy is two tags on the secret:

| Tag | Meaning |
|-----|---------|
| `xv:rotate_every` | Interval, e.g. `90d`. Its presence is what makes a secret policy-managed. |
| `xv:rotated_at` | RFC 3339 timestamp of the last rotation. |

Both are ordinary metadata: they travel with the secret through `xv migrate`,
are visible to anything else reading its tags, and occupy two of the backend's
tag slots (Azure allows 15 per secret).

### Setting a policy

```bash
# Policy only — does NOT change the value. The clock starts now.
xv update DB_PASSWORD --rotate-every 90d

# Rotate now and set (or change) the policy in one step.
xv rotate DB_PASSWORD --every 90d

# Remove the policy.
xv update DB_PASSWORD --clear-rotate-every
```

Interval syntax is `<number><unit>` with unit `m` (minutes), `h` (hours), `d`
(days), or `w` (weeks). The unit is **required** — a bare `90` is rejected rather
than guessed, because guessing minutes vs. days is the difference between
rotating constantly and never rotating. Maximum is 10 years.

`--rotate-every` and `--clear-rotate-every` are standalone operations, like
`--field`: combine them with other edits by running two commands.

---

## Acting on due secrets

### `--check` — report, change nothing

```bash
xv rotate --check
```

```
 Name          Status  Interval  Due
 API_KEY       ok                in 74 days (2026-10-07)
 DB_PASSWORD   due               12 days ago (2026-07-13)
 LEGACY_TOKEN  invalid  ninety   unparseable xv:rotate_every
```

Exits **51** (`xv-rotation-due`) when at least one secret is due, so a pipeline
can gate on staleness. Secrets with no policy are omitted entirely — unmanaged is
not the same as overdue. `--format json|yaml|csv` works as on any list command.

### `--due` — rotate everything that is due

```bash
xv rotate --due            # confirms once for the batch
xv rotate --due --force    # unattended (cron, CI)
```

Only policy-managed, currently-due secrets are touched. Each goes through the
same path as a manual `xv rotate`, so record handling, reserved-key guards, and
the audit/git hooks all apply identically. Generation flags (`--length`,
`--charset`, `--generator`) apply to the whole batch.

`--due` **fails** rather than proceeding if any secret's `xv:rotate_every` cannot
be parsed. An unreadable policy means its due-ness is unknown, and a run that
quietly skipped it would report success while leaving a possibly-overdue secret
in place. Fix or clear the tag, then re-run.

If some rotations fail, every failure is listed and the command exits non-zero
with a count — a partial batch never looks like success.

### `xv schedule` — run the sweep automatically

`xv` installs and manages the trigger itself, in the platform's own scheduler:

```bash
xv schedule install --vault myproj-prod-kv            # daily at 03:00
xv schedule install --vault v --interval hourly --at 00:15
xv schedule install --vault v --interval weekly --at 04:00   # Sundays
xv schedule status
xv schedule uninstall
```

| Platform | Mechanism | Unit |
|----------|-----------|------|
| macOS | launchd user agent | `~/Library/LaunchAgents/com.crosstache.xv-rotate.plist` |
| Linux | systemd **user** timer | `~/.config/systemd/user/xv-rotate.{service,timer}` |
| Windows | Task Scheduler | task `crosstache-xv-rotate` |

All three are per-user, never system-wide: the sweep runs as the user whose
credentials and config it needs, and uninstalling never requires root. There is
**no daemon** — a resident process would have to reimplement, worse, what these
schedulers already do, and would hold decryption credentials for its whole
lifetime.

#### `--print` — review the whole thing before committing to it

```bash
xv schedule install --vault v --print     # read-only preview, writes nothing
```

`--print` resolves the target and verifies the backend read-only, then prints
exactly what an install would produce:

- a header block — scheduler, cadence, the resolved target
  (`<alias-or-vault> -> <backend>/<vault>`), the backend and its identity
  digest, the config file, the `.xv.toml` and environment that participated,
  the working directory, the command, and the log path;
- the `manifest.json` that would be written, verbatim, with `installed_at`
  shown as `<set-at-install>` because the real timestamp is stamped by the
  write itself;
- the native unit file(s), and on Windows the exact `schtasks` invocation.

The pinned unit carries the manifest path, `HOME`, the working directory the
target was resolved in, and the log path — nothing else. In particular it does
**not** set `XDG_CONFIG_HOME`: the manifest already names the exact
configuration file and its digest, and an environment variable that redirects
config resolution is a target-selection input a pinned unit may not add.

It is a preview in the strict sense: it creates no directory, manifest, lock,
result, unit or log, touches no existing file, and calls no scheduler.

The `# command:` line is exactly what the installed unit runs. To drive a
scheduler `xv` does not manage — cron, a Kubernetes CronJob, a CI schedule —
paste that line, but only with the same pinned environment the preview's header
shows: `HOME`, and `XV_STATE_HOME`/`XDG_STATE_HOME` when one of them selected
the state root. The runner resolves the manifest path from its *own*
environment, so a job started with a different one refuses its own manifest
rather than guessing. Install first: the command has nothing to read until a
manifest exists.

#### What the scheduled job runs

Installed schedules are **target-pinned**. `xv schedule install` writes a
`manifest.json` and the unit runs only this:

```
xv schedule run --manifest <state dir>/schedules/rotation-default/manifest.json
```

Everything the sweep may act on comes from that file: the config file and its
digest, the `.xv.toml` path and environment that participated, the context
file, the workspace alias, the backend registry entry and its identity digest,
the real vault, the working directory the target was resolved in, the binary
path and version, the cadence, and the log path.

Before any backend is constructed the runner recomputes that target from the
recorded inputs and compares it with what the manifest says. A difference
refuses the sweep (see [Drift](#when-a-run-refuses-drift)). The sweep itself is
still `--due`-bounded — only secrets that already carry a policy and are already
past it — and the schedule never sets or changes a policy: `--every` is
deliberately not part of it, so a schedule cannot redefine what it is sweeping.

The unit contains an absolute binary path, the manifest path, `HOME`, the
recorded working directory, the log path, and — only when a variable selected
the state root — `XV_STATE_HOME` or `XDG_STATE_HOME`. **No credentials, no
secret values, no vault name, and no `XDG_CONFIG_HOME`.** The manifest already
names the exact configuration file and its digest, so a variable that redirects
config resolution would be a second, unversioned answer to "what does this job
rotate?" that can disagree with the manifest.

#### Manifest target fields

`manifest.json` is a versioned, unknown-field-denying schema. Every field
`xv schedule status` can report drift or a target summary for comes from here:

| Field | Meaning | Null when |
|---|---|---|
| `schema_version` | Fixed `1` for this shape. | never |
| `schedule_id` | Fixed `rotation-default`. | never |
| `installed_at` | RFC 3339 timestamp of the (re)install that wrote this file. | never |
| `cadence.kind`/`hour`/`minute` | The install's `--interval`/`--at`. | never |
| `execution.binary_path` | Canonical path to the `xv` that installed the schedule. | never |
| `execution.installed_version` | `xv` version at install time; compared against the current version at run time (warning, not drift, if the path is unchanged). | never |
| `execution.working_directory` | Canonicalized install `cwd`; the runner `chdir`s here before the sweep. | never |
| `execution.log_path` | Where the scheduled run's output is appended. | never |
| `target.config_path`/`config_digest` | The global `xv.conf` path and a SHA-256 of its exact bytes. | never — a saved config is required to install |
| `target.project_path`/`project_digest` | The `.xv.toml` that participated, and its digest. | no project file participated |
| `target.environment` | The project environment (`.xv.toml [env.*]`) selected at install. | no project environment participated |
| `target.context_path`/`context_digest` | The personal context file that participated, and its digest. | no context file participated |
| `target.workspace_source` | `project`, `context`, or `degenerate` — which layer produced the target. | never |
| `target.workspace_alias` | The attached workspace alias `--vault` named or resolved to. | a degenerate (no-workspace) target |
| `target.backend_name`/`backend_kind` | The selected registry backend's name and kind (`azure`/`aws`/`local`). | never |
| `target.backend_identity` | A schedule-specific SHA-256 digest over that backend instance's identity-bearing fields (account/tenant/subscription for Azure; region/profile/endpoint for AWS; canonical store path for local). Never the credentials themselves. | never |
| `target.vault` | The real vault name resolved at install. | never |
| `target.vault_selection` | `explicit` (install was given `--vault`) or `implicit` (resolved from the workspace/config default). Only an implicit **degenerate** target is re-derived and can drift when the default changes. | never |

`config_digest`, `project_digest` and `context_digest` are digests of exactly
the bytes read during resolution — never source bytes, never secret material.
See the [design's manifest schema](superpowers/specs/2026-09-09-scheduled-target-manifest-design.md#manifest-schema)
and its [golden `manifest.json`](superpowers/specs/2026-09-09-scheduled-target-manifest-goldens.md#schedule-install---print)
for a complete worked example.

#### Where the schedule's files live

The schedule directory sits under the per-user **state** root, which is
independent of the config directory:

| Platform | State root |
|---|---|
| Linux/macOS with `XDG_STATE_HOME` set | `$XDG_STATE_HOME/xv/schedules/rotation-default/` |
| Linux/macOS fallback | `~/.local/state/xv/schedules/rotation-default/` |
| Windows | `%LOCALAPPDATA%\xv\schedules\rotation-default\` |
| Any platform, test/embedding override | `$XV_STATE_HOME/xv/schedules/rotation-default/` |

`XV_STATE_HOME` takes priority over `XDG_STATE_HOME` on Unix; an empty value is
ignored on both. See [`XV_STATE_HOME`](#xv_state_home) below for why it is
never a target-selection input.

Within that directory (`schedules/rotation-default/`):

| File | Written by | Lifetime |
|------|------------|----------|
| `manifest.json` | install/reinstall | removed by `uninstall` |
| `last-run.json` | the scheduled run | retained by reinstall and `uninstall` |
| `run.lock` | the scheduled run | persistent lock inode; retained |
| `install.lock` | install/reinstall/uninstall | persistent lock inode; retained |
| `recovery/` | an install rollback that could not finish | created only then; retained |

`last-run.json` records the outcome of each firing and `run.lock` serializes
concurrent firings; `xv schedule status` reports both as `Last run:`. Neither is
ever recreated by reinstalling, which is why neither is `uninstall`'s to take.

The rotation log lives outside that directory (`~/.local/state/xv/rotate.log` by
default on Linux/macOS, `%LOCALAPPDATA%\xv\rotate.log` on Windows — always
`xv/rotate.log` under the same state root as the manifest — or wherever
`--log-file` pointed) and is likewise never removed.
Everything is owner-private: `0700` directories and `0600` files on Unix, an
owner-and-SYSTEM DACL on Windows.

#### When a run refuses (drift)

The recorded target is the acknowledgement you gave at install time. If any
recorded input changed, the scheduled run refuses **before** constructing a
backend and exits with the ordinary configuration-error code, and
`xv schedule status` says the same thing without contacting the provider:

```text
[error] The installed systemd user timer rotation schedule is unsafe to run.
  Ownership: managed
  Target:    payments -> aws-prod/payments-production
  Drift:     refused
  - project_digest changed; review /home/alice/work/service/.xv.toml and reinstall
  - backend_identity changed for aws-prod; review the account/provider and reinstall
[hint] Review the changes, then run 'xv schedule install --vault payments' to accept the new target.
```

Reinstall — `xv schedule install --vault <alias-or-vault>` — is the only
operation that accepts a changed target, which is the point: an unattended job
that mutates secrets should need a fresh acknowledgement after the ground moves
under it. So **reinstall after** editing `xv.conf` or `.xv.toml`, changing the
active environment, switching account/subscription or backend identity,
re-pointing a workspace alias, or moving the directory you installed from.

Ambient variables are deliberately *ignored*: `XV_BACKEND`, `XV_ENV`, the
current directory and the ambient context file do not change what a scheduled
run does, because the run replays the recorded inputs instead.

**Upgrading `xv` in place** is the one allowed difference: the same executable
path reporting a new version is a warning, not a refusal, and the run proceeds.
Reinstalling afterwards is still worth doing — it refreshes the rendered units
and the recorded version. A binary that **moved or disappeared** is a refusal.

#### Replacing a legacy schedule

A schedule installed by an older `xv` runs `rotate --due --force` directly and
has no manifest. It is not migrated automatically, and nothing but an explicit
install ever writes a manifest. `xv schedule status` labels it and shows what it
actually runs:

```text
[warn] A legacy systemd user timer rotation schedule is installed.
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv rotate --due --force --vault payments-production
  Target:    unverified (the legacy unit does not record backend or account identity)
[hint] Replace it explicitly with 'xv schedule install --vault <alias-or-vault>'.
```

Run that install (add `--force` where there is no terminal) and the legacy unit
is replaced in the same transaction as any other reinstall.

A pinned unit whose `manifest.json` has gone — deleted by hand, or a state
directory that moved — is reported under the same `legacy-unpinned` label,
because its target can no longer be proven either. Status names the manifest it
was pinned to instead of claiming the unit recorded nothing:

```text
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv schedule run --manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
  Target:    unverified (the recorded manifest /home/alice/.local/state/xv/schedules/rotation-default/manifest.json is missing)
```

That job would refuse itself at its next firing; reinstalling repairs it.

#### Install is transactional

Install and reinstall hold an exclusive `install.lock`, render everything in
memory, publish `manifest.json`, then write and register the native unit(s) and
**verify** that the scheduler really has an entry pointing at this executable,
this manifest, this cadence and this log path. If any of that fails, the prior
manifest and unit bytes are put back and the previous registration restored — a
failed reinstall leaves the schedule you had, and a failed first install leaves
nothing.

If the rollback *itself* fails, the prior bytes are written to owner-private
snapshots under `recovery/<UTC>-<artifact>` and the error reports both failures
and points at `xv schedule status`. Those snapshots are yours to inspect; `xv`
never reads them back on its own.

A file at an owned path that `xv` did not write — anything without its
`Managed by crosstache (xv schedule)` marker, or a symlink — is refused rather
than adopted or overwritten. `status` reports it as `foreign` and leaves it
alone.

#### What `uninstall` and reinstall touch, exactly

`xv schedule uninstall` deregisters the job and removes exactly two things: the
native unit file(s) or task entry, and `manifest.json`. A reinstall replaces
those same two things and nothing else. Everything in the state directory that
is *not* in that owned set is somebody's evidence, and none of it is recreated
by reinstalling:

| Artifact | `uninstall` | reinstall |
|---|---|---|
| `com.crosstache.xv-rotate.plist` / `xv-rotate.service` + `.timer` / the `crosstache-xv-rotate` task | removed | re-rendered and re-registered |
| `manifest.json` | removed | replaced; `installed_at` and the recorded binary version are updated |
| `last-run.json` | retained | retained, and labelled `(previous install)` until a new run completes |
| `run.lock` | retained (inode) | retained (inode) |
| `install.lock` | retained (inode) | retained (inode) |
| `recovery/` and its snapshots | retained | retained |
| the rotation log (`--log-file`, default `~/.local/state/xv/rotate.log`) | retained | retained; the directory is (re)created |
| anything else you left in the state directory | retained | retained |
| a file at an owned path that `xv` did not write | retained and reported | refused; the install does not proceed |
| a similarly named job of your own in the same unit directory | retained | untouched |

Deleting a lock file would stop it excluding anything, so both lock inodes are
permanent. The state directory itself is removed only if it ends up genuinely
empty, which in practice it does not — `install.lock` alone keeps it — and the
parent `schedules/` directory is never removed.

Removing nothing is success: `uninstall` is safe in teardown scripts and on hosts
that never had a schedule. A scheduler that fails for a reason other than "no
such job" is reported as an error rather than as absence.

One case deliberately stops short of deregistering. If a file `xv` did not write
sits at an owned *unit* path, `uninstall` leaves both that file and the
scheduler registration alone, and says so:

```text
[ok] Removed the systemd user timer rotation schedule.
  Removed:   /home/alice/.local/state/xv/schedules/rotation-default/manifest.json
  Retained:  /home/alice/.config/systemd/user/xv-rotate.timer (xv did not write it, so it was left alone)
  Retained:  the scheduler registration was left in place because /home/alice/.config/systemd/user/xv-rotate.timer is not managed by xv.
```

Tearing down a job whose unit file is yours, and then calling that file
"retained", would retain nothing. A foreign `manifest.json` is different: it sits
inside `xv`'s own private state directory, and the registration it would shield
is `xv`'s own, pointing at units this same call just removed — so the job *is*
deregistered and only the foreign manifest stays.

#### Retained history: `(previous install)`

An outcome in `last-run.json` is bound to the exact `manifest.json` bytes that
produced it. After a reinstall — or after an `uninstall` — the record is still
there but no longer describes the installation you have now, so `status` marks
it:

```text
  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, 2 rotated, 0 failed (previous install)
```

The label disappears the first time a run completes under the new manifest. After
`uninstall`, the same line appears under `[info] No ... rotation schedule is
installed.` — the history is retained, so it is also shown.

#### Concurrency: overlapping runs

Every `xv schedule run` — the scheduler firing on cadence, or you invoking the
same command by hand while it does — takes a nonblocking exclusive `run.lock`
before it reads config, checks drift, or touches a backend. Exactly one runner
wins the lock; that is the run that owns `last-run.json` for this firing.

The runner that loses logs one line to the scheduled job's own log and exits
successfully, without overwriting the active run's outcome or starting a
second sweep:

```text
schedule rotation skipped: another run is already_running
```

Between the lock being acquired and the outcome being written, `last-run.json`
is a `running` record. A process that is killed mid-sweep — `kill -9`, a host
reboot, an OOM — leaves that record in place forever unless something notices.
`xv schedule status` distinguishes the two cases by trying the same
nonblocking lock:

```text
  Last run:  running since 2026-09-10T03:00:00Z
```

is a run that is (as far as `status` can tell) still actually in flight — the
lock is held, so *something* has it. Once that something is confirmed gone —
`status` itself can take the lock — the same record renders:

```text
  Last run:  interrupted after 2026-09-10T03:00:00Z (no runner holds the lock)
```

`interrupted` is not a `state` value in the JSON; it is `status`'s
interpretation of a `running` record with no lock holder. The lock files
(`run.lock`, `install.lock`) are permanent inodes — deleting one would stop it
excluding anything — and neither is ever removed by `uninstall` or reinstall.

#### After you upgrade `xv` in place

Replacing the binary at the same path is the normal case and is **not** drift.
The scheduled run is allowed, with one warning line, and `status` recommends a
reinstall so the rendered unit and the manifest schema are refreshed:

```text
[ok] A systemd user timer rotation schedule is installed.
  Drift:     warning
  - installed_version changed from 0.39.0 to 0.40.0 at the same binary path; reinstall the schedule ('xv schedule install') to refresh the rendered unit
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.40.0)
[hint] Reinstall the schedule with 'xv schedule install --vault payments' to refresh the rendered unit and the recorded version.
```

A binary at a *different* path, or a recorded path that no longer holds a regular
executable file, is a `binary_path` refusal — the run exits with the
configuration-error code and rotates nothing. That is the difference between "the
same job, upgraded" and "some other program is about to rotate your secrets".

#### `XV_STATE_HOME`

The manifest lives under the per-user state directory
(`$XDG_STATE_HOME/xv/schedules/rotation-default/`, else
`~/.local/state/xv/...`; `%LOCALAPPDATA%\xv\...` on Windows).
`XV_STATE_HOME` overrides that root on every platform. It exists for tests and
embedding only — it is **not** a target-selection input, it changes nothing
about which vault or backend a schedule acts on, and an empty value is ignored.

Whichever variable selected the root at install time is pinned into the unit, so
the scheduled process computes the same manifest path the installing shell did.
Without that pin the two would disagree and the job would refuse its own
manifest at 3 a.m.

#### Status at a glance

`xv schedule status` is read-only — it never contacts the secrets provider —
and always reports the same independent dimensions: ownership, the intended
target, drift, the executable, the last run, the next run, and the log. What
follows are the states from the golden outputs, in the goldens' own wording.
The first two are complete blocks; the drift-refusal and the orphaned-manifest
examples are **excerpts** — a real run renders the full dimension set, and these
show only the lines that matter for that state.

A healthy schedule that has not fired yet:

```text
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  never
  Next run:  2026-09-10T03:00:00Z
  Log:       /home/alice/.local/state/xv/rotate.log (not yet written)
```

The same schedule after a successful firing:

```text
[ok] A systemd user timer rotation schedule is installed.
  Ownership: managed
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Backend:   aws-prod (aws)
  Config:    /home/alice/.config/xv/xv.conf
  Project:   /home/alice/work/service/.xv.toml (environment production)
  Cwd:       /home/alice/work/service
  Drift:     valid
  Binary:    /home/alice/bin/xv (installed 0.39.0, current 0.39.0)
  Last run:  success; 2026-09-10T03:00:00Z to 2026-09-10T03:00:02Z; 2 due, 2 rotated, 0 failed
  Next run:  unknown (scheduler did not report a next run)
  Log:       /home/alice/.local/state/xv/rotate.log (present)
```

`Next run` is genuinely platform-dependent: launchd rarely exposes a
parseable next-fire time at all (`launchctl print` does not reliably report
one), so macOS schedules commonly show `unknown` even when healthy. systemd
reports it whenever the timer is loaded. Task Scheduler prints a value in the
machine's own short-date format **and with no time zone**, so unless it happens
to name its own offset it renders `unknown`: attributing the host's current UTC
offset to a wall-clock time in the future is wrong across a daylight-saving
boundary, and a confidently wrong instant is worse than none.

A target that no longer matches what was installed (see
[Drift](#when-a-run-refuses-drift) above for the full example) fails status
with exit `3`:

```text
[error] The installed systemd user timer rotation schedule is unsafe to run.
  Ownership: managed
  Target:    payments -> aws-prod/payments-production
  Drift:     refused
  - project_digest changed; review /home/alice/work/service/.xv.toml and reinstall
  - backend_identity changed for aws-prod; review the account/provider and reinstall
  Last run:  refused_drift; 2026-09-10T03:00:00Z; exit 3; target_drift
  Next run:  2026-09-11T03:00:00Z
[hint] Review the changes, then run 'xv schedule install --vault payments' to accept the new target.
```

A schedule whose files are still on disk but which the scheduler has no record
of — `systemctl --user disable --now xv-rotate.timer`, `launchctl bootout`, or a
user session that never loaded it — is `managed` and **will not fire**. Status
says so and fails with exit `3`, because nothing else about the block would
tell you:

```text
[error] The systemd user timer rotation schedule is not registered.
  Ownership: managed
  Scheduler: not registered
  Schedule:  daily at 03:00
  Target:    payments -> aws-prod/payments-production
  Drift:     valid
[hint] Run 'xv schedule install --vault payments' to register it again.
```

A schedule from an older `xv`, with no manifest at all (see
[Replacing a legacy schedule](#replacing-a-legacy-schedule)):

```text
[warn] A legacy systemd user timer rotation schedule is installed.
  Ownership: legacy-unpinned
  Command:   /home/alice/bin/xv rotate --due --force --vault payments-production
  Target:    unverified (the legacy unit does not record backend or account identity)
[hint] Replace it explicitly with 'xv schedule install --vault <alias-or-vault>'.
```

A manifest with no native unit registered — deleted by hand, or the scheduler
lost its registration — is `orphaned-manifest`, not `legacy-unpinned`: its
target is still fully known, there is simply nothing installed to run it:

```text
[warn] A rotation manifest exists but no systemd user timer is installed.
  Ownership: orphaned-manifest
  Target:    payments -> aws-prod/payments-production
  Drift:     valid
[hint] Run 'xv schedule install --vault payments' to repair the schedule, or 'xv schedule uninstall' to remove the manifest.
```

#### Exit codes

`xv schedule status` and `xv schedule run` (the manifest runner the scheduler
invokes) use the project's ordinary exit-code table
([`exit-codes.md`](exit-codes.md)):

| Situation | Exit |
|---|---|
| `[ok]`, `[warn]` or `[info]` status — including `legacy-unpinned`, `orphaned-manifest`, and "nothing installed" | `0` |
| `[error]` status: drift would refuse the next run, the recorded target could not be read, the scheduler has no record of a unit `xv` installed, or the scheduler itself could not be queried | `3` (configuration error) |
| `schedule run --manifest ...`: target drift, a missing/malformed manifest, or any refusal before backend construction | `3` (configuration error) |
| `schedule run --manifest ...`: a second concurrent firing that lost the `run.lock` race | `0` — it is not a failure, just a skip (see [Concurrency](#concurrency-overlapping-runs)) |
| `schedule run --manifest ...`: the sweep ran and rotated everything due | `0`, `last-run.json` state `success` |
| `schedule run --manifest ...`: the sweep ran but some due secrets failed to rotate | non-zero, `last-run.json` state `partial_failure` or `failed` |

`xv schedule install`/`uninstall` follow the same convention as every other
mutating command: `0` on success, non-zero (commonly `2` for a bad flag or `3`
for an unresolvable target) on failure. None of these are new codes — `3` is
the same "Configuration error" family every other unattended-target failure in
`xv` already uses.

#### The limitation to plan around

A scheduled run has **no terminal**, so any credential needing interaction fails
there even though it works for you now. Azure CLI tokens work while the refresh
token is valid and the keyring is unlocked; managed identity, service principals,
and the local backend work unconditionally. Verify before trusting it:

```bash
xv rotate --due --force --vault v        # in a clean shell
xv schedule status                       # ownership, target and drift
cat ~/.local/state/xv/rotate.log         # what actually happened
```

Output is captured to that log on every platform (launchd `StandardOutPath`,
systemd `StandardOutput=append:`, a `>>` redirect for Task Scheduler), because a
3 a.m. failure with no record is indistinguishable from no failure at all.

For CI-driven rotation instead of host-driven, see
[`ci-cd.md`](ci-cd.md#rotation-gates-in-ci) — a scheduled workflow plus a
`--check` gate on pull requests.

---

## Where the schedule actually lives

AWS Secrets Manager rotates server-side: the service invokes a Lambda on its own
schedule. Azure Key Vault has no equivalent for secret *values* — it can
auto-rotate keys and emit near-expiry events, but nothing in it will regenerate a
secret. A local directory has no scheduler at all.

So the cadence has to come from somewhere outside the vault, and `xv schedule`
puts it in the host's own scheduler rather than in a daemon of its own. That
keeps reboot survival, catch-up after sleep, and log capture in the hands of
software that already does them correctly, and it means no long-lived `xv`
process holding credentials.

The division of labour: `xv` owns the policy, the due-date math, and the unit's
lifecycle; launchd/systemd/Task Scheduler owns the clock.

---

## Native rotation (AWS)

```bash
xv rotate DB_PASSWORD --native
```

Calls `RotateSecret`, which invokes the rotation Lambda configured on the secret;
rotation completes asynchronously. AWS-only, and errors with a capability hint on
other backends. `--native` cannot be combined with `--every`, `--due`, or
`--check`: the schedule and the new value are both AWS's to decide.

A secret can carry an `xv:rotate_every` policy on AWS too, but if AWS is already
rotating it on a schedule, tracking a second policy in tags is redundant.

---

## What rotation does not do

- **It does not restart anything.** An application that read the secret at
  startup keeps using the old value until it is restarted or redeployed.
  Rotation without a rollout plan is how a rotated credential becomes an
  outage — sequence them.
- **It does not update external systems.** For a database password, `xv rotate`
  changes the stored value, not the password on the database. Use
  `--generator` to hook a script that changes both, or `--native` on AWS where
  the Lambda owns that logic.
- **It does not fix a broken credential by itself.** If the scheduled sweep
  fails — expired login, locked keyring, a vault that moved — rotation silently
  stops happening until someone reads the log. Watch `rotate.log`, or run
  `xv rotate --check` from CI so a stale secret fails a pipeline rather than
  waiting to be noticed.
- **It does not purge old versions.** Previous values stay in version history
  (`xv history`) and, with `[local].git`, in commit history. That is deliberate —
  rollback needs them — but it means rotation is not a way to make an exposed
  value unrecoverable. Use `xv purge` for that, and remember it cannot rewrite
  git history that has already been pushed.
