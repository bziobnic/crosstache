"""
Unit tests for the StorageRoleManager class.
Tests storage account discovery, role mapping, and assignment logic.
"""

import unittest
from unittest.mock import Mock, patch, AsyncMock
import asyncio
import os
import sys
import json

# Add the parent directory to the path so we can import our modules
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from StorageRoleManager.storage_role_manager import StorageRoleManager
from utils.azure_helpers import is_guid, normalize_guid
from config import (
    OWNER_ROLE_ID,
    KEY_VAULT_ADMINISTRATOR_ROLE_ID,
    STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID,
    STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID,
    STORAGE_BLOB_DATA_OWNER_ROLE_ID,
)

class TestStorageRoleManager(unittest.TestCase):
    """Test cases for StorageRoleManager functionality."""
    
    def setUp(self):
        """Set up test fixtures before each test method."""
        # Mock environment variables
        os.environ["AZURE_TENANT_ID"] = "test-tenant-id"
        os.environ["AZURE_CLIENT_ID"] = "test-client-id"
        os.environ["AZURE_CLIENT_SECRET"] = "test-client-secret"
        
        # Mock Azure clients to avoid actual Azure calls during testing
        with patch('StorageRoleManager.storage_role_manager.ClientSecretCredential'):
            self.storage_manager = StorageRoleManager()
    
    def test_vault_role_to_storage_role_mapping(self):
        """Test mapping of vault roles to storage roles."""
        # Test Owner role mapping
        owner_role_def = f"/subscriptions/test/providers/Microsoft.Authorization/roleDefinitions/{OWNER_ROLE_ID}"
        owner_storage_roles = asyncio.run(
            self.storage_manager.get_storage_role_assignments_for_vault_role(owner_role_def)
        )

        expected_owner_roles = [
            STORAGE_ACCOUNT_CONTRIBUTOR_ROLE_ID,
            STORAGE_BLOB_DATA_OWNER_ROLE_ID,
        ]
        self.assertEqual(owner_storage_roles, expected_owner_roles)

        # Test Administrator role mapping
        admin_role_def = f"/subscriptions/test/providers/Microsoft.Authorization/roleDefinitions/{KEY_VAULT_ADMINISTRATOR_ROLE_ID}"
        admin_storage_roles = asyncio.run(
            self.storage_manager.get_storage_role_assignments_for_vault_role(admin_role_def)
        )

        expected_admin_roles = [STORAGE_BLOB_DATA_CONTRIBUTOR_ROLE_ID]
        self.assertEqual(admin_storage_roles, expected_admin_roles)
        
        # Test unknown role
        unknown_role_id = "/subscriptions/test/providers/Microsoft.Authorization/roleDefinitions/unknown-role"
        unknown_storage_roles = asyncio.run(
            self.storage_manager.get_storage_role_assignments_for_vault_role(unknown_role_id)
        )
        self.assertEqual(unknown_storage_roles, [])
    
    def test_guid_validation(self):
        """Test GUID validation and normalization."""
        # Test valid GUID
        valid_guid = "12345678-1234-1234-1234-123456789abc"
        self.assertTrue(is_guid(valid_guid))

        # Test invalid GUID
        invalid_guid = "not-a-guid"
        self.assertFalse(is_guid(invalid_guid))

        # Test GUID normalization
        guid_without_hyphens = "12345678123412341234123456789abc"
        normalized = normalize_guid(guid_without_hyphens)
        expected = "12345678-1234-1234-1234-123456789abc"
        self.assertEqual(normalized, expected)
    
    def test_discover_uses_only_exact_administrator_configured_ids(self):
        vault_resource_id = "/subscriptions/test-sub/resourceGroups/test-rg/providers/Microsoft.KeyVault/vaults/testvault"
        exact = "/subscriptions/test-sub/resourceGroups/test-rg/providers/Microsoft.Storage/storageAccounts/exactaccount"
        outside = "/subscriptions/test-sub/resourceGroups/other/providers/Microsoft.Storage/storageAccounts/outside"
        bindings = json.dumps({vault_resource_id: [exact, outside, "not-a-resource-id", exact]})
        with patch.dict(os.environ, {"VAULT_STORAGE_BINDINGS": bindings}):
            storage_accounts = asyncio.run(
                self.storage_manager.discover_associated_storage_accounts(vault_resource_id)
            )
        self.assertEqual(storage_accounts, [exact])

    def test_discover_skips_unconfigured_accounts(self):
        vault_resource_id = "/subscriptions/test-sub/resourceGroups/test-rg/providers/Microsoft.KeyVault/vaults/testvault"
        with patch.dict(os.environ, {"VAULT_STORAGE_BINDINGS": "{}"}):
            storage_accounts = asyncio.run(
                self.storage_manager.discover_associated_storage_accounts(vault_resource_id)
            )
        self.assertEqual(storage_accounts, [])

    @patch('StorageRoleManager.storage_role_manager.StorageManagementClient')
    def test_discover_storage_accounts_invalid_vault_id(self, mock_storage_client):
        """Test storage account discovery with invalid vault resource ID."""
        invalid_vault_id = "invalid-resource-id"
        
        storage_accounts = asyncio.run(
            self.storage_manager.discover_associated_storage_accounts(invalid_vault_id)
        )
        
        # Should return empty list for invalid resource ID
        self.assertEqual(storage_accounts, [])
        
        # Should not call Azure API
        mock_storage_client.assert_not_called()


class TestStorageRoleManagerAsync(unittest.IsolatedAsyncioTestCase):
    """Async test cases for StorageRoleManager functionality."""
    
    async def asyncSetUp(self):
        """Set up async test fixtures."""
        os.environ["AZURE_TENANT_ID"] = "test-tenant-id"
        os.environ["AZURE_CLIENT_ID"] = "test-client-id"
        os.environ["AZURE_CLIENT_SECRET"] = "test-client-secret"
        
        with patch('StorageRoleManager.storage_role_manager.ClientSecretCredential'):
            self.storage_manager = StorageRoleManager()
    
    @patch('StorageRoleManager.storage_role_manager.AuthorizationManagementClient')
    async def test_assign_storage_roles_empty_list(self, mock_auth_client):
        """Test storage role assignment with empty storage account list."""
        result = await self.storage_manager.assign_storage_roles_to_user(
            [], "test-role-def-id", "test-principal-id"
        )
        
        # Should return empty dict for empty list
        self.assertEqual(result, {})
        
        # Should not create auth client
        mock_auth_client.assert_not_called()
    
    async def test_assign_storage_roles_unknown_vault_role(self):
        """Test storage role assignment with unknown vault role."""
        storage_accounts = ["/subscriptions/test/resourceGroups/rg/providers/Microsoft.Storage/storageAccounts/test"]
        unknown_role = "/subscriptions/test/providers/Microsoft.Authorization/roleDefinitions/unknown"
        
        result = await self.storage_manager.assign_storage_roles_to_user(
            storage_accounts, unknown_role, "test-principal-id"
        )
        
        # Should return empty dict when no storage roles mapped
        self.assertEqual(result, {})


if __name__ == '__main__':
    # Run the tests
    unittest.main()
