use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use self::unix as imp;
#[cfg(windows)]
use self::windows as imp;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

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

pub fn replace_file_with_retry(tmp: &Path, dst: &Path) -> io::Result<()> {
    imp::replace_file_with_retry(tmp, dst)
}
