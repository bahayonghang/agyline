//! Self-update orchestration (`agyline update [branch]`). Builds the requested branch into a
//! throwaway staging root via `cargo install --git --locked`, verifies the staged binary, then
//! delegates in-place executable replacement to `platform::replace_executable`. Never invoked on
//! the live statusline render path.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cli::CliStreams;
use crate::platform;
use crate::state::{atomic_write_bytes, ensure_state_dir};

pub const DEFAULT_UPDATE_BRANCH: &str = "main";
pub const DEFAULT_UPDATE_REPO: &str = concat!(env!("CARGO_PKG_REPOSITORY"), ".git");
pub const UPDATE_REPO_ENV: &str = "AGYLINE_UPDATE_REPO";

const UPDATE_STATE_FILE_NAME: &str = "update_state.json";
// Bound `git ls-remote` precheck so an unreachable host cannot hang `agyline update` indefinitely.
const GIT_LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(15);

static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateOptions {
    pub branch: String,
    pub check: bool,
    pub force: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            branch: DEFAULT_UPDATE_BRANCH.to_string(),
            check: false,
            force: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateEnv {
    pub agyline_home: PathBuf,
    pub target_exe: PathBuf,
    pub repo_url: String,
    pub current_version: String,
}

impl UpdateEnv {
    /// Captures live process home, canonical executable path, and repository override once.
    pub fn from_process() -> Result<Self, UpdateError> {
        let lookup = |k: &str| std::env::var(k).ok();
        let explicit_home = lookup("AGYLINE_HOME").filter(|s| !s.trim().is_empty());
        let user_home = platform::user_home_dir(&lookup);
        let agyline_home = match (&explicit_home, &user_home) {
            (_, Some(home)) => crate::config::resolve_home_dir(&lookup, home),
            (Some(custom), None) => PathBuf::from(custom.trim()),
            (None, None) => return Err(UpdateError::MissingHomeDir),
        };

        let raw_exe = std::env::current_exe().map_err(|err| UpdateError::Io {
            path: PathBuf::from("<current_exe>"),
            message: err.to_string(),
        })?;
        // Resolve symlinks so self-update replaces the real executable instead of clobbering a link.
        let resolved_exe = fs::canonicalize(&raw_exe).unwrap_or(raw_exe);
        let target_exe = platform::strip_verbatim_prefix(resolved_exe);

        let repo_url = lookup(UPDATE_REPO_ENV)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_UPDATE_REPO.to_string());

        Ok(Self {
            agyline_home,
            target_exe,
            repo_url,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }

    pub fn state_path(&self) -> PathBuf {
        self.agyline_home.join("state").join(UPDATE_STATE_FILE_NAME)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    MissingHomeDir,
    InvalidBranch { branch: String, reason: String },
    CargoNotFound,
    CargoFailed(String),
    StagedBinaryMissing(PathBuf),
    VerificationFailed(String),
    TargetIsBuildArtifact(PathBuf),
    Io { path: PathBuf, message: String },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHomeDir => {
                write!(
                    f,
                    "cannot resolve user home directory; set HOME/USERPROFILE or AGYLINE_HOME"
                )
            }
            Self::InvalidBranch { branch, reason } => {
                write!(f, "invalid branch '{branch}': {reason}")
            }
            Self::CargoNotFound => {
                write!(
                    f,
                    "`cargo` was not found in PATH; install Rust 1.88+ to update agyline from source"
                )
            }
            Self::CargoFailed(msg) => write!(f, "cargo install failed: {msg}"),
            Self::StagedBinaryMissing(path) => {
                write!(f, "staged binary was not produced at '{}'", path.display())
            }
            Self::VerificationFailed(msg) => {
                write!(f, "failed to verify newly built agyline binary: {msg}")
            }
            Self::TargetIsBuildArtifact(path) => {
                write!(
                    f,
                    "refusing to overwrite local cargo build artifact '{}'; run the installed agyline binary, pass --check, or pass --force",
                    path.display()
                )
            }
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for UpdateError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteProbe {
    Commit(String),
    BranchNotFound,
    Skipped(String),
    TransportError(String),
}

/// Persisted after each successful `agyline update` under `<AGYLINE_HOME>/state/update_state.json`.
/// Recording the installed binary's size and mtime ensures manual `cargo install --path .` builds
/// invalidate stale commit metadata automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateState {
    pub repo: String,
    pub branch: String,
    pub commit: Option<String>,
    pub version: String,
    pub exe_len: u64,
    pub exe_mtime_secs: u64,
    pub exe_mtime_nanos: u32,
    pub updated_at_ms: u64,
}

/// External tool seam so tests exercise full update planning, state tracking, and executable swap
/// without spawning real network Cargo builds.
pub trait UpdateRunner {
    fn check_cargo_available(&self) -> Result<(), UpdateError>;
    fn probe_remote_branch(&self, repo_url: &str, branch: &str) -> RemoteProbe;
    fn build_staged_binary(
        &self,
        repo_url: &str,
        branch: &str,
        staging_root: &Path,
    ) -> Result<PathBuf, UpdateError>;
    fn verify_staged_binary(&self, staged_bin: &Path) -> Result<String, UpdateError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemUpdateRunner;

impl UpdateRunner for SystemUpdateRunner {
    fn check_cargo_available(&self) -> Result<(), UpdateError> {
        let mut cmd = Command::new("cargo");
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(UpdateError::CargoFailed(format!(
                "`cargo --version` exited with {status}"
            ))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Err(UpdateError::CargoNotFound),
            Err(err) => Err(UpdateError::CargoFailed(err.to_string())),
        }
    }

    fn probe_remote_branch(&self, repo_url: &str, branch: &str) -> RemoteProbe {
        probe_remote_branch(repo_url, branch, GIT_LS_REMOTE_TIMEOUT)
    }

    fn build_staged_binary(
        &self,
        repo_url: &str,
        branch: &str,
        staging_root: &Path,
    ) -> Result<PathBuf, UpdateError> {
        let status = Command::new("cargo")
            .args([
                "install", "--git", repo_url, "--branch", branch, "--bin", "agyline", "--locked",
                "--root",
            ])
            .arg(staging_root)
            .arg("--force")
            .stdin(Stdio::null())
            .status()
            .map_err(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    UpdateError::CargoNotFound
                } else {
                    UpdateError::CargoFailed(err.to_string())
                }
            })?;

        if !status.success() {
            return Err(UpdateError::CargoFailed(format!(
                "exit status {}",
                status
                    .code()
                    .map_or_else(|| "terminated by signal".to_string(), |c| c.to_string())
            )));
        }

        let staged_bin = staging_root
            .join("bin")
            .join(platform::exe_filename("agyline"));
        if !staged_bin.is_file() {
            return Err(UpdateError::StagedBinaryMissing(staged_bin));
        }
        Ok(staged_bin)
    }

    fn verify_staged_binary(&self, staged_bin: &Path) -> Result<String, UpdateError> {
        let mut cmd = Command::new(staged_bin);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        platform::configure_background_command(&mut cmd);
        let output = cmd
            .output()
            .map_err(|err| UpdateError::VerificationFailed(err.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(UpdateError::VerificationFailed(format!(
                "`--version` exited with {} ({})",
                output.status,
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_version_output(&stdout).ok_or_else(|| {
            UpdateError::VerificationFailed(format!(
                "unexpected `--version` output: '{}'",
                stdout.trim()
            ))
        })
    }
}

/// Validates a branch name against git check-ref-format rules and rejects leading `-` so branch
/// values can never be misinterpreted as flags by downstream tools.
pub fn validate_branch_name(raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err("branch name cannot be empty".to_string());
    }
    if raw.starts_with('-') {
        return Err("branch name cannot start with '-'".to_string());
    }
    if raw == "@" || raw.contains("@{") {
        return Err("branch name cannot be '@' or contain '@{'".to_string());
    }
    if raw.starts_with('/') || raw.ends_with('/') || raw.contains("//") {
        return Err("branch name has invalid slash placement".to_string());
    }
    if raw.starts_with('.') || raw.ends_with('.') || raw.contains("..") || raw.contains("/.") {
        return Err("branch name has invalid dot sequence".to_string());
    }
    if raw.ends_with(".lock") || raw.contains(".lock/") {
        return Err("branch name cannot end with '.lock'".to_string());
    }
    for ch in raw.chars() {
        if ch.is_ascii_control()
            || ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        {
            return Err(format!("branch name contains forbidden character '{ch}'"));
        }
    }
    Ok(raw.to_string())
}

/// Detects executables running straight out of a local Cargo `target/{debug,release}` directory so
/// `cargo run -- update` does not silently overwrite a build artifact.
pub fn is_cargo_target_artifact(exe_path: &Path) -> bool {
    let Some(profile_dir) = exe_path.parent() else {
        return false;
    };
    let Some(profile_name) = profile_dir.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !matches!(profile_name, "debug" | "release") {
        return false;
    }
    let Some(target_dir) = profile_dir.parent() else {
        return false;
    };
    if target_dir.file_name().and_then(|n| n.to_str()) != Some("target") {
        return false;
    }
    target_dir
        .parent()
        .is_some_and(|repo_root| repo_root.join("Cargo.toml").is_file())
}

/// Queries `git ls-remote --heads <repo_url> refs/heads/<branch>` with a hard wall-clock bound.
/// Distinguishes a reachable repo missing the branch (`BranchNotFound`) from missing `git`
/// (`Skipped`) or transport errors (`TransportError`).
pub fn probe_remote_branch(repo_url: &str, branch: &str, timeout: Duration) -> RemoteProbe {
    let ref_name = format!("refs/heads/{branch}");
    let mut cmd = Command::new("git");
    cmd.args(["ls-remote", "--heads", "--", repo_url, &ref_name])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::configure_background_command(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return RemoteProbe::Skipped("git not found in PATH".to_string());
        }
        Err(err) => return RemoteProbe::TransportError(format!("failed to start git: {err}")),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    // Drain both pipes on background threads so large error output cannot deadlock `try_wait`.
    let stdout_handle = thread::spawn(move || read_bounded_pipe(stdout_pipe, 64 * 1024));
    let stderr_handle = thread::spawn(move || read_bounded_pipe(stderr_pipe, 64 * 1024));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    let Some(status) = status else {
        return RemoteProbe::TransportError(format!(
            "git ls-remote timed out after {}s",
            timeout.as_secs()
        ));
    };

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        let msg = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("git ls-remote returned non-zero status");
        return RemoteProbe::TransportError(msg.to_string());
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    match parse_ls_remote_commit(&stdout, &ref_name) {
        Some(sha) => RemoteProbe::Commit(sha),
        None => RemoteProbe::BranchNotFound,
    }
}

pub fn load_update_state(path: &Path) -> Option<UpdateState> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<UpdateState>(&raw).ok()
}

pub fn run_update_cli(env: &UpdateEnv, opts: &UpdateOptions, streams: &mut CliStreams<'_>) -> u8 {
    let runner = SystemUpdateRunner;
    run_update_with_runner(env, opts, &runner, streams)
}

/// Core update pipeline parameterized by `UpdateRunner` so unit and integration tests can exercise
/// every branch, state check, and file-swap path offline.
pub fn run_update_with_runner(
    env: &UpdateEnv,
    opts: &UpdateOptions,
    runner: &dyn UpdateRunner,
    streams: &mut CliStreams<'_>,
) -> u8 {
    if let Err(reason) = validate_branch_name(&opts.branch) {
        let err = UpdateError::InvalidBranch {
            branch: opts.branch.clone(),
            reason,
        };
        let _ = writeln!(streams.stderr, "error: {err}");
        return 2;
    }

    if !opts.check {
        platform::cleanup_stale_executable_backups(&env.target_exe);
        if !opts.force && is_cargo_target_artifact(&env.target_exe) {
            let err = UpdateError::TargetIsBuildArtifact(env.target_exe.clone());
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    }

    let probe = runner.probe_remote_branch(&env.repo_url, &opts.branch);
    let remote_sha = match probe {
        RemoteProbe::Commit(sha) => Some(sha),
        RemoteProbe::BranchNotFound => {
            let _ = writeln!(
                streams.stderr,
                "error: remote branch '{}' not found in {}",
                opts.branch, env.repo_url
            );
            return 1;
        }
        RemoteProbe::TransportError(msg) => {
            if opts.check {
                let _ = writeln!(
                    streams.stderr,
                    "error: failed to check remote branch '{}': {msg}",
                    opts.branch
                );
                return 1;
            }
            // Users may configure proxies/mirrors only in Cargo's config rather than git, so warn
            // and let `cargo install --git` perform the fetch directly.
            let _ = writeln!(
                streams.stderr,
                "warning: could not precheck remote branch '{}' via git ({msg}); continuing with cargo...",
                opts.branch
            );
            None
        }
        RemoteProbe::Skipped(reason) => {
            if opts.check {
                let _ = writeln!(streams.stdout, "Remote precheck skipped ({reason}).");
            }
            None
        }
    };

    let state = load_update_state(&env.state_path());
    let already_up_to_date = remote_sha.as_deref().is_some_and(|sha| {
        state
            .as_ref()
            .is_some_and(|recorded| state_matches_installed(recorded, env, sha))
    });

    if opts.check {
        let _ = writeln!(
            streams.stdout,
            "Current binary: {} (v{})",
            env.target_exe.display(),
            env.current_version
        );
        let _ = writeln!(streams.stdout, "Repository:     {}", env.repo_url);
        let _ = writeln!(streams.stdout, "Target branch:  {}", opts.branch);
        if let Some(ref sha) = remote_sha {
            let _ = writeln!(streams.stdout, "Remote commit:  {} ({sha})", short_sha(sha));
        }
        if let Some(ref recorded) = state {
            let recorded_sha = recorded
                .commit
                .as_deref()
                .map(short_sha)
                .unwrap_or("unknown");
            let _ = writeln!(
                streams.stdout,
                "Recorded state: {} @ {} (v{})",
                recorded.branch, recorded_sha, recorded.version
            );
        }
        if already_up_to_date && !opts.force {
            let _ = writeln!(streams.stdout, "Status:         already up to date");
        } else {
            let status_label = if already_up_to_date {
                "rebuild forced (--force)"
            } else if remote_sha.is_some() {
                "update available"
            } else {
                "ready to build from remote branch"
            };
            let _ = writeln!(streams.stdout, "Status:         {status_label}");
            let _ = writeln!(
                streams.stdout,
                "Command:        cargo install --git {} --branch {} --bin agyline --locked --root <staging> --force",
                env.repo_url, opts.branch
            );
        }
        if let Err(err) = runner.check_cargo_available() {
            let _ = writeln!(streams.stderr, "warning: {err}");
        }
        let _ = writeln!(streams.stdout, "Dry run: no files were modified.");
        let _ = streams.stdout.flush();
        return 0;
    }

    if already_up_to_date && !opts.force {
        let sha = remote_sha.as_deref().unwrap_or("unknown");
        if let Some(mut recorded) = state
            && recorded.branch != opts.branch
        {
            recorded.branch = opts.branch.clone();
            let _ = save_update_state(&env.agyline_home, &recorded);
        }
        let _ = writeln!(
            streams.stdout,
            "agyline {} is already up to date on '{}' ({}).",
            env.current_version,
            opts.branch,
            short_sha(sha)
        );
        let _ = streams.stdout.flush();
        return 0;
    }

    if let Err(err) = runner.check_cargo_available() {
        let _ = writeln!(streams.stderr, "error: {err}");
        return 1;
    }

    match remote_sha.as_deref() {
        Some(sha) => {
            let _ = writeln!(
                streams.stdout,
                "Updating agyline from '{}' ({})...",
                opts.branch,
                short_sha(sha)
            );
        }
        None => {
            let _ = writeln!(streams.stdout, "Updating agyline from '{}'...", opts.branch);
        }
    }
    let _ = streams.stdout.flush();

    let staging = match StagingDir::create() {
        Ok(dir) => dir,
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    };

    let staged_bin = match runner.build_staged_binary(&env.repo_url, &opts.branch, staging.path()) {
        Ok(bin) => bin,
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    };

    let new_version = match runner.verify_staged_binary(&staged_bin) {
        Ok(ver) => ver,
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    };

    if let Err(err) = platform::replace_executable(&staged_bin, &env.target_exe) {
        let _ = writeln!(
            streams.stderr,
            "error: failed to replace '{}': {err}",
            env.target_exe.display()
        );
        let _ = writeln!(
            streams.stderr,
            "hint: close running agy sessions or install manually with: cargo install --git {} --branch {} --bin agyline --locked --force",
            env.repo_url, opts.branch
        );
        return 1;
    }

    if let Some((exe_len, exe_mtime_secs, exe_mtime_nanos)) = exe_fingerprint(&env.target_exe) {
        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let next_state = UpdateState {
            repo: env.repo_url.clone(),
            branch: opts.branch.clone(),
            commit: remote_sha.clone(),
            version: new_version.clone(),
            exe_len,
            exe_mtime_secs,
            exe_mtime_nanos,
            updated_at_ms,
        };
        if let Err(err) = save_update_state(&env.agyline_home, &next_state) {
            let _ = writeln!(
                streams.stderr,
                "warning: updated binary successfully, but failed to record update state: {err}"
            );
        }
    }

    let version_summary = if env.current_version == new_version {
        format!("v{new_version}")
    } else {
        format!("v{} -> v{new_version}", env.current_version)
    };
    match remote_sha.as_deref() {
        Some(sha) => {
            let _ = writeln!(
                streams.stdout,
                "Updated agyline {version_summary} ({} @ {}) at {}.",
                opts.branch,
                short_sha(sha),
                env.target_exe.display()
            );
        }
        None => {
            let _ = writeln!(
                streams.stdout,
                "Updated agyline {version_summary} ({}) at {}.",
                opts.branch,
                env.target_exe.display()
            );
        }
    }
    let _ = streams.stdout.flush();
    0
}

fn save_update_state(agyline_home: &Path, state: &UpdateState) -> io::Result<()> {
    let state_dir = ensure_state_dir(agyline_home)?;
    let mut bytes = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    bytes.push(b'\n');
    atomic_write_bytes(&state_dir, UPDATE_STATE_FILE_NAME, &bytes)
}

fn state_matches_installed(recorded: &UpdateState, env: &UpdateEnv, remote_sha: &str) -> bool {
    if recorded.repo != env.repo_url || recorded.commit.as_deref() != Some(remote_sha) {
        return false;
    }
    exe_fingerprint(&env.target_exe)
        == Some((
            recorded.exe_len,
            recorded.exe_mtime_secs,
            recorded.exe_mtime_nanos,
        ))
}

fn exe_fingerprint(path: &Path) -> Option<(u64, u64, u32)> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), modified.as_secs(), modified.subsec_nanos()))
}

fn parse_ls_remote_commit(stdout: &str, expected_ref: &str) -> Option<String> {
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else {
            continue;
        };
        let Some(ref_col) = parts.next() else {
            continue;
        };
        if ref_col == expected_ref && sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Some(sha.to_ascii_lowercase());
        }
    }
    None
}

fn parse_version_output(stdout: &str) -> Option<String> {
    let line = stdout.lines().next()?.trim();
    let ver = line.strip_prefix("agyline ")?.trim();
    if ver.is_empty() || ver.contains(char::is_whitespace) {
        return None;
    }
    Some(ver.to_string())
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn read_bounded_pipe(pipe: Option<impl Read>, limit: usize) -> Vec<u8> {
    let Some(pipe) = pipe else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let _ = pipe.take(limit as u64).read_to_end(&mut out);
    out
}

/// Throwaway Cargo `--root` directory removed automatically when dropped so failed or interrupted
/// updates never leak build outputs.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    fn create() -> Result<Self, UpdateError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agyline-update-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&path).map_err(|err| UpdateError::Io {
            path: path.clone(),
            message: err.to_string(),
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
