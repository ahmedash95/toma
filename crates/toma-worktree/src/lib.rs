use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use toma_domain::*;

const BRANCH_PREFIX: &str = "toma/";
const LOCK_DIRECTORY: &str = "toma-worktree-locks";
static LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorktreeRequest<'a> {
    pub repository_path: &'a Path,
    pub worktree_root: &'a Path,
    pub thread_id: ThreadId,
    pub base_ref: &'a str,
    pub created_at: TimestampMs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CleanupDecision {
    Safe,
    Retain { reasons: Vec<String> },
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("git command failed: {0}")]
    Git(String),
    #[error("worktree I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("write lock is held by {holder}")]
    WriteLocked { holder: String },
}

pub trait WorktreeManager: Send + Sync {
    fn ensure(&self, request: WorktreeRequest<'_>) -> Result<WorktreeRecord, WorktreeError>;
    fn acquire_write_lock(
        &self,
        thread_id: ThreadId,
        owner: &str,
    ) -> Result<WriteLease, WorktreeError>;
    fn cleanup_decision(
        &self,
        repository_path: &Path,
        record: &WorktreeRecord,
    ) -> Result<CleanupDecision, WorktreeError>;
}

#[derive(Debug)]
pub struct WriteLease {
    pub thread_id: ThreadId,
    pub owner: String,
    pub lock_path: PathBuf,
    contents: Vec<u8>,
}

pub struct GitWorktreeManager;

impl WorktreeManager for GitWorktreeManager {
    fn ensure(&self, request: WorktreeRequest<'_>) -> Result<WorktreeRecord, WorktreeError> {
        fs::create_dir_all(request.worktree_root)?;

        let repository_path = fs::canonicalize(request.repository_path)?;
        let worktree_root = absolute_path(request.worktree_root)?;
        let thread = request.thread_id.to_string();
        let path = worktree_root.join(&thread);
        let branch = format!("{BRANCH_PREFIX}{thread}");
        let record = WorktreeRecord {
            thread_id: request.thread_id,
            path,
            branch,
            created_at: request.created_at,
        };

        if record.path.exists() {
            validate_worktree(&repository_path, &record)?;
            return Ok(record);
        }

        let branch_ref = format!("refs/heads/{}", record.branch);
        let branch_exists = match git_status(
            &repository_path,
            ["show-ref", "--verify", "--quiet", branch_ref.as_str()],
        )? {
            status if status.success() => true,
            status if status.code() == Some(1) => false,
            status => {
                return Err(WorktreeError::Git(format!(
                    "could not inspect branch {} (git exited with {status})",
                    record.branch
                )));
            }
        };

        if let Err(error) = add_worktree(
            &repository_path,
            &record,
            branch_exists.then_some(record.branch.as_str()),
            request.base_ref,
        ) {
            // Another process may have completed the same deterministic creation.
            if record.path.exists() && validate_worktree(&repository_path, &record).is_ok() {
                return Ok(record);
            }
            return Err(error);
        }

        validate_worktree(&repository_path, &record)?;
        Ok(record)
    }

