# xfunction Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a Python installer that provisions all Azure infrastructure for the xfunction RBAC automation service with a single command.

**Architecture:** Modular Python package (`xfunction/installer/`) using `subprocess` to call `az` CLI commands. Each provisioning step is an independent module with `run()`, `check_exists()`, and `teardown()` functions. An `AzCli` wrapper class handles execution, JSON parsing, error handling, and secret redaction.

**Tech Stack:** Python 3.10+ stdlib only (subprocess, json, argparse, dataclasses, pathlib). External CLIs: `az` (required), `func` (optional), `xv` (optional).

**Spec:** `docs/superpowers/specs/2026-03-27-xfunction-installer-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `xfunction/installer/__init__.py` | Package marker |
| `xfunction/installer/__main__.py` | Entry point (`python -m installer`) |
| `xfunction/installer/cli.py` | Argument parsing, command dispatch, orchestration loop |
| `xfunction/installer/az.py` | `AzCli` class — subprocess wrapper, JSON parsing, error types, secret redaction |
| `xfunction/installer/config.py` | `InstallerConfig` dataclass, state file load/save |
| `xfunction/installer/steps/__init__.py` | Package marker, step registry |
| `xfunction/installer/steps/prerequisites.py` | Check az CLI, login, func CLI |
| `xfunction/installer/steps/resource_group.py` | Create/check/teardown resource group |
| `xfunction/installer/steps/storage_account.py` | Create/check/teardown storage account |
| `xfunction/installer/steps/app_registration.py` | Create app registration, service principal, Graph permissions |
| `xfunction/installer/steps/function_app.py` | Create function app, set app settings |
| `xfunction/installer/steps/rbac.py` | Assign 3 roles to service principal |
| `xfunction/installer/steps/deployment.py` | Deploy via func CLI or az zip deploy |
| `xfunction/installer/steps/verification.py` | Poll function list, health check |
| `xfunction/installer/steps/teardown.py` | Orchestrate reverse teardown |
| `xfunction/installer/utils/__init__.py` | Package marker |
| `xfunction/installer/utils/prompts.py` | Interactive prompts with defaults |
| `xfunction/installer/utils/output.py` | Colored output, progress, summary table |
| `xfunction/tests/test_az_cli.py` | Unit tests for AzCli wrapper |
| `xfunction/tests/test_installer_config.py` | Unit tests for config + state persistence |
| `xfunction/tests/test_installer_steps.py` | Unit tests for each step's check_exists and run logic |
| `xfunction/tests/test_installer_cli.py` | Unit tests for CLI argument parsing |

---

### Task 1: AzCli Wrapper and Error Types

**Files:**
- Create: `xfunction/installer/__init__.py`
- Create: `xfunction/installer/az.py`
- Create: `xfunction/tests/test_az_cli.py`

- [ ] **Step 1: Write failing tests for AzCli**

```python
# xfunction/tests/test_az_cli.py
"""Unit tests for the AzCli wrapper."""

import json
import unittest
from unittest.mock import patch, MagicMock

import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.az import AzCli, AzCliError, AzNotFoundError, AzAuthError


class TestAzCliRun(unittest.TestCase):
    """Tests for AzCli.run() method."""

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
    """Tests for AzCli.run_or_none() method."""

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
    """Tests for secret redaction in verbose/error output."""

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
    """Tests for check_login, get_subscription, get_tenant_id."""

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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd xfunction && python -m pytest tests/test_az_cli.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'installer'`

- [ ] **Step 3: Implement AzCli wrapper**

```python
# xfunction/installer/__init__.py
"""xfunction installer package."""
```

```python
# xfunction/installer/az.py
"""Azure CLI wrapper for subprocess execution with JSON parsing and secret redaction."""

import json
import re
import subprocess
from typing import Any


class AzCliError(Exception):
    """Base exception for Azure CLI errors."""

    def __init__(self, command: str, stderr: str, returncode: int):
        self.command = command
        self.stderr = stderr
        self.returncode = returncode
        super().__init__(f"az command failed (exit {returncode}): {stderr}")


class AzNotFoundError(AzCliError):
    """Raised when a resource is not found."""
    pass


class AzAuthError(AzCliError):
    """Raised when authentication fails."""
    pass


# Patterns that indicate authentication errors
_AUTH_PATTERNS = re.compile(
    r"(az login|AADSTS|authentication|unauthorized)", re.IGNORECASE
)

# Patterns that indicate resource-not-found errors
_NOT_FOUND_PATTERNS = re.compile(
    r"(not found|does not exist|ResourceNotFound|ResourceGroupNotFound)", re.IGNORECASE
)

# Flags whose next argument should be redacted
_SECRET_FLAGS = {"--password", "--secret", "--client-secret"}

# Settings patterns to redact (e.g., AZURE_CLIENT_SECRET=value)
_SECRET_SETTINGS = re.compile(r"(AZURE_CLIENT_SECRET|CLIENT_SECRET)=[^\s]+", re.IGNORECASE)


class AzCli:
    """Wrapper around the Azure CLI that executes commands via subprocess."""

    def __init__(self, verbose: bool = False, timeout: int = 120):
        self.verbose = verbose
        self.timeout = timeout

    def _redact_command(self, cmd: list[str]) -> str:
        """Redact sensitive values from a command for display."""
        redacted = list(cmd)
        i = 0
        while i < len(redacted):
            if redacted[i] in _SECRET_FLAGS and i + 1 < len(redacted):
                redacted[i + 1] = "***"
                i += 2
                continue
            redacted[i] = _SECRET_SETTINGS.sub(r"\1=***", redacted[i])
            i += 1
        return " ".join(redacted)

    def run(self, *args: str) -> Any:
        """Execute an az command and return parsed JSON output.

        Returns parsed JSON dict/list if output is valid JSON,
        otherwise returns the raw stdout string (stripped).

        Raises:
            AzNotFoundError: If the resource was not found (exit code 3 or 'not found' in stderr)
            AzAuthError: If authentication failed
            AzCliError: For all other az command failures
        """
        cmd = ["az"] + list(args) + ["--output", "json"]

        if self.verbose:
            print(f"  $ {self._redact_command(cmd)}")

        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=self.timeout,
        )

        redacted_cmd = self._redact_command(cmd)

        if result.returncode != 0:
            stderr = result.stderr.strip()

            if result.returncode == 3 or _NOT_FOUND_PATTERNS.search(stderr):
                raise AzNotFoundError(redacted_cmd, stderr, result.returncode)

            if _AUTH_PATTERNS.search(stderr):
                raise AzAuthError(redacted_cmd, stderr, result.returncode)

            raise AzCliError(redacted_cmd, stderr, result.returncode)

        stdout = result.stdout.strip()
        if not stdout:
            return {}

        try:
            return json.loads(stdout)
        except json.JSONDecodeError:
            return stdout

    def run_or_none(self, *args: str) -> Any | None:
        """Execute an az command, returning None if the resource is not found."""
        try:
            return self.run(*args)
        except AzNotFoundError:
            return None

    def check_login(self) -> bool:
        """Check if the user is logged in to Azure CLI."""
        try:
            self.run("account", "show")
            return True
        except (AzCliError, AzAuthError):
            return False

    def get_subscription(self) -> str:
        """Get the current subscription ID."""
        result = self.run("account", "show")
        return result["id"]

    def get_tenant_id(self) -> str:
        """Get the current tenant ID."""
        result = self.run("account", "show")
        return result["tenantId"]
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd xfunction && python -m pytest tests/test_az_cli.py -v`
Expected: All 12 tests PASS

- [ ] **Step 5: Commit**

```bash
cd .
git add xfunction/installer/__init__.py xfunction/installer/az.py xfunction/tests/test_az_cli.py
git commit -m "feat(installer): add AzCli wrapper with error types and secret redaction"
```

---

### Task 2: InstallerConfig and State Persistence

**Files:**
- Create: `xfunction/installer/config.py`
- Create: `xfunction/tests/test_installer_config.py`

- [ ] **Step 1: Write failing tests for InstallerConfig and state persistence**

```python
# xfunction/tests/test_installer_config.py
"""Unit tests for InstallerConfig dataclass and state persistence."""

