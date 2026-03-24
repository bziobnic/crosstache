"""
Unit tests for shared Azure helper utilities.
Tests GUID handling, principal detection, retry logic, and Graph API integration.
"""

import unittest
from unittest.mock import Mock, patch, AsyncMock, MagicMock
import asyncio
import os
import sys

# Add the parent directory to the path so we can import our modules
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from azure.core.exceptions import HttpResponseError
from utils.azure_helpers import (
    is_guid,
    normalize_guid,
    detect_principal_type,
    get_principal_id_for_user,
    retry_async,
    _is_retryable,
)


class TestGuidHelpers(unittest.TestCase):
    """Test cases for GUID validation and normalization."""

    def test_valid_guid_with_hyphens(self):
        self.assertTrue(is_guid("12345678-1234-1234-1234-123456789abc"))

    def test_valid_guid_without_hyphens(self):
        self.assertTrue(is_guid("12345678123412341234123456789abc"))

    def test_invalid_guid(self):
        self.assertFalse(is_guid("not-a-guid"))

    def test_empty_string(self):
        self.assertFalse(is_guid(""))

    def test_none_value(self):
        self.assertFalse(is_guid(None))

    def test_normalize_guid_adds_hyphens(self):
        result = normalize_guid("12345678123412341234123456789abc")
        self.assertEqual(result, "12345678-1234-1234-1234-123456789abc")

    def test_normalize_guid_preserves_hyphens(self):
        guid = "12345678-1234-1234-1234-123456789abc"
        result = normalize_guid(guid)
        self.assertEqual(result, guid)

    def test_normalize_guid_empty_string(self):
        self.assertEqual(normalize_guid(""), "")

    def test_normalize_guid_none(self):
        self.assertIsNone(normalize_guid(None))

    def test_normalize_guid_invalid_length(self):
        result = normalize_guid("short")
        self.assertEqual(result, "short")


class TestRetryLogic(unittest.TestCase):
    """Test cases for the retry decorator and retryable detection."""

    def test_429_is_retryable(self):
        ex = HttpResponseError(message="Too Many Requests")
        ex.status_code = 429
        self.assertTrue(_is_retryable(ex))

    def test_500_is_retryable(self):
        ex = HttpResponseError(message="Internal Server Error")
        ex.status_code = 500
        self.assertTrue(_is_retryable(ex))

    def test_503_is_retryable(self):
        ex = HttpResponseError(message="Service Unavailable")
        ex.status_code = 503
        self.assertTrue(_is_retryable(ex))

    def test_501_is_not_retryable(self):
        ex = HttpResponseError(message="Not Implemented")
        ex.status_code = 501
        self.assertFalse(_is_retryable(ex))

    def test_400_is_not_retryable(self):
        ex = HttpResponseError(message="Bad Request")
        ex.status_code = 400
        self.assertFalse(_is_retryable(ex))

    def test_404_is_not_retryable(self):
        ex = HttpResponseError(message="Not Found")
        ex.status_code = 404
        self.assertFalse(_is_retryable(ex))

    def test_value_error_is_not_retryable(self):
        self.assertFalse(_is_retryable(ValueError("bad value")))


