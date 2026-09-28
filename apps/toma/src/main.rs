use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use toma_core::{SystemClock, TomaCore};
use toma_runner::{ClaudeCodeRunner, CodexCliRunner};
use toma_storage::SqliteStore;
use toma_worktree::GitWorktreeManager;

/// Usage: `toma [repository]`; defaults to the current directory.
fn main() -> anyhow::Result<()> {
    let repository = match std::env::args_os().nth(1) {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    let repository = repository
        .canonicalize()
        .with_context(|| format!("repository {} does not exist", repository.display()))?;
    anyhow::ensure!(
        repository.join(".git").exists(),
        "{} is not a Git repository; pass one as the first argument",
        repository.display()
    );

    let state_dir = repository.join(".toma");
    std::fs::create_dir_all(&state_dir)?;
    let store = Arc::new(SqliteStore::open(state_dir.join("toma.db"))?);
    let workspace_id = store.seed_demo_workspace(&repository)?;

    let core = Arc::new(TomaCore::new(
        store,
        Arc::new(GitWorktreeManager),
        vec![
            Arc::new(ClaudeCodeRunner::new()),
            Arc::new(CodexCliRunner::new()),
        ],
        Arc::new(SystemClock),
    ));
    let snapshot = core.recover(workspace_id)?;
    toma_ui::run_with_core(core, workspace_id, snapshot);
    Ok(())
}
