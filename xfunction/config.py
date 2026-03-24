"""
Centralized configuration for Azure role definition IDs.

All Azure built-in role IDs used across the xfunction application are defined here
as named constants. This prevents duplication and ensures consistency when role IDs
are referenced in multiple modules.

Reference: https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles
"""

# ---------------------------------------------------------------------------
# Azure general built-in roles
# ---------------------------------------------------------------------------

# Owner: Full access to all resources, including the right to delegate access.
OWNER_ROLE_ID = "8e3af657-a8ff-443c-a75c-2fe8c4bcb635"

# ---------------------------------------------------------------------------
# Azure Key Vault built-in roles
# ---------------------------------------------------------------------------

# Key Vault Administrator: Perform all data plane operations on a key vault
# and all objects in it, including certificates, keys, and secrets.
KEY_VAULT_ADMINISTRATOR_ROLE_ID = "00482a5a-887f-4fb3-b363-3b7fe8e74483"

# ---------------------------------------------------------------------------
# Azure Storage built-in roles
# ---------------------------------------------------------------------------

# Storage Account Contributor: Lets you manage storage accounts, including
# accessing storage account keys which provide full access to storage data.
STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID = "17d1049b-9a84-46fb-8f53-869881c3d3ab"

# Storage Blob Data Owner: Full access to Azure Storage blob containers and
# data, including assigning POSIX access control.
STORAGE_BLOB_DATA_OWNER_ROLE_ID = "b7e6dc6d-f1e8-4753-8033-0f276bb0955b"

# Storage Blob Data Contributor: Read, write, and delete Azure Storage
# containers and blobs.
STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID = "ba92f5b4-2d11-453d-a403-e96b0029c9fe"

# ---------------------------------------------------------------------------
# Azure SDK client timeout configuration (seconds)
# ---------------------------------------------------------------------------

# Connection timeout for Azure SDK management clients
AZURE_CONNECTION_TIMEOUT = 30

# Read timeout for Azure SDK management clients
AZURE_READ_TIMEOUT = 120
