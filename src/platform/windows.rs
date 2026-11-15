use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetShortPathNameW(
        lpsz_long_path: *const u16,
        lpsz_short_path: *mut u16,
        cch_buffer: u32,
    ) -> u32;
}

pub fn host_platform() -> super::PlatformKind {
    super::PlatformKind::Windows
}

pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated UTF-16; null output buffer with length 0 queries required size.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `buf` has length `needed` UTF-16 code units as requested by the first call.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    if written == 0 || (written as usize) >= buf.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..written as usize])))
}

pub fn user_home_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(val) = env_lookup(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(PathBuf::from(trimmed));
            }
        }
    }
    None
}

pub fn relay_temp_dir(env_lookup: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    for key in ["TMPDIR", "TEMP", "TMP"] {
        if let Some(val) = env_lookup(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
        }
    }
    std::env::temp_dir()
}

pub fn spawn_detached_self(args: &[&OsStr]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
}