    fn acquire_write_lock(
        &self,
        thread_id: ThreadId,
        owner: &str,
    ) -> Result<WriteLease, WorktreeError> {
        let lock_directory = std::env::temp_dir().join(LOCK_DIRECTORY);
        fs::create_dir_all(&lock_directory)?;
        let lock_path = lock_directory.join(format!("{thread_id}.lock"));
        let token = lease_token();
        let contents = format!("{token}\0{owner}").into_bytes();

        loop {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }

            match options.open(&lock_path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&contents).and_then(|_| file.sync_all()) {
                        let _ = fs::remove_file(&lock_path);
                        return Err(error.into());
                    }
                    return Ok(WriteLease {
                        thread_id,
                        owner: owner.to_owned(),
                        lock_path,
                        contents,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    match fs::read(&lock_path) {
                        Ok(existing) => {
                            return Err(WorktreeError::WriteLocked {
                                holder: lock_owner(&existing),
                            });
                        }
                        Err(read_error) if read_error.kind() == std::io::ErrorKind::NotFound => {
                            continue;
                        }
                        Err(read_error) => return Err(read_error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn cleanup_decision(
        &self,
        repository_path: &Path,
        record: &WorktreeRecord,
    ) -> Result<CleanupDecision, WorktreeError> {
        let mut reasons = Vec::new();

        if let Err(error) = validate_worktree(repository_path, record) {
            reasons.push(format!("worktree identity is ambiguous: {error}"));
            return Ok(CleanupDecision::Retain { reasons });
        }

        match git(
            &record.path,
            ["status", "--porcelain=v1", "--untracked-files=all"],
        ) {
            Ok(output) => inspect_status(&output, &mut reasons),
            Err(error) => reasons.push(format!("working tree status is ambiguous: {error}")),
        }

        match (
            git(repository_path, ["rev-parse", "HEAD"]),
            git(&record.path, ["rev-parse", "HEAD"]),
        ) {
            (Ok(repository_head), Ok(worktree_head)) => {
                let repository_head = text_output(&repository_head);
                let worktree_head = text_output(&worktree_head);
                match git_status(
                    repository_path,
                    [
                        "merge-base",
                        "--is-ancestor",
                        worktree_head.as_str(),
                        repository_head.as_str(),
                    ],
                ) {
                    Ok(status) if status.success() => {}
                    Ok(status) if status.code() == Some(1) => {
                        reasons.push("worktree has commits not merged into repository HEAD".into());
                    }
                    Ok(status) => reasons.push(format!(
                        "commit ancestry is ambiguous (git exited with {status})"
                    )),
                    Err(error) => reasons.push(format!("commit ancestry is ambiguous: {error}")),
                }
            }
            (Err(error), _) | (_, Err(error)) => {
                reasons.push(format!("worktree commits are ambiguous: {error}"));
            }
        }

        if reasons.is_empty() {
            Ok(CleanupDecision::Safe)
        } else {
            Ok(CleanupDecision::Retain { reasons })
        }
    }
}

impl Drop for WriteLease {
    fn drop(&mut self) {
        if matches!(fs::read(&self.lock_path), Ok(contents) if contents == self.contents) {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn validate_worktree(repository_path: &Path, record: &WorktreeRecord) -> Result<(), WorktreeError> {
    let actual_branch = text_output(&git(
        &record.path,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?);
    if actual_branch != record.branch {
        return Err(WorktreeError::Git(format!(
            "expected branch {}, found {actual_branch}",
            record.branch
        )));
    }

    let repository_common = common_git_directory(repository_path)?;
    let worktree_common = common_git_directory(&record.path)?;
    if repository_common != worktree_common {
        return Err(WorktreeError::Git(
            "path belongs to a different git repository".into(),
        ));
    }
    Ok(())
}

fn common_git_directory(path: &Path) -> Result<PathBuf, WorktreeError> {
    let output = git(
        path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    fs::canonicalize(text_output(&output)).map_err(WorktreeError::Io)
}

fn absolute_path(path: &Path) -> Result<PathBuf, WorktreeError> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn inspect_status(output: &Output, reasons: &mut Vec<String>) {
    let status = String::from_utf8_lossy(&output.stdout);
    let mut untracked = false;
    let mut unmerged = false;
    let mut staged = false;
    let mut unstaged = false;

    for line in status.lines() {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            reasons.push("working tree status contains an ambiguous entry".into());
            continue;
        }
        let (index, worktree) = (bytes[0], bytes[1]);
        untracked |= index == b'?' && worktree == b'?';
        unmerged |= matches!(
            (index, worktree),
            (b'D', b'D')
                | (b'A', b'U')
                | (b'U', b'D')
                | (b'U', b'A')
                | (b'D', b'U')
                | (b'A', b'A')
                | (b'U', b'U')
        );
        staged |= index != b' ' && index != b'?';
        unstaged |= worktree != b' ' && worktree != b'?';
    }

    if untracked {
        reasons.push("worktree contains untracked files".into());
    }
    if unmerged {
        reasons.push("worktree contains unmerged changes".into());
    }
    if staged {
        reasons.push("worktree contains staged, uncommitted changes".into());
    }
    if unstaged {
        reasons.push("worktree contains unstaged changes".into());
    }
}

fn git<I, S>(working_directory: &Path, args: I) -> Result<Output, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .arg("-C")
        .arg(working_directory)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(WorktreeError::Git(format!(
            "git exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn add_worktree(
    repository_path: &Path,
    record: &WorktreeRecord,
    existing_branch: Option<&str>,
    base_ref: &str,
) -> Result<(), WorktreeError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository_path)
        .args(["worktree", "add"]);
    if existing_branch.is_none() {
        command.args(["-b", record.branch.as_str()]);
    }
    command.arg("--").arg(&record.path);
    command.arg(existing_branch.unwrap_or(base_ref));

    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(WorktreeError::Git(format!(
            "git worktree add exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn git_status<I, S>(working_directory: &Path, args: I) -> Result<ExitStatus, WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new("git")
        .arg("-C")
        .arg(working_directory)
        .args(args)
        .status()
        .map_err(WorktreeError::Io)
}

fn text_output(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn lease_token() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = LEASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp}-{sequence}", std::process::id())
}

fn lock_owner(contents: &[u8]) -> String {
    contents
        .iter()
        .position(|byte| *byte == 0)
        .map(|delimiter| String::from_utf8_lossy(&contents[delimiter + 1..]).into_owned())
        .unwrap_or_else(|| "unknown owner".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use tempfile::TempDir;

    struct Repository {
        _temporary_directory: TempDir,
        path: PathBuf,
        worktree_root: PathBuf,
    }

    impl Repository {
        fn new() -> Self {
            let temporary_directory = tempfile::tempdir().unwrap();
            let path = temporary_directory.path().join("repository");
            let worktree_root = temporary_directory.path().join("worktrees");
            fs::create_dir(&path).unwrap();
            run_git(&path, ["init", "-b", "main"]);
            run_git(&path, ["config", "user.name", "Toma Tests"]);
            run_git(&path, ["config", "user.email", "toma@example.invalid"]);
            fs::write(path.join("README.md"), "initial\n").unwrap();
            run_git(&path, ["add", "README.md"]);
            run_git(&path, ["commit", "-m", "initial"]);
            Self {
                _temporary_directory: temporary_directory,
                path,
                worktree_root,
            }
        }

        fn request(&self, thread_id: ThreadId) -> WorktreeRequest<'_> {
            WorktreeRequest {
                repository_path: &self.path,
                worktree_root: &self.worktree_root,
                thread_id,
                base_ref: "main",
                created_at: 42,
            }
        }
    }

    #[test]
    fn ensure_lazily_creates_deterministic_worktree_and_is_idempotent() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let thread_id = ThreadId::new();
        let expected_path = repository.worktree_root.join(thread_id.to_string());

        assert!(!expected_path.exists());
        let first = manager.ensure(repository.request(thread_id)).unwrap();
        let second = manager.ensure(repository.request(thread_id)).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.path, expected_path);
        assert_eq!(first.branch, format!("toma/{thread_id}"));
        assert_eq!(
            text_output(&git(&first.path, ["branch", "--show-current"]).unwrap()),
            first.branch
        );
        assert_eq!(
            manager.cleanup_decision(&repository.path, &first).unwrap(),
            CleanupDecision::Safe
        );
    }

    #[test]
    fn write_lease_is_exclusive_and_released_on_drop() {
        let manager = GitWorktreeManager;
        let thread_id = ThreadId::new();
        let lease = manager
            .acquire_write_lock(thread_id, "first owner")
            .unwrap();

        assert!(matches!(
            manager.acquire_write_lock(thread_id, "second owner"),
            Err(WorktreeError::WriteLocked { holder }) if holder == "first owner"
        ));

        let lock_path = lease.lock_path.clone();
        drop(lease);
        assert!(!lock_path.exists());
        manager
            .acquire_write_lock(thread_id, "second owner")
            .unwrap();
    }

    #[test]
    fn write_lease_excludes_another_process() {
        let manager = GitWorktreeManager;
        let thread_id = ThreadId::new();
        let lease = manager
            .acquire_write_lock(thread_id, "parent process")
            .unwrap();

        run_lock_helper(thread_id, "locked");
        drop(lease);
        run_lock_helper(thread_id, "available");
    }

    #[test]
    fn cleanup_retains_untracked_and_uncommitted_changes() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let record = manager.ensure(repository.request(ThreadId::new())).unwrap();

        fs::write(record.path.join("untracked.txt"), "new\n").unwrap();
        fs::write(record.path.join("README.md"), "changed\n").unwrap();
        run_git(&record.path, ["add", "README.md"]);

        let reasons =
            retained_reasons(manager.cleanup_decision(&repository.path, &record).unwrap());
        assert!(reasons.iter().any(|reason| reason.contains("untracked")));
        assert!(reasons.iter().any(|reason| reason.contains("uncommitted")));
    }

    #[test]
    fn cleanup_retains_unstaged_changes() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let record = manager.ensure(repository.request(ThreadId::new())).unwrap();

        fs::write(record.path.join("README.md"), "dirty\n").unwrap();

        let reasons =
            retained_reasons(manager.cleanup_decision(&repository.path, &record).unwrap());
        assert!(reasons.iter().any(|reason| reason.contains("unstaged")));
    }

    #[test]
    fn cleanup_retains_unmerged_commits() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let record = manager.ensure(repository.request(ThreadId::new())).unwrap();

        fs::write(record.path.join("README.md"), "branch commit\n").unwrap();
        run_git(&record.path, ["add", "README.md"]);
        run_git(&record.path, ["commit", "-m", "worktree change"]);

        let reasons =
            retained_reasons(manager.cleanup_decision(&repository.path, &record).unwrap());
        assert!(reasons.iter().any(|reason| reason.contains("not merged")));
        assert!(record.path.exists(), "cleanup decisions must never delete");
    }

    #[test]
    fn cleanup_retains_merge_conflicts() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let record = manager.ensure(repository.request(ThreadId::new())).unwrap();

        fs::write(repository.path.join("README.md"), "main change\n").unwrap();
        run_git(&repository.path, ["add", "README.md"]);
        run_git(&repository.path, ["commit", "-m", "main change"]);

        fs::write(record.path.join("README.md"), "worktree change\n").unwrap();
        run_git(&record.path, ["add", "README.md"]);
        run_git(&record.path, ["commit", "-m", "worktree change"]);
        assert!(git(&record.path, ["merge", "main"]).is_err());

        let reasons =
            retained_reasons(manager.cleanup_decision(&repository.path, &record).unwrap());
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("unmerged changes"))
        );
    }

    #[test]
    fn cleanup_retains_ambiguous_or_mismatched_worktrees() {
        let repository = Repository::new();
        let manager = GitWorktreeManager;
        let mut record = manager.ensure(repository.request(ThreadId::new())).unwrap();
        record.branch = "toma/not-the-checked-out-branch".into();

        let reasons =
            retained_reasons(manager.cleanup_decision(&repository.path, &record).unwrap());
        assert!(reasons.iter().any(|reason| reason.contains("ambiguous")));
    }

    #[test]
    #[ignore]
    fn lock_helper_process() {
        let Ok(thread_id) = std::env::var("TOMA_LOCK_TEST_THREAD") else {
            return;
        };
        let mode = std::env::var("TOMA_LOCK_TEST_MODE").unwrap();
        let thread_id = ThreadId::from_str(&thread_id).unwrap();
        let result = GitWorktreeManager.acquire_write_lock(thread_id, "child process");
        match mode.as_str() {
            "locked" => assert!(matches!(
                result,
                Err(WorktreeError::WriteLocked { holder }) if holder == "parent process"
            )),
            "available" => assert!(result.is_ok()),
            _ => panic!("unknown lock helper mode"),
        }
    }

    fn run_lock_helper(thread_id: ThreadId, mode: &str) {
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::lock_helper_process", "--ignored"])
            .env("TOMA_LOCK_TEST_THREAD", thread_id.to_string())
            .env("TOMA_LOCK_TEST_MODE", mode)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn retained_reasons(decision: CleanupDecision) -> Vec<String> {
        match decision {
            CleanupDecision::Retain { reasons } => reasons,
            CleanupDecision::Safe => panic!("expected cleanup to retain the worktree"),
        }
    }

    fn run_git<I, S>(working_directory: &Path, args: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new("git")
            .arg("-C")
            .arg(working_directory)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
