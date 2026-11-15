use std::io;
use std::path::{Path, PathBuf};

pub use self::git_cache::{
    GitCacheEntry, cwd_hash_hex, git_cache_path, git_lock_path, normalize_cwd_lexical,
    read_git_cache, write_git_cache,
};
pub use self::sweep::maybe_sweep_stale_state;
use crate::platform;

pub mod git_cache;
pub mod sweep;

pub fn ensure_state_dir(home_dir: &Path) -> io::Result<PathBuf> {
    let dir = home_dir.join("state");
    platform::ensure_private_dir(&dir)?;
    Ok(dir)
}
