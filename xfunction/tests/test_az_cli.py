"""Unit tests for the AzCli wrapper."""

import json
import unittest
from unittest.mock import patch, MagicMock

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.az import AzCli, AzCliError, AzNotFoundError, AzAuthError


class TestAzCliRun(unittest.TestCase):

    @patch("subprocess.run")
    def test_run_success_returns_parsed_json(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"name": "test-rg", "location": "eastus"}),
            stderr="",
        )
        az = AzCli(verbose=False)
        result = az.run("group", "show", "--name", "test-rg")
        self.assertEqual(result["name"], "test-rg")

    @patch("subprocess.run")
    def test_run_non_json_output_returns_raw_string(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0, stdout="true\n", stderr=""
        )
        az = AzCli(verbose=False)
        result = az.run("group", "exists", "--name", "test-rg")
        self.assertEqual(result, "true")

    @patch("subprocess.run")
    def test_run_failure_raises_az_cli_error(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=1,
            stdout="",
            stderr="ERROR: Resource group 'bad' not found.",
        )
        az = AzCli(verbose=False)
        with self.assertRaises(AzCliError) as ctx:
            az.run("group", "show", "--name", "bad")
        self.assertIn("not found", str(ctx.exception))

    @patch("subprocess.run")
    def test_run_not_found_raises_az_not_found_error(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=3,
            stdout="",
            stderr="ERROR: (ResourceNotFound) Resource not found.",
        )
        az = AzCli(verbose=False)
        with self.assertRaises(AzNotFoundError):
            az.run("group", "show", "--name", "missing")

    @patch("subprocess.run")
    def test_run_auth_error_raises_az_auth_error(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=1,
            stdout="",
            stderr="ERROR: AADSTS700016: Please run 'az login'",
        )
        az = AzCli(verbose=False)
        with self.assertRaises(AzAuthError):
            az.run("account", "show")


class TestAzCliRunOrNone(unittest.TestCase):

    @patch("subprocess.run")
    def test_run_or_none_returns_none_on_not_found(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=3, stdout="", stderr="not found"
        )
        az = AzCli(verbose=False)
        result = az.run_or_none("group", "show", "--name", "missing")
        self.assertIsNone(result)

    @patch("subprocess.run")
    def test_run_or_none_returns_result_on_success(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"name": "rg"}),
            stderr="",
        )
        az = AzCli(verbose=False)
        result = az.run_or_none("group", "show", "--name", "rg")
        self.assertEqual(result["name"], "rg")


class TestAzCliSecretRedaction(unittest.TestCase):

    def test_redact_secrets_in_command_string(self):
        az = AzCli(verbose=False)
        cmd = ["az", "ad", "app", "credential", "reset", "--password", "s3cret123"]
        redacted = az._redact_command(cmd)
        self.assertNotIn("s3cret123", redacted)
        self.assertIn("***", redacted)

    def test_redact_client_secret_flag(self):
        az = AzCli(verbose=False)
        cmd = ["az", "functionapp", "config", "--settings", "AZURE_CLIENT_SECRET=abc"]
        redacted = az._redact_command(cmd)
        self.assertNotIn("abc", redacted)


class TestAzCliHelpers(unittest.TestCase):

    @patch("subprocess.run")
    def test_check_login_returns_true_when_logged_in(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"user": {"name": "user@example.com"}}),
            stderr="",
        )
        az = AzCli(verbose=False)
        self.assertTrue(az.check_login())

    @patch("subprocess.run")
    def test_check_login_returns_false_when_not_logged_in(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=1, stdout="", stderr="Please run 'az login'"
        )
        az = AzCli(verbose=False)
        self.assertFalse(az.check_login())

    @patch("subprocess.run")
    def test_get_subscription_returns_id(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"id": "sub-123", "name": "My Sub"}),
            stderr="",
        )
        az = AzCli(verbose=False)
        self.assertEqual(az.get_subscription(), "sub-123")

    @patch("subprocess.run")
    def test_get_tenant_id_returns_id(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"tenantId": "tenant-abc"}),
            stderr="",
        )
        az = AzCli(verbose=False)
        self.assertEqual(az.get_tenant_id(), "tenant-abc")


if __name__ == "__main__":
    unittest.main()
