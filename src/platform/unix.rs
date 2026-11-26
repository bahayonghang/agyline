//! POSIX half of the platform seam. Same contracts as `windows.rs`, different mechanisms: a new
//! process group instead of creation flags, mode 0700 instead of an ACL, `ioctl` instead of the
//! console API. A detached worker never holds the render's stdout pipe and is never waited on.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static EXE_SWAP_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn host_platform() -> super::PlatformKind {
    super::PlatformKind::Unix
}

/// No 8.3 alias concept on POSIX, so this is always `None`.
pub fn short_path_name(_path: &Path) -> Option<PathBuf> {
    None
}

/// HOME only, trimmed; `None` when unset or blank. No passwd/NSS lookup, which could block the
/// render on a network directory service.
pub fn user_home_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(val) = env_lookup("HOME") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    None
}

/// Hardcoded `/tmp`, ignoring TMPDIR: `cache_relay.py` and `statusline.py` both use `/tmp` on
/// non-Windows, so the relay state file is only ever written there.
pub fn relay_temp_dir(_env_lookup: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    PathBuf::from("/tmp")
}

/// Detached worker via a new process group and null stdio; returns without reaping the child.
pub fn spawn_detached_self(args: &[&OsStr]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    // Null stdio keeps worker output out of the render's stdout pipe, which `agy` prints.
    // A new process group keeps terminal Ctrl+C off the worker. Dropping `Child` without `wait`
    // is deliberate: the render exits at once and init reaps the orphan.
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// Creates `path`, then chmods the leaf to 0700: state holds session ids and telemetry. Parents
/// made by `create_dir_all` keep the umask mode.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// POSIX symlinks need no privilege, unlike Windows (OS error 1314 without Developer Mode).
pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

/// One `rename` is atomic here and replaces a file other processes hold open, so there is no
/// retry loop; `tmp` is still cleaned up on failure.
pub fn replace_file_with_retry(tmp: &Path, dst: &Path) -> io::Result<()> {
    match fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(tmp);
            Err(err)
        }
    }
}

/// No-op: a POSIX child has no console window to hide.
pub fn configure_background_command(_cmd: &mut Command) {}

// Must match the kernel's `struct winsize`: `ioctl` fills it through a raw pointer.
#[repr(C)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

// ioctl request numbers encode direction and struct size and differ per OS, so the value is
// pinned per target instead of pulling in libc for this one call.
#[cfg(target_os = "macos")]
const TIOCGWINSZ: core::ffi::c_ulong = 0x4008_7468;
#[cfg(not(target_os = "macos"))]
const TIOCGWINSZ: core::ffi::c_ulong = 0x5413;

unsafe extern "C" {
    fn ioctl(fd: core::ffi::c_int, request: core::ffi::c_ulong, ...) -> core::ffi::c_int;
}

/// Queries fd 1 (stdout) with TIOCGWINSZ. A pipe fails with ENOTTY, so `COLUMNS` is the fallback;
/// `None` when neither yields a positive width.
pub fn stdout_terminal_columns() -> Option<usize> {
    let mut ws = Winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `ws` is a valid mutable `Winsize` struct passed to `TIOCGWINSZ` on stdout fd 1.
    let rc = unsafe { ioctl(1, TIOCGWINSZ, &mut ws) };
    if rc == 0 && ws.ws_col > 0 {
        return Some(usize::from(ws.ws_col));
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
}

/// POSIX terminals signal polarity via `COLORFGBG`; we avoid spawning `defaults`/`gsettings` on
/// the render hot path, so this returns `None` and lets `auto` default to dark.
pub fn system_theme_polarity() -> Option<crate::config::ThemePolarity> {
    None
}

pub fn exe_filename(stem: &str) -> String {
    stem.to_string()
}

pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

/// Copies `staged_bin` onto `target_exe`'s filesystem with mode 0755, then atomically renames it
/// over `target_exe`. Concurrent `agy` renders keep executing the unlinked old inode cleanly.
pub fn replace_executable(staged_bin: &Path, target_exe: &Path) -> io::Result<()> {
    let parent = target_exe.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "target executable has no parent directory",
        )
    })?;
    let target_name = target_exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "target executable has no valid filename",
            )
        })?;

    let seq = EXE_SWAP_SEQ.fetch_add(1, Ordering::Relaxed);
    let sibling_new = parent.join(format!(".{target_name}.new.{}.{seq}", std::process::id()));
    if let Err(err) = fs::copy(staged_bin, &sibling_new) {
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }
    if let Err(err) = fs::set_permissions(&sibling_new, fs::Permissions::from_mode(0o755)) {
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }
    if let Err(err) = fs::rename(&sibling_new, target_exe) {
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }
    Ok(())
}

/// Best-effort sweep for `<exe>.old.*` backups beside `target_exe`.
pub fn cleanup_stale_executable_backups(target_exe: &Path) {
    let Some(parent) = target_exe.parent() else {
        return;
    };
    let Some(target_name) = target_exe.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix = format!("{target_name}.old.");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.starts_with(&prefix) {
            let _ = fs::remove_file(entry.path());
        }
    }
}
