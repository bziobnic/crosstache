"""CLI argument parsing and install/uninstall orchestration."""
import argparse
import json as json_module
import os
import signal
import shutil
import subprocess
import sys
from installer.az import AzCli, AzCliError
from installer.config import InstallerConfig, InstallerState
from installer.utils.output import success, error, warning, bold, step_header, summary_table
from installer.utils.prompts import prompt, confirm
from installer.steps import INSTALL_STEPS
from installer.steps import prerequisites, resource_group, storage_account, app_registration, function_app, rbac, deployment, verification
from installer.steps.teardown import run as run_teardown

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
    parser = argparse.ArgumentParser(prog="installer", description="xfunction Azure Function installer")
    subparsers = parser.add_subparsers(dest="command", required=True)

    # Install
    install_p = subparsers.add_parser("install", help="Set up all Azure resources")
    install_p.add_argument("--subscription-id", default=None)
    install_p.add_argument("--resource-group", default=None)
    install_p.add_argument("--location", default=None)
    install_p.add_argument("--function-app-name", default=None)
    install_p.add_argument("--storage-account", default=None)
    install_p.add_argument("--app-name", default=None)
    install_p.add_argument("--non-interactive", action="store_true")
    install_p.add_argument("--verbose", action="store_true")
    install_p.add_argument("--skip-deploy", action="store_true")
    install_p.add_argument("--config-file", default=None)
    install_p.add_argument("--resume", action="store_true")
    install_p.add_argument("--output", dest="output_format", default=None, choices=["text", "json"])

    # Uninstall
    uninstall_p = subparsers.add_parser("uninstall", help="Remove all Azure resources")
    uninstall_p.add_argument("--subscription-id", default=None)
    uninstall_p.add_argument("--resource-group", default=None)
    uninstall_p.add_argument("--function-app-name", default=None)
    uninstall_p.add_argument("--app-name", default=None)
    uninstall_p.add_argument("--non-interactive", action="store_true")
    uninstall_p.add_argument("--verbose", action="store_true")
    uninstall_p.add_argument("--keep-resource-group", action="store_true")
    uninstall_p.add_argument("--output", dest="output_format", default=None, choices=["text", "json"])

    # Status
    status_p = subparsers.add_parser("status", help="Show resource state")
    status_p.add_argument("--subscription-id", default=None)
    status_p.add_argument("--resource-group", default=None)
    status_p.add_argument("--function-app-name", default=None)
    status_p.add_argument("--app-name", default=None)
    status_p.add_argument("--verbose", action="store_true")
    status_p.add_argument("--output", dest="output_format", default=None, choices=["text", "json"])

    # Verify
    verify_p = subparsers.add_parser("verify", help="Run health check")
    verify_p.add_argument("--subscription-id", default=None)
    verify_p.add_argument("--resource-group", default=None)
    verify_p.add_argument("--function-app-name", default=None)
    verify_p.add_argument("--verbose", action="store_true")
    verify_p.add_argument("--output", dest="output_format", default=None, choices=["text", "json"])

    return parser.parse_args(argv)

def build_config(args: argparse.Namespace) -> InstallerConfig:
    if hasattr(args, "config_file") and args.config_file:
        config = InstallerConfig.from_json_file(args.config_file)
    else:
        config = InstallerConfig()
    # Apply CLI args over config-file values using type-aware logic:
    #   - String args default to None in parse_args; None means "not provided" → don't override.
    #     Any non-None value (including one that matches the dataclass default) was explicit → override.
    #   - Boolean flags use action="store_true" so False means "not passed" → only override when True.
    defaults = InstallerConfig()
    for field_name in InstallerConfig.__dataclass_fields__:
        arg_name = field_name.replace("-", "_")
        if hasattr(args, arg_name):
            val = getattr(args, arg_name)
            if isinstance(getattr(defaults, field_name), bool):
                if val:  # True means user explicitly passed the flag
                    setattr(config, field_name, val)
            else:
                if val is not None:  # non-None means user explicitly provided a value
                    setattr(config, field_name, val)
    return config

