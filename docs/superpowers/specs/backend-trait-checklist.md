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
