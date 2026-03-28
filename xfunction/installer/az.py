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


_AUTH_PATTERNS = re.compile(
    r"(az login|AADSTS|authentication|unauthorized)", re.IGNORECASE
)

_NOT_FOUND_PATTERNS = re.compile(
    r"(not found|does not exist|ResourceNotFound|ResourceGroupNotFound)", re.IGNORECASE
)

_SECRET_FLAGS = {"--password", "--secret", "--client-secret"}

_SECRET_SETTINGS = re.compile(r"(AZURE_CLIENT_SECRET|CLIENT_SECRET)=[^\s]+", re.IGNORECASE)


class AzCli:
    """Wrapper around the Azure CLI that executes commands via subprocess."""

    def __init__(self, verbose: bool = False, timeout: int = 120):
        self.verbose = verbose
        self.timeout = timeout

    def _redact_command(self, cmd: list[str]) -> str:
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
        cmd = ["az"] + list(args) + ["--output", "json"]

        if self.verbose:
            print(f"  $ {self._redact_command(cmd)}")

        result = subprocess.run(
            cmd, capture_output=True, text=True, timeout=self.timeout,
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

        # Only attempt JSON parsing for objects and arrays; bare primitives
        # (e.g. "true", "false", plain strings) are returned as-is so callers
        # can compare them without unexpected type coercion.
        if stdout.startswith(("{", "[")):
            try:
                return json.loads(stdout)
            except json.JSONDecodeError:
                pass

        return stdout

    def run_or_none(self, *args: str) -> Any | None:
        try:
            return self.run(*args)
        except AzNotFoundError:
            return None

    def check_login(self) -> bool:
        try:
            self.run("account", "show")
            return True
        except (AzCliError, AzAuthError):
            return False

    def get_subscription(self) -> str:
        result = self.run("account", "show")
        return result["id"]

    def get_tenant_id(self) -> str:
        result = self.run("account", "show")
        return result["tenantId"]