def prompt_config(config: InstallerConfig, az: AzCli) -> InstallerConfig:
    if config.non_interactive:
        if not config.subscription_id:
            config.subscription_id = az.get_subscription()
        return config
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
    az = AzCli(verbose=config.verbose)
    state_path = os.path.join(os.getcwd(), ".xfunction-installer-state.json")
    state = InstallerState.load(state_path) if config.resume else InstallerState(state_path)

    def _sigint_handler(sig, frame):
        print("\n")
        warning("Interrupted — saving state...")
        state.save()
        warning("Resume with: python -m installer install --resume")
        sys.exit(130)
    signal.signal(signal.SIGINT, _sigint_handler)

    total_steps = len(INSTALL_STEPS) - (1 if config.skip_deploy else 0)
    step_num = 0
    app_reg_data = state.get_step_data("app_registration") if config.resume else {}
    sa_data = state.get_step_data("storage_account") if config.resume else {}
    prereq_data = state.get_step_data("prerequisites") if config.resume else {}
    _secret_rotated = False  # track if secret was rotated so function_app settings can be re-applied

    for step_name in INSTALL_STEPS:
        if step_name == "deployment" and config.skip_deploy:
            continue
        step_num += 1

        # When resuming, skip already-completed steps — but if the client secret was just
        # rotated, don't skip function_app: we must re-apply settings or the deployed
        # function will silently fail to authenticate with the invalidated old secret.
        if config.resume and state.is_completed(step_name) and step_name != "verification":
            if step_name == "function_app" and _secret_rotated:
                warning("Client secret was rotated — re-applying function app settings...")
                # fall through to re-run this step with the new secret
            else:
                warning(f"Step '{step_name}' already completed — skipping")
                if step_name == "app_registration":
                    app_reg_data = state.get_step_data(step_name)
                    # Secret not in state file — offer rotation if needed
                    if not app_reg_data.get("client_secret"):
                        app_id = app_reg_data.get("app_id", "")
                        if app_id:
                            warning("Client secret not available (not stored in state file)")
                            if not config.non_interactive:
                                if confirm("Rotate the App Registration secret?", default=True):
                                    cred = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
                                    app_reg_data["client_secret"] = cred.get("password", "")
                                    success("Client secret rotated")
                                    _secret_rotated = True
                                if not app_reg_data.get("client_secret"):
                                    app_reg_data["client_secret"] = prompt("Enter client secret manually", required=True)
                                    _secret_rotated = True
                            else:
                                # In non-interactive mode, auto-rotate the secret
                                try:
                                    cred = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
                                    app_reg_data["client_secret"] = cred.get("password", "")
                                    success("Client secret auto-rotated (non-interactive)")
                                    _secret_rotated = True
                                except Exception as ex:
                                    error(f"Cannot obtain client secret in non-interactive mode: {ex}")
                                    return 1
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
                if not config.subscription_id:
                    config.subscription_id = result.get("subscription_id", "")
            elif step_name == "function_app":
                merged = {**app_reg_data, "tenant_id": prereq_data.get("tenant_id", "")}
                config.storage_account = sa_data.get("name", config.storage_account)
                result = module.run(config, az, app_registration_data=merged)
            elif step_name == "rbac":
                sp_object_id = app_reg_data.get("sp_object_id", "")
                result = module.run(config, az, sp_object_id=sp_object_id)
            else:
                result = module.run(config, az)

            if step_name == "app_registration":
                app_reg_data = result
                # app_registration.run() returns client_secret=None when the app already existed
                # (no new credential was created). If we proceed without a secret, function_app
                # will silently skip setting AZURE_CLIENT_SECRET and auth will fail.
                if not app_reg_data.get("client_secret"):
                    app_id = app_reg_data.get("app_id", "")
                    if app_id:
                        warning("App registration already existed — a new client secret is needed")
                        if not config.non_interactive:
                            if confirm("Generate a new client secret for the existing app?", default=True):
                                cred = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
                                app_reg_data["client_secret"] = cred.get("password", "")
                                success("Client secret generated")
                            if not app_reg_data.get("client_secret"):
                                app_reg_data["client_secret"] = prompt("Enter client secret manually", required=True)
                        else:
                            cred = az.run("ad", "app", "credential", "reset", "--id", app_id, "--years", "2")
                            app_reg_data["client_secret"] = cred.get("password", "")
                            success("Client secret generated (non-interactive)")
            elif step_name == "storage_account":
                sa_data = result
                config.storage_account = result.get("name", "")

            state.mark_completed(step_name, result)
            state.save()

        except Exception as ex:
            error(f"Step '{step_name}' failed: {ex}")
            state.save()
            error("Resume with: python -m installer install --resume")
            return 1

    # Credential storage
    _offer_xv_storage(config, app_reg_data, prereq_data)

    # Summary
    summary_rows = [
        ("Resource Group", config.resource_group, "Created"),
        ("Storage Account", sa_data.get("name", ""), "Created"),
        ("App Registration", config.app_name, "Created"),
        ("Function App", config.function_app_name, "Deployed" if not config.skip_deploy else "Created"),
        ("RBAC Assignments", "3 roles", "Assigned"),
    ]
    if config.output_format == "json":
        print(json_module.dumps({"resources": [{"type": r, "name": n, "status": s} for r, n, s in summary_rows]}, indent=2))
    else:
        summary_table(summary_rows)
        url = f"https://{config.function_app_name}.azurewebsites.net"
        print(f"\n{bold('Function App URL:')} {url}")
        print(f"  Set in your environment: FUNCTION_APP_URL={url}/api\n")
    return 0