import json
import os
import tempfile
import unittest

import sys
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.config import InstallerConfig, InstallerState


class TestInstallerConfig(unittest.TestCase):
    """Tests for InstallerConfig dataclass."""

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
        self.assertEqual(config.resource_group, "rg-xfunction")  # default preserved

    def test_from_json_file_not_found_raises(self):
        with self.assertRaises(FileNotFoundError):
            InstallerConfig.from_json_file("/nonexistent/config.json")


class TestInstallerState(unittest.TestCase):
    """Tests for InstallerState persistence."""

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

    def test_clear_removes_file(self):
        state = InstallerState(self.state_path)
        state.mark_completed("resource_group", {})
        state.save()
        self.assertTrue(os.path.exists(self.state_path))
        state.clear()
        self.assertFalse(os.path.exists(self.state_path))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd xfunction && python -m pytest tests/test_installer_config.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'installer.config'`

- [ ] **Step 3: Implement InstallerConfig and InstallerState**

```python
# xfunction/installer/config.py
"""Installer configuration and state persistence."""

import json
import os
from dataclasses import dataclass, field, asdict


# Keys that must never be written to the state file
_REDACTED_KEYS = {"client_secret"}


@dataclass
class InstallerConfig:
    """Configuration for the xfunction installer."""

    subscription_id: str = ""
    resource_group: str = "rg-xfunction"
    location: str = "eastus"
    function_app_name: str = "fa-xfunction"
    storage_account: str = ""
    app_name: str = "xfunction-rbac"
    non_interactive: bool = False
    verbose: bool = False
    skip_deploy: bool = False
    output_format: str = "text"
    resume: bool = False
    keep_resource_group: bool = False

    @classmethod
    def from_json_file(cls, path: str) -> "InstallerConfig":
        """Load config from a JSON file, using defaults for missing fields."""
        if not os.path.exists(path):
            raise FileNotFoundError(f"Config file not found: {path}")
        with open(path) as f:
            data = json.load(f)
        return cls(**{k: v for k, v in data.items() if k in cls.__dataclass_fields__})


class InstallerState:
    """Tracks installer progress for idempotent re-runs.

    Persists which steps have completed and their output data
    (resource names, IDs) to a JSON file. Client secrets are
    never written to disk.
    """

    def __init__(self, path: str):
        self.path = path
        self._steps: dict[str, dict] = {}

    @property
    def completed_steps(self) -> list[str]:
        return list(self._steps.keys())

    def is_completed(self, step_name: str) -> bool:
        return step_name in self._steps

    def mark_completed(self, step_name: str, data: dict) -> None:
        # Strip secrets before storing
        clean = {k: v for k, v in data.items() if k not in _REDACTED_KEYS}
        self._steps[step_name] = clean

    def get_step_data(self, step_name: str) -> dict:
        return self._steps.get(step_name, {})

    def save(self) -> None:
        with open(self.path, "w") as f:
            json.dump({"steps": self._steps}, f, indent=2)

    @classmethod
    def load(cls, path: str) -> "InstallerState":
        state = cls(path)
        if os.path.exists(path):
            with open(path) as f:
                data = json.load(f)
            state._steps = data.get("steps", {})
        return state

    def clear(self) -> None:
        self._steps = {}
        if os.path.exists(self.path):
            os.unlink(self.path)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd xfunction && python -m pytest tests/test_installer_config.py -v`
Expected: All 8 tests PASS

- [ ] **Step 5: Commit**

```bash
cd .
git add xfunction/installer/config.py xfunction/tests/test_installer_config.py
git commit -m "feat(installer): add InstallerConfig dataclass and state persistence"
```

---

### Task 3: Output Utilities and Interactive Prompts

**Files:**
- Create: `xfunction/installer/utils/__init__.py`
- Create: `xfunction/installer/utils/output.py`
- Create: `xfunction/installer/utils/prompts.py`

- [ ] **Step 1: Implement output utilities**

```python
# xfunction/installer/utils/__init__.py
"""Installer utility package."""
```

```python
# xfunction/installer/utils/output.py
"""Colored output, progress indicators, and summary table formatting."""

import sys


# ANSI color codes (disabled if not a terminal)
_USE_COLOR = hasattr(sys.stdout, "isatty") and sys.stdout.isatty()


def _color(code: str, text: str) -> str:
    if _USE_COLOR:
        return f"\033[{code}m{text}\033[0m"
    return text


def green(text: str) -> str:
    return _color("32", text)


def red(text: str) -> str:
    return _color("31", text)


def yellow(text: str) -> str:
    return _color("33", text)


def bold(text: str) -> str:
    return _color("1", text)


def dim(text: str) -> str:
    return _color("2", text)


def success(msg: str) -> None:
    print(f"  {green('✓')} {msg}")


def error(msg: str) -> None:
    print(f"  {red('✗')} {msg}")


def warning(msg: str) -> None:
    print(f"  {yellow('⚠')} {msg}")


def step_header(step_num: int, total: int, description: str) -> None:
    print(f"\n{bold(f'[{step_num}/{total}]')} {description}")


def summary_table(rows: list[tuple[str, str, str]]) -> None:
    """Print a summary table with Resource, Name, Status columns."""
    if not rows:
        return
    col1 = max(len(r[0]) for r in rows)
    col2 = max(len(r[1]) for r in rows)
    col3 = max(len(r[2]) for r in rows)
    col1 = max(col1, len("Resource"))
    col2 = max(col2, len("Name"))
    col3 = max(col3, len("Status"))

    header = f"{'Resource':<{col1}} | {'Name':<{col2}} | {'Status':<{col3}}"
    separator = f"{'─' * col1}─┼─{'─' * col2}─┼─{'─' * col3}"
    print(f"\n{bold(header)}")
    print(dim(separator))
    for resource, name, status in rows:
        status_colored = green(status) if status in ("Created", "Deployed", "Configured", "Assigned") else yellow(status) if status == "Skipped" else red(status)
        print(f"{resource:<{col1}} | {name:<{col2}} | {status_colored}")
    print()
```

```python
# xfunction/installer/utils/prompts.py
"""Interactive prompts with defaults for the installer."""


def prompt(message: str, default: str = "", required: bool = True) -> str:
    """Prompt the user for input with an optional default value."""
    if default:
        display = f"{message} [{default}]: "
    else:
        display = f"{message}: "

    while True:
        value = input(display).strip()
        if not value and default:
            return default
        if not value and required:
            print("  This field is required. Please enter a value.")
            continue
        return value


