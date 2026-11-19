#!/usr/bin/env python3
"""Drive `agy` in a ConPTY with an isolated home and record when it runs `statusLine.command`.

Needs `pywinpty` and `pyte` (a throwaway venv). Nothing under the real `~/.gemini` is touched:
`USERPROFILE`, `HOME`, `APPDATA`, `LOCALAPPDATA`, `TEMP` and `TMP` of the agy process point into
the run directory, the model endpoint is a loopback server that answers 500, and every other
outbound request goes to a loopback proxy that logs the target and refuses it.

    python observe.py --root %TEMP%\\agyline-research-fr --name a1 --kind minimal \\
        --step "boot||12" --step "slash|/|3" --step "model|model\\r|5" --step "esc|\\x1b|3"

A step is `label|keys|seconds`: send the keys (Python escapes allowed), keep reading for that
long, then dump the screen. `@resize:ROWSxCOLS` as keys resizes the pseudo-terminal instead.

Run directory layout (`<root>/runs/<name>/`):

    events.jsonl        everything the harness did or saw, with epoch `t` and `rel` since spawn
    invocations/        what sl_logger.py recorded for each status line invocation
    screens/            the reconstructed screen after each step and at each visibility change
    raw.jsonl           timestamped pty output, for replay
    model_server.jsonl  requests that reached the loopback model endpoint
    proxy.jsonl         outbound targets that were refused
    summary.json        the invocation timeline
    home/ tmp/ ws/      the isolated home, temp directory and workspace
    patched/            --kind ps1-patched only: the modified copy of statusline.ps1

Status line commands (--kind): `minimal` is sl_logger.py printing a marker; `py`, `ps1` and
`ps1-patched` are the reference scripts run through sl_logger.py, which records stdin, exit code
and output; `py-direct` and `ps1-direct` are the reference scripts alone, as a user configures
them, observed through the process watcher and the screen; `none` leaves statusLine unset.
"""

from __future__ import annotations

import argparse
import codecs
import http.server
import json
import os
import queue
import re
import shutil
import socketserver
import subprocess
import sys
import threading
import time
from pathlib import Path

import pyte
from winpty import PtyProcess

import sl_logger

HARNESS = Path(__file__).resolve().parent
REPO = HARNESS.parents[3]
DEFAULT_AGY = Path(os.environ.get("LOCALAPPDATA", "")) / "agy" / "bin" / "agy.exe"
STRIP_ENV = ("AGY_", "GEMINI_", "GOOGLE_", "ANTIGRAVITY_")
MARK_RE = r"AGYLINE-MARK #\d+ [\d:.]+"
REFERENCE_RE = r"\S+ ws \| [^\n]*"  # the reference line's directory segment; the workspace is always named `ws`


def posix(path: Path) -> str:
    return str(path).replace("\\", "/")


PS1_EMPTY_ID = (
    "$sessionId = [string]($d.session_id ?? $d.conversation_id ?? 'default')\n"
    "if ([string]::IsNullOrEmpty($sessionId)) { exit 0 }\n"
)
PS1_EMPTY_ID_AS_PY = (  # what statusline.py does: an empty id falls through to the next candidate
    "$sessionId = [string]$d.session_id\n"
    "if ([string]::IsNullOrEmpty($sessionId)) { $sessionId = [string]$d.conversation_id }\n"
    "if ([string]::IsNullOrEmpty($sessionId)) { $sessionId = 'default' }\n"
)


def patched_ps1(run: Path) -> Path:
    """A copy of the reference statusline.ps1, in the run directory, that treats an empty session id as missing."""
    source = (REPO / "ref/agy-patch/statusline.ps1").read_text(encoding="utf-8").replace("\r\n", "\n")
    if source.count(PS1_EMPTY_ID) != 1:
        raise SystemExit("statusline.ps1 no longer has the session id lines this control experiment replaces")
    dest = run / "patched" / "statusline.ps1"
    dest.parent.mkdir()
    dest.write_text(source.replace(PS1_EMPTY_ID, PS1_EMPTY_ID_AS_PY), encoding="utf-8")
    return dest


def status_command(kind: str, run: Path, invdir: Path, extra: list[str]) -> str:
    logger = f"python -I {posix(HARNESS / 'sl_logger.py')} {posix(invdir)}"
    py = f"python -I {posix(REPO / 'ref/agy-patch/statusline.py')}"
    pwsh = "pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File"
    ps1 = f"{pwsh} {posix(REPO / 'ref/agy-patch/statusline.ps1')}"
    if kind == "minimal":
        return f"{logger} minimal {' '.join(extra)}".strip()
    if kind == "py":
        return f"{logger} wrap -- {py}"
    if kind == "ps1":
        return f"{logger} wrap -- {ps1}"
    if kind == "ps1-patched":
        return f"{logger} wrap -- {pwsh} {posix(patched_ps1(run))}"
    if kind == "py-direct":
        return py
    if kind == "ps1-direct":
        return ps1
    raise SystemExit(f"unknown --kind {kind!r}")


