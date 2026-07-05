//! Read-only Terminal UI for crosstache. Feature-gated on `tui`.
//! See `docs/tui.md` for the user-facing contract.

pub mod app;
pub mod clipboard;
pub mod data;
pub mod event;
pub mod message;
pub mod overlays;
pub mod update;
pub mod view;

use crate::backend::Backend;
use crate::config::Config;
use crate::error::Result;
use app::{App, WorkspaceTarget};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use message::Message;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use update::Command;

pub async fn run_tui(
    config: Config,
    registry: Option<&crate::backend::BackendRegistry>,
) -> Result<()> {
    // For non-Azure backends, extract a cloneable Arc so the TUI data-loading
    // tasks can use the backend trait layer instead of hard-coded Azure auth.
    let backend: Option<Arc<dyn Backend>> = registry.and_then(|r| {
        if r.active().kind() != crate::backend::BackendKind::Azure {
            Some(r.active_arc())
        } else {
            None
        }
    });

    // Workspace-aware vault pane (Phase C Task 13): resolved once at
    // startup. `None` when no REAL (configured) workspace is attached ⇒ the
    // vault pane and secrets loading below behave exactly as before (spec
    // §Backward compatibility). `resolve_configured_workspace` returns `None`
    // with no configured workspace, so `xv tui` renders as the single-vault
    // TUI, not a 1-entry workspace browser. Any resolution error is swallowed
    // to `None` rather than failing `xv tui` outright.
    let workspace = crate::workspace::resolve_configured_workspace(&config)
        .await
        .ok()
        .flatten();
    let mut workspace_backends: std::collections::HashMap<String, Arc<dyn Backend>> =
        std::collections::HashMap::new();
    if let Some(ws) = &workspace {
        let backend_names: Vec<String> = ws.entries.iter().map(|e| e.backend.clone()).collect();
        if let Ok(ws_registry) = crate::backend::BackendRegistry::with_lazy(&config, &backend_names)
        {
            for entry in &ws.entries {
                if let Ok(b) = ws_registry.materialize(&entry.backend) {
                    workspace_backends.insert(entry.alias.clone(), b);
                }
            }
        }
    }

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        reset_terminal_sync();
        previous_hook(panic_info);
    }));

    let mut terminal = setup_terminal()?;
    let result = run_loop(
        &mut terminal,
        config,
        backend,
        workspace,
        workspace_backends,
    )
    .await;
    teardown_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()
        .map_err(|e| crate::error::CrosstacheError::config(format!("raw mode: {e}")))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| crate::error::CrosstacheError::config(format!("alt screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
        .map_err(|e| crate::error::CrosstacheError::config(format!("terminal: {e}")))
}

fn reset_terminal_sync() {
    if let Err(e) = disable_raw_mode() {
        eprintln!("warning: failed to disable raw mode: {e}");
    }
    if let Err(e) = execute!(io::stdout(), LeaveAlternateScreen) {
        eprintln!("warning: failed to leave alternate screen: {e}");
    }
}

fn teardown_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    reset_terminal_sync();
    if let Err(e) = terminal.show_cursor() {
        eprintln!("warning: failed to show cursor: {e}");
    }
    Ok(())
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config: Config,
    backend: Option<Arc<dyn Backend>>,
    workspace: Option<crate::workspace::Workspace>,
    workspace_backends: std::collections::HashMap<String, Arc<dyn Backend>>,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(64);
    let mut app = App::new(config.clone());

    // Shared shutdown flag so the blocking event-reader thread can exit cleanly
    // when the UI quits, instead of staying parked in `crossterm::event::read()`
    // until the next keystroke (which delayed process exit / prompt return).
    let shutdown = Arc::new(AtomicBool::new(false));

    let _evt = event::spawn_event_reader(tx.clone(), shutdown.clone());
    let _tick = event::spawn_tick_timer(tx.clone());

    app.workspace = workspace;
    app.workspace_backends = workspace_backends;

    if app.workspace.is_some() {
        // Workspace attached: the vault pane is populated directly from
        // attached entries (no network call — this is config, not a vault
        // listing) instead of spawning `spawn_load_vaults` against a single
        // active backend, which can't represent a multi-backend workspace.
        // Shares the exact population logic the `R`-refresh in workspace
        // mode uses (`App::repopulate_vaults_from_workspace`), so the two
        // can never diverge — the default entry is ordered first.
        app.repopulate_vaults_from_workspace();
        app.vaults_loading = false;

        // Bugbot MEDIUM fix, round 3: the non-workspace path relies on
        // `Message::VaultsLoaded` to select index 0 and issue the initial
        // `LoadSecrets` — the workspace path never sends that message (it
        // populates synchronously above), so without this the secrets pane
        // stayed empty until the user moved. Select the default entry
        // (index 0, per the ordering above) and issue the same initial
        // load, reusing `handle_command`'s workspace-aware resolution
        // rather than duplicating it.
        if !app.vaults.is_empty() {
            app.vault_state.select(Some(0));
        }
        if let Some(name) = app.selected_vault().map(String::from) {
            app.secrets_loading = true;
            handle_command(&app, &tx, Command::LoadSecrets { vault: name }, &backend).await;
        }
    } else {
        drop(data::spawn_load_vaults(
            config.clone(),
            tx.clone(),
            backend.clone(),
        ));
    }

    while !app.quit {
        terminal
            .draw(|f| view::view(&app, f))
            .map_err(|e| crate::error::CrosstacheError::config(format!("draw: {e}")))?;
        let Some(msg) = rx.recv().await else { break };
        let cmds = update::update(&mut app, msg);
        for cmd in cmds {
            handle_command(&app, &tx, cmd, &backend).await;
        }
    }

    // Signal the blocking reader to stop so it doesn't hold up shutdown.
    shutdown.store(true, Ordering::Relaxed);
    Ok(())
}

