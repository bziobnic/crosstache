//! Git-native versioning command handlers (`xv git ...`).
//!
//! These operate on the **local** store only. Azure and AWS keep their
//! backend-native version history (`xv history` / `xv rollback`); mirroring
//! cloud secret values into a git repository would create a second, effectively
//! permanent copy of every secret version, so it is deliberately not offered.

use crate::backend::local::git::LocalGitStore;
use crate::backend::local::LocalBackend;
use crate::cli::commands::GitCommands;
use crate::config::Config;
use crate::error::{CrosstacheError, Result};
use crate::utils::format::{OutputFormat, TableFormatter};
use crate::utils::output;
use std::sync::Arc;
use tabled::Tabled;

pub(crate) async fn execute_git_command(command: GitCommands, config: Config) -> Result<()> {
    let store = open_store(&config)?;

    match command {
        GitCommands::Init => execute_init(&store, &config),
        GitCommands::Log { secret, limit } => {
            execute_log(&store, secret.as_deref(), limit, &config)
        }
        GitCommands::Status => execute_status(&store),
        GitCommands::Diff { rev } => execute_diff(&store, rev.as_deref()),
        GitCommands::Push { remote, branch } => execute_push(&store, &remote, branch.as_deref()),
        GitCommands::Pull { remote, branch } => execute_pull(&store, &remote, branch.as_deref()),
    }
}

/// Open a git store for the configured local backend.
///
/// When `[local].git` is on, this reuses the very store the backend commits
/// through, so the CLI and the write path can never disagree about which
/// repository they mean. When it is off, an unconditional store is built anyway
/// — `xv git init` has to work *before* the flag is switched on, or enabling
/// versioning would require a repo and the repo would require the flag.
fn open_store(config: &Config) -> Result<Arc<LocalGitStore>> {
    if config.effective_backend_name() != "local" {
        return Err(CrosstacheError::InvalidArgument(format!(
            "`xv git` versions the local age-encrypted store, but the active backend is '{}'. \
             On Azure and AWS, secret history lives in the backend itself — use 'xv history' and \
             'xv rollback'. Set backend = \"local\" (or pass --backend local) to use git \
             versioning.",
            config.effective_backend_name()
        )));
    }

    let backend = LocalBackend::new(config.local.as_ref())
        .map_err(|e| CrosstacheError::config(format!("failed to open local backend: {e}")))?;
    Ok(match backend.git_store() {
        Some(store) => Arc::clone(store),
        None => Arc::new(backend.git_store_unconditional()),
    })
}

fn execute_init(store: &LocalGitStore, config: &Config) -> Result<()> {
    let existed = store.is_repo();
    store
        .ensure_repo()
        .map_err(|e| CrosstacheError::config(format!("git init failed: {e}")))?;

    if existed {
        output::success("The local store is already a git repository; refreshed .gitignore.");
    } else {
        output::success("Initialized a git repository in the local store.");
    }

    // The repo alone changes nothing until the flag turns on auto-commit, so
    // say so rather than letting the user assume writes are being recorded.
    let auto_commit = config.local.as_ref().and_then(|c| c.git).unwrap_or(false);
    if !auto_commit {
        output::hint(
            "Set `git = true` under [local] in your config to commit automatically on every \
             write. Until then, this repository will not record new changes.",
        );
    }
    Ok(())
}

/// One row of `xv git log`. Column names match the repo's list-command style so
/// `--columns Commit,Subject` behaves like it does elsewhere.
#[derive(Tabled, serde::Serialize)]
struct CommitRow {
    #[tabled(rename = "Commit")]
    commit: String,
    #[tabled(rename = "Date")]
    date: String,
    #[tabled(rename = "Subject")]
    subject: String,
}

fn execute_log(
    store: &LocalGitStore,
    secret: Option<&str>,
    limit: usize,
    config: &Config,
) -> Result<()> {
    let commits = store
        .log(secret, limit)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;

    let rows: Vec<CommitRow> = commits
        .into_iter()
        .map(|c| CommitRow {
            commit: c.hash,
            date: c.date,
            subject: c.subject,
        })
        .collect();

    let fmt = config.runtime_output_format;
    let human_table_like = matches!(
        fmt,
        OutputFormat::Table | OutputFormat::Plain | OutputFormat::Raw
    );
    let formatter = TableFormatter::new(
        fmt,
        config.no_color,
        config.template.clone(),
        config.runtime_columns.clone(),
    );

    if rows.is_empty() {
        if human_table_like {
            formatter.validate_columns::<CommitRow>()?;
            let scope = secret.map_or_else(String::new, |s| format!(" for '{s}'"));
            output::info(&format!("No commits{scope}."));
        } else {
            // Valid-empty machine output, so `| jq` still works.
            println!("{}", formatter.format_table(&rows)?);
        }
        return Ok(());
    }

    println!("{}", formatter.format_table(&rows)?);
    Ok(())
}

fn execute_status(store: &LocalGitStore) -> Result<()> {
    let status = store
        .status()
        .map_err(|e| CrosstacheError::config(e.to_string()))?;

    // `--branch` always emits a leading `## <branch>` line, so emptiness is not
    // the cleanliness test — the absence of any *entry* line is.
    let has_changes = status
        .lines()
        .any(|line| !line.trim().is_empty() && !line.starts_with("##"));

    if has_changes {
        print!("{status}");
    } else {
        output::success("The local store is clean; every change is committed.");
    }
    Ok(())
}

fn execute_diff(store: &LocalGitStore, rev: Option<&str>) -> Result<()> {
    let diff = store
        .diff(rev)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;
    print!("{diff}");
    Ok(())
}

fn execute_push(store: &LocalGitStore, remote: &str, branch: Option<&str>) -> Result<()> {
    output::step(&format!("Pushing the local store to '{remote}'"));
    let out = store
        .push(remote, branch)
        .map_err(|e| CrosstacheError::config(e.to_string()))?;
    if !out.trim().is_empty() {
        print!("{out}");
    }
    output::success(&format!("Pushed the local store to '{remote}'."));
    Ok(())
}

fn execute_pull(store: &LocalGitStore, remote: &str, branch: Option<&str>) -> Result<()> {
    output::step(&format!("Pulling the local store from '{remote}'"));
    let out = store.pull(remote, branch).map_err(|e| {
        CrosstacheError::config(format!(
            "{e}\n  hint: 'xv git pull' is fast-forward only. Divergent stores cannot be \
             merged automatically, because a merge conflict inside age ciphertext is not \
             resolvable. Reconcile the histories manually with git."
        ))
    })?;
    if !out.trim().is_empty() {
        print!("{out}");
    }
    output::success(&format!("Pulled the local store from '{remote}'."));
    Ok(())
}
