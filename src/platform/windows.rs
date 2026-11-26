//! Win32 half of the platform seam. kernel32 entry points and `#[repr(C)]` layouts are declared
//! by hand, so field order must match the SDK; no windows-sys dependency. The printing path
//! spawns no shell and waits on no `git`: workers get no console, no inherited handles and no
//! window, and the parent returns without waiting.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

static EXE_SWAP_SEQ: AtomicU64 = AtomicU64::new(0);

// Worker creation flags: no inherited console, a separate process group so terminal Ctrl+C does
// not kill the refresh, and no console window flashing while the line is being printed.
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// Win32 pseudo-handles are negative i32 constants; the API takes them as u32.
const STD_OUTPUT_HANDLE: u32 = (-11_i32) as u32;
const INVALID_HANDLE_VALUE: isize = -1;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
// Predefined Win32 registry root (`((HKEY)(ULONG_PTR)((LONG)0x80000001))`).
const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
const ERROR_SUCCESS: i32 = 0;

type Handle = *mut core::ffi::c_void;

#[repr(C)]
struct Coord {
    x: i16,
    y: i16,
}

#[repr(C)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
struct ConsoleScreenBufferInfo {
    dw_size: Coord,
    dw_cursor_position: Coord,
    w_attributes: u16,
    sr_window: SmallRect,
    dw_maximum_window_size: Coord,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    lp_reserved: *mut u16,
    lp_desktop: *mut u16,
    lp_title: *mut u16,
    dw_x: u32,
    dw_y: u32,
    dw_x_size: u32,
    dw_y_size: u32,
    dw_x_count_chars: u32,
    dw_y_count_chars: u32,
    dw_fill_attribute: u32,
    dw_flags: u32,
    w_show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: Handle,
    h_std_output: Handle,
    h_std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
    h_process: Handle,
    h_thread: Handle,
    dw_process_id: u32,
    dw_thread_id: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateProcessW(
        lp_application_name: *const u16,
        lp_command_line: *mut u16,
        lp_process_attributes: *const core::ffi::c_void,
        lp_thread_attributes: *const core::ffi::c_void,
        b_inherit_handles: i32,
        dw_creation_flags: u32,
        lp_environment: *const core::ffi::c_void,
        lp_current_directory: *const u16,
        lp_startup_info: *const StartupInfoW,
        lp_process_information: *mut ProcessInformation,
    ) -> i32;

    fn CloseHandle(h_object: Handle) -> i32;

    fn GetShortPathNameW(
        lpsz_long_path: *const u16,
        lpsz_short_path: *mut u16,
        cch_buffer: u32,
    ) -> u32;

    fn GetStdHandle(n_std_handle: u32) -> Handle;

    fn GetConsoleScreenBufferInfo(
        h_console_output: Handle,
        lp_console_screen_buffer_info: *mut ConsoleScreenBufferInfo,
    ) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        hkey: Handle,
        lp_sub_key: *const u16,
        lp_value: *const u16,
        dw_flags: u32,
        pdw_type: *mut u32,
        pv_data: *mut core::ffi::c_void,
        pcb_data: *mut u32,
    ) -> i32;
}

pub fn host_platform() -> super::PlatformKind {
    super::PlatformKind::Windows
}

/// 8.3 alias of `path`, which install prefers when the exe path contains spaces or cmd.exe
/// metacharacters. `None` if the API fails or the alias would not fit the queried buffer.
pub fn short_path_name(path: &Path) -> Option<PathBuf> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: `wide` is NUL-terminated UTF-16; null output buffer with length 0 queries required size.
    let needed = unsafe { GetShortPathNameW(wide.as_ptr(), ptr::null_mut(), 0) };
    if needed == 0 {
        return None;
    }
    let mut buf = vec![0u16; needed as usize];
    // SAFETY: `buf` has length `needed` UTF-16 code units as requested by the first call.
    let written = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), needed) };
    // A full buffer means truncation with no NUL terminator; reject rather than over-read.
    if written == 0 || (written as usize) >= buf.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..written as usize])))
}

/// USERPROFILE first, then HOME for shells that export it; blank values are skipped. `None` lets
/// the caller fall back to `.`.
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

/// Where `cache_relay.py` writes `agy_cache_<session>.dat`: TMPDIR/TEMP/TMP in the same order as
/// Python's `tempfile.gettempdir()`, then the OS temp dir.
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

