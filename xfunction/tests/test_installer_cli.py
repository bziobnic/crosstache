"""Unit tests for installer CLI argument parsing."""
import unittest
import sys
import os
from unittest.mock import patch, MagicMock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.cli import parse_args, _offer_xv_storage
from installer.config import InstallerConfig

class TestParseArgs(unittest.TestCase):

    def test_install_defaults(self):
        args = parse_args(["install"])
        self.assertEqual(args.command, "install")
        # String args default to None so build_config can distinguish "not provided"
        # from "explicitly set to the same value as the dataclass default".
        self.assertIsNone(args.resource_group)
        self.assertIsNone(args.location)
        self.assertFalse(args.non_interactive)
        self.assertFalse(args.verbose)

    def test_install_with_flags(self):
        args = parse_args([
            "install", "--subscription-id", "sub-123",
            "--resource-group", "my-rg", "--location", "westus2",
            "--non-interactive", "--verbose",
        ])
        self.assertEqual(args.subscription_id, "sub-123")
        self.assertEqual(args.resource_group, "my-rg")
        self.assertEqual(args.location, "westus2")
        self.assertTrue(args.non_interactive)
        self.assertTrue(args.verbose)

    def test_uninstall_command(self):
        args = parse_args(["uninstall", "--keep-resource-group"])
        self.assertEqual(args.command, "uninstall")
        self.assertTrue(args.keep_resource_group)

    def test_status_command(self):
        args = parse_args(["status"])
        self.assertEqual(args.command, "status")

    def test_verify_command(self):
        args = parse_args(["verify"])
        self.assertEqual(args.command, "verify")

class TestOfferXvStorage(unittest.TestCase):
    """_offer_xv_storage must pass secret values via stdin, never argv."""

    def _config(self):
        return InstallerConfig(function_app_name="fa-xfunction", non_interactive=False)

    @patch("installer.cli.subprocess.run")
    @patch("installer.cli.confirm", return_value=True)
    @patch("installer.cli.shutil.which", return_value="/usr/local/bin/xv")
    def test_stores_secrets_via_stdin_not_argv(self, mock_which, mock_confirm, mock_run):
        app_reg_data = {"app_id": "client-123", "client_secret": "s3cr3t-value"}
        prereq_data = {"tenant_id": "tenant-abc"}

        _offer_xv_storage(self._config(), app_reg_data, prereq_data)

        self.assertTrue(mock_run.called)
        for call in mock_run.call_args_list:
            argv = call.args[0]
            kwargs = call.kwargs
            # The secret value must never appear as a bare CLI argument.
            self.assertNotIn("--value", argv)
            self.assertIn("--stdin", argv)
            self.assertNotIn("s3cr3t-value", argv)
            self.assertNotIn("client-123", argv)
            self.assertNotIn("tenant-abc", argv)
            # The value must be supplied verbatim via stdin instead.
            self.assertIn("input", kwargs)
            self.assertTrue(kwargs.get("text"))

    @patch("installer.cli.subprocess.run")
    @patch("installer.cli.confirm", return_value=True)
    @patch("installer.cli.shutil.which", return_value="/usr/local/bin/xv")
    def test_client_secret_value_passed_verbatim_via_stdin(self, mock_which, mock_confirm, mock_run):
        app_reg_data = {"app_id": "client-123", "client_secret": "s3cr3t-value"}
        prereq_data = {"tenant_id": "tenant-abc"}

        _offer_xv_storage(self._config(), app_reg_data, prereq_data)

        secret_calls = [
            call for call in mock_run.call_args_list
            if call.args[0][1] == "set" and call.args[0][2] == "azure-client-secret"
        ]
        self.assertEqual(len(secret_calls), 1)
        self.assertEqual(secret_calls[0].kwargs["input"], "s3cr3t-value")

    @patch("installer.cli.subprocess.run")
    @patch("installer.cli.shutil.which", return_value=None)
    def test_skips_when_xv_not_installed(self, mock_which, mock_run):
        _offer_xv_storage(self._config(), {"app_id": "x"}, {"tenant_id": "t"})
        mock_run.assert_not_called()

    @patch("installer.cli.subprocess.run")
    @patch("installer.cli.shutil.which", return_value="/usr/local/bin/xv")
    def test_skips_when_non_interactive(self, mock_which, mock_run):
        config = InstallerConfig(function_app_name="fa-xfunction", non_interactive=True)
        _offer_xv_storage(config, {"app_id": "x"}, {"tenant_id": "t"})
        mock_run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