def _offer_xv_storage(config: InstallerConfig, app_reg_data: dict, prereq_data: dict) -> None:
    if not shutil.which("xv"):
        return
    if config.non_interactive:
        return
    if not confirm("Store credentials in xv (crosstache)?", default=True):
        return
    tenant_id = prereq_data.get("tenant_id", "")
    client_id = app_reg_data.get("app_id", "")
    client_secret = app_reg_data.get("client_secret", "")
    url = f"https://{config.function_app_name}.azurewebsites.net/api"
    secrets = [("azure-tenant-id", tenant_id), ("azure-client-id", client_id), ("function-app-url", url)]
    if client_secret:
        secrets.append(("azure-client-secret", client_secret))
    for name, value in secrets:
        if value:
            subprocess.run(["xv", "set", name, "--value", value, "--group", "xfunction"], capture_output=True, timeout=10)
    success("Credentials stored in xv (group: xfunction)")

def run_uninstall(config: InstallerConfig) -> int:
    az = AzCli(verbose=config.verbose)
    state_path = os.path.join(os.getcwd(), ".xfunction-installer-state.json")
    state = InstallerState.load(state_path)
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
    az = AzCli(verbose=getattr(config, "verbose", False))
    status_data = {}
    rg = resource_group.check_exists(config, az)
    status_data["resource_group"] = {"name": config.resource_group, "exists": rg}
    sa = storage_account.check_exists(config, az) if rg else False
    status_data["storage_account"] = {"exists": sa}
    fa = function_app.check_exists(config, az)
    status_data["function_app"] = {"name": config.function_app_name, "exists": fa}
    app = app_registration.check_exists(config, az)
    status_data["app_registration"] = {"name": config.app_name, "exists": app}
    if config.output_format == "json":
        print(json_module.dumps(status_data, indent=2))
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
    az = AzCli(verbose=getattr(config, "verbose", False))
    result = verification.run(config, az)
    if config.output_format == "json":
        print(json_module.dumps(result, indent=2))
    return 0 if result.get("status") == "verified" else 1
