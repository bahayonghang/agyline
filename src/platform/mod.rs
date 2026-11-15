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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformKind {
    Windows,
    Unix,
}

pub fn host_platform() -> PlatformKind {
    imp::host_platform()
}

pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    imp::short_path_name(path)
}

pub fn user_home_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    imp::user_home_dir(env_lookup)
}

pub fn relay_temp_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    imp::relay_temp_dir(env_lookup)
}

pub fn spawn_detached_self(args: &[&OsStr]) -> io::Result<()> {
    imp::spawn_detached_self(args)
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    imp::ensure_private_dir(path)
}

pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    imp::create_file_symlink(original, link)
}

pub fn replace_file_with_retry(tmp_path: &Path, dst_path: &Path) -> io::Result<()> {
    imp::replace_file_with_retry(tmp_path, dst_path)
}

pub fn configure_background_command(cmd: &mut Command) {
    imp::configure_background_command(cmd);
}

pub fn stdout_terminal_columns() -> Option<usize> {
    imp::stdout_terminal_columns()
}