class Log:
    def __init__(self, path: Path, t0: float) -> None:
        self.file = path.open("a", encoding="utf-8")
        self.t0 = t0
        self.lock = threading.Lock()

    def write(self, event: str, /, **fields: object) -> None:
        now = time.time()
        with self.lock:
            self.file.write(json.dumps({"t": round(now, 4), "rel": round(now - self.t0, 3), "event": event, **fields}, ensure_ascii=False) + "\n")
            self.file.flush()


class ModelEndpoint(http.server.BaseHTTPRequestHandler):
    """Stands in for the Gemini API: an empty model list, and 500 for every generation request."""

    sink: Log
    protocol_version = "HTTP/1.1"

    def log_message(self, *_: object) -> None:
        pass

    def _reply(self, status: int, body: bytes) -> None:
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        self.sink.write("request", method="GET", path=self.path.split("?")[0])
        self._reply(200, b'{"models":[]}')

    def do_POST(self) -> None:
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        self.sink.write("request", method="POST", path=self.path.split("?")[0], body_len=len(body))
        self._reply(500, b'{"error":{"code":500,"message":"captured","status":"INTERNAL"}}')


class RefusingProxy(socketserver.BaseRequestHandler):
    """Logs the request line of anything agy tries to reach through the proxy, then refuses it."""

    sink: Log

    def handle(self) -> None:
        try:
            line = self.request.recv(4096).split(b"\r\n", 1)[0].decode("latin-1")
            self.sink.write("refused", line=line.split("?")[0])
            self.request.sendall(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        except OSError:
            pass


def serve(server: socketserver.BaseServer) -> None:
    threading.Thread(target=server.serve_forever, daemon=True).start()


def isolated_env(run: Path, model_port: int, proxy_port: int, extra: dict[str, str]) -> dict[str, str]:
    home = run / "home"
    for sub in ("AppData/Roaming", "AppData/Local", ".gemini/antigravity-cli"):
        (home / sub).mkdir(parents=True, exist_ok=True)
    (run / "tmp").mkdir(exist_ok=True)
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(STRIP_ENV)}
    env.update(
        USERPROFILE=str(home), HOME=str(home), HOMEDRIVE=home.drive, HOMEPATH=str(home)[len(home.drive):],
        APPDATA=str(home / "AppData" / "Roaming"), LOCALAPPDATA=str(home / "AppData" / "Local"),
        TEMP=str(run / "tmp"), TMP=str(run / "tmp"),
        GEMINI_API_KEY="probe", GOOGLE_GEMINI_BASE_URL=f"http://127.0.0.1:{model_port}",
        AGY_CLI_DISABLE_AUTO_UPDATE="true",
        HTTPS_PROXY=f"http://127.0.0.1:{proxy_port}", HTTP_PROXY=f"http://127.0.0.1:{proxy_port}",
        NO_PROXY="127.0.0.1,localhost",
        TERM="xterm-256color", COLORTERM="truecolor",
    )
    env.update(extra)
    return env


def descendants(table: dict[int, tuple[int, str]], root: int) -> dict[int, str]:
    found = {}
    for pid, (_, exe) in table.items():
        cursor, hops = pid, 0
        while cursor in table and hops < 10:
            cursor, hops = table[cursor][0], hops + 1
            if cursor == root:
                found[pid] = exe
                break
    return found


