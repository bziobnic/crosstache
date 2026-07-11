"""Unit tests for installer step modules."""
import unittest
from unittest.mock import patch, MagicMock
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.az import AzCli, AzCliError, AzAuthError
from installer.config import InstallerConfig
from installer.steps.prerequisites import run as run_prerequisites, check_exists
from installer.steps.resource_group import (
    run as run_rg, check_exists as rg_exists, teardown as rg_teardown
)
from installer.steps.storage_account import (
    run as run_sa, check_exists as sa_exists, teardown as sa_teardown
)
from installer.steps.app_registration import (
    run as run_app_reg, check_exists as app_reg_exists, teardown as app_reg_teardown
)
from installer.steps.deployment import _create_deployment_zip
from installer.steps.function_app import run as run_function_app, teardown as function_app_teardown
from installer.steps.rbac import run as run_rbac, _rbac_condition


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

    def test_teardown_refuses_replaced_resource_group(self):
        az = MagicMock()
        az.run_or_none.return_value = {"id": "/subscriptions/sub-123/resourceGroups/replaced"}
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaisesRegex(RuntimeError, "persisted resource ID"):
            rg_teardown(config, az, {
                "status": "created",
                "resource_id": "/subscriptions/sub-123/resourceGroups/rg-xfunction",
            })
        az.run.assert_not_called()

    def test_teardown_uses_persisted_custom_resource_group(self):
        az = MagicMock()
        expected_id = "/subscriptions/sub-123/resourceGroups/custom-rg"
        az.run_or_none.return_value = {"id": expected_id}

        rg_teardown(InstallerConfig(subscription_id="sub-123"), az, {
            "status": "created", "name": "custom-rg", "resource_id": expected_id,
        })

        self.assertIn("custom-rg", az.run_or_none.call_args.args)
        self.assertIn("custom-rg", az.run.call_args.args)


