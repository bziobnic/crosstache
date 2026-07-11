"""Installer configuration and state persistence."""

import json
import os
import tempfile
from dataclasses import dataclass, field, asdict

_REDACTED_KEYS = {"client_secret"}


@dataclass
class InstallerConfig:
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
        if not os.path.exists(path):
            raise FileNotFoundError(f"Config file not found: {path}")
        with open(path) as f:
            data = json.load(f)
        return cls(**{k: v for k, v in data.items() if k in cls.__dataclass_fields__})


class InstallerState:
    def __init__(self, path: str):
        self.path = path
        self._steps: dict[str, dict] = {}

    @property
    def completed_steps(self) -> list[str]:
        return list(self._steps.keys())

    def is_completed(self, step_name: str) -> bool:
        return step_name in self._steps

    def mark_completed(self, step_name: str, data: dict) -> None:
        clean = {k: v for k, v in data.items() if k not in _REDACTED_KEYS}
        self._steps[step_name] = clean

    def get_step_data(self, step_name: str) -> dict:
        return self._steps.get(step_name, {})

    def save(self) -> None:
        if os.path.lexists(self.path) and os.path.islink(self.path):
            raise OSError(f"Refusing symlinked installer state path: {self.path}")
        parent = os.path.dirname(os.path.abspath(self.path)) or "."
        os.makedirs(parent, mode=0o700, exist_ok=True)
        fd, temp_path = tempfile.mkstemp(prefix=".xfunction-state-", dir=parent)
        try:
            os.fchmod(fd, 0o600)
            payload = json.dumps({"steps": self._steps}, indent=2).encode("utf-8")
            with os.fdopen(fd, "wb") as handle:
                fd = -1
                handle.write(payload)
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temp_path, self.path)
        finally:
            if fd >= 0:
                os.close(fd)
            if os.path.exists(temp_path):
                os.unlink(temp_path)

    @classmethod
    def load(cls, path: str) -> "InstallerState":
        state = cls(path)
        if os.path.lexists(path) and os.path.islink(path):
            raise OSError(f"Refusing symlinked installer state path: {path}")
        if os.path.exists(path):
            with open(path) as f:
                data = json.load(f)
            state._steps = data.get("steps", {})
        return state

    def clear(self) -> None:
        self._steps = {}
        if os.path.exists(self.path):
            os.unlink(self.path)