def watch_processes(root: int, log: Log, stop: threading.Event, seen: dict[int, str]) -> None:
    """Poll for child processes of agy: an invocation count that does not depend on the logger.

    A process is followed by pid once seen, so one whose parent was killed still reports its own end.
    """
    live: dict[int, str] = {}
    while not stop.is_set():
        table = sl_logger.process_table()
        for pid, exe in descendants(table, root).items():
            if pid not in live:
                live[pid] = seen[pid] = exe
                log.write("proc_start", pid=pid, exe=exe, parent=table[pid][0])
        for pid in [p for p, exe in live.items() if table.get(p, (0, ""))[1] != exe]:
            log.write("proc_end", pid=pid, exe=live.pop(pid))
        stop.wait(0.02)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True, help="scratch directory; runs go to <root>/runs/<name>")
    ap.add_argument("--name", required=True)
    ap.add_argument("--kind", choices=["minimal", "py", "ps1", "ps1-patched", "py-direct", "ps1-direct", "none"], default="minimal")
    ap.add_argument("--logger-arg", action="append", default=[], help="extra argument for `sl_logger.py minimal`")
    ap.add_argument("--agy", type=Path, default=DEFAULT_AGY)
    ap.add_argument("--agy-arg", action="append", default=[], help="extra agy argument")
    ap.add_argument("--settings", type=json.loads, default={}, help="extra settings.json keys, as JSON")
    ap.add_argument("--session-settings", type=json.loads, default={"modelProvider": "gemini"}, help="value of agy --settings")
    ap.add_argument("--home-template", type=Path, help="copy this prepared home before the run")
    ap.add_argument("--workspace", type=Path, help="run in this existing directory instead of a new <run>/ws")
    ap.add_argument("--env", action="append", default=[], help="NAME=VALUE for the agy process")
    ap.add_argument("--unset-env", action="append", default=[], help="variable to remove from the agy process's environment")
    ap.add_argument("--git", action="store_true", help="make the workspace a git repository")
    ap.add_argument("--onboarding", action="store_true", help="leave the home fresh, so agy opens with its onboarding screens")
    ap.add_argument("--size", default="40x120", help="ROWSxCOLS")
    ap.add_argument("--visible-regex", help="what counts as the status line being on screen (default: by --kind)")
    ap.add_argument("--step", action="append", default=[], help="label|keys|seconds")
    args = ap.parse_args()

    run = args.root.resolve() / "runs" / args.name
    if (Path.home() / ".gemini").resolve() in run.parents:
        raise SystemExit("--root must not be inside the real ~/.gemini")
    if run.exists():
        raise SystemExit(f"{run} exists; pick another --name")
    home, ws, invdir, screens = run / "home", args.workspace or run / "ws", run / "invocations", run / "screens"
    for d in (ws, invdir, screens):
        d.mkdir(parents=True, exist_ok=True)
    if args.home_template:
        shutil.copytree(args.home_template, home)

    t0 = time.time()
    log = Log(run / "events.jsonl", t0)
    ModelEndpoint.sink = Log(run / "model_server.jsonl", t0)
    RefusingProxy.sink = Log(run / "proxy.jsonl", t0)
    model_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ModelEndpoint)
    proxy_server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), RefusingProxy)
    serve(model_server)
    serve(proxy_server)

    env = isolated_env(run, model_server.server_port, proxy_server.server_address[1], dict(e.split("=", 1) for e in args.env))
    for name in args.unset_env:
        env.pop(name, None)
    settings_path = home / ".gemini" / "antigravity-cli" / "settings.json"
    settings = json.loads(settings_path.read_text(encoding="utf-8")) if settings_path.exists() else {}
    if str(ws) not in settings.setdefault("trustedWorkspaces", []):
        settings["trustedWorkspaces"].append(str(ws))
    if args.kind != "none":
        settings["statusLine"] = {"type": "command", "command": status_command(args.kind, run, invdir, args.logger_arg), "enabled": True}
    settings.update(args.settings)
    if not args.onboarding:  # the state of an installation that has been used before
        settings.setdefault("colorScheme", "colorblind-friendly dark")
        cache = settings_path.parent / "cache"
        cache.mkdir(exist_ok=True)
        (cache / "onboarding.json").write_text(json.dumps(
            {"consumerOnboardingComplete": True, "enterpriseOnboardingComplete": False, "onboardingComplete": True}, indent=2))
    settings_path.write_text(json.dumps(settings, indent=2), encoding="utf-8")
    (run / "settings.initial.json").write_text(json.dumps(settings, indent=2), encoding="utf-8")
    if args.git and not (ws / ".git").exists():
        subprocess.run(["git", "init", "-q"], cwd=ws, env=env, check=True)

    rows, cols = (int(n) for n in args.size.split("x"))
    argv = [str(args.agy), *(["--settings", json.dumps(args.session_settings)] if args.session_settings else []), *args.agy_arg]
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)
    visible_re = re.compile(args.visible_regex or (MARK_RE if args.kind in ("minimal", "none") else REFERENCE_RE))
    raw = (run / "raw.jsonl").open("a", encoding="utf-8")
    chunks: queue.Queue[str | None] = queue.Queue()
    state = {"visible": None, "dumps": 0, "first_output": False, "eof": False}

    t0 = log.t0 = ModelEndpoint.sink.t0 = RefusingProxy.sink.t0 = time.time()
    pty = PtyProcess.spawn(argv, cwd=str(ws), env=env, dimensions=(rows, cols))
    (run / "agy.pid").write_text(str(pty.pid))
    log.write("spawn", pid=pty.pid, argv=argv, cwd=str(ws), size=args.size, kind=args.kind,
              status_command=settings.get("statusLine", {}).get("command"))

    def reader() -> None:
        while True:
            try:
                data = pty.read(65536)
            except (EOFError, OSError):
                chunks.put(None)
                return
            if data:
                chunks.put(data)

    threading.Thread(target=reader, daemon=True).start()
    stop_watch = threading.Event()
    seen_children: dict[int, str] = {}
    watcher = threading.Thread(target=watch_processes, args=(pty.pid, log, stop_watch, seen_children), daemon=True)
    watcher.start()

    def dump(label: str) -> str:
        state["dumps"] += 1
        text = "\n".join(line.rstrip() for line in screen.display)
        name = f"{state['dumps']:02d}-{label}.txt"
        (screens / name).write_text(text + "\n", encoding="utf-8")
        return name

    def handle(chunk: str) -> None:
        raw.write(json.dumps({"t": round(time.time(), 4), "data": chunk}, ensure_ascii=False) + "\n")
        if not state["first_output"]:
            state["first_output"] = True
            log.write("first_output")
        # Answer what a terminal would: device attributes, cursor position, default colors.
        if "\x1b[c" in chunk or "\x1b[0c" in chunk:
            pty.write("\x1b[?62;22c")
            log.write("query", what="DA1")
        if "\x1b[6n" in chunk:
            pty.write(f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R")
            log.write("query", what="CPR")
        if "\x1b]11;?" in chunk:
            pty.write("\x1b]11;rgb:0000/0000/0000\x1b\\")
            log.write("query", what="OSC11")
        if "\x1b]10;?" in chunk:
            pty.write("\x1b]10;rgb:ffff/ffff/ffff\x1b\\")
            log.write("query", what="OSC10")
        stream.feed(chunk)
        match = visible_re.search("\n".join(screen.display))
        shown = match.group(0) if match else None
        if shown != state["visible"]:
            state["visible"] = shown
            log.write("visible_change", shown=shown, screen=dump("visible" if shown else "hidden"))

    def pump(seconds: float) -> None:
        deadline = time.time() + seconds
        while time.time() < deadline and not state["eof"]:
            try:
                chunk = chunks.get(timeout=0.05)
            except queue.Empty:
                continue
            if chunk is None:
                state["eof"] = True
                log.write("pty_eof")
                return
            handle(chunk)

    try:
        for spec in args.step:
            label, keys, seconds = spec.split("|")
            keys = codecs.decode(keys, "unicode_escape")
            if keys.startswith("@resize:"):
                rows, cols = (int(n) for n in keys.split(":", 1)[1].split("x"))
                pty.setwinsize(rows, cols)
                screen.resize(rows, cols)
                log.write("resize", label=label, rows=rows, cols=cols)
            elif keys:
                pty.write(keys)
                log.write("keys", label=label, keys=keys)
            else:
                log.write("wait", label=label)
            pump(float(seconds))
            log.write("step_done", label=label, shown=state["visible"], screen=dump(label),
                      invocations=len(list(invdir.glob("inv-*.stdin.bin"))))
    finally:
        for _ in range(2):
            if pty.isalive():
                pty.write("\x03")
                pump(0.8)
        if pty.isalive():  # only ever the process tree this harness started
            subprocess.run(["taskkill", "/PID", str(pty.pid), "/T", "/F"], capture_output=True)
            log.write("taskkill", pid=pty.pid)
        pump(1.0)
        stop_watch.set()
        watcher.join(timeout=2)
        leftover = {pid: exe for pid, (_, exe) in sl_logger.process_table().items() if seen_children.get(pid) == exe}
        log.write("exit", alive=pty.isalive(), leftover_children=leftover)
        model_server.shutdown()
        proxy_server.shutdown()
        raw.close()

    timeline = []
    for meta_path in sorted(invdir.glob("inv-*.meta.json")):
        meta = json.loads(meta_path.read_text())
        entry = {"seq": meta["seq"]}
        for key in ("t_process_created", "t_start", "t_stdin_eof", "t_end"):
            if meta.get(key) is not None:
                entry[key.replace("t_", "rel_")] = round(meta[key] - t0, 3)
        for key in ("stdin_len", "stdout_len", "exit_code", "seconds", "child_seconds", "stdout_write"):
            if key in meta:
                entry[key] = meta[key]
        entry["ancestors"] = [a["exe"] for a in meta.get("ancestors", [])]
        stdin_path = invdir / f"inv-{meta['seq']:04d}.stdin.bin"
        if stdin_path.exists():
            try:
                payload = json.loads(stdin_path.read_bytes())
                entry["payload_keys"] = sorted(payload)
                for key in ("session_id", "conversation_id", "agent_state"):
                    entry[key] = payload.get(key, "<absent>")
            except ValueError as exc:
                entry["payload_error"] = str(exc)
        timeline.append(entry)
    (run / "summary.json").write_text(json.dumps({"t0": t0, "kind": args.kind, "steps": args.step, "timeline": timeline}, indent=1), encoding="utf-8")
    for entry in timeline:
        print(json.dumps(entry, ensure_ascii=False))
    print(f"{len(timeline)} invocation(s); run directory {run}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
