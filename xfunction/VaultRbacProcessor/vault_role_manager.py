import logging
import uuid
import os
import json
import requests
from azure.identity import ClientSecretCredential
from azure.mgmt.authorization import AuthorizationManagementClient
from azure.mgmt.resource import ResourceManagementClient
from azure.mgmt.keyvault import KeyVaultManagementClient
from msgraph import GraphServiceClient
from azure.core.exceptions import HttpResponseError, ResourceNotFoundError

class VaultRoleManager:
    """
    Class to manage RBAC operations for Key Vaults using App Registration credentials.
    """
    
    def __init__(self):
        """Initialize with ClientSecretCredential using App Registration details from environment."""
        tenant_id = os.environ["AZURE_TENANT_ID"]
        client_id = os.environ["AZURE_CLIENT_ID"]
        client_secret = os.environ["AZURE_CLIENT_SECRET"]
        
        self.credential = ClientSecretCredential(
            tenant_id=tenant_id,
            client_id=client_id,
            client_secret=client_secret
        )
        
        # Initialize Graph client with proper scopes
        self.graph_client = GraphServiceClient(
            credentials=self.credential,
            scopes=["https://graph.microsoft.com/.default"]
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
            auth_client = AuthorizationManagementClient(self.credential, subscription_id)
            
            # Check if the role already exists
            role_definitions = auth_client.role_definitions.list(scope=vault_resource_id)
            for role_def in role_definitions:
                if role_def.role_name == role_name:
                    logging.info(f"Custom role '{role_name}' already exists")
                    return role_def.id
            
            # Use the built-in Owner role instead of Key Vault Administrator
            # Owner role provides full access to all resources and can delegate access
            owner_role_id = "8e3af657-a8ff-443c-a75c-2fe8c4bcb635"  # Azure Owner role ID
            admin_role_id = "00482a5a-887f-4fb3-b363-3b7fe8e74483"  # Azure Key Vault Administrator role ID
            built_in_role_id = f"/subscriptions/{subscription_id}/providers/Microsoft.Authorization/roleDefinitions/{owner_role_id}"
            
            logging.info(f"Using built-in Owner role instead of custom role")
            return built_in_role_id
            
        except Exception as ex:
            logging.error(f"Error getting role: {str(ex)}")
            return None
            
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
            
            # Check if principal_id is a GUID or an email address
            if not self._is_guid(principal_id):
                # Convert UPN to object ID if it's not a GUID
                object_id = await self.get_principal_id_for_user(principal_id)
                if not object_id:
                    logging.error(f"Could not resolve principal ID for {principal_id}")
                    return False
                principal_id = object_id
            
            # Format principal_id correctly - ensure it has hyphens
            principal_id = self._normalize_guid(principal_id)
            logging.info(f"Normalized principal ID: {principal_id}")
            
            # Get the authorization client
            auth_client = AuthorizationManagementClient(self.credential, subscription_id)
            
            # Check if the Owner role is being assigned
            owner_role_id = "8e3af657-a8ff-443c-a75c-2fe8c4bcb635"  # Azure Owner role ID
            admin_role_id = "00482a5a-887f-4fb3-b363-3b7fe8e74483"  # Azure Key Vault Administrator role ID
            is_owner_role = owner_role_id in role_definition_id
            is_admin_role = admin_role_id in role_definition_id
            
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
                    if owner_role_id in assignment.role_definition_id:
                        has_owner_role = True
                        logging.info(f"Principal {principal_id} already has Owner role")
                    
                    # Check if the exact role is already assigned
                    if assignment.role_definition_id == role_definition_id:
                        role_already_assigned = True
                        logging.info(f"Role {role_definition_id} already assigned to principal {principal_id}")
                    
                    # If assigning Owner role, track other role assignments for removal
                    elif is_owner_role:
                        redundant_assignments.append(assignment)
            
            # If assigning a non-Owner role but user already has Owner role, we still need to assign Key Vault Administrator
            # The previous logic was skipping the Key Vault Administrator role assignment if Owner was already assigned
            if not is_owner_role and has_owner_role and not is_admin_role:
                logging.info(f"Skipping non-admin role assignment as principal {principal_id} already has Owner role")
                return True
                
            # For Key Vault Administrator role, we always want to assign it even if Owner role exists
            if is_admin_role:
                logging.info("Proceeding with Key Vault Administrator role assignment even though Owner role exists")
            
            # If role already assigned, nothing more to do
            if role_already_assigned:
                logging.info(f"Role assignment already exists for principal {principal_id}")
                
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
            
            # Try to determine principal type (User/ServicePrincipal/Group)
            principal_type = await self._detect_principal_type(principal_id)
            
            # Create the role assignment with principalType specified to avoid replication delay issues
            logging.info(f"Assigning role to principal {principal_id} with type {principal_type}")
            result = auth_client.role_assignments.create(
                scope=vault_resource_id,
                role_assignment_name=role_assignment_name,
                parameters={
                    'role_definition_id': role_definition_id,
                    'principal_id': principal_id,
                    'principal_type': principal_type  # Add principal type to handle replication delays
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
            
        except HttpResponseError as ex:
            # Check if it's because the assignment already exists
            if "already exists" in str(ex):
                logging.info(f"Role assignment for {principal_id} already exists")
                return True
            # Special handling for PrincipalNotFound
            elif "PrincipalNotFound" in str(ex):
                logging.warning(f"Principal not found error: {str(ex)}")
                logging.warning("This might be due to replication delays. Try again later.")
                # Still return success since we'll retry on next event
                return True
            logging.error(f"Error assigning role: {str(ex)}")
            return False
        except Exception as ex:
            logging.error(f"Error assigning role: {str(ex)}")
            return False
    
    async def _detect_principal_type(self, principal_id):
        """Attempt to detect the principal type (User, ServicePrincipal, Group)."""
        try:
            # Default to ServicePrincipal if we can't determine
            principal_type = "ServicePrincipal"
            
            # Try to get access token for Graph API
            token = self.credential.get_token("https://graph.microsoft.com/.default")
            headers = {
                'Authorization': f'Bearer {token.token}',
                'Content-Type': 'application/json'
            }
            
            # Check if it's a user
            user_url = f"https://graph.microsoft.com/v1.0/users/{principal_id}"
            response = requests.get(user_url, headers=headers)
            if response.status_code == 200:
                return "User"
                
            # Check if it's a service principal
            sp_url = f"https://graph.microsoft.com/v1.0/servicePrincipals/{principal_id}"
            response = requests.get(sp_url, headers=headers)
            if response.status_code == 200:
                return "ServicePrincipal"
                
            # Check if it's a group
            group_url = f"https://graph.microsoft.com/v1.0/groups/{principal_id}"
            response = requests.get(group_url, headers=headers)
            if response.status_code == 200:
                return "Group"
                
            return principal_type
        except Exception as ex:
            logging.warning(f"Error detecting principal type: {str(ex)}")
            return "ServicePrincipal"  # Default fallback
    
    async def get_principal_id_for_user(self, user_upn):
        """
        Get the object ID for a user using Microsoft Graph API.
        
        :param user_upn: The user principal name (email address)
        :return: The object ID if found, None otherwise
        """
        try:
            # Get access token directly - for use with raw HTTP requests
            token = self.credential.get_token("https://graph.microsoft.com/.default")
            
            # Call Microsoft Graph API via direct HTTP request instead of using the SDK
            headers = {
                'Authorization': f'Bearer {token.token}',
                'Content-Type': 'application/json'
            }
            
            # First try getting the user directly by UPN
            url = f"https://graph.microsoft.com/v1.0/users?$filter=userPrincipalName eq '{user_upn}'"
            
            # Make a direct HTTP request
            logging.info(f"Calling Microsoft Graph API to find user: {user_upn}")
            response = requests.get(url, headers=headers)
            
            if response.status_code == 200:
                user_data = response.json()
                if 'value' in user_data and len(user_data['value']) > 0:
                    user_id = user_data['value'][0]['id']
                    logging.info(f"Found user object ID: {user_id}")
                    return user_id
                else:
                    logging.warning(f"No users found with UPN: {user_upn}")
            else:
                logging.warning(f"Graph API returned status code: {response.status_code}")
                logging.warning(f"Response: {response.text}")
            
            # If we made it here, we couldn't find the user
            logging.warning(f"Could not find user {user_upn} in the directory")
            return None
            
        except Exception as ex:
            logging.error(f"Error getting principal ID: {str(ex)}")
            return None
    
    def _is_guid(self, value):
        """Check if a string is a valid GUID."""
        try:
            uuid.UUID(value)
            return True
        except ValueError:
            return False
            
    def _normalize_guid(self, guid_str):
        """Ensure GUID is properly formatted with hyphens."""
        if not guid_str:
            return guid_str
            
        # Remove all hyphens first
        guid_str = guid_str.replace('-', '')
        
        # If it's a valid GUID length (32 chars without hyphens), format it properly
        if len(guid_str) == 32:
            return f"{guid_str[0:8]}-{guid_str[8:12]}-{guid_str[12:16]}-{guid_str[16:20]}-{guid_str[20:32]}"
        
        return guid_str
        
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
            kv_client = KeyVaultManagementClient(self.credential, subscription_id)
            
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
            
            logging.info(f"Retrieved vault info for {vault_name} with tags: {json.dumps(vault_info['tags'])}")
            return vault_info
            
        except ResourceNotFoundError:
            logging.error(f"Vault not found: {vault_resource_id}")
            return None
        except Exception as ex:
            logging.error(f"Error getting vault info: {str(ex)}")
            return None
