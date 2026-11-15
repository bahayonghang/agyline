use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::thread;
use std::time::Duration;

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        lpApplicationName: *const u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *const core::ffi::c_void,
        lpThreadAttributes: *const core::ffi::c_void,
        bInheritHandles: i32,
        dwCreationFlags: u32,
        lpEnvironment: *const core::ffi::c_void,
        lpCurrentDirectory: *const u16,
        lpStartupInfo: *const StartupInfoW,
        lpProcessInformation: *mut ProcessInformation,
    ) -> i32;

    fn CloseHandle(hObject: Handle) -> i32;
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
    if app_wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable path contains interior NUL",
        ));
    }
    app_wide.push(0);

    let mut cmdline = Vec::new();
    append_windows_arg(&mut cmdline, exe.as_os_str())?;
    for arg in args {
        cmdline.push(u16::from(b' '));
        append_windows_arg(&mut cmdline, arg)?;
    }
    cmdline.push(0);

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
    // `bInheritHandles` is 0 (`FALSE`) so zero parent pipe handles leak to the detached worker,
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
    unsafe {
        let _ = CloseHandle(pi.h_thread);
        let _ = CloseHandle(pi.h_process);
    }
    Ok(())
}

fn append_windows_arg(cmdline: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let wide: Vec<u16> = arg.encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains interior NUL",
        ));
    }

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
    for _ in 0..(backslashes * 2) {
        cmdline.push(u16::from(b'\\'));
    }
    cmdline.push(u16::from(b'"'));
    Ok(())
}

pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

pub fn replace_file_with_retry(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut last_err = None;
    for attempt in 0..4 {
        match fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let is_transient = err.kind() == io::ErrorKind::PermissionDenied
                    || matches!(err.raw_os_error(), Some(5 | 32));
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
