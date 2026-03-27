"""Interactive prompts with defaults for the installer."""

def prompt(message: str, default: str = "", required: bool = True) -> str:
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
