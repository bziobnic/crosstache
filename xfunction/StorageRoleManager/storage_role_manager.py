import logging
import uuid
import os
import json
from azure.identity import ClientSecretCredential
from azure.mgmt.authorization import AuthorizationManagementClient
from azure.mgmt.storage import StorageManagementClient
from azure.core.exceptions import HttpResponseError, ResourceNotFoundError, ResourceExistsError
from config import (
    OWNER_ROLE_ID,
    KEY_VAULT_ADMINISTRATOR_ROLE_ID,
    STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID,
    STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID,
    STORAGE_BLOB_DATA_OWNER_ROLE_ID,
    AZURE_CONNECTION_TIMEOUT,
    AZURE_READ_TIMEOUT,
)
from utils.azure_helpers import is_guid, normalize_guid, retry_async

class StorageRoleManager:
    """
    Class to manage RBAC operations for Azure Storage Accounts using App Registration credentials.
    """

    def __init__(self):
        """Initialize with ClientSecretCredential using App Registration details from environment."""
        required_vars = ["AZURE_TENANT_ID", "AZURE_CLIENT_ID", "AZURE_CLIENT_SECRET"]
        missing = [v for v in required_vars if v not in os.environ]
        if missing:
            raise ValueError(f"Missing required environment variables: {', '.join(missing)}")

        tenant_id = os.environ["AZURE_TENANT_ID"]
        client_id = os.environ["AZURE_CLIENT_ID"]
        client_secret = os.environ["AZURE_CLIENT_SECRET"]

        self.credential = ClientSecretCredential(
            tenant_id=tenant_id,
            client_id=client_id,
            client_secret=client_secret
        )

    async def discover_associated_storage_accounts(self, vault_resource_id):
        """
        Resolve administrator-configured exact storage resource IDs for a vault.

        :param vault_resource_id: The resource ID of the Key Vault
        :return: List of storage account resource IDs
        """
        try:
            # Extract subscription ID and resource group from vault resource ID
            # Format: /subscriptions/{subId}/resourceGroups/{rg}/providers/Microsoft.KeyVault/vaults/{name}
            parts = vault_resource_id.split('/')
            if len(parts) < 9 or parts[6] != 'Microsoft.KeyVault' or parts[7] != 'vaults':
                logging.error(f"Invalid vault resource ID format: {vault_resource_id}")
                return []

            subscription_id = parts[2]
            resource_group = parts[4]
            raw_bindings = os.environ.get("VAULT_STORAGE_BINDINGS", "{}")
            bindings = json.loads(raw_bindings)
            if not isinstance(bindings, dict):
                logging.error("VAULT_STORAGE_BINDINGS must be a JSON object")
                return []

            configured = next(
                (value for key, value in bindings.items() if key.rstrip('/').lower() == vault_resource_id.rstrip('/').lower()),
                [],
            )
            if not isinstance(configured, list):
                logging.error("Configured storage binding must be a list of exact resource IDs")
                return []

            required_prefix = (
                f"/subscriptions/{subscription_id}/resourceGroups/{resource_group}"
                "/providers/Microsoft.Storage/storageAccounts/"
            )
            storage_accounts = []
            for resource_id in configured:
                if not isinstance(resource_id, str):
                    logging.error("Ignoring non-string storage resource binding")
                    continue
                account_name = resource_id[len(required_prefix):] if resource_id.lower().startswith(required_prefix.lower()) else ""
                if not account_name or '/' in account_name:
                    logging.error("Ignoring storage binding outside the validated resource group")
                    continue
                if resource_id.lower() not in {item.lower() for item in storage_accounts}:
                    storage_accounts.append(resource_id)

            logging.info("Resolved %d administrator-configured storage binding(s)", len(storage_accounts))
            return storage_accounts

        except Exception as ex:
            logging.error(f"Error discovering storage accounts: {str(ex)}")
            return []

    async def get_storage_role_assignments_for_vault_role(self, vault_role_id):
        """
        Get the storage role assignments that correspond to a vault role.

        :param vault_role_id: The vault role ID (Owner or Key Vault Administrator)
        :return: List of storage role IDs to assign
        """
        if OWNER_ROLE_ID in vault_role_id:
            # Owner gets both Storage Account Contributor and Storage Blob Data Owner
            return [
                STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID,
                STORAGE_BLOB_DATA_OWNER_ROLE_ID,
            ]
        elif KEY_VAULT_ADMINISTRATOR_ROLE_ID in vault_role_id:
            # Administrator gets Storage Blob Data Contributor
            return [STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID]
        else:
            logging.warning(f"Unknown vault role ID: {vault_role_id}")
            return []

    async def assign_storage_roles_to_user(self, storage_resource_ids, vault_role_definition_id, principal_id):
        """
        Assign appropriate storage roles to a user based on their vault role.

        :param storage_resource_ids: List of storage account resource IDs
        :param vault_role_definition_id: The vault role definition ID being assigned
        :param principal_id: The principal ID to assign roles to
        :return: Dictionary with assignment results per storage account
        """
        if not storage_resource_ids:
            logging.info("No storage accounts to assign roles to")
            return {}

        # Get storage roles to assign based on vault role
        storage_role_ids = await self.get_storage_role_assignments_for_vault_role(vault_role_definition_id)
        if not storage_role_ids:
            logging.warning("No storage roles mapped for vault role")
            return {}

        results = {}

        for storage_resource_id in storage_resource_ids:
            storage_name = storage_resource_id.split('/')[-1]
            results[storage_name] = {}

            logging.info(f"Assigning storage roles on associated account {storage_name}")

            # Extract subscription ID for role definition formatting
            subscription_id = storage_resource_id.split('/')[2]

            for role_id in storage_role_ids:
                role_definition_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{role_id}"

                success = await self._assign_role_to_storage_account(
                    storage_resource_id,
                    role_definition_id,
                    principal_id,
                    role_id
                )

                results[storage_name][role_id] = success

        return results

    @retry_async
    async def _assign_role_to_storage_account(self, storage_resource_id, role_definition_id, principal_id, role_id):
        """
        Assign a specific role to a user for a storage account.

        :param storage_resource_id: The storage account resource ID
        :param role_definition_id: The role definition ID to assign
        :param principal_id: The principal ID to assign the role to
        :param role_id: The role ID for logging purposes
        :return: True if successful, False otherwise
        """
        try:
            # Extract subscription ID from storage resource ID
            subscription_id = storage_resource_id.split('/')[2]

            # Normalize principal ID
            if not is_guid(principal_id):
                logging.error("Authenticated principal ID is not a GUID")
                return False

            principal_id = normalize_guid(principal_id)

            # Get authorization client
            auth_client = AuthorizationManagementClient(self.credential, subscription_id, connection_timeout=AZURE_CONNECTION_TIMEOUT, read_timeout=AZURE_READ_TIMEOUT)

            # Check if role already assigned
            assignments = list(auth_client.role_assignments.list_for_scope(
                scope=storage_resource_id,
                filter="atScope()"
            ))

            for assignment in assignments:
                if (assignment.principal_id == principal_id and
                    assignment.role_definition_id == role_definition_id):
                    logging.info("Requested storage role is already assigned")
                    return True

            principal_type = "User"

            # Create role assignment
            role_assignment_name = str(uuid.uuid4())

            logging.info("Assigning requested storage role to authenticated principal")
            result = auth_client.role_assignments.create(
                scope=storage_resource_id,
                role_assignment_name=role_assignment_name,
                parameters={
                    'role_definition_id': role_definition_id,
                    'principal_id': principal_id,
                    'principal_type': principal_type
                }
            )

            logging.info(f"Storage role assignment created: {result.id}")
            return True

        except ResourceExistsError:
            logging.info(f"Storage role assignment already exists")
            return True
        except HttpResponseError as ex:
            error_code = getattr(getattr(ex, 'error', None), 'code', None) or ""
            if error_code == "RoleAssignmentExists" or "already exists" in str(ex):
                logging.info(f"Storage role assignment already exists")
                return True
            elif error_code == "PrincipalNotFound" or "PrincipalNotFound" in str(ex):
                logging.error(f"Principal not found error for storage role (replication delay or invalid principal): {str(ex)}")
                return False
            logging.error(f"Error assigning storage role: {str(ex)}")
            return False
        except Exception as ex:
            logging.error(f"Error assigning storage role: {str(ex)}")
            return False

    async def get_storage_account_info(self, storage_resource_id):
        """
        Get information about a storage account, including its tags.

        :param storage_resource_id: The resource ID of the storage account
        :return: Dictionary containing storage account information, or None if not found
        """
        try:
            # Extract subscription ID, resource group, and storage account name
            # Format: /subscriptions/{subId}/resourceGroups/{rg}/providers/Microsoft.Storage/storageAccounts/{name}
            parts = storage_resource_id.split('/')
            if len(parts) < 9 or parts[6] != 'Microsoft.Storage' or parts[7] != 'storageAccounts':
                logging.error(f"Invalid storage account resource ID format: {storage_resource_id}")
                return None

            subscription_id = parts[2]
            resource_group = parts[4]
            storage_name = parts[8]

            logging.info(f"Getting storage account info for {storage_name} in resource group {resource_group}")

            # Get Storage Management client
            storage_client = StorageManagementClient(self.credential, subscription_id, connection_timeout=AZURE_CONNECTION_TIMEOUT, read_timeout=AZURE_READ_TIMEOUT)

            # Get the storage account
            account = storage_client.storage_accounts.get_properties(resource_group, storage_name)

            # Extract relevant information
            account_info = {
                "id": account.id,
                "name": account.name,
                "location": account.location,
                "tags": account.tags or {},
                "properties": {
                    "creation_time": account.creation_time,
                    "primary_location": account.primary_location,
                    "status_of_primary": account.status_of_primary,
                    "access_tier": account.access_tier,
                    "kind": account.kind,
                    "sku": {
                        "name": account.sku.name,
                        "tier": account.sku.tier
                    } if account.sku else None
                }
            }

            logging.info(f"Retrieved required metadata for associated storage account {storage_name}")
            return account_info

        except ResourceNotFoundError:
            logging.error(f"Storage account not found: {storage_resource_id}")
            return None
        except Exception as ex:
            logging.error(f"Error getting storage account info: {str(ex)}")
            return None
