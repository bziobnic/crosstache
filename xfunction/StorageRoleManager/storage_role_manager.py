import logging
import uuid
import os
import json
import requests
from azure.identity import ClientSecretCredential
from azure.mgmt.authorization import AuthorizationManagementClient
from azure.mgmt.resource import ResourceManagementClient
from azure.mgmt.storage import StorageManagementClient
from msgraph import GraphServiceClient
from azure.core.exceptions import HttpResponseError, ResourceNotFoundError

class StorageRoleManager:
    """
    Class to manage RBAC operations for Azure Storage Accounts using App Registration credentials.
    """
    
    # Azure Storage built-in role IDs
    STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID = "17d1049b-9a84-46fb-8f53-869881c3d3ab"
    STORAGE_BLOB_DATA_OWNER_ROLE_ID = "b7e6dc6d-f1e8-4753-8033-0f276bb0955c" 
    STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID = "ba92f5b4-2d11-453d-a403-e96b0029c9fe"
    
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
        
    async def discover_associated_storage_accounts(self, vault_resource_id):
        """
        Discover storage accounts associated with a given vault using multiple strategies.
        
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
            vault_name = parts[8]
            
            logging.info(f"Discovering storage accounts for vault {vault_name} in resource group {resource_group}")
            
            # Initialize Storage Management client
            storage_client = StorageManagementClient(self.credential, subscription_id)
            
            # Strategy 1: Find storage accounts in the same resource group
            storage_accounts = []
            try:
                accounts_in_rg = list(storage_client.storage_accounts.list_by_resource_group(resource_group))
                logging.info(f"Found {len(accounts_in_rg)} storage accounts in resource group {resource_group}")
                
                for account in accounts_in_rg:
                    # Strategy 2: Check for tag-based association
                    if account.tags and account.tags.get('AssociatedVault') == vault_name:
                        logging.info(f"Found storage account {account.name} linked via AssociatedVault tag")
                        storage_accounts.append(account.id)
                        continue
                    
                    # Strategy 3: Check naming convention
                    if self._matches_naming_convention(account.name, vault_name):
                        logging.info(f"Found storage account {account.name} linked via naming convention")
                        storage_accounts.append(account.id)
                        continue
                
                # If no specific associations found, include all storage accounts in the resource group
                if not storage_accounts and accounts_in_rg:
                    logging.info(f"No explicit associations found, including all storage accounts in resource group")
                    storage_accounts = [account.id for account in accounts_in_rg]
                
            except Exception as ex:
                logging.error(f"Error listing storage accounts in resource group: {str(ex)}")
                return []
            
            logging.info(f"Discovered {len(storage_accounts)} associated storage accounts")
            return storage_accounts
            
        except Exception as ex:
            logging.error(f"Error discovering storage accounts: {str(ex)}")
            return []
    
    def _matches_naming_convention(self, storage_name, vault_name):
        """
        Check if storage account name matches naming convention with vault name.
        
        :param storage_name: Name of the storage account
        :param vault_name: Name of the vault
        :return: True if matches naming convention
        """
        # Convert to lowercase for comparison (Azure storage names are lowercase)
        storage_lower = storage_name.lower()
        vault_lower = vault_name.lower()
        
        # Common patterns: {vault}storage, {vault}stor, stor{vault}
        patterns = [
            f"{vault_lower}storage",
            f"{vault_lower}stor", 
            f"stor{vault_lower}",
            f"{vault_lower}st"
        ]
        
        for pattern in patterns:
            if pattern in storage_lower:
                return True
        
        return False
    
    async def get_storage_role_assignments_for_vault_role(self, vault_role_id):
        """
        Get the storage role assignments that correspond to a vault role.
        
        :param vault_role_id: The vault role ID (Owner or Key Vault Administrator)
        :return: List of storage role IDs to assign
        """
        # Key Vault Owner role ID
        owner_role_id = "8e3af657-a8ff-443c-a75c-2fe8c4bcb635"
        # Key Vault Administrator role ID  
        admin_role_id = "00482a5a-887f-4fb3-b363-3b7fe8e74483"
        
        if owner_role_id in vault_role_id:
            # Owner gets both Storage Account Contributor and Storage Blob Data Owner
            return [
                self.STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID,
                self.STORAGE_BLOB_DATA_OWNER_ROLE_ID
            ]
        elif admin_role_id in vault_role_id:
            # Administrator gets Storage Blob Data Contributor
            return [self.STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID]
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
            
            logging.info(f"Assigning storage roles to {storage_name} for principal {principal_id}")
            
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
            if not self._is_guid(principal_id):
                # Convert UPN to object ID if needed
                object_id = await self.get_principal_id_for_user(principal_id)
                if not object_id:
                    logging.error(f"Could not resolve principal ID for {principal_id}")
                    return False
                principal_id = object_id
            
            principal_id = self._normalize_guid(principal_id)
            
            # Get authorization client
            auth_client = AuthorizationManagementClient(self.credential, subscription_id)
            
            # Check if role already assigned
            assignments = list(auth_client.role_assignments.list_for_scope(
                scope=storage_resource_id,
                filter="atScope()"
            ))
            
            for assignment in assignments:
                if (assignment.principal_id == principal_id and 
                    assignment.role_definition_id == role_definition_id):
                    logging.info(f"Role {role_id} already assigned to principal {principal_id} for storage account")
                    return True
            
            # Detect principal type
            principal_type = await self._detect_principal_type(principal_id)
            
            # Create role assignment
            role_assignment_name = str(uuid.uuid4())
            
            logging.info(f"Assigning storage role {role_id} to principal {principal_id}")
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
            
        except HttpResponseError as ex:
            if "already exists" in str(ex):
                logging.info(f"Storage role assignment already exists")
                return True
            elif "PrincipalNotFound" in str(ex):
                logging.warning(f"Principal not found error for storage role: {str(ex)}")
                return True
            logging.error(f"Error assigning storage role: {str(ex)}")
            return False
        except Exception as ex:
            logging.error(f"Error assigning storage role: {str(ex)}")
            return False
    
    async def _detect_principal_type(self, principal_id):
        """Attempt to detect the principal type (User, ServicePrincipal, Group)."""
        try:
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
                
            return "ServicePrincipal"  # Default fallback
        except Exception as ex:
            logging.warning(f"Error detecting principal type: {str(ex)}")
            return "ServicePrincipal"
    
    async def get_principal_id_for_user(self, user_upn):
        """
        Get the object ID for a user using Microsoft Graph API.
        
        :param user_upn: The user principal name (email address)
        :return: The object ID if found, None otherwise
        """
        try:
            token = self.credential.get_token("https://graph.microsoft.com/.default")
            headers = {
                'Authorization': f'Bearer {token.token}',
                'Content-Type': 'application/json'
            }
            
            url = f"https://graph.microsoft.com/v1.0/users?$filter=userPrincipalName eq '{user_upn}'"
            
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
            storage_client = StorageManagementClient(self.credential, subscription_id)
            
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
            
            logging.info(f"Retrieved storage account info for {storage_name} with tags: {json.dumps(account_info['tags'])}")
            return account_info
            
        except ResourceNotFoundError:
            logging.error(f"Storage account not found: {storage_resource_id}")
            return None
        except Exception as ex:
            logging.error(f"Error getting storage account info: {str(ex)}")
            return None