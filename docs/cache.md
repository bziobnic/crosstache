# Listing cache

`xv` keeps a small client-side cache of expensive **listing** operations
(`xv ls`, `xv vault list`, `xv file list`) so repeated reads are fast. It stores
listing *metadata only* — never secret values — as JSON files on disk with a
configurable TTL and background refresh.

- Enable/disable: `[cache_enabled]` (default on) in `xv.conf`.
- Freshness window: `cache_ttl_secs` (seconds). `0` disables caching.
- Location: `$XV_CACHE_DIR` if set, else the OS cache dir joined with `xv`
  (e.g. `~/.cache/xv` on Linux).

Manage it with `xv cache status`, `xv cache clear [<vault>]`, and
`xv cache refresh --key <key>` (the last is normally spawned automatically as a
background stale-while-revalidate refresh).

`xv cache status` and `xv cache clear <vault>` are **scoped to the current
identity** (see the fingerprint section below). `xv cache clear` with no vault
is a **global reset**: it removes the entire cache directory — every identity's
entries and any pre-v5 leftovers — not just the current identity's. Other
identities' caches simply repopulate on next use.

## Security posture (v5)

The cache is hardened so that a shared or multi-tenant machine cannot leak
listing metadata across accounts, and so a stray symlink or a corrupt file
cannot be abused or cause silent misbehaviour.

### Private modes and no-follow I/O

All cache files are written **owner-only (0600)** and all cache directories are
**owner-only (0700)**. Writes are atomic and refuse to follow symlinks: the
entry is written to a randomly named temporary file in the destination
directory and atomically renamed into place, every path component is opened
with `O_NOFOLLOW`, and a symlink at the final destination is refused rather
than written through. The background-refresh lock file is likewise created
0600.

A cache tree created by an older `xv` (world-readable 0644/0755) is tightened
in place on first use each process: directories become 0700 and
`.json`/`.lock`/`.corrupt` files become 0600. This is best-effort and never
fatal — warm caches are kept, not deleted.

These protections apply on Unix. On Windows the atomic writer uses a protected
owner+SYSTEM DACL and reparse-point-safe opens instead of numeric modes.

### Account/config fingerprint (v5 path layout)

Cache paths previously keyed on `(backend, vault)` only, so two different
accounts/tenants/configs reached through the *same backend name* (two Azure
tenants both using `azure`, or a real vs. LocalStack AWS endpoint both using
`aws`) shared cache files. v5 inserts a short **identity fingerprint** as the
top-level path component:

```
<cache_dir>/<fingerprint>/<backend>/<vault>/<entry>.json
```

The fingerprint is a SHA-256 (truncated to 16 hex chars) over a stable,
deterministic serialization of:

- the resolved global config path,
- the effective backend name, and
- the active backend's identity fields — Azure: tenant + subscription; AWS:
  region + profile + endpoint URL; Local: resolved store path.

It contains **no secret material, timestamps, or randomness**, so it is
identical between a foreground command and the `xv cache refresh` child process
it spawns. Entries written under one identity simply **miss** under another.

The entry filenames also carry a version suffix (`secrets-list-v5.json`,
`files-list-v5.json`, `files-list-recursive-v5.json`); a pre-v5 file cannot be
read at the new path, so upgrading is a clean miss rather than a cross-identity
hit.

### Corruption quarantine

If a cache entry cannot be parsed as JSON on read, `xv` treats it as a miss
**and** renames the bad file aside to `<name>.corrupt` instead of leaving it to
be re-read and silently rewritten forever. `xv cache status` reports a
`Quarantined` count and lists the offending files. Delete them any time with
`xv cache clear`.

### Strict / fail-loud mode

Cache failures are normally logged at `debug!` so a broken cache never disturbs
a command. Set `XV_CACHE_STRICT=1` (or `true`) to promote cache-failure logs —
write errors, quarantine events, permission-tightening failures — to `warn!`,
which is useful in CI. This changes logging **only**: `xv ls`/`vault list`/
`file list` still succeed with a broken cache, and reads still degrade to a
live fetch. Loud, never fatal.

### `xv doctor` cache check

`xv doctor` prints a `Cache:` line and reports whether the cache tree uses
private modes, is free of `.corrupt` files, and carries only the current (v5)
layout. It is advisory only and does not change doctor's exit status — a
degraded cache is non-fatal and self-heals on the next command (modes are
re-tightened, missed entries are re-fetched).

## What the cache never does

- It never stores secret **values** — only listing metadata (names, folders,
  groups, timestamps, and similar). This boundary is deliberate and must not be
  widened.
- It never encrypts its contents at rest (deliberately deferred); protection is
  via filesystem permissions and per-identity isolation.

## Related

- Config recovery and the cache check: [`doctor.md`](doctor.md)
- Backends and identity: [`backends.md`](backends.md)
- Historical design: [`superpowers/specs/2026-03-19-cache-feature-design.md`](superpowers/specs/2026-03-19-cache-feature-design.md)
