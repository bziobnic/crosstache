# Listing cache

`xv` caches **listing** responses on disk so repeated `xv ls`, `xv vault list`,
`xv file list`, and `xv group list` do not re-hit the backend. It never caches
secret **values** (`xv get`, `xv totp`, `xv run`). Cache I/O errors are logged
at debug level and never fail the command.

- [What is cached](#what-is-cached)
- [Commands](#commands)
- [Configuration](#configuration)
- [Layout](#layout)
- [Freshness](#freshness)
- [Pitfalls](#pitfalls)

## What is cached

| Listing | Cache key | Payload |
|---------|-----------|---------|
| `xv ls` / `xv group list` | `secrets:<backend>:<vault>` | `SecretSummary` rows (name, note, folder, groups, tags, expiry, …) |
| `xv vault list` (no `--resource-group`) | `vaults` | `VaultSummary` rows |
| `xv file list` | `files:<backend>:<vault>` or `files-recursive:<backend>:<vault>` | file listing |

Not cached: `xv get`, `xv ls --deleted`, `xv ls --expiring` / `--expired`
(those need a per-secret detail fetch), `xv vault list --resource-group`,
and every write.

Writes invalidate the matching secrets-list or file-list entry for the
`(backend, vault)` that was actually mutated — using the registry backend
name, not the backend *kind*, so a named backend does not leave the wrong
file behind.

## Commands

```bash
xv ls --no-cache                 # one-shot bypass (also on vault list / file list / group list)
xv cache status                  # directory, enabled, TTL, entries, fresh/stale
xv cache clear                   # wipe the whole cache directory
xv cache clear --vault myvault   # that vault name on every backend
xv config set cache_enabled false
xv config set cache_ttl_secs 300
```

`xv cache refresh --key <key>` is a **hidden** internal command. The listing
path spawns it as a detached child when an entry is past 80% of its TTL
(stale-while-revalidate). It writes nothing to stdout. Operators should use
`--no-cache` or `xv cache clear`, not `refresh`.

## Configuration

| Knob | Default | Notes |
|------|---------|-------|
| `cache_enabled` | `true` | Config file or `CACHE_ENABLED=true\|1` |
| `cache_ttl_secs` | `900` (15 min) | Config (`cache_ttl` alias), `CACHE_TTL`, or `xv config set cache_ttl_secs N`. `0` disables the cache entirely |
| `XV_CACHE_DIR` | OS cache dir + `xv` | Linux `~/.cache/xv`, macOS `~/Library/Caches/xv`. Empty/unset falls through; last resort `/tmp/xv` |

`cache_enabled = false` **or** `cache_ttl_secs = 0` turns every cache operation
into a no-op. `--no-cache` is then redundant.

A relative `XV_CACHE_DIR` is resolved against the process cwd, which moves
under `cd`. Use an absolute path.

## Layout

```text
$XV_CACHE_DIR/
├── vaults-list.json
└── <backend>/<vault>/
    ├── secrets-list-v4.json
    ├── files-list.json
    └── files-list-recursive.json
```

Secrets-list and file-list keys are scoped per `(backend, vault)` so two
workspace entries that share a vault *name* on different backends never
collide. The secrets-list filename is versioned (`v4`); a schema change to
`SecretSummary` bumps it so old files miss instead of deserializing with
empty tags or missing expiry.

`xv cache clear --vault NAME` walks every backend directory for that vault
name. If a vault is literally named like a backend (`local`, `azure`, a
`named_backends` key), the clearer refuses to delete the backend directory
as a whole and only removes that vault's nested entries.

## Freshness

- An entry older than the TTL is a miss; the command fetches and rewrites it.
- Past **80% of TTL**, the current read still returns the cached data and a
  background `xv cache refresh` is spawned. One atomic lock file per entry
  (create-new, 60 s stale reclaim) prevents two refreshes of the same key.
- Writes delete the matching entry immediately.

## Pitfalls

**Vault-list cache is not per-backend.** `vaults-list.json` sits at the cache
root and is filled from the *active* backend. After `xv config set backend …`,
`xv vault list` can show the previous backend's vaults until the TTL expires.
Use `xv vault list --no-cache` or `xv cache clear` after switching.

**Tab completion is cache-only.** Hidden `__complete-secrets` /
`__complete-folders` never talk to a backend — a cold cache means no
completions on Tab. Run `xv ls` once to warm it.

**Metadata, not values — but still sensitive.** Cached JSON includes secret
names, notes, groups, folders, and tags (`xv-type`, `f.*`). It does not
include secret values. Treat the cache directory as listing metadata; it is
not written `0600`.

**`--expiring` / `--expired` skip the cache** because they fetch per-secret
detail. `--deleted` uses a different API and is never cached.

**External edits look stale** until TTL, `--no-cache`, or a write from this
`xv` process (which invalidates). Another machine, the Azure portal, or a
second `xv` with a different `XV_CACHE_DIR` will not see your invalidations.
