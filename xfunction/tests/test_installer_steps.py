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


if __name__ == "__main__":
    unittest.main()
