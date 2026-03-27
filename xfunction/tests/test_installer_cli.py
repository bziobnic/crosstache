"""Unit tests for installer CLI argument parsing."""
import unittest
import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.cli import parse_args

class TestParseArgs(unittest.TestCase):

    def test_install_defaults(self):
        args = parse_args(["install"])
        self.assertEqual(args.command, "install")
        self.assertEqual(args.resource_group, "rg-xfunction")
        self.assertEqual(args.location, "eastus")
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

if __name__ == "__main__":
    unittest.main()
