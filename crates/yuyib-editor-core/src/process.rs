use std::{
    error::Error,
    fmt, fs, io,
    path::Path,
    process::{ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use command_group::{CommandGroup, GroupChild};

static CARGO_PROCESS_ACTIVE: AtomicBool = AtomicBool::new(false);
const CARGO_CHECK_TIMEOUT: Duration = Duration::from_mins(15);

/// Purpose of one isolated editor child process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessKind {
    /// Runtime Play Mode process.
    Play,
    /// Scoped Cargo check for one package.
    CargoCheck {
        /// Explicit package passed to `cargo check -p`.
        package: String,
    },
}

/// Non-blocking child state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessPoll {
    /// Child is still running.
    Running,
    /// Child exited with this status.
    Exited(ExitStatus),
    /// The configured deadline elapsed and the complete process group was killed.
    TimedOut(ExitStatus),
}

/// Process-isolated Play Mode or developer tool invocation.
///
/// Stdout/stderr are piped so the editor can drain them on background readers
/// and publish bounded diagnostics. Dropping a live process terminates and
/// reaps it rather than leaving an orphan game or Cargo job.
pub struct ManagedProcess {
    kind: ProcessKind,
    child: GroupChild,
    owns_cargo_gate: bool,
    deadline: Option<Instant>,
}

