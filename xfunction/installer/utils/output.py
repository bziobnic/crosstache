"""Colored output, progress indicators, and summary table formatting."""
import sys

_USE_COLOR = hasattr(sys.stdout, "isatty") and sys.stdout.isatty()

def _color(code: str, text: str) -> str:
    if _USE_COLOR:
        return f"\033[{code}m{text}\033[0m"
    return text

def green(text: str) -> str: return _color("32", text)
def red(text: str) -> str: return _color("31", text)
def yellow(text: str) -> str: return _color("33", text)
def bold(text: str) -> str: return _color("1", text)
def dim(text: str) -> str: return _color("2", text)

def success(msg: str) -> None: print(f"  {green('✓')} {msg}")
def error(msg: str) -> None: print(f"  {red('✗')} {msg}")
def warning(msg: str) -> None: print(f"  {yellow('⚠')} {msg}")

def step_header(step_num: int, total: int, description: str) -> None:
    print(f"\n{bold(f'[{step_num}/{total}]')} {description}")

def summary_table(rows: list[tuple[str, str, str]]) -> None:
    if not rows:
        return
    col1 = max(max(len(r[0]) for r in rows), len("Resource"))
    col2 = max(max(len(r[1]) for r in rows), len("Name"))
    col3 = max(max(len(r[2]) for r in rows), len("Status"))
    header = f"{'Resource':<{col1}} | {'Name':<{col2}} | {'Status':<{col3}}"
    separator = f"{'─' * col1}─┼─{'─' * col2}─┼─{'─' * col3}"
    print(f"\n{bold(header)}")
    print(dim(separator))
    for resource, name, status in rows:
        status_colored = green(status) if status in ("Created", "Deployed", "Configured", "Assigned") else yellow(status) if status == "Skipped" else red(status) if "Failed" in status else status
        print(f"{resource:<{col1}} | {name:<{col2}} | {status_colored}")
    print()