class TestStorageAccount(unittest.TestCase):

    @patch("subprocess.run")
    def test_check_exists_finds_exact_configured_account(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0,
            stdout=json.dumps({"name": "xfuncabc12345"}),
            stderr="",
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123", storage_account="xfuncabc12345")
        self.assertTrue(sa_exists(config, az))

    @patch("subprocess.run")
    def test_check_exists_returns_false_when_no_tagged_account(self, mock_subprocess):
        mock_subprocess.return_value = MagicMock(
            returncode=0, stdout=json.dumps([]), stderr=""
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        self.assertFalse(sa_exists(config, az))

    def test_teardown_refuses_replaced_storage_account(self):
        az = MagicMock()
        az.run_or_none.return_value = {"id": "/subscriptions/sub-123/storage/replaced"}
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaisesRegex(RuntimeError, "persisted resource ID"):
            sa_teardown(config, az, {
                "status": "created", "name": "xfunc123",
                "resource_id": "/subscriptions/sub-123/storage/original",
            })
        az.run.assert_not_called()

    def test_teardown_uses_resource_group_from_persisted_storage_id(self):
        az = MagicMock()
        expected_id = (
            "/subscriptions/sub-123/resourceGroups/custom-rg/providers/"
            "Microsoft.Storage/storageAccounts/customstore"
        )
        az.run_or_none.return_value = {"id": expected_id}

        sa_teardown(InstallerConfig(subscription_id="sub-123"), az, {
            "status": "created", "name": "customstore", "resource_id": expected_id,
        })

        self.assertIn("custom-rg", az.run_or_none.call_args.args)
        self.assertIn("customstore", az.run_or_none.call_args.args)

    @patch("subprocess.run")
    def test_check_exists_returns_false_when_resource_group_not_found(self, mock_subprocess):
        # Regression: when resource group was externally deleted, az.run() raised
        # AzNotFoundError and crashed teardown before the confirmation prompt.
        mock_subprocess.return_value = MagicMock(
            returncode=3, stdout="", stderr="ResourceGroupNotFound: Resource group not found"
        )
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123", resource_group="deleted-rg")
        # Must return False, not raise
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

    def test_run_refuses_app_preclaimed_by_display_name(self):
        az = MagicMock()
        az.run.return_value = [
            {"appId": "attacker-app", "id": "attacker-object", "displayName": "xfunction-rbac"}
        ]
        config = InstallerConfig(subscription_id="sub-123")

        with self.assertRaisesRegex(RuntimeError, "ownership"):
            run_app_reg(config, az)

    def test_teardown_refuses_replaced_app_registration(self):
        az = MagicMock()
        az.run_or_none.return_value = {"id": "replaced-object"}
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaisesRegex(RuntimeError, "persisted object ID"):
            app_reg_teardown(config, az, {
                "status": "created", "app_id": "app-123", "app_object_id": "original-object",
            })
        az.run.assert_not_called()


class TestFunctionAppSecurity(unittest.TestCase):

    def test_run_refuses_unowned_existing_function_app(self):
        az = MagicMock()
        az.run_or_none.return_value = {
            "id": "/subscriptions/sub-123/resourceGroups/rg-xfunction/providers/Microsoft.Web/sites/fa-xfunction"
        }
        config = InstallerConfig(subscription_id="sub-123")

        with self.assertRaisesRegex(RuntimeError, "ownership"):
            run_function_app(
                config,
                az,
                app_registration_data={
                    "tenant_id": "tenant",
                    "app_id": "client",
                    "client_secret": "super-secret",
                },
            )

        az.run.assert_not_called()

    def test_client_secret_is_not_passed_in_azure_cli_argv(self):
        az = MagicMock()
        az.run_or_none.return_value = None
        az.run.side_effect = [
            {"id": "/subscriptions/sub-123/resourceGroups/rg-xfunction/providers/Microsoft.Web/sites/fa-xfunction"},
            {},
        ]
        config = InstallerConfig(subscription_id="sub-123")

        run_function_app(
            config,
            az,
            app_registration_data={
                "tenant_id": "tenant",
                "app_id": "client",
                "client_secret": "super-secret",
            },
        )

        flattened = [str(arg) for call in az.run.call_args_list for arg in call.args]
        self.assertNotIn("AZURE_CLIENT_SECRET=super-secret", flattened)
        self.assertTrue(any(arg.startswith("@") for arg in flattened))

    def test_teardown_refuses_replaced_function_app(self):
        az = MagicMock()
        az.run_or_none.return_value = {"id": "/subscriptions/sub-123/sites/replaced"}
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaisesRegex(RuntimeError, "persisted resource ID"):
            function_app_teardown(config, az, {
                "status": "created", "resource_id": "/subscriptions/sub-123/sites/original",
            })
        az.run.assert_not_called()

    def test_resume_update_preserves_created_provenance(self):
        az = MagicMock()
        expected_id = (
            "/subscriptions/sub-123/resourceGroups/custom-rg/providers/"
            "Microsoft.Web/sites/custom-function"
        )
        az.run_or_none.return_value = {"id": expected_id}
        config = InstallerConfig(
            subscription_id="sub-123",
            resource_group="custom-rg",
            function_app_name="custom-function",
        )

        result = run_function_app(
            config,
            az,
            expected_resource_id=expected_id,
            expected_status="created",
        )

        self.assertEqual(result["status"], "created")

    def test_teardown_uses_function_identity_from_persisted_id(self):
        az = MagicMock()
        expected_id = (
            "/subscriptions/sub-123/resourceGroups/custom-rg/providers/"
            "Microsoft.Web/sites/custom-function"
        )
        az.run_or_none.return_value = {"id": expected_id}

        function_app_teardown(InstallerConfig(subscription_id="sub-123"), az, {
            "status": "created", "resource_id": expected_id,
        })

        self.assertIn("custom-rg", az.run_or_none.call_args.args)
        self.assertIn("custom-function", az.run_or_none.call_args.args)


class TestRbacDelegationSecurity(unittest.TestCase):

    def test_condition_constrains_role_and_exact_principal(self):
        principal_id = "11111111-1111-1111-1111-111111111111"
        condition = _rbac_condition(principal_id)
        self.assertIn("RoleDefinitionId", condition)
        self.assertIn("PrincipalId", condition)
        self.assertIn(principal_id, condition)

    def test_unconditioned_existing_rbac_admin_is_rejected(self):
        az = MagicMock()
        az.run.return_value = [{
            "roleDefinitionName": "Role Based Access Control Administrator",
            "scope": "/subscriptions/sub-123/resourceGroups/rg-xfunction",
            "condition": None,
            "conditionVersion": None,
        }]
        config = InstallerConfig(subscription_id="sub-123")

        with self.assertRaisesRegex(RuntimeError, "condition"):
            run_rbac(
                config,
                az,
                sp_object_id="22222222-2222-2222-2222-222222222222",
                delegated_principal_id="11111111-1111-1111-1111-111111111111",
            )

        self.assertFalse(any(call.args[:4] == ("role", "assignment", "create") for call in az.run.call_args_list))


class TestDeploymentArchiveSecurity(unittest.TestCase):

    @unittest.skipUnless(hasattr(os, "symlink"), "symlinks unsupported")
    def test_deployment_zip_rejects_symlinked_files(self):
        with tempfile.TemporaryDirectory() as source, tempfile.TemporaryDirectory() as outside:
            outside_secret = os.path.join(outside, "secret.txt")
            with open(outside_secret, "w") as handle:
                handle.write("must-not-be-archived")
            os.symlink(outside_secret, os.path.join(source, "linked.txt"))
            zip_path = os.path.join(source, "deploy.zip")

            with self.assertRaisesRegex(ValueError, "symlink"):
                _create_deployment_zip(source, zip_path)


if __name__ == "__main__":
    unittest.main()
