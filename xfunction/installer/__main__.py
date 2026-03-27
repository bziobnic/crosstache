"""Entry point for python -m installer."""
import sys
from installer.cli import parse_args, build_config, prompt_config, run_install, run_uninstall, run_status, run_verify
from installer.az import AzCli

def main() -> int:
    args = parse_args()
    config = build_config(args)
    az = AzCli(verbose=getattr(config, "verbose", False))
    if args.command == "install":
        if not config.non_interactive:
            config = prompt_config(config, az)
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