/// Starts this exe as a background worker (`__refresh-git`, `__touch`) and returns without
/// waiting. The child inherits no handles and gets no console, so it cannot write into the
/// line's stdout pipe, and it keeps running after the render exits.
pub fn spawn_detached_self(args: &[&OsStr]) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut app_wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    // CreateProcessW wants NUL-terminated UTF-16; an interior NUL would truncate the exe path.
    if app_wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable path contains interior NUL",
        ));
    }
    app_wide.push(0);

    // lpApplicationName is not split into argv, so the child's CRT still needs argv[0] repeated
    // at the front of lpCommandLine.
    let mut cmdline = Vec::new();
    append_windows_arg(&mut cmdline, exe.as_os_str())?;
    for arg in args {
        cmdline.push(u16::from(b' '));
        append_windows_arg(&mut cmdline, arg)?;
    }
    cmdline.push(0);

    // dw_flags stays 0 (no STARTF_USESTDHANDLES), so the null std handles are ignored.
    let si = StartupInfoW {
        cb: core::mem::size_of::<StartupInfoW>() as u32,
        lp_reserved: ptr::null_mut(),
        lp_desktop: ptr::null_mut(),
        lp_title: ptr::null_mut(),
        dw_x: 0,
        dw_y: 0,
        dw_x_size: 0,
        dw_y_size: 0,
        dw_x_count_chars: 0,
        dw_y_count_chars: 0,
        dw_fill_attribute: 0,
        dw_flags: 0,
        w_show_window: 0,
        cb_reserved2: 0,
        lp_reserved2: ptr::null_mut(),
        h_std_input: ptr::null_mut(),
        h_std_output: ptr::null_mut(),
        h_std_error: ptr::null_mut(),
    };
    let mut pi = ProcessInformation {
        h_process: ptr::null_mut(),
        h_thread: ptr::null_mut(),
        dw_process_id: 0,
        dw_thread_id: 0,
    };

    // SAFETY: `app_wide` and `cmdline` are NUL-terminated UTF-16 buffers valid for this call,
    // `b_inherit_handles` is 0 (`FALSE`) so zero parent pipe handles leak to the detached worker,
    // and returned process/thread handles are closed immediately on success.
    let ok = unsafe {
        CreateProcessW(
            app_wide.as_ptr(),
            cmdline.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // Close both handles so repeated spawns cannot leak them; the worker itself keeps running.
    unsafe {
        let _ = CloseHandle(pi.h_thread);
        let _ = CloseHandle(pi.h_process);
    }
    Ok(())
}

/// Appends one argument to `lpCommandLine`, quoted per the MSVCRT rules the child parses.
/// Interior NULs are rejected because they would truncate the command line.
fn append_windows_arg(cmdline: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let wide: Vec<u16> = arg.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains interior NUL",
        ));
    }

    // An empty argument must be quoted or it disappears; spaces, tabs, quotes and control
    // characters also force quoting.
    let quote = wide.is_empty()
        || wide.iter().any(|&c| {
            c == u16::from(b' ') || c == u16::from(b'\t') || c == u16::from(b'"') || c < 0x20
        });
    if !quote {
        cmdline.extend_from_slice(&wide);
        return Ok(());
    }

    cmdline.push(u16::from(b'"'));
    let mut backslashes: usize = 0;
    for &ch in &wide {
        if ch == u16::from(b'\\') {
            backslashes += 1;
        } else if ch == u16::from(b'"') {
            // 2n+1 backslashes: n pairs stay literal and the last one escapes the quote.
            for _ in 0..(backslashes * 2 + 1) {
                cmdline.push(u16::from(b'\\'));
            }
            cmdline.push(u16::from(b'"'));
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                cmdline.push(u16::from(b'\\'));
            }
            cmdline.push(ch);
            backslashes = 0;
        }
    }
    // Double trailing backslashes so they do not escape the closing quote.
    for _ in 0..(backslashes * 2) {
        cmdline.push(u16::from(b'\\'));
    }
    cmdline.push(u16::from(b'"'));
    Ok(())
}

/// Create only, no DACL rewrite: Windows has no cheap mode bit, and the default state location
/// under the user profile already inherits a private ACL (Unix chmods 0700 instead).
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Needs SeCreateSymbolicLinkPrivilege (admin) or Developer Mode; an unprivileged host fails with
/// OS error 1314 (ERROR_PRIVILEGE_NOT_HELD); callers must handle that, not assume success.
pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
}

/// Atomic tmp -> dst swap. Windows cannot rename over a file another process holds open, so
/// ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION (antivirus, indexer, a concurrent reader) get
/// four attempts 3 ms apart. `tmp` is removed on failure so no `.tmp.*` litter survives.
pub fn replace_file_with_retry(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut last_err = None;
    for attempt in 0..4 {
        match fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let is_transient = err.kind() == io::ErrorKind::PermissionDenied
                    || matches!(
                        err.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
                    );
                last_err = Some(err);
                if !is_transient || attempt == 3 {
                    break;
                }
                thread::sleep(Duration::from_millis(3));
            }
        }
    }
    let _ = fs::remove_file(tmp);
    Err(last_err.unwrap_or_else(|| io::Error::other("rename failed")))
}

pub fn exe_filename(stem: &str) -> String {
    format!("{stem}.exe")
}

pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