def confirm(message: str, default: bool = True) -> bool:
    """Ask the user for a yes/no confirmation."""
    suffix = "[Y/n]" if default else "[y/N]"
    while True:
        value = input(f"{message} {suffix}: ").strip().lower()
        if not value:
            return default
        if value in ("y", "yes"):
            return True
        if value in ("n", "no"):
            return False
        print("  Please enter 'y' or 'n'.")
```

- [ ] **Step 2: Commit**

```bash
cd .
git add xfunction/installer/utils/__init__.py xfunction/installer/utils/output.py xfunction/installer/utils/prompts.py
git commit -m "feat(installer): add output utilities and interactive prompts"
```

---

### Task 4: Prerequisites Step

**Files:**
- Create: `xfunction/installer/steps/__init__.py`
- Create: `xfunction/installer/steps/prerequisites.py`
- Add to: `xfunction/tests/test_installer_steps.py`

- [ ] **Step 1: Write failing tests for prerequisites**

```python
# xfunction/tests/test_installer_steps.py
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
    """Tests for prerequisites step."""

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

    @patch("subprocess.run")
    def test_run_fails_when_not_logged_in(self, mock_subprocess):
        # First call: az version succeeds
        # Second call: func --version succeeds
        # Third call: az account show fails (not logged in)
        mock_subprocess.side_effect = [
            MagicMock(returncode=0, stdout=json.dumps({"azure-cli": "2.58.0"}), stderr=""),
            MagicMock(returncode=0, stdout="4.0.5\n", stderr=""),
            MagicMock(returncode=1, stdout="", stderr="Please run 'az login'"),
        ]
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        with self.assertRaises(SystemExit):
            run_prerequisites(config, az)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd xfunction && python -m pytest tests/test_installer_steps.py::TestPrerequisites -v`
Expected: FAIL — `ModuleNotFoundError`

- [ ] **Step 3: Implement prerequisites step**

```python
# xfunction/installer/steps/__init__.py
"""Installer steps package.

Each step module exports:
  run(config, az_client) -> dict   # Execute the step
  check_exists(config, az_client) -> bool  # Idempotency check
  teardown(config, az_client) -> None  # Reverse the step
"""

# Step execution order
INSTALL_STEPS = [
    "prerequisites",
    "resource_group",
    "storage_account",
    "app_registration",
    "function_app",
    "rbac",
    "deployment",
    "verification",
]
```

```python
# xfunction/installer/steps/prerequisites.py
"""Check prerequisites: az CLI, login status, func CLI."""

import re
import shutil
import sys

from installer.az import AzCli, AzAuthError
from installer.config import InstallerConfig
from installer.utils.output import success, error, warning


_MIN_AZ_VERSION = (2, 50)


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if az CLI is installed."""
    return shutil.which("az") is not None


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Verify all prerequisites are met."""
    result = {}

    # Check az CLI version
    try:
        version_info = az.run("version")
        version_str = version_info.get("azure-cli", "0.0.0") if isinstance(version_info, dict) else "0.0.0"
        result["az_version"] = version_str
        match = re.match(r"(\d+)\.(\d+)", version_str)
        if match:
            major, minor = int(match.group(1)), int(match.group(2))
            if (major, minor) < _MIN_AZ_VERSION:
                error(f"Azure CLI {version_str} is below minimum {_MIN_AZ_VERSION[0]}.{_MIN_AZ_VERSION[1]}")
                sys.exit(1)
        success(f"Azure CLI v{version_str}")
    except Exception:
        error("Azure CLI is not installed. Install from https://aka.ms/installazurecli")
        sys.exit(1)

    # Check resource-graph extension
    try:
        extensions = az.run("extension", "list")
        ext_names = [e.get("name", "") for e in extensions] if isinstance(extensions, list) else []
        if "resource-graph" in ext_names:
            success("Extension 'resource-graph' installed")
        else:
            warning("Extension 'resource-graph' not found — installing...")
            az.run("extension", "add", "--name", "resource-graph", "--yes")
            success("Extension 'resource-graph' installed")
    except Exception:
        warning("Could not verify resource-graph extension")

    # Check func CLI
    func_path = shutil.which("func")
    if func_path:
        try:
            import subprocess
            proc = subprocess.run(["func", "--version"], capture_output=True, text=True, timeout=10)
            result["func_version"] = proc.stdout.strip()
            success(f"Functions Core Tools v{result['func_version']}")
        except Exception:
            warning("Functions Core Tools found but version check failed")
            result["func_version"] = "unknown"
    else:
        warning("Functions Core Tools not found — will use az for deployment")
        result["func_version"] = None

    # Check login
    try:
        account = az.run("account", "show")
        user_name = account.get("user", {}).get("name", "unknown")
        sub_name = account.get("name", "unknown")
        sub_id = account.get("id", "unknown")
        result["user"] = user_name
        result["subscription_name"] = sub_name
        result["subscription_id"] = sub_id
        result["tenant_id"] = account.get("tenantId", "")
        success(f"Logged in as {user_name}")
        success(f"Subscription: {sub_name} ({sub_id})")
    except (AzAuthError, Exception):
        error("Not logged in. Run 'az login' first.")
        sys.exit(1)

    return result


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """No teardown needed for prerequisites."""
    pass
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd xfunction && python -m pytest tests/test_installer_steps.py::TestPrerequisites -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
cd .
git add xfunction/installer/steps/__init__.py xfunction/installer/steps/prerequisites.py xfunction/tests/test_installer_steps.py
git commit -m "feat(installer): add prerequisites check step"
```

---

### Task 5: Resource Group, Storage Account, and App Registration Steps

**Files:**
- Create: `xfunction/installer/steps/resource_group.py`
- Create: `xfunction/installer/steps/storage_account.py`
- Create: `xfunction/installer/steps/app_registration.py`
- Modify: `xfunction/tests/test_installer_steps.py`

- [ ] **Step 1: Write failing tests for resource_group, storage_account, app_registration**

Append to `xfunction/tests/test_installer_steps.py`:

```python
from installer.steps.resource_group import (
    run as run_rg, check_exists as rg_exists, teardown as rg_teardown
)
from installer.steps.storage_account import (
    run as run_sa, check_exists as sa_exists
)
from installer.steps.app_registration import (
    run as run_app_reg, check_exists as app_reg_exists
)


class TestResourceGroup(unittest.TestCase):
    """Tests for resource_group step."""

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
            # check exists: not found
            MagicMock(returncode=3, stdout="", stderr="not found"),
            # create
            MagicMock(returncode=0, stdout=json.dumps({"name": "rg-xfunction", "location": "eastus"}), stderr=""),
        ]
        az = AzCli(verbose=False)
        config = InstallerConfig(subscription_id="sub-123")
        result = run_rg(config, az)
        self.assertEqual(result["name"], "rg-xfunction")


class TestStorageAccount(unittest.TestCase):
    """Tests for storage_account step."""

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
    """Tests for app_registration step."""

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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd xfunction && python -m pytest tests/test_installer_steps.py -v`
Expected: FAIL — import errors

- [ ] **Step 3: Implement resource_group step**

```python
# xfunction/installer/steps/resource_group.py
"""Create or verify Azure resource group."""

