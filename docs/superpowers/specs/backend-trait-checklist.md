# Backend Trait Checklist

Soft-commitment audit: every PR adds a line below for each new manager-method
read-surface call introduced. The end-of-quarter audit will turn this
into the spec for phase 2 ("Backend Pluggability Initiative").

## v0.6.1 — `xv find`

- `SecretManager::list_secrets(vault_name, group_filter)` — used for the
  single-vault find path. Cacheable; current call ignores `group_filter`
  (always None).
- `VaultManager::vault_ops().list_vaults(subscription_id, resource_group)`
  — used by `--all-vaults`. Per-call; no cache.

These are read-only and align with the soft-commitment goal of keeping
the read surface small and well-known before phase 2.

## v0.7.0 — `xv scan`

- `SecretManager::secret_ops().list_secrets(vault, group_filter)` — already on checklist; reused.
- `SecretManager::secret_ops().get_secret(vault, name, include_value)` — **new entry**. Used to fetch values into the scan engine. Per-call; concurrency bounded by tokio Semaphore (default 10).
- `VaultManager::vault_ops().list_vaults(subscription_id, resource_group)` — already on checklist; reused for `--all-vaults`.

The `get_secret` method is the only NEW read-surface entry this plan introduces.

## v0.7.0 — `xv tui`

- `SecretManager::secret_ops().list_secrets(vault, group_filter)` — already on checklist.
- `SecretManager::secret_ops().get_secret(vault, name, include_value)` — already on checklist.
- `SecretManager::secret_ops().get_secret_versions(vault, name)` — **new entry**. Used by the History overlay.
- `VaultManager::vault_ops().list_vaults(...)` — already on checklist.
- Audit-events read path — **placeholder in v0.7.0**; real integration deferred to v0.7.1.

`get_secret_versions` is the only NEW read-surface entry this plan introduces. The audit-events read path is an open trait-design question for phase 2.
