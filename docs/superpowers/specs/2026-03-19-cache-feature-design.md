# Cache Feature Design Spec

**Date:** 2026-03-19
**Status:** Approved
**Version:** 0.1

## Overview

A client-side caching layer for expensive listing operations (`xv ls`, `xv vault list`, `xv file list`). Cache entries are stored as flat JSON files organized by vault, with configurable TTL and automatic background refresh before expiration. Write operations eagerly invalidate relevant cache entries. Caching is purely additive — cache failures never cause command failures.

## Goals

- Reduce redundant Azure API calls for repeated listing operations
- Provide instant responses from cache when data is fresh
- Keep cached data current through background refresh and eager invalidation on writes
- Give users full control: enable/disable, configure TTL, bypass per-command, clear manually

## Non-Goals

- Caching `xv get` or other targeted read operations (fast enough without caching)
- Offline mode or disconnected operation
- Distributed cache or shared cache across machines
- Caching write operation responses

## Module Structure

New module at `src/cache/`:

```
src/cache/
├── mod.rs          # Public API re-exports
├── manager.rs      # CacheManager: get, set, invalidate, clear, status
├── models.rs       # CacheEntry, CacheKey, CacheEntryType, CacheStatus
└── refresh.rs      # Background refresh logic (detached child process)
```

Follows the existing module pattern used by `blob/`, `secret/`, and `vault/`.

## Cache Directory Layout

```
~/.cache/xv/
├── <vault-name>/
│   ├── secrets-list.json       # xv ls (full unfiltered listing)
│   └── files-list.json         # xv file list
└── vaults-list.json            # xv vault list (subscription-scoped)
```

Cache directory location follows platform conventions via the `dirs` crate (`dirs::cache_dir()`). On macOS this resolves to `~/Library/Caches/xv/`; on Linux, `~/.cache/xv/`.

## Data Models

### CacheEntry

Each JSON file on disk contains a `CacheEntry`:

```rust
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheEntry<T: Serialize + DeserializeOwned> {
    pub created_at: DateTime<Utc>,
    pub ttl_secs: u64,
    pub vault_name: Option<String>,
    pub entry_type: CacheEntryType,
    pub data: T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEntryType {
    SecretsList,
    VaultList,
    FileList,
}
```

### CacheKey

Determines the file path for a cache entry:

```rust
#[derive(Debug, Clone)]
pub enum CacheKey {
    SecretsList { vault_name: String },
    VaultList,
    FileList { vault_name: String },
}
```

Path resolution:
- `SecretsList { vault_name: "myvault" }` → `<cache_dir>/myvault/secrets-list.json`
- `VaultList` → `<cache_dir>/vaults-list.json`
- `FileList { vault_name: "myvault" }` → `<cache_dir>/myvault/files-list.json`

`CacheKey` implements `Display` for use as the `--key` argument in background refresh (e.g., `secrets:myvault`, `vaults`, `files:myvault`).

### CacheStatus

Returned by `xv cache status`:

```rust
pub struct CacheStatus {
    pub cache_dir: PathBuf,
    pub enabled: bool,
    pub ttl_secs: u64,
    pub entry_count: usize,
    pub total_size_bytes: u64,
    pub entries: Vec<CacheEntryInfo>,
}

pub struct CacheEntryInfo {
    pub key: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub is_stale: bool,
}
```

## CacheManager API

```rust
pub struct CacheManager {
    cache_dir: PathBuf,
    enabled: bool,
    ttl_secs: u64,
}
```

Constructed from `Config` at the start of command execution.

### `get<T: DeserializeOwned>(key: &CacheKey) -> Option<T>`

1. If `!self.enabled`, return `None`.
2. Resolve `key` to file path.
3. If file doesn't exist, return `None`.
4. Read and deserialize `CacheEntry<T>`. On failure (corrupt JSON, I/O error), log at debug level, delete the file, return `None`.
5. If expired (`created_at + ttl_secs < now`): delete the file, return `None`.
6. If within refresh window (80% of TTL elapsed): trigger background refresh, return cached `data`.
7. Return cached `data`.

### `set<T: Serialize>(key: &CacheKey, data: &T)`

1. If `!self.enabled`, return silently.
2. Resolve `key` to file path.
3. Create parent directories via `std::fs::create_dir_all`.
4. Serialize `CacheEntry` to JSON.
5. Atomic write: create a temporary file **in the same directory** as the target (not via `tempfile::NamedTempFile` which defaults to `/tmp`), write contents, then `std::fs::rename` to the target path. Same-directory temp file ensures the rename is an atomic same-filesystem operation. Prevents partial reads by concurrent processes.
6. On any failure, log at debug level and return silently.

### `invalidate(key: &CacheKey)`

1. Resolve `key` to file path.
2. Delete the file if it exists. Also delete any associated `.lock` file.
3. On failure, log at debug level.

