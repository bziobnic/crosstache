"""Unit tests for installer step modules."""
import unittest
from unittest.mock import patch, MagicMock
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.az import AzCli, AzCliError, AzAuthError
from installer.config import InstallerConfig
from installer.steps.prerequisites import run as run_prerequisites, check_exists
from installer.steps.resource_group import (
    run as run_rg, check_exists as rg_exists, teardown as rg_teardown
)
from installer.steps.storage_account import (
    run as run_sa, check_exists as sa_exists
)
from installer.steps.app_registration import (
    run as run_app_reg, check_exists as app_reg_exists
)


class TestPrerequisites(unittest.TestCase):

    @patch("shutil.which", return_value="/usr/bin/az")
    def test_check_exists_returns_true_when_az_installed(self, mock_which):
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertTrue(check_exists(config, az))

    @patch("shutil.which", return_value=None)
    def test_check_exists_returns_false_when_az_not_installed(self, mock_which):
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertFalse(check_exists(config, az))

    @patch("shutil.which", return_value="/usr/bin/func")
    @patch("subprocess.run")
    def test_run_fails_when_not_logged_in(self, mock_subprocess, mock_which):
        mock_subprocess.side_effect = [
            MagicMock(returncode=0, stdout=json.dumps({"azure-cli": "2.58.0"}), stderr=""),
            MagicMock(returncode=0, stdout=json.dumps([]), stderr=""),
            MagicMock(returncode=0, stdout="4.0.5\n", stderr=""),
            MagicMock(returncode=0, stdout="4.0.5\n", stderr=""),
            MagicMock(returncode=1, stdout="", stderr="Please run 'az login'"),
        ]
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaises(SystemExit):
            run_prerequisites(config, az)


class TestResourceGroup(unittest.TestCase):

    @patch("subprocess.run")
    def test_check_exists_returns_true_when_group_exists(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"name": "rg-xfunction", "properties": {"provisioningState": "Succeeded"}}),
            stderr="",
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertTrue(rg_exists(config, az))

    @patch("subprocess.run")
    def test_check_exists_returns_false_when_missing(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=3, stdout="", stderr="not found"
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertFalse(rg_exists(config, az))

    @patch("subprocess.run")
    def test_run_creates_resource_group(self, mock_subprocess):
        mock_subprocess.side_effect = [
            MagicMock(returncode=3, stdout="", stderr="not found"),
            MagicMock(returncode=0, stdout=json.dumps({"name": "rg-xfunction", "location": "eastus"}), stderr=""),
        ]
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        result = run_rg(config, az)
        self.assertEqual(result["name"], "rg-xfunction")
        self.assertEqual(result["status"], "created")


class TestStorageAccount(unittest.TestCase):

    @patch("subprocess.run")
    def test_check_exists_finds_tagged_account(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps([{"name": "xfuncabc12345", "tags": {"xfunction-installer": "true"}}]),
            stderr="",
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertTrue(sa_exists(config, az))

    @patch("subprocess.run")
    def test_check_exists_returns_false_when_no_tagged_account(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0, stdout=json.dumps([]), stderr=""
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertFalse(sa_exists(config, az))


class TestAppRegistration(unittest.TestCase):

    @patch("subprocess.run")
    def test_check_exists_finds_app_by_display_name(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps([{"appId": "app-123", "displayName": "xfunction-rbac"}]),
            stderr="",
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertTrue(app_reg_exists(config, az))

    @patch("subprocess.run")
    def test_check_exists_returns_false_when_no_app(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0, stdout=json.dumps([]), stderr=""
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertFalse(app_reg_exists(config, az))


if __name__ == "__main__":
    unittest.main()
