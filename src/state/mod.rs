use std::io;
use std::path::{Path, PathBuf};

use crate::platform;

pub fn ensure_state_dir(home_dir: &Path) -> io::Result<PathBuf> {
    let dir = home_dir.join("state");
    platform::ensure_private_dir(&dir)?;
    Ok(dir)
}