### `invalidate_vault(vault_name: &str)`

Delete the entire `<cache_dir>/<vault_name>/` directory. Convenience method for write operations that may affect multiple cache entries within a vault.

### `clear(vault: Option<&str>)`

- If `vault` is `Some`, delete `<cache_dir>/<vault>/`.
- If `vault` is `None`, delete all contents of `<cache_dir>/`.

### `status() -> CacheStatus`

Walk the cache directory, collect metadata on each entry file, and return a `CacheStatus` summary.

## Configuration

### New Config fields

Added to the `Config` struct in `src/config/settings.rs`:

```rust
/// Whether client-side caching is enabled for listing operations
#[tabled(rename = "Cache Enabled")]
#[serde(default = "default_cache_enabled")]
pub cache_enabled: bool,          // default: true

/// Cache time-to-live in seconds (0 to disable)
#[tabled(rename = "Cache TTL")]
#[serde(default = "default_cache_ttl_secs")]
pub cache_ttl_secs: u64,          // default: 900 (15 minutes)
```

The existing `cache_ttl: Duration` field is replaced by `cache_ttl_secs: u64` for consistency with how `clipboard_timeout` is handled — simpler for serde serialization and config file representation. The existing `CACHE_TTL` env var continues to work with the same semantics (integer seconds), so this is a non-breaking change.

### Configuration sources (standard hierarchy)

| Source | Key | Example |
|--------|-----|---------|
| Config file | `cache_enabled`, `cache_ttl_secs` | `cache_enabled = true` |
| Env var | `CACHE_ENABLED`, `CACHE_TTL` | `CACHE_TTL=900` |
| CLI | `xv config set` | `xv config set cache_ttl_secs 900` |

### Special value: `cache_ttl_secs = 0`

Treated as "cache disabled" — entries expire immediately on read.

## CLI Changes

### New `--no-cache` flag

Added to the three cacheable listing commands:

```rust
// In Commands::List
#[arg(long, help = "Bypass cache and fetch fresh data")]
no_cache: bool,

// In VaultCommands::List
#[arg(long, help = "Bypass cache and fetch fresh data")]
no_cache: bool,

// In FileCommands::List
#[arg(long, help = "Bypass cache and fetch fresh data")]
no_cache: bool,
```

When `--no-cache` is set, the command skips cache lookup and does not write to cache.

### New `xv cache` subcommand

```rust
#[derive(Debug, Subcommand)]
enum CacheCommands {
    /// Remove cached data
    Clear {
        /// Clear cache for a specific vault only
        #[arg(long)]
        vault: Option<String>,
    },
    /// Show cache status and statistics
    Status,
}
```

### Hidden internal subcommand: `xv cache refresh`

```rust
/// Internal: refresh a cache entry in the background (not shown in --help)
#[command(hide = true)]
Refresh {
    #[arg(long)]
    key: String,
}
```

Used exclusively by the background refresh mechanism. Not user-facing.

## Read Flow

```
xv ls [--group G] [--expiring] [--expired] [--no-cache]

1. Construct CacheManager from Config
2. If cache_enabled AND NOT --no-cache:
   a. cache_manager.get(SecretsList { vault_name })
   b. If Some(data):
      - Apply client-side filters (--group, --expiring, --expired)
      - Display results
      - Return
   c. If None: fall through to step 3
3. Call Azure API (existing code path)
4. Display results
5. If cache_enabled AND NOT --no-cache:
   - cache_manager.set(SecretsList { vault_name }, &data)
```

The cache stores the **full unfiltered listing**. Flags like `--group`, `--expiring`, and `--expired` are applied as client-side filters on cached data, identical to how they work on live API responses today.

Same pattern applies to `xv vault list` and `xv file list`.

## Write Invalidation

Write operations invalidate relevant cache entries after successful execution:

### Secret write operations → invalidate `SecretsList`

Commands: `set`, `delete`, `update`, `purge`, `rotate`, `move`, `copy`, `import`, `restore`, `rollback`

```rust
// After successful write:
cache_manager.invalidate(&CacheKey::SecretsList { vault_name });
```

For `move` and `copy` which involve two vaults, both source and destination vault caches are invalidated.

### Vault write operations → invalidate `VaultList`

Commands: `vault create`, `vault delete`, `vault purge`, `vault restore`, `vault update`

```rust
cache_manager.invalidate(&CacheKey::VaultList);
```

### File write operations → invalidate `FileList`

Commands: `file upload`, `file delete`

```rust
cache_manager.invalidate(&CacheKey::FileList { vault_name });
```

## Background Refresh

### Refresh window

A cache entry enters the refresh window when **80% of its TTL has elapsed**. For the default 900-second TTL, this is at 720 seconds (12 minutes).

