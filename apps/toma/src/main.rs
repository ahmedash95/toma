use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use toma_core::{SystemClock, TomaCore};
use toma_runner::{ClaudeCodeRunner, CodexCliRunner, CursorCliRunner};
use toma_storage::SqliteStore;
use toma_worktree::GitWorktreeManager;

/// Usage: `toma [repository]`; defaults to the current directory.
fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        #[cfg(target_os = "macos")]
        macos::show_alert(&format!("{error:#}"));
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    // Claude launches this executable as the MCP server that asks the person for permission.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "mcp-approval")
    {
        let socket = std::env::args_os().nth(2).context("missing socket path")?;
        return Ok(toma_runner::run_approval_bridge(socket.as_ref())?);
    }

    let repository = resolve_repository()?;
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
            Arc::new(ClaudeCodeRunner::new().with_approvals(std::env::current_exe()?)),
            Arc::new(CodexCliRunner::new()),
            Arc::new(CursorCliRunner::new()),
        ],
        Arc::new(SystemClock),
    ));
    let snapshot = core.recover(workspace_id)?;
    toma_ui::run_with_core(core, workspace_id, snapshot);
    Ok(())
}

fn resolve_repository() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::args_os().nth(1) {
        return Ok(PathBuf::from(path));
    }

    let cwd = std::env::current_dir()?;
    if cwd.join(".git").exists() {
        return Ok(cwd);
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = macos::choose_git_repository()? {
        return Ok(path);
    }

    anyhow::bail!(
        "no Git repository was selected; run `open Toma.app --args /path/to/repo` or pass a repository path"
    )
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{path::PathBuf, process::Command};

    pub fn show_alert(message: &str) {
        let message = message.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(r#"display alert "Toma" message "{message}" as critical"#);
        let _ = Command::new("osascript").args(["-e", &script]).status();
    }

    pub fn choose_git_repository() -> anyhow::Result<Option<PathBuf>> {
        let output = Command::new("osascript")
            .args([
                "-e",
                r#"POSIX path of (choose folder with prompt "Choose a Git repository for Toma")"#,
            ])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let path = String::from_utf8(output.stdout)?.trim().to_string();
        if path.is_empty() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(path.trim_end_matches('/'))))
    }
}
