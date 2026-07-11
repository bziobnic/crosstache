"""Unit tests for InstallerConfig dataclass and state persistence."""

import json
import os
import tempfile
import unittest

import sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.config import InstallerConfig, InstallerState


class TestInstallerConfig(unittest.TestCase):

    def test_default_values(self):
        config = InstallerConfig(subscription_id="sub-123")
        self.assertEqual(config.resource_group, "rg-xfunction")
        self.assertEqual(config.location, "eastus")
        self.assertEqual(config.function_app_name, "fa-xfunction")
        self.assertEqual(config.storage_account, "")
        self.assertEqual(config.app_name, "xfunction-rbac")
        self.assertFalse(config.non_interactive)
        self.assertFalse(config.verbose)
        self.assertFalse(config.skip_deploy)
        self.assertEqual(config.output_format, "text")

    def test_from_json_file(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json", delete=False) as f:
            json.dump({"subscription_id": "sub-abc", "location": "westus2"}, f)
            f.flush()
            config = InstallerConfig.from_json_file(f.name)
        os.unlink(f.name)
        self.assertEqual(config.subscription_id, "sub-abc")
        self.assertEqual(config.location, "westus2")
        self.assertEqual(config.resource_group, "rg-xfunction")

    def test_from_json_file_not_found_raises(self):
        with self.assertRaises(FileNotFoundError):
            InstallerConfig.from_json_file("/nonexistent/config.json")


class TestInstallerState(unittest.TestCase):

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        self.state_path = os.path.join(self.tmpdir, ".xfunction-installer-state.json")

    def tearDown(self):
        if os.path.exists(self.state_path):
            os.unlink(self.state_path)
        os.rmdir(self.tmpdir)

    def test_new_state_has_no_completed_steps(self):
        state = InstallerState(self.state_path)
        self.assertEqual(state.completed_steps, [])

    def test_mark_step_completed_and_save(self):
        state = InstallerState(self.state_path)
        state.mark_completed("resource_group", {"name": "rg-xfunction"})
        state.save()
        loaded = InstallerState.load(self.state_path)
        self.assertIn("resource_group", loaded.completed_steps)
        self.assertEqual(loaded.get_step_data("resource_group")["name"], "rg-xfunction")

    def test_is_completed(self):
        state = InstallerState(self.state_path)
        self.assertFalse(state.is_completed("resource_group"))
        state.mark_completed("resource_group", {})
        self.assertTrue(state.is_completed("resource_group"))

    def test_load_nonexistent_returns_empty_state(self):
        state = InstallerState.load("/nonexistent/state.json")
        self.assertEqual(state.completed_steps, [])

    def test_secret_not_in_state_file(self):
        state = InstallerState(self.state_path)
        state.mark_completed("app_registration", {
            "client_id": "app-123",
            "client_secret": "should-not-persist",
            "sp_object_id": "sp-456",
        })
        state.save()
        with open(self.state_path) as f:
            raw = f.read()
        self.assertNotIn("should-not-persist", raw)
        self.assertIn("app-123", raw)

    @unittest.skipUnless(hasattr(os, "symlink"), "symlinks unsupported")
    def test_save_rejects_symlink_state_path(self):
        with tempfile.NamedTemporaryFile("w", delete=False) as handle:
            handle.write("outside-original")
            outside = handle.name
        self.addCleanup(lambda: os.path.exists(outside) and os.unlink(outside))
        os.symlink(outside, self.state_path)
        state = InstallerState(self.state_path)
        state.mark_completed("test", {"ok": True})

        with self.assertRaises(OSError):
            state.save()

        with open(outside) as handle:
            self.assertEqual(handle.read(), "outside-original")

    @unittest.skipUnless(os.name == "posix", "POSIX permissions only")
    def test_saved_state_is_owner_only(self):
        state = InstallerState(self.state_path)
        state.mark_completed("test", {"ok": True})
        state.save()

        self.assertEqual(os.stat(self.state_path).st_mode & 0o777, 0o600)

    def test_clear_removes_file(self):
        state = InstallerState(self.state_path)
        state.mark_completed("resource_group", {})
        state.save()
        self.assertTrue(os.path.exists(self.state_path))
        state.clear()
        self.assertFalse(os.path.exists(self.state_path))


if __name__ == "__main__":
    unittest.main()