from installer.az import AzCli, AzNotFoundError
from installer.config import InstallerConfig
from installer.utils.output import success, warning


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if the resource group already exists."""
    result = az.run_or_none("group", "show", "--name", config.resource_group)
    return result is not None


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Create the resource group if it doesn't exist."""
    if check_exists(config, az):
        warning(f"Resource group '{config.resource_group}' already exists — skipping")
        return {"name": config.resource_group, "status": "exists"}

    result = az.run(
        "group", "create",
        "--name", config.resource_group,
        "--location", config.location,
    )
    success(f"Resource group '{config.resource_group}' created in {config.location}")
    return {"name": config.resource_group, "location": config.location, "status": "created"}


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """Delete the resource group."""
    if check_exists(config, az):
        az.run("group", "delete", "--name", config.resource_group, "--yes", "--no-wait")
        success(f"Resource group '{config.resource_group}' deletion initiated")
```

- [ ] **Step 4: Implement storage_account step**

```python
# xfunction/installer/steps/storage_account.py
"""Create or verify Azure storage account for Functions runtime."""

import random
import string

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning


def _generate_name() -> str:
    """Generate a globally unique storage account name."""
    suffix = "".join(random.choices(string.ascii_lowercase + string.digits, k=8))
    return f"xfunc{suffix}"


def _find_tagged_account(config: InstallerConfig, az: AzCli) -> dict | None:
    """Find an existing storage account tagged by the installer."""
    accounts = az.run(
        "storage", "account", "list",
        "--resource-group", config.resource_group,
        "--query", "[?tags.\"xfunction-installer\"=='true']",
    )
    if isinstance(accounts, list) and accounts:
        return accounts[0]
    return None


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if a tagged storage account exists in the resource group."""
    return _find_tagged_account(config, az) is not None


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Create a storage account or return the existing one."""
    existing = _find_tagged_account(config, az)
    if existing:
        name = existing["name"]
        warning(f"Storage account '{name}' already exists — skipping")
        return {"name": name, "status": "exists"}

    name = config.storage_account if config.storage_account else _generate_name()

    # Check name availability, retry if taken
    for _ in range(5):
        check = az.run("storage", "account", "check-name", "--name", name)
        if isinstance(check, dict) and check.get("nameAvailable", False):
            break
        name = _generate_name()
    else:
        raise RuntimeError("Failed to find available storage account name after 5 attempts")

    az.run(
        "storage", "account", "create",
        "--name", name,
        "--resource-group", config.resource_group,
        "--sku", "Standard_LRS",
        "--tags", "xfunction-installer=true",
    )
    success(f"Storage account '{name}' created")
    return {"name": name, "status": "created"}


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """Delete the tagged storage account."""
    existing = _find_tagged_account(config, az)
    if existing:
        az.run(
            "storage", "account", "delete",
            "--name", existing["name"],
            "--resource-group", config.resource_group,
            "--yes",
        )
        success(f"Storage account '{existing['name']}' deleted")
```

- [ ] **Step 5: Implement app_registration step**

```python
# xfunction/installer/steps/app_registration.py
"""Create or verify Azure AD App Registration with Graph permissions."""

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

# Microsoft Graph API ID
_GRAPH_API_ID = "00000003-0000-0000-c000-000000000000"

# Permission IDs (application type)
_USER_READ_ALL = "df021288-bdef-4463-88db-98f22de89214"
_APP_READ_ALL = "9a5d68dd-52b0-4cc2-bd40-abcf44ac3a30"


def _find_app_by_name(config: InstallerConfig, az: AzCli) -> dict | None:
    """Find an existing app registration by display name."""
    apps = az.run(
        "ad", "app", "list",
        "--display-name", config.app_name,
        "--query", f"[?displayName=='{config.app_name}']",
    )
    if isinstance(apps, list) and apps:
        return apps[0]
    return None


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if the app registration exists."""
    return _find_app_by_name(config, az) is not None


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Create app registration, service principal, and configure Graph permissions."""
    existing = _find_app_by_name(config, az)

    if existing:
        app_id = existing["appId"]
        warning(f"App registration '{config.app_name}' already exists (appId: {app_id})")

        # Get or create service principal
        sp = az.run_or_none("ad", "sp", "show", "--id", app_id)
        sp_object_id = sp["id"] if sp else None

        if not sp_object_id:
            sp_result = az.run("ad", "sp", "create", "--id", app_id)
            sp_object_id = sp_result["id"]

        return {
            "app_id": app_id,
            "sp_object_id": sp_object_id,
            "client_secret": None,  # Not rotating by default
            "status": "exists",
        }

    # Create app registration
    app_result = az.run("ad", "app", "create", "--display-name", config.app_name)
    app_id = app_result["appId"]
    success(f"App registration '{config.app_name}' created (appId: {app_id})")

    # Create service principal
    sp_result = az.run("ad", "sp", "create", "--id", app_id)
    sp_object_id = sp_result["id"]
    success(f"Service principal created (objectId: {sp_object_id})")

    # Generate client secret
    cred_result = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
    client_secret = cred_result.get("password", "")
    success("Client secret generated (valid for 2 years)")

    # Add Graph API permissions
    az.run(
        "ad", "app", "permission", "add",
        "--id", app_id,
        "--api", _GRAPH_API_ID,
        "--api-permissions", f"{_USER_READ_ALL}=Role", f"{_APP_READ_ALL}=Role",
    )
    success("Graph API permissions added (User.Read.All, Application.Read.All)")

    # Grant admin consent
    try:
        az.run("ad", "app", "permission", "admin-consent", "--id", app_id)
        success("Admin consent granted")
    except Exception:
        warning("Admin consent failed — you may need to grant consent manually in Azure Portal")

    return {
        "app_id": app_id,
        "sp_object_id": sp_object_id,
        "client_secret": client_secret,
        "status": "created",
    }


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """Delete the app registration and service principal."""
    existing = _find_app_by_name(config, az)
    if existing:
        app_id = existing["appId"]
        # Delete SP first (deleting app also deletes SP, but be explicit)
        az.run_or_none("ad", "sp", "delete", "--id", app_id)
        az.run("ad", "app", "delete", "--id", app_id)
        success(f"App registration '{config.app_name}' deleted")
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd xfunction && python -m pytest tests/test_installer_steps.py -v`
Expected: All tests PASS

- [ ] **Step 7: Commit**

```bash
cd .
git add xfunction/installer/steps/resource_group.py xfunction/installer/steps/storage_account.py xfunction/installer/steps/app_registration.py xfunction/tests/test_installer_steps.py
git commit -m "feat(installer): add resource_group, storage_account, and app_registration steps"
```

---

### Task 6: Function App, RBAC, Deployment, and Verification Steps

**Files:**
- Create: `xfunction/installer/steps/function_app.py`
- Create: `xfunction/installer/steps/rbac.py`
- Create: `xfunction/installer/steps/deployment.py`
- Create: `xfunction/installer/steps/verification.py`

- [ ] **Step 1: Implement function_app step**

```python
# xfunction/installer/steps/function_app.py
"""Create or verify Azure Function App with app settings."""

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if the function app exists."""
    result = az.run_or_none("functionapp", "show", "--name", config.function_app_name, "--resource-group", config.resource_group)
    return result is not None


def run(config: InstallerConfig, az: AzCli, app_registration_data: dict | None = None) -> dict:
    """Create function app and configure app settings.

    Args:
        config: Installer configuration
        az: Azure CLI wrapper
        app_registration_data: Output from app_registration step containing
            app_id, client_secret, and tenant_id
    """
    created = False

    if check_exists(config, az):
        warning(f"Function app '{config.function_app_name}' already exists — updating settings")
    else:
        az.run(
            "functionapp", "create",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--storage-account", config.storage_account,
            "--consumption-plan-location", config.location,
            "--runtime", "python",
            "--runtime-version", "3.11",
            "--functions-version", "4",
            "--os-type", "Linux",
            "--assign-identity", "[system]",
        )
        success(f"Function app '{config.function_app_name}' created")
        created = True

    # Set app settings (always update, even if app existed)
    if app_registration_data:
        tenant_id = app_registration_data.get("tenant_id", "")
        client_id = app_registration_data.get("app_id", "")
        client_secret = app_registration_data.get("client_secret", "")

        settings = [
            f"AZURE_TENANT_ID={tenant_id}",
            f"AZURE_CLIENT_ID={client_id}",
            "FUNCTIONS_WORKER_RUNTIME=python",
            f"EXPECTED_AUDIENCE={client_id}",
        ]
        if client_secret:
            settings.append(f"AZURE_CLIENT_SECRET={client_secret}")

        az.run(
            "functionapp", "config", "appsettings", "set",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--settings", *settings,
        )
        success("App settings configured")
    else:
        warning("No app registration data — skipping app settings configuration")

    return {
        "name": config.function_app_name,
        "url": f"https://{config.function_app_name}.azurewebsites.net",
        "status": "created" if created else "updated",
    }


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """Delete the function app."""
    if check_exists(config, az):
        az.run(
            "functionapp", "delete",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
            "--yes",
        )
        success(f"Function app '{config.function_app_name}' deleted")
```

- [ ] **Step 2: Implement rbac step**

```python
# xfunction/installer/steps/rbac.py
"""Assign RBAC roles to the App Registration's service principal."""

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

