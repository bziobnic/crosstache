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

Review before committing to it:

```bash
xv schedule install --vault v --print     # renders the unit, writes nothing
```

`--print` is also the way to drive a scheduler `xv` does not manage: it prints
the exact command line to paste into cron, Kubernetes CronJob, or a CI schedule.

#### What the scheduled job runs

```
xv rotate --due --force --vault <vault>
```

`--due` bounds the blast radius to secrets that already carry a policy and are
already past it; `--force` is required because there is no terminal to confirm
at. The schedule never sets or changes a policy — `--every` is deliberately not
passed, so a schedule can never redefine what it is sweeping.

The unit contains only an absolute binary path, those arguments, a log path, and
`HOME`/`XDG_CONFIG_HOME`. **No credentials and no secret values.** The env pair
matters: a scheduled process does not inherit your shell's environment, and a job
that resolves a different config than you tested against is the classic way this
silently sweeps the wrong vault.

#### The limitation to plan around

A scheduled run has **no terminal**, so any credential needing interaction fails
there even though it works for you now. Azure CLI tokens work while the refresh
token is valid and the keyring is unlocked; managed identity, service principals,
and the local backend work unconditionally. Verify before trusting it:

```bash
xv rotate --due --force --vault v        # in a clean shell
xv schedule status                       # what the scheduler thinks
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
