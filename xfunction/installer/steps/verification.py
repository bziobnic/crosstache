"""Verify deployment by checking function registration."""
import time
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    try:
        result = az.run("functionapp", "function", "list", "--name", config.function_app_name, "--resource-group", config.resource_group)
        return isinstance(result, list) and len(result) > 0
    except Exception:
        return False

def run(config: InstallerConfig, az: AzCli) -> dict:
    max_wait = 60
    interval = 5
    elapsed = 0
    while elapsed < max_wait:
        try:
            result = az.run("functionapp", "function", "list", "--name", config.function_app_name, "--resource-group", config.resource_group)
            if isinstance(result, list) and len(result) > 0:
                func_names = [f.get("name", "unknown") for f in result]
                success(f"Functions registered: {', '.join(func_names)}")
                return {"functions": func_names, "url": f"https://{config.function_app_name}.azurewebsites.net", "status": "verified"}
        except Exception:
            pass
        if elapsed + interval < max_wait:
            time.sleep(interval)
        elapsed += interval
    warning("Functions not yet registered — deployment may still be in progress")
    return {"functions": [], "status": "pending"}

def teardown(config: InstallerConfig, az: AzCli) -> None:
    pass
