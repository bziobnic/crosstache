# Backend Trait Checklist

> **Status:** 🟢 Living document. Phase 2 (backend trait extraction) shipped in v0.8.0 (#165); phase 3 (AWS backend) in v0.10.0-rc.1. New backends should append entries below.

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

## Phase 2 — legacy manager retirement (US-101 trait surface)

Trait methods added so the CLI can retire `SecretManager`/`VaultManager`
construction (see `2026-07-05-multi-backend-workspace-convergence-design.md`,
Phase 2 + ADR-2). All are optional (default `Unsupported`/empty); only the
Azure backend implements them — `LocalVaultBackend`/`AwsVaultBackend` inherit
the defaults.

- `VaultBackend::grant_secret_access(vault, secret, principal, level)` /
  `revoke_secret_access(vault, secret, principal)` /
  `list_secret_access(vault, secret)` — **new**. Secret-scoped RBAC; the Azure
  impl delegates to `AzureVaultOperations` (`vault/operations.rs`), supplying
  the resource group from config. Replaces the CLI's direct
  `VaultManager`/`AzureVaultOperations` secret-RBAC calls.
- `VaultBackend::resolve_principal(user) -> object_id` — **new**. Replaces the
  CLI's direct `AzureAuthProvider::resolve_user_to_object_id`. Azure delegates
  through a new default `VaultOperations::resolve_user_to_object_id`, keeping
  the Graph API call inside the Azure layer.
- `VaultBackend::resolve_principal_ids(ids) -> {id: (name, email)}` — **new**.
  Enriches access listings with display names; replaces the resolution half of
  `VaultManager::resolve_and_filter_roles` (the include-all FILTER stays
  presentation-side in the CLI).
- Already sufficient (no change needed): `VaultBackend::create_vault` takes a
  `VaultCreateRequest` that already carries `location`/`resource_group`/`sku`/…
  (so the CLI can create vaults without `VaultManager::create_vault_with_setup`
  — config-derived defaults applied caller-side), and `VaultBackend::get_vault`
  returns `VaultProperties.enable_rbac_authorization` (what
  `check_vault_rbac_mode` needs). `list_vaults` sources subscription/RG from
  config inside the Azure adapter.
- Non-trait: `parse_connection_string`'s described-component builder moved off
  `SecretManager` to the standalone `secret::manager::parse_connection_components`
  (a pure parser + `connection_string_key_description`).