impl ManagedProcess {
    /// Starts Play via `cargo run` when `development.play_executable` is unset.
    ///
    /// Shares the Cargo process gate with scoped check so only one Cargo job
    /// runs at a time. Uses [`ProcessKind::Play`] for Stop/poll semantics.
    ///
    /// # Errors
    ///
    /// Rejects an invalid package name, missing `Cargo.toml`, concurrent Cargo,
    /// or spawn failure.
    pub fn start_play_cargo(
        package: Option<&str>,
        arguments: &[String],
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ManagedProcessError> {
        let package = package.map(str::to_owned);
        if let Some(package) = &package
            && (package.is_empty()
                || !package
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
        {
            return Err(ManagedProcessError::InvalidPackage(package.clone()));
        }
        let project_root = project_root.as_ref();
        let cargo_toml_path = project_root.join("Cargo.toml");
        if !cargo_toml_path.is_file() {
            return Err(ManagedProcessError::MissingCargoToml(
                project_root.to_path_buf(),
            ));
        }
        let cargo_toml = fs::read_to_string(&cargo_toml_path).map_err(ManagedProcessError::Io)?;
        let requires_package_selector = cargo_toml.contains("[workspace]")
            && !cargo_toml
                .lines()
                .any(|line| line.trim_start().starts_with("[package]"));
        if requires_package_selector && package.is_none() {
            return Err(ManagedProcessError::InvalidPackage(String::new()));
        }
        let mut command = Command::new("cargo");
        command.arg("run");
        if requires_package_selector {
            command
                .arg("-p")
                .arg(package.as_deref().unwrap_or_default());
        }
        command.env("CARGO_BUILD_JOBS", "2");
        if !arguments.is_empty() {
            command.arg("--");
            command.args(arguments);
        }
        if CARGO_PROCESS_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ManagedProcessError::CargoAlreadyRunning);
        }
        match Self::spawn(
            ProcessKind::Play,
            command,
            project_root,
            Some(Instant::now() + CARGO_CHECK_TIMEOUT),
        ) {
            Ok(mut process) => {
                process.owns_cargo_gate = true;
                Ok(process)
            }
            Err(error) => {
                CARGO_PROCESS_ACTIVE.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Starts a project executable in an isolated child process.
    ///
    /// # Errors
    ///
    /// Returns an error when the project root is invalid or spawn fails.
    pub fn start_play(
        executable: impl AsRef<Path>,
        arguments: &[String],
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ManagedProcessError> {
        Self::start_play_inner(executable, arguments, project_root, false)
    }

    /// Starts the engine Play runner (may live outside the project tree).
    ///
    /// Working directory remains the project root so relative `--scene` paths
    /// resolve. The executable itself may be the workspace `yuyib-play` binary.
    ///
    /// # Errors
    ///
    /// Returns an error when the project root is invalid, the binary is missing,
    /// or spawn fails.
    pub fn start_play_engine_runner(
        executable: impl AsRef<Path>,
        arguments: &[String],
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ManagedProcessError> {
        Self::start_play_inner(executable, arguments, project_root, true)
    }

    fn start_play_inner(
        executable: impl AsRef<Path>,
        arguments: &[String],
        project_root: impl AsRef<Path>,
        allow_external_executable: bool,
    ) -> Result<Self, ManagedProcessError> {
        let project_root = project_root
            .as_ref()
            .canonicalize()
            .map_err(ManagedProcessError::Io)?;
        if !project_root.is_dir() {
            return Err(ManagedProcessError::ProjectRootNotDirectory(project_root));
        }
        let requested = executable.as_ref();
        let executable = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            project_root.join(requested)
        }
        .canonicalize()
        .map_err(ManagedProcessError::Io)?;
        if !executable.is_file() {
            return Err(ManagedProcessError::ExecutableOutsideProject(executable));
        }
        if !allow_external_executable && !executable.starts_with(&project_root) {
            return Err(ManagedProcessError::ExecutableOutsideProject(executable));
        }
        let mut command = Command::new(executable);
        command.args(arguments);
        Self::spawn(ProcessKind::Play, command, &project_root, None)
    }

    /// Starts scoped `cargo build` with Yuyib's compilation job bound.
    ///
    /// Used by Play when no compiled binary exists yet.
    ///
    /// # Errors
    ///
    /// Rejects an invalid package name, missing `Cargo.toml`, concurrent Cargo,
    /// or spawn failure.
    pub fn start_cargo_build(
        package: impl Into<String>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ManagedProcessError> {
        let package = package.into();
        if package.is_empty()
            || !package
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ManagedProcessError::InvalidPackage(package));
        }
        let project_root = project_root.as_ref();
        let cargo_toml_path = project_root.join("Cargo.toml");
        if !cargo_toml_path.is_file() {
            return Err(ManagedProcessError::MissingCargoToml(
                project_root.to_path_buf(),
            ));
        }
        let cargo_toml = fs::read_to_string(&cargo_toml_path).map_err(ManagedProcessError::Io)?;
        let requires_package_selector = cargo_toml.contains("[workspace]")
            && !cargo_toml
                .lines()
                .any(|line| line.trim_start().starts_with("[package]"));
        let mut command = Command::new("cargo");
        command.arg("build");
        if requires_package_selector {
            command.arg("-p").arg(&package);
        }
        command
            .arg("--message-format=json-diagnostic-rendered-ansi")
            .env("CARGO_BUILD_JOBS", "2");
        if CARGO_PROCESS_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ManagedProcessError::CargoAlreadyRunning);
        }
        match Self::spawn(
            ProcessKind::CargoCheck {
                package: package.clone(),
            },
            command,
            project_root,
            Some(Instant::now() + CARGO_CHECK_TIMEOUT),
        ) {
            Ok(mut process) => {
                process.owns_cargo_gate = true;
                Ok(process)
            }
            Err(error) => {
                CARGO_PROCESS_ACTIVE.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    /// Starts scoped `cargo check` with Yuyib's compilation job bound.
    ///
    /// Single-package project roots (scaffolded Editor projects) run plain
    /// `cargo check` so a renamed `[package].name` cannot break the action.
    /// Virtual workspaces still use `cargo check -p <package>`.
    ///
    /// Package names are restricted to Cargo's ordinary ASCII identifier
    /// subset so this API never becomes an arbitrary argument shell.
    ///
    /// # Errors
    ///
    /// Rejects an invalid package name, missing `Cargo.toml`, invalid project
    /// root, or spawn failure.
    pub fn start_cargo_check(
        package: impl Into<String>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ManagedProcessError> {
        let package = package.into();
        if package.is_empty()
            || !package
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ManagedProcessError::InvalidPackage(package));
        }
        let project_root = project_root.as_ref();
        let cargo_toml_path = project_root.join("Cargo.toml");
        if !cargo_toml_path.is_file() {
            return Err(ManagedProcessError::MissingCargoToml(
                project_root.to_path_buf(),
            ));
        }
        let cargo_toml = fs::read_to_string(&cargo_toml_path).map_err(ManagedProcessError::Io)?;
        let requires_package_selector = cargo_toml.contains("[workspace]")
            && !cargo_toml
                .lines()
                .any(|line| line.trim_start().starts_with("[package]"));
        let mut command = Command::new("cargo");
        command.arg("check");
        if requires_package_selector {
            command.arg("-p").arg(&package);
        }
        command
            .arg("--message-format=json-diagnostic-rendered-ansi")
            .env("CARGO_BUILD_JOBS", "2");
        if CARGO_PROCESS_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ManagedProcessError::CargoAlreadyRunning);
        }
        match Self::spawn(
            ProcessKind::CargoCheck { package },
            command,
            project_root,
            Some(Instant::now() + CARGO_CHECK_TIMEOUT),
        ) {
            Ok(mut process) => {
                process.owns_cargo_gate = true;
                Ok(process)
            }
            Err(error) => {
                CARGO_PROCESS_ACTIVE.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    fn spawn(
        kind: ProcessKind,
        mut command: Command,
        project_root: &Path,
        deadline: Option<Instant>,
    ) -> Result<Self, ManagedProcessError> {
        let project_root = project_root
            .canonicalize()
            .map_err(ManagedProcessError::Io)?;
        if !project_root.is_dir() {
            return Err(ManagedProcessError::ProjectRootNotDirectory(project_root));
        }
        command
            .current_dir(project_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.group_spawn().map_err(ManagedProcessError::Io)?;
        Ok(Self {
            kind,
            child,
            owns_cargo_gate: false,
            deadline,
        })
    }

    /// Returns why this child was started.
    #[must_use]
    pub const fn kind(&self) -> &ProcessKind {
        &self.kind
    }

    /// Takes stdout for a bounded background log reader.
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.inner().stdout.take()
    }

    /// Takes stderr for a bounded background diagnostic reader.
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.inner().stderr.take()
    }

    /// Polls without blocking the editor UI thread.
    ///
    /// # Errors
    ///
    /// Returns an OS process query failure.
    pub fn poll(&mut self) -> Result<ProcessPoll, ManagedProcessError> {
        if let Some(status) = self.child.try_wait().map_err(ManagedProcessError::Io)? {
            return Ok(ProcessPoll::Exited(status));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.child.kill().map_err(ManagedProcessError::Io)?;
            return self
                .child
                .wait()
                .map(ProcessPoll::TimedOut)
                .map_err(ManagedProcessError::Io);
        }
        Ok(ProcessPoll::Running)
    }

    /// Terminates and reaps the child. Calling this after normal exit is safe.
    ///
    /// # Errors
    ///
    /// Returns an OS process termination or wait failure.
    pub fn stop(&mut self) -> Result<ExitStatus, ManagedProcessError> {
        if let Some(status) = self.child.try_wait().map_err(ManagedProcessError::Io)? {
            return Ok(status);
        }
        self.child.kill().map_err(ManagedProcessError::Io)?;
        self.child.wait().map_err(ManagedProcessError::Io)
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if self.owns_cargo_gate {
            CARGO_PROCESS_ACTIVE.store(false, Ordering::Release);
        }
    }
}

/// Managed child-process failure.
#[derive(Debug)]
pub enum ManagedProcessError {
    /// Cargo package identifier contains unsupported characters.
    InvalidPackage(String),
    /// This Editor process already supervises a Cargo invocation.
    CargoAlreadyRunning,
    /// The project root has no `Cargo.toml`, so `cargo -p` would walk into a parent workspace.
    MissingCargoToml(std::path::PathBuf),
    /// Canonical project root is not a directory.
    ProjectRootNotDirectory(std::path::PathBuf),
    /// Play executable is not a regular file confined below the project root.
    ExecutableOutsideProject(std::path::PathBuf),
    /// OS process or filesystem operation failed.
    Io(io::Error),
}

impl fmt::Display for ManagedProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPackage(package) => write!(formatter, "invalid Cargo package: {package}"),
            Self::CargoAlreadyRunning => {
                formatter.write_str("a scoped Cargo process is already running")
            }
            Self::MissingCargoToml(path) => write!(
                formatter,
                "project is missing Cargo.toml under {}; create or reopen the project so scoped cargo check does not walk into a parent workspace",
                path.display()
            ),
            Self::ProjectRootNotDirectory(path) => {
                write!(
                    formatter,
                    "process project root is not a directory: {}",
                    path.display()
                )
            }
            Self::ExecutableOutsideProject(path) => write!(
                formatter,
                "Play executable must be a project file: {}",
                path.display()
            ),
            Self::Io(error) => write!(formatter, "managed process failed: {error}"),
        }
    }
}

