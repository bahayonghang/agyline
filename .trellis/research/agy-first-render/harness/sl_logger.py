#!/usr/bin/env python3
"""Status line command that records each invocation. Standard library only.

    python -I sl_logger.py <logdir> minimal [--sleep S] [--exit N] [--empty]
    python -I sl_logger.py <logdir> wrap -- <real status line command ...>

`minimal` prints one marker line, `AGYLINE-MARK #<seq> <HH:MM:SS.mmm>`, so a screen dump
shows which invocation's output is on screen. `wrap` feeds the same stdin to the real
command, records its exit code, duration and output, and passes its stdout through.

Each invocation writes three files into <logdir>:

    inv-<seq>.meta.json    times (epoch seconds), pids, ancestor process names, exit code
    inv-<seq>.stdin.bin    the raw stdin bytes agy wrote
    inv-<seq>.stdout.bin   the bytes this command printed
"""

from __future__ import annotations

import ctypes
import json
import os
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path


class PROCESSENTRY32W(ctypes.Structure):
    _fields_ = [
        ("dwSize", wintypes.DWORD),
        ("cntUsage", wintypes.DWORD),
        ("th32ProcessID", wintypes.DWORD),
        ("th32DefaultHeapID", ctypes.c_size_t),
        ("th32ModuleID", wintypes.DWORD),
        ("cntThreads", wintypes.DWORD),
        ("th32ParentProcessID", wintypes.DWORD),
        ("pcPriClassBase", ctypes.c_long),
        ("dwFlags", wintypes.DWORD),
        ("szExeFile", ctypes.c_wchar * 260),
    ]


def process_table() -> dict[int, tuple[int, str]]:
    """pid -> (parent pid, exe name) for every process, from one toolhelp snapshot."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel32.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel32.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(PROCESSENTRY32W)]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = kernel32.CreateToolhelp32Snapshot(0x2, 0)
    table: dict[int, tuple[int, str]] = {}
    if snapshot in (None, wintypes.HANDLE(-1).value):
        return table
    entry = PROCESSENTRY32W()
    entry.dwSize = ctypes.sizeof(PROCESSENTRY32W)
    ok = kernel32.Process32FirstW(snapshot, ctypes.byref(entry))
    while ok:
        table[entry.th32ProcessID] = (entry.th32ParentProcessID, entry.szExeFile)
        ok = kernel32.Process32NextW(snapshot, ctypes.byref(entry))
    kernel32.CloseHandle(snapshot)
    return table


def process_created() -> float | None:
    """Epoch seconds at which this process was created, which precedes interpreter start-up."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    times = [wintypes.FILETIME() for _ in range(4)]
    if not kernel32.GetProcessTimes(kernel32.GetCurrentProcess(), *(ctypes.byref(t) for t in times)):
        return None
    ticks = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime  # 100 ns since 1601-01-01
    return ticks / 1e7 - 11644473600


def command_line(pid: int) -> str | None:
    """Command line of another process (ProcessCommandLineInformation), or None when it cannot be read."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    ntdll = ctypes.WinDLL("ntdll")
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    ntdll.NtQueryInformationProcess.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.ULONG, ctypes.POINTER(wintypes.ULONG)]
    handle = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
    if not handle:
        return None
    buffer = ctypes.create_string_buffer(16384)
    status = ntdll.NtQueryInformationProcess(handle, 60, buffer, len(buffer), ctypes.byref(wintypes.ULONG()))
    kernel32.CloseHandle(handle)
    if status != 0:
        return None
    # UNICODE_STRING on x64: USHORT Length, USHORT MaximumLength, padding, PWSTR Buffer (inside our buffer).
    length = int.from_bytes(buffer.raw[0:2], "little")
    offset = int.from_bytes(buffer.raw[8:16], "little") - ctypes.addressof(buffer)
    if not 0 <= offset <= len(buffer) - length:
        return None
    return buffer.raw[offset:offset + length].decode("utf-16-le", "replace")


def ancestors(pid: int, depth: int = 6) -> list[dict]:
    table = process_table()
    chain = []
    for _ in range(depth):
        if pid not in table:
            break
        parent, exe = table[pid]
        chain.append({"pid": pid, "exe": exe})
        pid = parent
    return chain


def claim_sequence(logdir: Path) -> int:
    """Take the next free sequence number; exclusive create keeps concurrent invocations apart."""
    seq = 1
    while True:
        try:
            (logdir / f"inv-{seq:04d}.meta.json").open("x").close()
            return seq
        except FileExistsError:
            seq += 1


def main() -> int:
    t_start = time.time()
    logdir = Path(sys.argv[1])
    mode = sys.argv[2]
    rest = sys.argv[3:]
    logdir.mkdir(parents=True, exist_ok=True)
    seq = claim_sequence(logdir)
    meta: dict = {
        "seq": seq, "mode": mode, "t_process_created": process_created(), "t_start": t_start, "pid": os.getpid(),
        "argv": sys.argv, "cwd": os.getcwd(), "ancestors": ancestors(os.getpid()),
        "parent_command_line": command_line(os.getppid()),
        "stdin_isatty": sys.stdin.isatty(), "stdout_isatty": sys.stdout.isatty(),
        "env_names_agy": sorted(k for k in os.environ if k.upper().startswith(("AGY", "ANTIGRAVITY", "GEMINI"))),
        "env_term": {k: os.environ.get(k) for k in ("TERM", "COLORTERM", "COLUMNS", "LINES", "COLORFGBG", "NO_COLOR")},
    }
    meta_path = logdir / f"inv-{seq:04d}.meta.json"
    meta_path.write_text(json.dumps(meta, indent=1))  # survives if agy kills us while stdin is still open

    stdin_bytes = sys.stdin.buffer.read()
    meta["t_stdin_eof"] = time.time()
    meta["stdin_len"] = len(stdin_bytes)
    (logdir / f"inv-{seq:04d}.stdin.bin").write_bytes(stdin_bytes)

    exit_code = 0
    if mode == "minimal":
        if "--sleep" in rest:
            time.sleep(float(rest[rest.index("--sleep") + 1]))
        if "--exit" in rest:
            exit_code = int(rest[rest.index("--exit") + 1])
        stamp = time.strftime("%H:%M:%S", time.localtime(t_start)) + f".{int(t_start * 1000) % 1000:03d}"
        out = b"" if "--empty" in rest else f"AGYLINE-MARK #{seq:04d} {stamp}\n".encode()
    elif mode == "wrap":
        command = rest[rest.index("--") + 1:]
        meta["wrapped"] = command
        meta["t_child_start"] = time.time()
        proc = subprocess.run(command, input=stdin_bytes, capture_output=True)
        meta["t_child_end"] = time.time()
        meta["child_seconds"] = round(meta["t_child_end"] - meta["t_child_start"], 4)
        meta["child_stderr"] = proc.stderr.decode("utf-8", "replace")[-2000:]
        exit_code = proc.returncode
        out = proc.stdout
    else:
        raise SystemExit(f"unknown mode {mode!r}")

    (logdir / f"inv-{seq:04d}.stdout.bin").write_bytes(out)
    meta["stdout_len"] = len(out)
    meta["exit_code"] = exit_code
    try:
        sys.stdout.buffer.write(out)
        sys.stdout.buffer.flush()
        meta["stdout_write"] = "ok"
    except OSError as exc:  # agy closed the pipe before we answered
        meta["stdout_write"] = f"failed: {exc}"
    meta["t_end"] = time.time()
    meta["seconds"] = round(meta["t_end"] - t_start, 4)
    meta_path.write_text(json.dumps(meta, indent=1))
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