/// Windows cannot overwrite or delete a running `.exe`, but NTFS allows renaming it out of the
/// way. Copy `staged_bin` to a sibling file on the same volume first so cross-drive errors or
/// read-only target directories fail before `target_exe` is touched.
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
    let pid = std::process::id();
    let sibling_new = parent.join(format!(".{target_name}.new.{pid}.{seq}"));
    if let Err(err) = fs::copy(staged_bin, &sibling_new) {
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }

    if !target_exe.exists() {
        return rename_exe_with_retry(&sibling_new, target_exe).inspect_err(|_| {
            let _ = fs::remove_file(&sibling_new);
        });
    }

    let backup_old = parent.join(format!("{target_name}.old.{pid}.{seq}"));
    if let Err(err) = rename_exe_with_retry(target_exe, &backup_old) {
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }

    if let Err(err) = rename_exe_with_retry(&sibling_new, target_exe) {
        // Restore the original binary if placing the staged binary fails mid-swap.
        let _ = rename_exe_with_retry(&backup_old, target_exe);
        let _ = fs::remove_file(&sibling_new);
        return Err(err);
    }

    // Succeeds for non-running targets (such as tests); for the active exe Windows rejects deletion
    // until exit and the next `agyline update` sweeps the `.old.*` file.
    let _ = fs::remove_file(&backup_old);
    Ok(())
}

/// Removes prior `<exe>.old.*` files left behind when replacing a running Windows executable.
pub fn cleanup_stale_executable_backups(target_exe: &Path) {
    let Some(parent) = target_exe.parent() else {
        return;
    };
    let Some(target_name) = target_exe.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix = format!("{}.old.", target_name.to_ascii_lowercase());
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if name_str.to_ascii_lowercase().starts_with(&prefix) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

// Executables face brief antivirus/indexer scans longer than small JSON state files, so allow
// ~200 ms of transient ACCESS_DENIED / SHARING_VIOLATION retries before failing the swap.
fn rename_exe_with_retry(src: &Path, dst: &Path) -> io::Result<()> {
    let mut last_err = None;
    for attempt in 0..20 {
        match fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let is_transient = err.kind() == io::ErrorKind::PermissionDenied
                    || matches!(
                        err.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
                    );
                last_err = Some(err);
                if !is_transient || attempt == 19 {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Hides the console window of the git child the refresh worker still reads from. Unlike
/// `spawn_detached_self` this child stays attached, so its piped stdout keeps working.
pub fn configure_background_command(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Visible console window width, not the screen-buffer width. `None` when stdout is a pipe (the
/// normal `agy` render) or has no console; `COLUMNS` is the fallback.
pub fn stdout_terminal_columns() -> Option<usize> {
    // SAFETY: `GetStdHandle` and `GetConsoleScreenBufferInfo` take standard Win32 console arguments
    // and write into a stack-initialized `ConsoleScreenBufferInfo`.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if !h.is_null() && (h as isize) != INVALID_HANDLE_VALUE {
            let mut info = ConsoleScreenBufferInfo {
                dw_size: Coord { x: 0, y: 0 },
                dw_cursor_position: Coord { x: 0, y: 0 },
                w_attributes: 0,
                sr_window: SmallRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                dw_maximum_window_size: Coord { x: 0, y: 0 },
            };
            // sr_window is inclusive on both edges (+1); dw_size is the buffer and can be wider
            // than the visible window. Guard the subtraction so a bogus rect cannot wrap.
            if GetConsoleScreenBufferInfo(h, &mut info) != 0
                && info.sr_window.right >= info.sr_window.left
            {
                let cols =
                    (i32::from(info.sr_window.right) - i32::from(info.sr_window.left) + 1) as usize;
                if cols > 0 {
                    return Some(cols);
                }
            }
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|&c| c > 0)
}

/// Queries `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
/// in-process via `RegGetValueW` (zero subprocess overhead) so `theme = "auto"` adapts to Windows
/// light/dark mode when `COLORFGBG` is unset.
pub fn system_theme_polarity() -> Option<crate::config::ThemePolarity> {
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut data: u32 = 0;
    let mut cb_data = core::mem::size_of::<u32>() as u32;
    // SAFETY: `subkey` and `value` are NUL-terminated UTF-16 buffers valid for this call,
    // `RRF_RT_REG_DWORD` restricts the returned type to a 4-byte DWORD, and `data` has size
    // `cb_data` (4 bytes).
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER as Handle,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut data).cast(),
            &raw mut cb_data,
        )
    };
    if rc == ERROR_SUCCESS && cb_data == core::mem::size_of::<u32>() as u32 {
        Some(if data != 0 {
            crate::config::ThemePolarity::Light
        } else {
            crate::config::ThemePolarity::Dark
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::system_theme_polarity;

    #[test]
    fn windows_personalize_registry_probe_does_not_fail_unexpectedly() {
        // Most interactive Windows sessions expose AppsUseLightTheme; ensure calling the Win32
        // FFI succeeds cleanly without UB and yields a valid polarity when present.
        let _ = system_theme_polarity();
    }
}
