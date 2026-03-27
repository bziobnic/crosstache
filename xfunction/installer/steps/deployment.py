"""Deploy xfunction code to Azure."""
import os
import shutil
import subprocess
import tempfile
import zipfile
from installer.az import AzCli
from installer.config import InstallerConfig
from installer.utils.output import success, warning

def check_exists(config: InstallerConfig, az: AzCli) -> bool:
    try:
        result = az.run("functionapp", "function", "list", "--name", config.function_app_name, "--resource-group", config.resource_group)
        return isinstance(result, list) and len(result) > 0
    except Exception:
        return False

def _find_xfunction_dir() -> str:
    installer_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    if os.path.exists(os.path.join(installer_dir, "function_app.py")):
        return installer_dir
    raise FileNotFoundError("Cannot find xfunction directory. Run the installer from the xfunction/ directory.")

def _create_deployment_zip(source_dir: str, zip_path: str) -> None:
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

def run(config: InstallerConfig, az: AzCli) -> dict:
    xfunction_dir = _find_xfunction_dir()
    if shutil.which("func"):
        try:
            proc = subprocess.run(
                ["func", "azure", "functionapp", "publish", config.function_app_name],
                cwd=xfunction_dir, capture_output=True, text=True, timeout=300,
            )
            if proc.returncode == 0:
                success(f"Function deployed via func CLI to '{config.function_app_name}'")
                return {"method": "func", "status": "deployed"}
            else:
                warning(f"func CLI deployment failed: {proc.stderr[:200]}")
                warning("Falling back to zip deployment...")
        except Exception as ex:
            warning(f"func CLI error: {ex}. Falling back to zip deployment...")

    with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tmp:
        zip_path = tmp.name
    try:
        _create_deployment_zip(xfunction_dir, zip_path)
        az.run("functionapp", "deployment", "source", "config-zip", "--resource-group", config.resource_group, "--name", config.function_app_name, "--src", zip_path)
        success(f"Function deployed via zip to '{config.function_app_name}'")
        return {"method": "zip", "status": "deployed"}
    finally:
        if os.path.exists(zip_path):
            os.unlink(zip_path)

def teardown(config: InstallerConfig, az: AzCli) -> None:
    pass
