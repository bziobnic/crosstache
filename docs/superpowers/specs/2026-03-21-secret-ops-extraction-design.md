# Secret Commands Extraction Design Spec

## Goal

Extract all secret command execution handlers from `commands.rs` into `cli/secret_ops.rs`, completing the CLI module decomposition. After this, `commands.rs` retains only clap definitions, the `Cli::execute()` dispatcher, and unit tests.

## Architecture

Pure mechanical move — no logic changes. All ~39 secret-related functions (lines 1139–4069) move to `secret_ops.rs`. The dispatcher in `Cli::execute()` calls `pub(crate)` entry points in `secret_ops`. One-way dependency: `secret_ops` imports from domain modules; `commands.rs` never imports from `secret_ops` (dispatch uses path-qualified calls).

## Functions to Move

### Direct wrappers (called from dispatcher, must be `pub(crate)`):
1. `execute_secret_set_direct`
2. `execute_secret_get_direct`
3. `execute_secret_find_direct`
4. `execute_secret_list_direct`
5. `execute_secret_delete_direct`
6. `execute_secret_history_direct`
7. `execute_secret_rollback_direct`
8. `execute_secret_rotate_direct`
9. `execute_secret_run_direct`
10. `execute_secret_inject_direct`
11. `execute_secret_update_direct`
12. `execute_secret_purge_direct`
13. `execute_secret_restore_direct`
14. `execute_diff_command`
15. `execute_secret_copy_direct`
16. `execute_secret_move_direct`
17. `execute_secret_parse_direct`
18. `execute_secret_share_direct`

### Helper (called from direct wrappers, pub(crate) for use by display):
19. `display_cached_secret_list`

### Full implementations (called by direct wrappers, remain private):
20. `execute_secret_set`
21. `execute_secret_get`
22. `execute_secret_find`
23. `execute_secret_history`
24. `resolve_version_to_guid`
25. `execute_secret_rollback`
26. `execute_secret_rotate`
27. `execute_secret_run`
28. `execute_secret_inject`
29. `execute_secret_list`
30. `execute_secret_delete`
31. `execute_secret_update`
32. `execute_secret_purge`
33. `execute_secret_restore`
34. `execute_secret_copy`
35. `execute_secret_move`
36. `execute_secret_parse`
37. `execute_secret_share`
38. `execute_secret_set_bulk`
39. `execute_secret_delete_group`

## Module Imports

`secret_ops.rs` needs these at module level:
```rust
use crate::cli::commands::{CharsetType, ShareCommands};
use crate::cli::helpers::{
    copy_to_clipboard, generate_random_value, mask_secrets,
    schedule_clipboard_clear,
};
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::OutputFormat;
use crate::utils::output;
use zeroize::Zeroizing;
```

Additional imports used by specific functions stay as inner `use` statements (matching existing pattern in vault_ops/config_ops/system_ops).

## Dispatcher Changes

Each secret dispatch arm in `Cli::execute()` changes from calling local functions to path-qualified calls:
```rust
Commands::Set { .. } => crate::cli::secret_ops::execute_secret_set_direct(...).await,
```

## Expected Outcome

- `commands.rs`: ~1,140 lines (clap defs + dispatcher + tests)
- `secret_ops.rs`: ~2,930 lines
- No logic changes, no behavior changes