class TestRetryDecorator(unittest.IsolatedAsyncioTestCase):
    """Async test cases for the retry_async decorator."""

    async def test_success_no_retry(self):
        """Function succeeds on first call — no retries needed."""
        call_count = 0

        @retry_async
        async def succeed():
            nonlocal call_count
            call_count += 1
            return "ok"

        result = await succeed()
        self.assertEqual(result, "ok")
        self.assertEqual(call_count, 1)

    async def test_non_retryable_error_raises_immediately(self):
        """Non-retryable error is raised without retrying."""
        call_count = 0

        @retry_async
        async def fail_permanently():
            nonlocal call_count
            call_count += 1
            raise ValueError("permanent failure")

        with self.assertRaises(ValueError):
            await fail_permanently()
        self.assertEqual(call_count, 1)

    @patch('utils.azure_helpers.asyncio.sleep', new_callable=AsyncMock)
    async def test_retryable_error_retries_then_succeeds(self, mock_sleep):
        """Retryable error triggers retry, then succeeds."""
        call_count = 0

        @retry_async
        async def fail_then_succeed():
            nonlocal call_count
            call_count += 1
            if call_count < 3:
                ex = HttpResponseError(message="throttled")
                ex.status_code = 429
                raise ex
            return "recovered"

        result = await fail_then_succeed()
        self.assertEqual(result, "recovered")
        self.assertEqual(call_count, 3)
        self.assertEqual(mock_sleep.call_count, 2)

    @patch('utils.azure_helpers.asyncio.sleep', new_callable=AsyncMock)
    async def test_retryable_error_exhausts_retries(self, mock_sleep):
        """Retryable error exhausts all retries then raises."""
        call_count = 0

        @retry_async
        async def always_throttled():
            nonlocal call_count
            call_count += 1
            ex = HttpResponseError(message="throttled")
            ex.status_code = 429
            raise ex

        with self.assertRaises(HttpResponseError):
            await always_throttled()
        self.assertEqual(call_count, 4)  # 1 initial + 3 retries


class TestDetectPrincipalType(unittest.IsolatedAsyncioTestCase):
    """Async test cases for principal type detection."""

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_detects_user(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"@odata.type": "#microsoft.graph.user"}
        mock_get.return_value = mock_response

        result = await detect_principal_type(Mock(), "test-id")
        self.assertEqual(result, "User")

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_detects_service_principal(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"@odata.type": "#microsoft.graph.servicePrincipal"}
        mock_get.return_value = mock_response

        result = await detect_principal_type(Mock(), "test-id")
        self.assertEqual(result, "ServicePrincipal")

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_detects_group(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"@odata.type": "#microsoft.graph.group"}
        mock_get.return_value = mock_response

        result = await detect_principal_type(Mock(), "test-id")
        self.assertEqual(result, "Group")

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_defaults_to_service_principal_on_failure(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 404
        mock_get.return_value = mock_response

        result = await detect_principal_type(Mock(), "test-id")
        self.assertEqual(result, "ServicePrincipal")

    @patch('utils.azure_helpers._get_graph_headers')
    async def test_defaults_to_service_principal_on_exception(self, mock_headers):
        mock_headers.side_effect = Exception("token error")
        result = await detect_principal_type(Mock(), "test-id")
        self.assertEqual(result, "ServicePrincipal")


class TestGetPrincipalIdForUser(unittest.IsolatedAsyncioTestCase):
    """Async test cases for user principal ID lookup."""

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_finds_user(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {
            "value": [{"id": "found-user-id"}]
        }
        mock_get.return_value = mock_response

        result = await get_principal_id_for_user(Mock(), "user@example.com")
        self.assertEqual(result, "found-user-id")

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_returns_none_when_not_found(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"value": []}
        mock_get.return_value = mock_response

        result = await get_principal_id_for_user(Mock(), "nobody@example.com")
        self.assertIsNone(result)

    @patch('utils.azure_helpers.requests.get')
    @patch('utils.azure_helpers._get_graph_headers')
    async def test_returns_none_on_api_error(self, mock_headers, mock_get):
        mock_headers.return_value = {"Authorization": "Bearer test"}
        mock_response = Mock()
        mock_response.status_code = 403
        mock_get.return_value = mock_response

        result = await get_principal_id_for_user(Mock(), "user@example.com")
        self.assertIsNone(result)

    @patch('utils.azure_helpers._get_graph_headers')
    async def test_returns_none_on_exception(self, mock_headers):
        mock_headers.side_effect = Exception("credential error")
        result = await get_principal_id_for_user(Mock(), "user@example.com")
        self.assertIsNone(result)


if __name__ == '__main__':
    unittest.main()
