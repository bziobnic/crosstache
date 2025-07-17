# CLI Design Patterns

## Command Structure

### Binary Name
- Primary binary: `xv` (short for "crosstache vault")
- All commands follow the pattern: `xv <command> [subcommand] [options]`

### Command Categories
- **Direct Secret Operations**: `xv set`, `xv get`, `xv list`, `xv delete` (context-aware)
- **Vault Management**: `xv vault create`, `xv vault list`, `xv vault delete`
- **Configuration**: `xv config show`, `xv config set`, `xv init`
- **Context Management**: `xv context use`, `xv context show` (alias: `xv cx`)
- **Access Control**: `xv share grant`, `xv vault share grant`

### Aliases
- `xv ls` → `xv list`
- `xv rm` → `xv delete`
- `xv cx` → `xv context`

## User Experience Principles

### Context Awareness
- Commands operate on the current vault context when possible
- Explicit vault specification overrides context
- Clear error messages when context is missing or invalid

### Output Formats
- Default: Human-readable table format
- `--format json` for machine-readable output
- `--raw` flag for secret values (bypasses clipboard)
- Consistent formatting across all commands

### Interactive Features
- Confirmation prompts for destructive operations
- `--force` flag to bypass confirmations
- Password input masking for sensitive values
- Clipboard integration for secret retrieval

### Error Handling
- User-friendly error messages with emoji indicators
- Actionable guidance for common issues
- Structured error information for debugging
- Graceful handling of network and authentication failures

## Command Patterns

### Standard Flags
- `--vault`: Override current vault context
- `--resource-group`: Specify Azure resource group
- `--subscription`: Override default subscription
- `--format`: Output format (table, json, plain)
- `--force`: Skip confirmation prompts
- `--debug`: Enable debug logging

### Secret Operations
- Support for groups via `--group` flag
- Tag management with `--tags key=value` format
- Folder organization with `--folder` parameter
- Name sanitization handled automatically