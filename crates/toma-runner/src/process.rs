use crate::{RunRequest, RunnerError, RunnerEvent};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use toma_domain::RunId;

pub(crate) struct CommandSpec {
    pub(crate) arguments: Vec<OsString>,
}

struct ActiveProcess {
    child: Mutex<Child>,
    cancelled: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct ProcessRunner {
    executable: PathBuf,
    active: Arc<Mutex<HashMap<RunId, Arc<ActiveProcess>>>>,
}

enum StreamMessage {
    Line { line: String, stderr: bool },
    Error(std::io::Error),
}

impl ProcessRunner {
    pub(crate) fn discover(name: &str) -> Self {
        Self::with_executable(find_executable(name).unwrap_or_else(|| PathBuf::from(name)))
    }

    pub(crate) fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn availability(&self) -> Result<(), RunnerError> {
        let output = Command::new(&self.executable)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| {
                RunnerError::Unavailable(format!(
                    "could not execute {}: {error}",
                    self.executable.display()
                ))
            })?;

        if output.status.success() {
            return Ok(());
        }

        let detail = output_detail(&output.stdout, &output.stderr);
        Err(RunnerError::Unavailable(format!(
            "{} --version exited with {}{detail}",
            self.executable.display(),
            display_status(output.status)
        )))
    }

    pub(crate) fn run(
        &self,
        request: RunRequest<'_>,
        spec: CommandSpec,
        emit: &mut dyn FnMut(RunnerEvent),
    ) -> Result<(), RunnerError> {
        let mut command = Command::new(&self.executable);
        command
            .args(spec.arguments)
            .current_dir(request.working_directory)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let (active, stdout, stderr) = {
            let mut processes = self.active.lock().map_err(lock_error)?;
            if processes.contains_key(&request.run_id) {
                return Err(RunnerError::Protocol(format!(
                    "run {} is already active",
                    request.run_id
                )));
            }

            // Keep the bookkeeping lock through spawn so the same run ID cannot
            // start twice between the duplicate check and registration.
            let mut child = command.spawn().map_err(|error| {
                RunnerError::Unavailable(format!(
                    "could not start {}: {error}",
                    self.executable.display()
                ))
            })?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| RunnerError::Protocol("child stdout was not captured".into()))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| RunnerError::Protocol("child stderr was not captured".into()))?;
            let active = Arc::new(ActiveProcess {
                child: Mutex::new(child),
                cancelled: AtomicBool::new(false),
            });
            processes.insert(request.run_id, Arc::clone(&active));
            (active, stdout, stderr)
        };

        emit(RunnerEvent::Started {
            provider_session_id: request.provider_session_id.map(str::to_owned),
        });

        let (sender, receiver) = mpsc::channel();
        let stdout_reader = spawn_reader(stdout, false, sender.clone());
        let stderr_reader = spawn_reader(stderr, true, sender);
        let mut stderr_lines = Vec::new();
        let result = drive_process(&active, receiver, &mut stderr_lines, emit);

        if result.is_err() {
            let _ = active.child.lock().map_err(lock_error)?.kill();
        }
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        if result.is_err() {
            let _ = active.child.lock().map_err(lock_error)?.wait();
        }
        if let Ok(mut processes) = self.active.lock() {
            processes.remove(&request.run_id);
        }

        match result {
            Ok(_) if active.cancelled.load(Ordering::Acquire) => {
                emit(RunnerEvent::Cancelled);
                Ok(())
            }
            Ok(status) if status.success() => {
                emit(RunnerEvent::Completed);
                Ok(())
            }
            Ok(status) => {
                let stderr = stderr_lines.join("\n");
                let suffix = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                };
                let message = format!(
                    "{} exited with {}{suffix}",
                    self.executable.display(),
                    display_status(status)
                );
                emit(RunnerEvent::Failed(message.clone()));
                Err(RunnerError::Protocol(message))
            }
            Err(error) => {
                emit(RunnerEvent::Failed(error.to_string()));
                Err(error)
            }
        }
    }

    pub(crate) fn cancel(&self, run_id: RunId) -> Result<(), RunnerError> {
        let active = self
            .active
            .lock()
            .map_err(lock_error)?
            .get(&run_id)
            .cloned()
            .ok_or_else(|| RunnerError::Protocol(format!("run {run_id} is not active")))?;

        active.cancelled.store(true, Ordering::Release);
        match active.child.lock().map_err(lock_error)?.kill() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(RunnerError::Io(error)),
        }
    }
}

fn drive_process(
    active: &ActiveProcess,
    receiver: mpsc::Receiver<StreamMessage>,
    stderr_lines: &mut Vec<String>,
    emit: &mut dyn FnMut(RunnerEvent),
) -> Result<ExitStatus, RunnerError> {
    let mut streams_open = true;
    loop {
        if streams_open {
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(StreamMessage::Line { line, stderr }) => {
                    if stderr {
                        stderr_lines.push(line.clone());
                    }
                    emit(RunnerEvent::Output(line));
                }
                Ok(StreamMessage::Error(error)) => return Err(RunnerError::Io(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => streams_open = false,
            }
        } else {
            thread::sleep(Duration::from_millis(20));
        }

        let status = active.child.lock().map_err(lock_error)?.try_wait()?;
        if let Some(status) = status {
            while let Ok(message) = receiver.recv() {
                match message {
                    StreamMessage::Line { line, stderr } => {
                        if stderr {
                            stderr_lines.push(line.clone());
                        }
                        emit(RunnerEvent::Output(line));
                    }
                    StreamMessage::Error(error) => return Err(RunnerError::Io(error)),
                }
            }
            return Ok(status);
        }
    }
}

/// Finds an executable in PATH and common per-user/system locations used by GUI apps.
pub fn find_executable(name: impl AsRef<OsStr>) -> Option<PathBuf> {
    let name = name.as_ref();
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }

    let path_candidates = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>());
    let home_candidates = std::env::var_os("HOME")
        .map(PathBuf::from)
        .into_iter()
        .map(|home| home.join(".local/bin"));
    path_candidates
        .chain(home_candidates)
        .chain([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
        ])
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

fn spawn_reader(
    stream: impl Read + Send + 'static,
    stderr: bool,
    sender: mpsc::Sender<StreamMessage>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let message = match line {
                Ok(line) => StreamMessage::Line { line, stderr },
                Err(error) => StreamMessage::Error(error),
            };
            if sender.send(message).is_err() {
                break;
            }
        }
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}

fn display_status(status: ExitStatus) -> String {
    status
        .code()
        .map(|code| format!("status {code}"))
        .unwrap_or_else(|| status.to_string())
}

fn lock_error<T>(error: std::sync::PoisonError<T>) -> RunnerError {
    RunnerError::Protocol(format!("runner state lock was poisoned: {error}"))
}