### Mechanism

When `CacheManager::get` detects an entry in the refresh window, it spawns a detached child process:

```rust
use std::process::{Command, Stdio};

fn trigger_background_refresh(key: &CacheKey) -> Result<()> {
    Command::new(std::env::current_exe()?)
        .args(["cache", "refresh", "--key", &key.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}
```

The `xv cache refresh --key <key>` hidden subcommand:

1. Acquires a lock file (`<cache-file>.lock`)
2. Loads config and authenticates with Azure using the same credential chain (the child process inherits the same environment, config file, and credential context as any normal `xv` invocation)
3. Fetches fresh listing data
4. Writes updated cache entry via `CacheManager::set`
5. Removes the lock file
6. Exits

### Why a detached child process?

The main `xv` process needs to exit promptly after displaying results. A `tokio::spawn` task would be killed when the tokio runtime shuts down. A detached OS process runs independently of the parent's lifecycle.

### Staleness guard

Prevents multiple concurrent refreshes for the same cache key:

1. Before refreshing, check for `<cache-file>.lock`
2. If the lock file exists and is less than 60 seconds old, exit immediately (another refresh is in progress)
3. If the lock file exists but is older than 60 seconds, treat it as stale (previous refresh crashed), delete it, and proceed
4. Create the lock file before starting the refresh
5. Delete the lock file after completion

### Failure handling

If the background refresh fails for any reason (auth error, network timeout, API error), it exits silently. The existing cache entry remains valid until its TTL expires naturally. No user-visible errors are produced. The next manual invocation of the listing command will fetch fresh data normally.

## Error Handling

Cache operations are purely additive — they never cause a working command to fail.

| Scenario | Behavior |
|----------|----------|
| Cache read fails (corrupt JSON, I/O error) | Debug log, treat as cache miss, fetch from Azure |
| Cache write fails (disk full, permissions) | Debug log, command succeeds normally without caching |
| Cache directory missing | Created on first write via `create_dir_all` |
| Background refresh fails | Exits silently, stale entry serves until TTL expires |
| `cache_enabled = false` | Cache never read or written, `--no-cache` is a no-op |
| `cache_ttl_secs = 0` | Entries expire immediately, equivalent to disabled |

All cache errors are logged at `tracing::debug!` level, visible only when `DEBUG=true`.

## Edge Cases

- **First run / empty cache**: all operations are cache misses, behavior identical to current behavior
- **Concurrent reads**: safe — reads are non-destructive file operations
- **Concurrent writes to same cache entry**: safe — atomic write (temp file + rename) prevents partial reads
- **Concurrent background refreshes**: prevented by lock file mechanism
- **Vault names with special characters**: vault names are already sanitized by Azure Key Vault naming rules (alphanumeric and hyphens only), safe for filesystem paths
- **User switches contexts**: cache is per-vault, not per-context. Different contexts pointing to the same vault share cache entries (correct behavior — same data). Different vaults have separate entries.

## Testing Strategy

### Unit tests (`src/cache/manager.rs`)

- `get` returns `None` for missing entry
- `get` returns data for valid, non-expired entry
- `get` returns `None` and deletes file for expired entry
- `get` returns data and triggers refresh signal for entries in refresh window
- `set` creates directory structure and writes valid JSON
- `set` performs atomic write (verify no partial files)
- `invalidate` deletes specific cache file
- `invalidate_vault` deletes entire vault cache directory
- `clear` with vault name deletes only that vault's cache
- `clear` without vault name deletes all cache entries
- `status` returns accurate counts and sizes
- Corrupt JSON file handled gracefully (treated as miss)
- I/O permission errors handled gracefully

### Unit tests (`src/cache/refresh.rs`)

- Lock file creation and detection
- Stale lock file cleanup (>60 seconds old)
- Lock file prevents concurrent refresh

### Integration tests (`tests/cache_tests.rs`)

- `xv ls` creates cache file in expected location
- Second `xv ls` reads from cache (verify via timing or file mtime)
- `xv ls --no-cache` bypasses cache
- `xv set` invalidates secrets list cache
- `xv cache clear` removes all cache files
- `xv cache clear --vault myvault` removes only that vault's cache
- `xv cache status` displays accurate information
- `xv config set cache_enabled false` disables caching

All unit tests use `tempfile::TempDir` for isolated cache directories. No new crate dependencies required — `serde_json`, `chrono`, `tempfile`, and `std::fs` are already in the project.

## Dependencies

No new crate dependencies. All required functionality is covered by existing dependencies:

- `serde` / `serde_json`: serialization
- `chrono`: timestamps
- `tempfile`: atomic writes and test isolation
- `dirs`: platform-appropriate cache directory
- `tracing`: debug logging
- `std::process::Command`: background refresh child process
- `std::fs`: file I/O, directory management
