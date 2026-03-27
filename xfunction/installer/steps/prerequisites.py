"""Check prerequisites: az CLI, login status, func CLI."""
import re
import shutil
import sys

from installer.az import AzCli, AzAuthError
from installer.config import InstallerConfig
from installer.utils.output import success, error, warning

_MIN_AZ_VERSION = (2, 50)

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    return shutil.which("az") is not None

def run(config: InstallerConfig, az: AzCli) -> dict:
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
    pass
