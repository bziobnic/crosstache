import logging
import uuid
import os
from azure.identity import ClientSecretCredential
from azure.mgmt.authorization import AuthorizationManagementClient
from azure.mgmt.keyvault import KeyVaultManagementClient
from azure.core.exceptions import HttpResponseError, ResourceNotFoundError, ResourceExistsError
from config import OWNER_ROLE_ID, KEY_VAULT_ADMINISTRATOR_ROLE_ID, AZURE_CONNECTION_TIMEOUT, AZURE_READ_TIMEOUT
from utils.azure_helpers import is_guid, normalize_guid, retry_async

class VaultRoleManager:
    """
    Class to manage RBAC operations for Key Vaults using App Registration credentials.
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

    async def create_or_get_custom_role(self, vault_resource_id, role_name):
        """
        Create a custom role for vault management if it doesn't exist,
        or get the existing one.

        :param vault_resource_id: The resource ID of the Key Vault
        :param role_name: The name for the custom role
        :return: The role definition ID if successful, None otherwise
        """
        try:
            # Extract subscription ID from the vault resource ID
            # Format: /subscriptions/{subId}/resourceGroups/{rg}/providers/Microsoft.KeyVault/vaults/{name}
            parts = vault_resource_id.split('/')
            subscription_id = parts[2]

            # Get the authorization client
            auth_client = AuthorizationManagementClient(self.credential, subscription_id, connection_timeout=AZURE_CONNECTION_TIMEOUT, read_timeout=AZURE_READ_TIMEOUT)

            # Check if the role already exists
            role_definitions = auth_client.role_definitions.list(scope=vault_resource_id)
            for role_def in role_definitions:
                if role_def.role_name == role_name:
                    logging.info(f"Custom role '{role_name}' already exists")
                    return role_def.id

            # Use the built-in Owner role instead of Key Vault Administrator
            # Owner role provides full access to all resources and can delegate access
            built_in_role_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{OWNER_ROLE_ID}"

            logging.info(f"Using built-in Owner role instead of custom role")
            return built_in_role_id

        except Exception as ex:
            logging.error(f"Error getting role: {str(ex)}")
            return None

    async def caller_has_rbac_authority(self, vault_resource_id, principal_id):
        """Require existing RBAC delegation authority in addition to mutable tags."""
        parts = vault_resource_id.split('/')
        if len(parts) < 9 or parts[1].lower() != 'subscriptions' or parts[3].lower() != 'resourcegroups':
            return False
        subscription_id = parts[2]
        resource_group = parts[4]
        scopes = [
            vault_resource_id,
            f"/subscriptions/{subscription_id}/resourceGroups/{resource_group}",
            f"/subscriptions/{subscription_id}",
        ]
        authority_roles = {
            OWNER_ROLE_ID.lower(),
            "f58310d9-a9f6-439a-9e8d-f62e7b41a168",
            "18d7d88d-d35e-4fb5-a5c3-7773c20a72d9",
        }
        try:
            auth_client = AuthorizationManagementClient(
                self.credential,
                subscription_id,
                connection_timeout=AZURE_CONNECTION_TIMEOUT,
                read_timeout=AZURE_READ_TIMEOUT,
            )
            for scope in scopes:
                assignments = auth_client.role_assignments.list_for_scope(
                    scope=scope,
                    filter="atScope()",
                )
                for assignment in assignments:
                    role_id = (assignment.role_definition_id or "").rsplit('/', 1)[-1].lower()
                    if (
                        (assignment.principal_id or "").lower() == principal_id.lower()
                        and role_id in authority_roles
                    ):
                        return True
            return False
        except Exception as ex:
            logging.error("Failed to verify caller RBAC authority: %s", type(ex).__name__)
            return False

    @retry_async
    async def assign_role_to_user(self, vault_resource_id, role_definition_id, principal_id):
        """
        Assign the role to the specified user or service principal.
        If assigning Owner role, removes any redundant role assignments.

        :param vault_resource_id: The resource ID of the Key Vault
        :param role_definition_id: The role definition ID to assign
        :param principal_id: The object ID or UPN of the principal to assign the role to
        :return: True if successful, False otherwise
        """
        try:
            # Extract subscription ID from the vault resource ID
            parts = vault_resource_id.split('/')
            subscription_id = parts[2]

            # The authenticated endpoint supplies an Entra object ID. Refuse
            # alternate identifiers instead of performing directory-wide Graph lookups.
            if not is_guid(principal_id):
                logging.error("Authenticated principal ID is not a GUID")
                return False

            # Format principal_id correctly - ensure it has hyphens
            principal_id = normalize_guid(principal_id)
            logging.info("Validated and normalized authenticated principal ID")

            # Get the authorization client
            auth_client = AuthorizationManagementClient(self.credential, subscription_id, connection_timeout=AZURE_CONNECTION_TIMEOUT, read_timeout=AZURE_READ_TIMEOUT)

            # Check if the Owner role is being assigned
            is_owner_role = OWNER_ROLE_ID in role_definition_id
            is_admin_role = KEY_VAULT_ADMINISTRATOR_ROLE_ID in role_definition_id

            logging.info(f"Role assignment check - is_owner_role: {is_owner_role}, is_admin_role: {is_admin_role}")

            # Get all role assignments at this scope
            assignments = list(auth_client.role_assignments.list_for_scope(
                scope=vault_resource_id,
                filter="atScope()"
            ))

            # Check if user already has the role being assigned
            role_already_assigned = False
            has_owner_role = False
            redundant_assignments = []

            for assignment in assignments:
                if assignment.principal_id == principal_id:
                    # Check if user already has Owner role
                    if OWNER_ROLE_ID in assignment.role_definition_id:
                        has_owner_role = True
                        logging.info("Authenticated principal already has Owner role")

                    # Check if the exact role is already assigned
                    if assignment.role_definition_id == role_definition_id:
                        role_already_assigned = True
                        logging.info("Requested role is already assigned to authenticated principal")

                    # If assigning Owner role, track other role assignments for removal
                    elif is_owner_role:
                        redundant_assignments.append(assignment)

            # If assigning a non-Owner role but user already has Owner role, we still need to assign Key Vault Administrator
            if not is_owner_role and has_owner_role and not is_admin_role:
                logging.info("Skipping redundant non-admin role assignment")
                return True

            # For Key Vault Administrator role, we always want to assign it even if Owner role exists
            if is_admin_role:
                logging.info("Proceeding with Key Vault Administrator role assignment even though Owner role exists")

            # If role already assigned, nothing more to do
            if role_already_assigned:
                logging.info("Requested role assignment already exists")

                # If Owner role is assigned but there are redundant roles, remove them
                if is_owner_role and redundant_assignments:
                    logging.info(f"Removing {len(redundant_assignments)} redundant role assignments")
                    for assignment in redundant_assignments:
                        try:
                            auth_client.role_assignments.delete(
                                scope=vault_resource_id,
                                role_assignment_name=assignment.name
                            )
                            logging.info(f"Removed redundant role assignment: {assignment.id}")
                        except Exception as ex:
                            logging.warning(f"Error removing redundant role: {str(ex)}")

                return True

            # Create a unique name for the role assignment
            role_assignment_name = str(uuid.uuid4())

            principal_type = "User"

            # Create the role assignment with principalType specified to avoid replication delay issues
            logging.info("Assigning requested role to authenticated principal")
            result = auth_client.role_assignments.create(
                scope=vault_resource_id,
                role_assignment_name=role_assignment_name,
                parameters={
                    'role_definition_id': role_definition_id,
                    'principal_id': principal_id,
                    'principal_type': principal_type
                }
            )

            # If Owner role was assigned, remove redundant role assignments
            if is_owner_role and redundant_assignments:
                logging.info(f"Removing {len(redundant_assignments)} redundant role assignments")
                for assignment in redundant_assignments:
                    try:
                        auth_client.role_assignments.delete(
                            scope=vault_resource_id,
                            role_assignment_name=assignment.name
                        )
                        logging.info(f"Removed redundant role assignment: {assignment.id}")
                    except Exception as ex:
                        logging.warning(f"Error removing redundant role: {str(ex)}")

            logging.info(f"Role assignment created: {result.id}")
            return True

        except ResourceExistsError:
            logging.info("Requested role assignment already exists")
            return True
        except HttpResponseError as ex:
            error_code = getattr(getattr(ex, 'error', None), 'code', None) or ""
            if error_code == "RoleAssignmentExists" or "already exists" in str(ex):
                logging.info("Requested role assignment already exists")
                return True
            elif error_code == "PrincipalNotFound" or "PrincipalNotFound" in str(ex):
                logging.error(f"Principal not found error (replication delay or invalid principal): {str(ex)}")
                return False
            logging.error(f"Error assigning role: {str(ex)}")
            return False
        except Exception as ex:
            logging.error(f"Error assigning role: {str(ex)}")
            return False

    async def get_vault_info(self, vault_resource_id):
        """
        Get information about a Key Vault, including its tags.

        :param vault_resource_id: The resource ID of the Key Vault
        :return: Dictionary containing vault information including tags, or None if not found
        """
        try:
            # Extract subscription ID, resource group, and vault name from the vault resource ID
            # Format: /subscriptions/{subId}/resourceGroups/{rg}/providers/Microsoft.KeyVault/vaults/{name}
            parts = vault_resource_id.split('/')
            if len(parts) < 9 or parts[6] != 'Microsoft.KeyVault' or parts[7] != 'vaults':
                logging.error(f"Invalid vault resource ID format: {vault_resource_id}")
                return None

            subscription_id = parts[2]
            resource_group = parts[4]
            vault_name = parts[8]

            logging.info(f"Getting vault info for {vault_name} in resource group {resource_group}")

            # Get the Key Vault Management client
            kv_client = KeyVaultManagementClient(self.credential, subscription_id, connection_timeout=AZURE_CONNECTION_TIMEOUT, read_timeout=AZURE_READ_TIMEOUT)

            # Get the vault
            vault = kv_client.vaults.get(resource_group, vault_name)

            # Extract relevant information
            vault_info = {
                "id": vault.id,
                "name": vault.name,
                "location": vault.location,
                "tags": vault.tags or {},
                "properties": {
                    "tenant_id": vault.properties.tenant_id,
                    "vault_uri": vault.properties.vault_uri,
                    "enabled_for_deployment": vault.properties.enabled_for_deployment,
                    "enabled_for_disk_encryption": vault.properties.enabled_for_disk_encryption,
                    "enabled_for_template_deployment": vault.properties.enabled_for_template_deployment,
                    "enable_rbac_authorization": vault.properties.enable_rbac_authorization,
                }
            }

            logging.info("Retrieved required vault authorization metadata")
            return vault_info

        except ResourceNotFoundError:
            logging.error(f"Vault not found: {vault_resource_id}")
            return None
        except Exception as ex:
            logging.error(f"Error getting vault info: {str(ex)}")
            return None
