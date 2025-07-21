import unittest
import json
import os
import sys
import jwt
import asyncio
from datetime import datetime, timedelta
from unittest.mock import AsyncMock, patch, MagicMock

# Add parent directory to path to import function_app
sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import azure.functions as func

# Create mock for jwt module to avoid dependency issues
sys.modules['jwt'] = jwt

from function_app import direct_vault_rbac_processor


class TestDirectVaultRbacProcessor(unittest.TestCase):
    """Test cases for the direct_vault_rbac_processor HTTP trigger function."""

    def setUp(self):
        """Set up test fixtures before each test method."""
        # Create a valid JWT token for testing
        self.valid_token_payload = {
            "oid": "test-user-id",
            "exp": int((datetime.now() + timedelta(hours=1)).timestamp()),
            "iss": "https://login.microsoftonline.com/test-tenant-id/v2.0",
            "aud": "test-client-id",
            "name": "Test User",
            "preferred_username": "testuser@example.com"
        }
        self.valid_token = jwt.encode(self.valid_token_payload, "test-secret")
        
        # Create a valid request body
        self.valid_request_body = {
            "resourceUri": "/subscriptions/test-subscription-id/resourceGroups/Vaults/providers/Microsoft.KeyVault/vaults/test-vault",
            "subscriptionId": "test-subscription-id"
        }
        
        # Mock the VaultRoleManager
        self.mock_manager = AsyncMock()
        self.mock_manager.assign_role_to_user = AsyncMock(return_value=True)
        
        # Mock vault info with creator ID matching the token
        self.mock_vault_info = {
            "id": "/subscriptions/test-subscription-id/resourceGroups/Vaults/providers/Microsoft.KeyVault/vaults/test-vault",
            "name": "test-vault",
            "location": "eastus2",
            "tags": {
                "CreatedBy": "Test User",
                "CreatedByID": "test-user-id",
                "CreatedAt": "2023-03-31T12:00:00Z"
            },
            "properties": {
                "tenant_id": "test-tenant-id",
                "vault_uri": "https://test-vault.vault.azure.net/",
                "enabled_for_deployment": True,
                "enabled_for_disk_encryption": True,
                "enabled_for_template_deployment": True,
                "enable_rbac_authorization": True
            }
        }
        self.mock_manager.get_vault_info = AsyncMock(return_value=self.mock_vault_info)
        
        # Create event loop for running async tests
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)

    def tearDown(self):
        """Clean up after each test method."""
        self.loop.close()

    def _run_async_test(self, coro):
        """Helper method to run an async test function."""
        return self.loop.run_until_complete(coro)

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_valid_request(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with valid token and request body."""
        # Setup mocks
        mock_vault_role_manager_class.return_value = self.mock_manager
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 200)
        resp_body = json.loads(resp.get_body())
        self.assertTrue(resp_body['success'])
        self.assertTrue(resp_body['ownerRoleAssigned'])
        self.assertTrue(resp_body['adminRoleAssigned'])
        
        # Assert manager was called correctly
        self.assertEqual(mock_vault_role_manager_class.call_count, 1)
        self.assertEqual(self.mock_manager.assign_role_to_user.call_count, 2)

    @patch('function_app.VaultRoleManager')
    def test_missing_auth_header(self, mock_vault_role_manager_class):
        """Test with missing Authorization header."""
        # Create request without Authorization header
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 401)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Missing or invalid Authorization header', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_invalid_token_format(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with invalid token format."""
        # Setup mock to raise exception
        mock_jwt_decode.side_effect = Exception("Invalid token format")
        
        # Create request with invalid token format
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': 'Bearer invalid-token-format'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 401)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Invalid token', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_expired_token(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with expired token."""
        # Create expired token and setup mock
        expired_token_payload = self.valid_token_payload.copy()
        expired_token_payload['exp'] = int((datetime.now() - timedelta(hours=1)).timestamp())
        mock_jwt_decode.return_value = expired_token_payload
        
        # Create request with expired token
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 401)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Token expired', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_missing_user_id(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with token missing user ID."""
        # Create token without user ID and setup mock
        no_user_id_payload = self.valid_token_payload.copy()
        del no_user_id_payload['oid']
        mock_jwt_decode.return_value = no_user_id_payload
        
        # Create request with token missing user ID
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 401)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('User identity not found', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_invalid_request_body(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with invalid JSON in request body."""
        # Setup mock
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create request with invalid JSON
        req = func.HttpRequest(
            method='POST',
            body=b'invalid-json',
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 400)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Invalid JSON', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_missing_required_parameters(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test with missing required parameters in request body."""
        # Setup mock
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create request with missing parameters
        req = func.HttpRequest(
            method='POST',
            body=json.dumps({}).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 400)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Missing required parameters', resp_body['error'])
        
        # Assert manager was not called
        mock_vault_role_manager_class.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_role_assignment_failure(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test role assignment failure."""
        # Setup mocks
        mock_jwt_decode.return_value = self.valid_token_payload
        mock_manager = AsyncMock()
        mock_manager.assign_role_to_user = AsyncMock(side_effect=[False, True])
        mock_vault_role_manager_class.return_value = mock_manager
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 500)
        resp_body = json.loads(resp.get_body())
        self.assertFalse(resp_body['success'])
        self.assertFalse(resp_body['ownerRoleAssigned'])
        self.assertTrue(resp_body['adminRoleAssigned'])
        
        # Assert manager was called correctly
        self.assertEqual(mock_vault_role_manager_class.call_count, 1)
        self.assertEqual(mock_manager.assign_role_to_user.call_count, 2)

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_vault_creator_verification_success(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test successful verification of vault creator."""
        # Setup mocks
        mock_vault_role_manager_class.return_value = self.mock_manager
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 200)
        resp_body = json.loads(resp.get_body())
        self.assertTrue(resp_body['success'])
        self.assertTrue(resp_body['isCreator'])
        
        # Assert get_vault_info was called
        self.mock_manager.get_vault_info.assert_called_once_with(self.valid_request_body['resourceUri'])

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_vault_creator_verification_failure(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test failure when user is not the vault creator."""
        # Setup mocks
        mock_vault_role_manager_class.return_value = self.mock_manager
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create vault info with different creator ID
        different_creator_vault_info = self.mock_vault_info.copy()
        different_creator_vault_info['tags'] = {
            "CreatedBy": "Different User",
            "CreatedByID": "different-user-id",
            "CreatedAt": "2023-03-31T12:00:00Z"
        }
        self.mock_manager.get_vault_info = AsyncMock(return_value=different_creator_vault_info)
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 403)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Unauthorized', resp_body['error'])
        self.assertEqual(resp_body['userId'], 'test-user-id')
        self.assertEqual(resp_body['creatorId'], 'different-user-id')
        
        # Assert role assignment was not attempted
        self.mock_manager.assign_role_to_user.assert_not_called()

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_vault_missing_creator_tag(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test when vault doesn't have a CreatedByID tag."""
        # Setup mocks
        mock_vault_role_manager_class.return_value = self.mock_manager
        mock_jwt_decode.return_value = self.valid_token_payload
        
        # Create vault info without CreatedByID tag
        no_creator_vault_info = self.mock_vault_info.copy()
        no_creator_vault_info['tags'] = {
            "CreatedAt": "2023-03-31T12:00:00Z"
        }
        self.mock_manager.get_vault_info = AsyncMock(return_value=no_creator_vault_info)
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response - should still succeed but with a warning logged
        self.assertEqual(resp.status_code, 200)
        resp_body = json.loads(resp.get_body())
        self.assertTrue(resp_body['success'])
        self.assertFalse(resp_body['isCreator'])
        
        # Assert role assignment was attempted
        self.assertEqual(self.mock_manager.assign_role_to_user.call_count, 2)

    @patch('jwt.decode')
    @patch('function_app.VaultRoleManager')
    def test_vault_info_not_found(self, mock_vault_role_manager_class, mock_jwt_decode):
        """Test when vault info cannot be retrieved."""
        # Setup mocks
        mock_vault_role_manager_class.return_value = self.mock_manager
        mock_jwt_decode.return_value = self.valid_token_payload
        self.mock_manager.get_vault_info = AsyncMock(return_value=None)
        
        # Create request
        req = func.HttpRequest(
            method='POST',
            body=json.dumps(self.valid_request_body).encode(),
            url='/api/assign-roles',
            headers={'Authorization': f'Bearer {self.valid_token}'}
        )
        
        # Call function
        resp = self._run_async_test(direct_vault_rbac_processor(req))
        
        # Assert response
        self.assertEqual(resp.status_code, 404)
        resp_body = json.loads(resp.get_body())
        self.assertIn('error', resp_body)
        self.assertIn('Could not retrieve vault information', resp_body['error'])
        
        # Assert role assignment was not attempted
        self.mock_manager.assign_role_to_user.assert_not_called()


if __name__ == '__main__':
    unittest.main()