# Roles to assign to the service principal
_ROLES = [
    ("Role Based Access Control Administrator", "Create/manage role assignments"),
    ("Key Vault Administrator", "Read vault tags for creator verification"),
    ("Reader", "List storage accounts for discovery"),
]


def check_exists(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> bool:
    """Check if all role assignments exist."""
    if not sp_object_id:
        return False
    scope = f"/subscriptions/{config.subscription_id}"
    assignments = az.run(
        "role", "assignment", "list",
        "--assignee", sp_object_id,
        "--scope", scope,
    )
    if not isinstance(assignments, list):
        return False
    assigned_roles = {a.get("roleDefinitionName", "") for a in assignments}
    return all(role_name in assigned_roles for role_name, _ in _ROLES)


def run(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> dict:
    """Assign required roles to the service principal."""
    if not sp_object_id:
        raise ValueError("sp_object_id is required for RBAC step")

    scope = f"/subscriptions/{config.subscription_id}"
    results = {}

    # Check existing assignments
    existing = az.run(
        "role", "assignment", "list",
        "--assignee", sp_object_id,
        "--scope", scope,
    )
    assigned_roles = {a.get("roleDefinitionName", "") for a in existing} if isinstance(existing, list) else set()

    for role_name, purpose in _ROLES:
        if role_name in assigned_roles:
            warning(f"Role '{role_name}' already assigned — skipping")
            results[role_name] = "exists"
            continue

        az.run(
            "role", "assignment", "create",
            "--assignee-object-id", sp_object_id,
            "--assignee-principal-type", "ServicePrincipal",
            "--role", role_name,
            "--scope", scope,
        )
        success(f"Role '{role_name}' assigned ({purpose})")
        results[role_name] = "assigned"

    return {"roles": results, "status": "configured"}


def teardown(config: InstallerConfig, az: AzCli, sp_object_id: str = "") -> None:
    """Remove role assignments."""
    if not sp_object_id:
        return
    scope = f"/subscriptions/{config.subscription_id}"
    for role_name, _ in _ROLES:
        try:
            az.run(
                "role", "assignment", "delete",
                "--assignee", sp_object_id,
                "--role", role_name,
                "--scope", scope,
                "--yes",
            )
            success(f"Role '{role_name}' removed")
        except Exception:
            warning(f"Could not remove role '{role_name}' — may not exist")
```

- [ ] **Step 3: Implement deployment step**

```python
# xfunction/installer/steps/deployment.py
"""Deploy xfunction code to Azure."""

import os
import shutil
import subprocess
import tempfile
import zipfile

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning, error


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if the function app has deployed functions."""
    try:
        result = az.run(
            "functionapp", "function", "list",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
        )
        return isinstance(result, list) and len(result) > 0
    except Exception:
        return False


def _find_xfunction_dir() -> str:
    """Find the xfunction directory relative to the installer."""
    # The installer is at xfunction/installer/, so xfunction/ is the parent
    installer_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    if os.path.exists(os.path.join(installer_dir, "function_app.py")):
        return installer_dir
    raise FileNotFoundError(
        "Cannot find xfunction directory. Run the installer from the xfunction/ directory."
    )


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Deploy the function code."""
    xfunction_dir = _find_xfunction_dir()

    # Try func CLI first
    if shutil.which("func"):
        try:
            proc = subprocess.run(
                ["func", "azure", "functionapp", "publish", config.function_app_name],
                cwd=xfunction_dir,
                capture_output=True,
                text=True,
                timeout=300,
            )
            if proc.returncode == 0:
                success(f"Function deployed via func CLI to '{config.function_app_name}'")
                return {"method": "func", "status": "deployed"}
            else:
                warning(f"func CLI deployment failed: {proc.stderr[:200]}")
                warning("Falling back to zip deployment...")
        except Exception as ex:
            warning(f"func CLI error: {ex}. Falling back to zip deployment...")

    # Fallback: zip deployment
    with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tmp:
        zip_path = tmp.name

    try:
        _create_deployment_zip(xfunction_dir, zip_path)
        az.run(
            "functionapp", "deployment", "source", "config-zip",
            "--resource-group", config.resource_group,
            "--name", config.function_app_name,
            "--src", zip_path,
        )
        success(f"Function deployed via zip to '{config.function_app_name}'")
        return {"method": "zip", "status": "deployed"}
    finally:
        if os.path.exists(zip_path):
            os.unlink(zip_path)


def _create_deployment_zip(source_dir: str, zip_path: str) -> None:
    """Create a deployment zip excluding unnecessary files."""
    exclude_dirs = {".venv", "__pycache__", ".pytest_cache", ".vscode", "tests", "installer", ".git", "scripts", "dev"}
    exclude_files = {".gitignore", ".funcignore", "local.settings.json"}

    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
        for root, dirs, files in os.walk(source_dir):
            dirs[:] = [d for d in dirs if d not in exclude_dirs]
            for file in files:
                if file in exclude_files:
                    continue
                filepath = os.path.join(root, file)
                arcname = os.path.relpath(filepath, source_dir)
                zf.write(filepath, arcname)


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """No specific teardown — function app deletion handles this."""
    pass
```

- [ ] **Step 4: Implement verification step**

```python
# xfunction/installer/steps/verification.py
"""Verify deployment by checking function registration."""

import time

from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning, error


def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    """Check if functions are registered."""
    try:
        result = az.run(
            "functionapp", "function", "list",
            "--name", config.function_app_name,
            "--resource-group", config.resource_group,
        )
        return isinstance(result, list) and len(result) > 0
    except Exception:
        return False


def run(config: InstallerConfig, az: AzCli) -> dict:
    """Poll until functions are registered or timeout."""
    max_wait = 60
    interval = 5
    elapsed = 0

    while elapsed < max_wait:
        try:
            result = az.run(
                "functionapp", "function", "list",
                "--name", config.function_app_name,
                "--resource-group", config.resource_group,
            )
            if isinstance(result, list) and len(result) > 0:
                func_names = [f.get("name", "unknown") for f in result]
                success(f"Functions registered: {', '.join(func_names)}")
                return {
                    "functions": func_names,
                    "url": f"https://{config.function_app_name}.azurewebsites.net",
                    "status": "verified",
                }
        except Exception:
            pass

        if elapsed + interval < max_wait:
            time.sleep(interval)
        elapsed += interval

    warning("Functions not yet registered — deployment may still be in progress")
    return {"functions": [], "status": "pending"}


def teardown(config: InstallerConfig, az: AzCli) -> None:
    """No teardown needed for verification."""
    pass
```

- [ ] **Step 5: Commit**

```bash
cd .
git add xfunction/installer/steps/function_app.py xfunction/installer/steps/rbac.py xfunction/installer/steps/deployment.py xfunction/installer/steps/verification.py
git commit -m "feat(installer): add function_app, rbac, deployment, and verification steps"
```

---

### Task 7: Teardown Orchestrator

**Files:**
- Create: `xfunction/installer/steps/teardown.py`

- [ ] **Step 1: Implement teardown orchestrator**

```python
# xfunction/installer/steps/teardown.py
"""Orchestrate teardown of all installer-created resources."""

import shutil

from installer.az import AzCli
from installer.config import InstallerConfig, InstallerState
from installer.utils.output import success, warning, step_header, summary_table
from installer.utils.prompts import confirm
from installer.steps import resource_group, storage_account, app_registration, function_app, rbac


def run(config: InstallerConfig, az: AzCli, state: InstallerState) -> None:
    """Tear down all resources in reverse order."""
    # Collect what exists for confirmation
    resources_to_delete = []

    sp_object_id = state.get_step_data("app_registration").get("sp_object_id", "")

    if sp_object_id:
        resources_to_delete.append(("RBAC Assignments", "3 roles", ""))
    if app_registration.check_exists(config, az):
        resources_to_delete.append(("App Registration", config.app_name, ""))
    if function_app.check_exists(config, az):
        resources_to_delete.append(("Function App", config.function_app_name, ""))
    if storage_account.check_exists(config, az):
        sa_data = state.get_step_data("storage_account")
        resources_to_delete.append(("Storage Account", sa_data.get("name", "tagged account"), ""))
    if not config.keep_resource_group and resource_group.check_exists(config, az):
        resources_to_delete.append(("Resource Group", config.resource_group, ""))

    if not resources_to_delete:
        warning("No resources found to delete")
        return

    # Show what will be deleted
    print("\nThe following resources will be deleted:")
    for resource, name, _ in resources_to_delete:
        print(f"  - {resource}: {name}")

    if not config.non_interactive:
        if not confirm("\nProceed with deletion?", default=False):
            warning("Teardown cancelled")
            return

    total = len(resources_to_delete)
    step_num = 0
    results = []

    # 1. Remove RBAC assignments
    if sp_object_id:
        step_num += 1
        step_header(step_num, total, "Removing role assignments...")
        try:
            rbac.teardown(config, az, sp_object_id=sp_object_id)
            results.append(("RBAC Assignments", "3 roles", "Removed"))
        except Exception as ex:
            results.append(("RBAC Assignments", "3 roles", f"Failed: {ex}"))

    # 2. Delete App Registration
    if app_registration.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting app registration...")
        try:
            app_registration.teardown(config, az)
            results.append(("App Registration", config.app_name, "Deleted"))
        except Exception as ex:
            results.append(("App Registration", config.app_name, f"Failed: {ex}"))

    # 3. Delete Function App
    if function_app.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting function app...")
        try:
            function_app.teardown(config, az)
            results.append(("Function App", config.function_app_name, "Deleted"))
        except Exception as ex:
            results.append(("Function App", config.function_app_name, f"Failed: {ex}"))

    # 4. Delete Storage Account
    if storage_account.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting storage account...")
        try:
            storage_account.teardown(config, az)
            sa_name = state.get_step_data("storage_account").get("name", "")
            results.append(("Storage Account", sa_name, "Deleted"))
        except Exception as ex:
            results.append(("Storage Account", "", f"Failed: {ex}"))

    # 5. Delete Resource Group
    if not config.keep_resource_group and resource_group.check_exists(config, az):
        step_num += 1
        step_header(step_num, total, "Deleting resource group...")
        try:
            resource_group.teardown(config, az)
            results.append(("Resource Group", config.resource_group, "Deleted"))
        except Exception as ex:
            results.append(("Resource Group", config.resource_group, f"Failed: {ex}"))

    # Clean up xv credentials
    if shutil.which("xv"):
        should_clean_xv = config.non_interactive
        if not config.non_interactive:
            from installer.utils.prompts import confirm
            should_clean_xv = confirm("Remove xv-stored credentials (group: xfunction)?", default=True)
        if should_clean_xv:
            try:
                import subprocess
                for secret in ["azure-tenant-id", "azure-client-id", "azure-client-secret", "function-app-url"]:
                    subprocess.run(
                        ["xv", "delete", secret, "--group", "xfunction"],
                        capture_output=True, timeout=10,
                    )
                success("Removed xv-stored credentials")
            except Exception:
                warning("Could not clean up xv credentials")

    # Clean up state file
    state.clear()
    success("State file removed")

    summary_table(results)
```

- [ ] **Step 2: Commit**

```bash
cd .
git add xfunction/installer/steps/teardown.py
git commit -m "feat(installer): add teardown orchestrator"
```

---

### Task 8: CLI Entry Point and Orchestration

**Files:**
- Create: `xfunction/installer/__main__.py`
- Create: `xfunction/installer/cli.py`
- Create: `xfunction/tests/test_installer_cli.py`

- [ ] **Step 1: Write failing tests for CLI argument parsing**

```python
# xfunction/tests/test_installer_cli.py
"""Unit tests for installer CLI argument parsing."""

import unittest
import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from installer.cli import parse_args


class TestParseArgs(unittest.TestCase):
    """Tests for CLI argument parsing."""

    def test_install_defaults(self):
        args = parse_args(["install"])
        self.assertEqual(args.command, "install")
        self.assertEqual(args.resource_group, "rg-xfunction")
        self.assertEqual(args.location, "eastus")
        self.assertFalse(args.non_interactive)
        self.assertFalse(args.verbose)

    def test_install_with_flags(self):
        args = parse_args([
            "install",
            "--subscription-id", "sub-123",
            "--resource-group", "my-rg",
            "--location", "westus2",
            "--non-interactive",
            "--verbose",
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd xfunction && python -m pytest tests/test_installer_cli.py -v`
Expected: FAIL — import error

- [ ] **Step 3: Implement CLI and orchestration**

```python
# xfunction/installer/cli.py
"""CLI argument parsing and install/uninstall orchestration."""

import argparse
import os
import signal
import sys

from installer.az import AzCli, AzCliError
from installer.config import InstallerConfig, InstallerState
from installer.utils.output import (
    success, error, warning, bold, step_header, summary_table,
)
from installer.utils.prompts import prompt, confirm
from installer.steps import INSTALL_STEPS
from installer.steps import (
    prerequisites,
    resource_group,
    storage_account,
    app_registration,
    function_app,
    rbac,
    deployment,
    verification,
)
from installer.steps.teardown import run as run_teardown


# Map step names to modules
_STEP_MODULES = {
    "prerequisites": prerequisites,
    "resource_group": resource_group,
    "storage_account": storage_account,
    "app_registration": app_registration,
    "function_app": function_app,
    "rbac": rbac,
    "deployment": deployment,
    "verification": verification,
}


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser(
        prog="installer",
        description="xfunction Azure Function installer — provisions all required Azure resources",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Install command
    install_parser = subparsers.add_parser("install", help="Set up all Azure resources")
    install_parser.add_argument("--subscription-id", default="")
    install_parser.add_argument("--resource-group", default="rg-xfunction")
    install_parser.add_argument("--location", default="eastus")
    install_parser.add_argument("--function-app-name", default="fa-xfunction")
    install_parser.add_argument("--storage-account", default="")
    install_parser.add_argument("--app-name", default="xfunction-rbac")
    install_parser.add_argument("--non-interactive", action="store_true")
    install_parser.add_argument("--verbose", action="store_true")
    install_parser.add_argument("--skip-deploy", action="store_true")
    install_parser.add_argument("--config-file", default="")
    install_parser.add_argument("--resume", action="store_true")
    install_parser.add_argument("--output", default="text", choices=["text", "json"])

    # Uninstall command
    uninstall_parser = subparsers.add_parser("uninstall", help="Remove all Azure resources")
    uninstall_parser.add_argument("--subscription-id", default="")
    uninstall_parser.add_argument("--resource-group", default="rg-xfunction")
    uninstall_parser.add_argument("--function-app-name", default="fa-xfunction")
    uninstall_parser.add_argument("--app-name", default="xfunction-rbac")
    uninstall_parser.add_argument("--non-interactive", action="store_true")
    uninstall_parser.add_argument("--verbose", action="store_true")
    uninstall_parser.add_argument("--keep-resource-group", action="store_true")
    uninstall_parser.add_argument("--output", default="text", choices=["text", "json"])

    # Status command
    status_parser = subparsers.add_parser("status", help="Show resource state")
    status_parser.add_argument("--subscription-id", default="")
    status_parser.add_argument("--resource-group", default="rg-xfunction")
    status_parser.add_argument("--function-app-name", default="fa-xfunction")
    status_parser.add_argument("--app-name", default="xfunction-rbac")
    status_parser.add_argument("--verbose", action="store_true")
    status_parser.add_argument("--output", default="text", choices=["text", "json"])

    # Verify command
    verify_parser = subparsers.add_parser("verify", help="Run health check")
    verify_parser.add_argument("--subscription-id", default="")
    verify_parser.add_argument("--resource-group", default="rg-xfunction")
    verify_parser.add_argument("--function-app-name", default="fa-xfunction")
    verify_parser.add_argument("--verbose", action="store_true")
    verify_parser.add_argument("--output", default="text", choices=["text", "json"])

    return parser.parse_args(argv)


def _build_config(args: argparse.Namespace) -> InstallerConfig:
    """Build InstallerConfig from parsed arguments."""
    if hasattr(args, "config_file") and args.config_file:
        config = InstallerConfig.from_json_file(args.config_file)
    else:
        config = InstallerConfig()

    # Override with CLI flags (non-default values)
    for field_name in InstallerConfig.__dataclass_fields__:
        arg_name = field_name.replace("-", "_")
        if hasattr(args, arg_name):
            val = getattr(args, arg_name)
            if val:  # Only override if explicitly set
                setattr(config, field_name, val)

    return config


def _prompt_config(config: InstallerConfig, az: AzCli) -> InstallerConfig:
    """Interactively prompt for missing config values."""
    if config.non_interactive:
        if not config.subscription_id:
            config.subscription_id = az.get_subscription()
        return config

    # Get subscription if not set
    if not config.subscription_id:
        default_sub = az.get_subscription()
        config.subscription_id = prompt("Subscription ID", default=default_sub)

    config.resource_group = prompt("Resource group", default=config.resource_group)
    config.location = prompt("Location", default=config.location)
    config.function_app_name = prompt("Function app name", default=config.function_app_name)
    config.app_name = prompt("App registration name", default=config.app_name)

    print()
    return config


def run_install(config: InstallerConfig) -> int:
    """Execute the install workflow."""
    az = AzCli(verbose=config.verbose)
    state_path = os.path.join(os.getcwd(), ".xfunction-installer-state.json")
    state = InstallerState.load(state_path) if config.resume else InstallerState(state_path)

    # Ctrl+C handler
    def _sigint_handler(sig, frame):
        print("\n")
        warning("Interrupted — saving state...")
        state.save()
        warning(f"Resume with: python -m installer install --resume")
        sys.exit(130)
    signal.signal(signal.SIGINT, _sigint_handler)

    total_steps = len(INSTALL_STEPS) - (1 if config.skip_deploy else 0)
    step_num = 0
    results = []

    # Shared data passed between steps
    app_reg_data = state.get_step_data("app_registration") if config.resume else {}
    sa_data = state.get_step_data("storage_account") if config.resume else {}
    prereq_data = state.get_step_data("prerequisites") if config.resume else {}

    for step_name in INSTALL_STEPS:
        if step_name == "deployment" and config.skip_deploy:
            continue

        step_num += 1

        # Skip completed steps in resume mode
        if config.resume and state.is_completed(step_name) and step_name != "verification":
            warning(f"Step '{step_name}' already completed — skipping")
            # Recover shared data
            if step_name == "app_registration":
                app_reg_data = state.get_step_data(step_name)
                # Secret is never in state file — offer rotation if needed later
                if not app_reg_data.get("client_secret"):
                    app_id = app_reg_data.get("app_id", "")
                    if app_id:
                        warning("Client secret not available (not stored in state file)")
                        if not config.non_interactive:
                            from installer.utils.prompts import confirm as _confirm
                            if _confirm("Rotate the App Registration secret?", default=True):
                                cred = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
                                app_reg_data["client_secret"] = cred.get("password", "")
                                success("Client secret rotated")
                        if not app_reg_data.get("client_secret"):
                            from installer.utils.prompts import prompt as _prompt
                            app_reg_data["client_secret"] = _prompt("Enter client secret manually", required=True)
            elif step_name == "storage_account":
                sa_data = state.get_step_data(step_name)
            elif step_name == "prerequisites":
                prereq_data = state.get_step_data(step_name)
            continue

        module = _STEP_MODULES[step_name]
        step_header(step_num, total_steps, f"{step_name.replace('_', ' ').title()}...")

        try:
            if step_name == "prerequisites":
                result = module.run(config, az)
                prereq_data = result
                # Update config with detected values
                if not config.subscription_id:
                    config.subscription_id = result.get("subscription_id", "")

            elif step_name == "function_app":
                # Pass app registration data for app settings
                merged = {**app_reg_data, "tenant_id": prereq_data.get("tenant_id", "")}
                config.storage_account = sa_data.get("name", config.storage_account)
                result = module.run(config, az, app_registration_data=merged)

            elif step_name == "rbac":
                sp_object_id = app_reg_data.get("sp_object_id", "")
                result = module.run(config, az, sp_object_id=sp_object_id)

            else:
                result = module.run(config, az)

            # Capture shared data
            if step_name == "app_registration":
                app_reg_data = result
            elif step_name == "storage_account":
                sa_data = result
                config.storage_account = result.get("name", "")

            state.mark_completed(step_name, result)
            state.save()
            results.append((step_name.replace("_", " ").title(), result.get("name", result.get("status", "")), result.get("status", "done")))

        except Exception as ex:
            error(f"Step '{step_name}' failed: {ex}")
            state.save()
            error(f"Resume with: python -m installer install --resume")
            return 1

    # Credential storage
    _offer_xv_storage(config, app_reg_data, prereq_data)

    # Print summary
    summary_rows = [
        ("Resource Group", config.resource_group, "Created"),
        ("Storage Account", sa_data.get("name", ""), "Created"),
        ("App Registration", config.app_name, "Created"),
        ("Function App", config.function_app_name, "Deployed" if not config.skip_deploy else "Created"),
        ("RBAC Assignments", "3 roles", "Assigned"),
    ]
    summary_table(summary_rows)

    url = f"https://{config.function_app_name}.azurewebsites.net"
    print(f"\n{bold('Function App URL:')} {url}")
    print(f"  Set in your environment: FUNCTION_APP_URL={url}/api\n")

    return 0


def _offer_xv_storage(config: InstallerConfig, app_reg_data: dict, prereq_data: dict) -> None:
    """Offer to store credentials in xv if available."""
    import shutil
    if not shutil.which("xv"):
        return

    if config.non_interactive:
        return

    if not confirm("Store credentials in xv (crosstache)?", default=True):
        return

    import subprocess
    tenant_id = prereq_data.get("tenant_id", "")
    client_id = app_reg_data.get("app_id", "")
    client_secret = app_reg_data.get("client_secret", "")
    url = f"https://{config.function_app_name}.azurewebsites.net/api"

    secrets = [
        ("azure-tenant-id", tenant_id),
        ("azure-client-id", client_id),
        ("function-app-url", url),
    ]
    if client_secret:
        secrets.append(("azure-client-secret", client_secret))

    for name, value in secrets:
        if value:
            subprocess.run(
                ["xv", "set", name, "--value", value, "--group", "xfunction"],
                capture_output=True, timeout=10,
            )
    success("Credentials stored in xv (group: xfunction)")


def run_uninstall(config: InstallerConfig) -> int:
    """Execute the uninstall workflow."""
    az = AzCli(verbose=config.verbose)
    state_path = os.path.join(os.getcwd(), ".xfunction-installer-state.json")
    state = InstallerState.load(state_path)

    # Ctrl+C handler
    def _sigint_handler(sig, frame):
        print("\n")
        warning("Interrupted — teardown may be incomplete")
        sys.exit(130)
    signal.signal(signal.SIGINT, _sigint_handler)

    if not config.subscription_id:
        config.subscription_id = az.get_subscription()

    run_teardown(config, az, state)
    return 0


def run_status(config: InstallerConfig) -> int:
    """Show current status of all resources."""
    az = AzCli(verbose=getattr(config, "verbose", False))
    status_data = {}

    # Check each resource
    rg = resource_group.check_exists(config, az)
    status_data["resource_group"] = {"name": config.resource_group, "exists": rg}

    sa = storage_account.check_exists(config, az) if rg else False
    status_data["storage_account"] = {"exists": sa}

    fa = function_app.check_exists(config, az)
    status_data["function_app"] = {"name": config.function_app_name, "exists": fa}

    app = app_registration.check_exists(config, az)
    status_data["app_registration"] = {"name": config.app_name, "exists": app}

    if config.output_format == "json":
        import json
        print(json.dumps(status_data, indent=2))
    else:
        rows = [
            ("Resource Group", config.resource_group, "Exists" if rg else "Not Found"),
            ("Storage Account", "", "Exists" if sa else "Not Found"),
            ("Function App", config.function_app_name, "Exists" if fa else "Not Found"),
            ("App Registration", config.app_name, "Exists" if app else "Not Found"),
        ]
        summary_table(rows)
    return 0


def run_verify(config: InstallerConfig) -> int:
    """Run verification only."""
    az = AzCli(verbose=getattr(config, "verbose", False))
    result = verification.run(config, az)
    if config.output_format == "json":
        import json
        print(json.dumps(result, indent=2))
    return 0 if result.get("status") == "verified" else 1
```

```python
# xfunction/installer/__main__.py
"""Entry point for python -m installer."""

import sys
from installer.cli import parse_args, _build_config, _prompt_config, run_install, run_uninstall, run_status, run_verify
from installer.az import AzCli


def main() -> int:
    args = parse_args()
    config = _build_config(args)
    az = AzCli(verbose=getattr(config, "verbose", False))

    if args.command == "install":
        if not config.non_interactive:
            config = _prompt_config(config, az)
        return run_install(config)

    elif args.command == "uninstall":
        if not config.subscription_id:
            config.subscription_id = az.get_subscription()
        return run_uninstall(config)

    elif args.command == "status":
        return run_status(config)

    elif args.command == "verify":
        return run_verify(config)

    return 1


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Run CLI tests to verify they pass**

Run: `cd xfunction && python -m pytest tests/test_installer_cli.py -v`
Expected: All 5 tests PASS

- [ ] **Step 5: Run all installer tests**

Run: `cd xfunction && python -m pytest tests/test_az_cli.py tests/test_installer_config.py tests/test_installer_steps.py tests/test_installer_cli.py -v`
Expected: All tests PASS

- [ ] **Step 6: Commit**

```bash
cd .
git add xfunction/installer/__main__.py xfunction/installer/cli.py xfunction/tests/test_installer_cli.py
git commit -m "feat(installer): add CLI entry point and install/uninstall orchestration"
```

---

### Task 9: Update .gitignore and Add to xfunction .gitignore

**Files:**
- Modify: `xfunction/.gitignore`

- [ ] **Step 1: Add state file to .gitignore**

Add the following line to `xfunction/.gitignore`:

```
.xfunction-installer-state.json
```

- [ ] **Step 2: Commit**

```bash
cd .
git add xfunction/.gitignore
git commit -m "chore: add installer state file to .gitignore"
```

---

### Task 10: Final Integration Test and Documentation

**Files:**
- Verify: all installer modules
- Run: full test suite

- [ ] **Step 1: Run all xfunction tests together**

Run: `cd xfunction && python -m pytest tests/ -v --ignore=tests/test_integration.py`
Expected: All tests PASS (unit tests for az_cli, config, steps, cli, plus existing tests)

- [ ] **Step 2: Manual smoke test of --help output**

Run: `cd xfunction && python -m installer install --help`
Expected: Shows usage with all install options

Run: `cd xfunction && python -m installer uninstall --help`
Expected: Shows usage with uninstall options

- [ ] **Step 3: Commit any final fixes**

```bash
cd .
git add -A xfunction/installer/ xfunction/tests/test_az_cli.py xfunction/tests/test_installer_config.py xfunction/tests/test_installer_steps.py xfunction/tests/test_installer_cli.py
git commit -m "feat(installer): complete xfunction installer implementation"
```
