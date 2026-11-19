//! OS seam for host services: detached self-spawn, private state dirs, symlinks, atomic file
//! swap, home/temp resolution, and terminal width. The printing path spawns no shell and waits
//! on no `git`; `spawn_detached_self` moves that work into `__refresh-git` / `__touch` workers.
//! Only the host OS module is compiled, aliased as `imp`, so the wrappers below stay `cfg`-free.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use self::unix as imp;
#[cfg(windows)]
use self::windows as imp;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Which OS's rules apply, as a value rather than a `cfg`: install branches on it to pick a
/// command-string strategy, so tests can exercise Windows paths on a Unix host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Windows,
    Unix,
}

pub fn host_platform() -> PlatformKind {
    imp::host_platform()
}

/// Windows 8.3 alias for `path`, or `None` on Unix and whenever the alias cannot be produced.
/// Install uses it to write a `statusLine.command` that cmd.exe can run without quoting.
pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    imp::short_path_name(path)
}

/// Profile directory from the environment; `None` when no candidate is set or all are blank.
/// `env_lookup` is injected so tests can vary it without mutating process-wide env.
pub fn user_home_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    imp::user_home_dir(env_lookup)
}

/// Directory where `cache_relay.py` keeps `agy_cache_<session>.dat`; the line must read the same
/// directory the relay writes. Always returns a path, falling back to the OS temp dir.
pub fn relay_temp_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    imp::relay_temp_dir(env_lookup)
}

/// Re-invokes this binary as a background worker (`__refresh-git`, `__touch`) and returns as soon
/// as the child exists: no waiting, no inherited stdio, and the child outlives the render. This is
/// the mechanism that keeps git and relay touches off the printing path.
pub fn spawn_detached_self(args: &[&OsStr]) -> io::Result<()> {
    imp::spawn_detached_self(args)
}

/// Creates `path` and restricts it to the current user where the OS offers a cheap mode bit
/// (0700 on Unix); on Windows it only creates, relying on the inherited profile ACL.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    imp::ensure_private_dir(path)
}

/// Creates a file symlink. Windows needs admin privilege or Developer Mode and otherwise fails
/// with OS error 1314, so callers must handle that error instead of assuming success.
pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    imp::create_file_symlink(original, link)
}

/// Atomic `tmp_path` -> `dst_path` swap for state writes. Removes `tmp_path` on failure so no
/// `.tmp.*` litter is left behind; Windows retries transient sharing violations first.
pub fn replace_file_with_retry(tmp_path: &Path, dst_path: &Path) -> io::Result<()> {
    imp::replace_file_with_retry(tmp_path, dst_path)
}

/// Hides the console window of a child the parent still reads from (the worker's bounded git
/// call). No effect on Unix.
pub fn configure_background_command(cmd: &mut Command) {
    imp::configure_background_command(cmd);
}

/// Visible width of stdout's terminal, always positive. `None` when stdout is a pipe (the live
/// `agy` render) or no size is available; `COLUMNS` is the fallback before giving up.
pub fn stdout_terminal_columns() -> Option<usize> {
    imp::stdout_terminal_columns()
}