async fn handle_command(
    app: &App,
    tx: &tokio::sync::mpsc::Sender<Message>,
    cmd: Command,
    backend: &Option<Arc<dyn Backend>>,
) {
    match cmd {
        Command::Quit => {}
        Command::LoadVaults => {
            // Detached background task; the JoinHandle is intentionally dropped.
            drop(data::spawn_load_vaults(
                app.config.clone(),
                tx.clone(),
                backend.clone(),
            ));
        }
        Command::LoadSecrets { vault } => {
            // Workspace-aware: `vault` is the selected `app.vaults[i].name`,
            // which is the ALIAS in workspace mode — resolve it (via the
            // shared `App::workspace_target_for` helper) to that entry's
            // own materialized backend and REAL vault name rather than the
            // single shared `backend`/raw alias string (spec §Backward
            // compatibility: `app.workspace` is `None` outside a
            // workspace, so this is unchanged there). The alias itself
            // stays the `Message::SecretsLoaded` key so
            // `secrets_by_vault`/`selected_vault()` keep matching, even
            // though two aliases can share the same real vault name.
            //
            // If this entry's backend failed to materialize at startup,
            // `workspace_target_for` returns `Unavailable` — surface a
            // visible toast (the same `Message::Error` path every other
            // TUI failure uses) and never issue a query for that entry.
            let (real_vault, entry_backend) = match app.workspace_target_for(&vault) {
                WorkspaceTarget::Entry { backend, vault } => (vault, Some(backend)),
                WorkspaceTarget::NoWorkspace => (vault.clone(), backend.clone()),
                WorkspaceTarget::Unavailable => {
                    let _ = tx
                        .send(Message::Error(crate::error::CrosstacheError::config(
                            format!(
                                "workspace entry '{vault}' unavailable: its backend failed \
                                 to initialize; secrets were not loaded"
                            ),
                        )))
                        .await;
                    return;
                }
            };
            drop(data::spawn_load_secrets(
                app.config.clone(),
                real_vault,
                vault,
                tx.clone(),
                entry_backend,
            ));
        }
        Command::LoadValue { vault, name } => {
            // Bugbot HIGH fix (round 2): must resolve through the SAME
            // `workspace_target_for` helper `LoadSecrets` uses — this used
            // to pass the shared `backend` and the alias-as-vault-name
            // unconditionally, so revealing a value (or its record Fields
            // section) queried the wrong backend/vault (or the legacy
            // Azure path) while the secrets list looked correct.
            let (real_vault, entry_backend) = match app.workspace_target_for(&vault) {
                WorkspaceTarget::Entry {
                    backend,
                    vault: real_vault,
                } => (real_vault, Some(backend)),
                WorkspaceTarget::NoWorkspace => (vault.clone(), backend.clone()),
                WorkspaceTarget::Unavailable => {
                    let _ = tx
                        .send(Message::Error(crate::error::CrosstacheError::config(
                            format!(
                                "workspace entry '{vault}' unavailable: its backend failed \
                                 to initialize; value was not loaded"
                            ),
                        )))
                        .await;
                    return;
                }
            };
            drop(data::spawn_load_value(
                app.config.clone(),
                real_vault,
                vault,
                name,
                tx.clone(),
                entry_backend,
            ));
        }
        Command::LoadHistory { vault, name } => {
            // Bugbot HIGH fix (round 2): same helper as LoadSecrets/LoadValue.
            let (real_vault, entry_backend) = match app.workspace_target_for(&vault) {
                WorkspaceTarget::Entry {
                    backend,
                    vault: real_vault,
                } => (real_vault, Some(backend)),
                WorkspaceTarget::NoWorkspace => (vault.clone(), backend.clone()),
                WorkspaceTarget::Unavailable => {
                    let _ = tx
                        .send(Message::Error(crate::error::CrosstacheError::config(
                            format!(
                                "workspace entry '{vault}' unavailable: its backend failed \
                                 to initialize; history was not loaded"
                            ),
                        )))
                        .await;
                    return;
                }
            };
            drop(data::spawn_load_history(
                app.config.clone(),
                real_vault,
                vault,
                name,
                tx.clone(),
                entry_backend,
            ));
        }
        Command::LoadAudit { vault, name } => {
            drop(data::spawn_load_audit(
                app.config.clone(),
                vault,
                name,
                tx.clone(),
            ));
        }
        Command::CopyToClipboard(s) => {
            if let Err(e) = clipboard::copy_string(&s) {
                let _ = tx.send(Message::Error(e)).await;
            }
        }
    }
}