impl Error for ManagedProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidPackage(_)
            | Self::CargoAlreadyRunning
            | Self::MissingCargoToml(_)
            | Self::ProjectRootNotDirectory(_)
            | Self::ExecutableOutsideProject(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_package_validation_rejects_argument_injection() {
        let result = ManagedProcess::start_cargo_check("demo --workspace", ".");
        assert!(matches!(
            result,
            Err(ManagedProcessError::InvalidPackage(_))
        ));
    }

    #[test]
    fn cargo_check_rejects_missing_manifest_before_spawn() {
        let project =
            std::env::temp_dir().join(format!("yuyib-editor-cargo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&project).expect("temporary project");
        assert!(matches!(
            ManagedProcess::start_cargo_check("prj", &project),
            Err(ManagedProcessError::MissingCargoToml(_))
        ));
        std::fs::remove_dir_all(project).expect("remove temporary project");
    }

    #[test]
    fn play_executable_must_remain_inside_project_root() {
        let project =
            std::env::temp_dir().join(format!("yuyib-editor-process-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&project).expect("temporary project");
        let outside = std::env::current_exe().expect("current test executable");
        assert!(matches!(
            ManagedProcess::start_play(&outside, &[], &project),
            Err(ManagedProcessError::ExecutableOutsideProject(_))
        ));
        std::fs::remove_dir_all(project).expect("remove temporary project");
    }
}
