use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::thread;
use std::time::Duration;

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;

type Handle = *mut core::ffi::c_void;

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
}

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
    let mut app_wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    app_wide.push(0);

    let mut cmd_wide = Vec::new();
    append_quoted_wide_arg(&mut cmd_wide, exe.as_os_str());
    for arg in args {
        cmd_wide.push(u16::from(b' '));
        append_quoted_wide_arg(&mut cmd_wide, arg);
    }
    cmd_wide.push(0);

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

    // Pass `bInheritHandles = 0` (`FALSE`) so zero pipe/file handles from `agy` or `cmd.exe`
    // can ever leak into the detached background worker and delay parent stdout/stderr EOF.
    let ok = unsafe {
        CreateProcessW(
            app_wide.as_ptr(),
            cmd_wide.as_mut_ptr(),
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
    unsafe {
        let _ = CloseHandle(pi.h_thread);
        let _ = CloseHandle(pi.h_process);
    }
    Ok(())
}

fn append_quoted_wide_arg(cmd: &mut Vec<u16>, arg: &OsStr) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;

    cmd.push(QUOTE);
    let mut backslashes = 0_usize;
    for unit in arg.encode_wide() {
        if unit == BACKSLASH {
            backslashes += 1;
        } else if unit == QUOTE {
            cmd.extend(core::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
            cmd.push(QUOTE);
            backslashes = 0;
        } else {
            cmd.extend(core::iter::repeat_n(BACKSLASH, backslashes));
            cmd.push(unit);
            backslashes = 0;
        }
    }
    cmd.extend(core::iter::repeat_n(BACKSLASH, backslashes * 2));
    cmd.push(QUOTE);
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

pub fn create_file_symlink(original: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(original, link)
}

pub fn replace_file_with_retry(tmp_path: &Path, dst_path: &Path) -> io::Result<()> {
    let mut attempt = 0_u32;
    loop {
        match fs::rename(tmp_path, dst_path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let retryable = err.kind() == io::ErrorKind::PermissionDenied
                    || matches!(
                        err.raw_os_error(),
                        Some(ERROR_ACCESS_DENIED) | Some(ERROR_SHARING_VIOLATION)
                    );
                if !retryable || attempt >= 3 {
                    return Err(err);
                }
                attempt += 1;
                thread::sleep(Duration::from_millis(3));
            }
        }
    }
}

pub fn configure_background_command(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}
