#!/usr/bin/env python3
"""Capture statusLine.command stdin payloads in a real logged-in agy session.

Temporarily wraps `statusLine.command` in `~/.gemini/antigravity-cli/settings.json`
with `sl_logger.py <invdir> wrap -- <original command>`, drives one real ConPTY
session (startup -> `/model` + Esc -> prompt with one `research` subagent), and
restores `settings.json` byte-for-byte in a `finally` block.

Raw unredacted files stay in `<raw-dir>` outside the git working tree.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time
from pathlib import Path

import pyte
from winpty import PtyProcess

HARNESS_DIR = Path(__file__).resolve().parents[1] / "agy-first-render" / "harness"
sys.path.insert(0, str(HARNESS_DIR))
import sl_logger  # noqa: E402

REPO = Path(__file__).resolve().parents[3]
SL_LOGGER = HARNESS_DIR / "sl_logger.py"
DEFAULT_AGY = Path(os.environ.get("LOCALAPPDATA", "")) / "agy" / "bin" / "agy.exe"
SETTINGS_PATH = Path.home() / ".gemini" / "antigravity-cli" / "settings.json"
CLI_LOG_DIR = Path.home() / ".gemini" / "antigravity-cli" / "log"

PROMPT_TEXT = (
    "Call invoke_subagent once with TypeName research, Role ProbeRoleLabel, "
    "Model flash_lite, and Prompt Say PONG only and call no tools. "
    "Wait for it to finish, then reply with DONE only and call no other tools."
)
SIMPLE_PROMPT_TEXT = "Reply with the single word OK and do not call any tools."
FOLLOWUP_SUBAGENT_PROMPT = (
    "You did not call invoke_subagent yet. Call invoke_subagent now with "
    "TypeName research, Role ProbeRoleLabel, Model flash_lite, and Prompt PONG."
)


def posix(path: Path) -> str:
    return str(path).replace("\\", "/")


class EventLog:
    def __init__(self, path: Path, t0: float) -> None:
        self.file = path.open("a", encoding="utf-8")
        self.t0 = t0
        self.lock = threading.Lock()

    def write(self, event: str, /, **fields: object) -> None:
        now = time.time()
        with self.lock:
            record = {"t": round(now, 4), "rel": round(now - self.t0, 3), "event": event, **fields}
            self.file.write(json.dumps(record, ensure_ascii=False) + "\n")
            self.file.flush()

    def close(self) -> None:
        self.file.close()


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


def watch_processes(root: int, log: EventLog, stop: threading.Event, seen: dict[int, str]) -> None:
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


def clean_user_env() -> dict[str, str]:
    """Strip parent-agent harness env vars so the child agy runs like a user terminal."""
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("ANTIGRAVITY_")}
    if "PATH" in env:
        parts = env["PATH"].split(os.pathsep)
        cleaned = [
            p for p in parts
            if p.replace("\\", "/").rstrip("/").lower()
            != (Path.home() / ".gemini" / "antigravity-cli" / "bin").as_posix().lower()
        ]
        env["PATH"] = os.pathsep.join(cleaned)
    env.setdefault("TERM", "xterm-256color")
    env.setdefault("COLORTERM", "truecolor")
    return env


def read_invocations(inv_dir: Path) -> list[tuple[int, dict]]:
    items: list[tuple[int, dict]] = []
    for path in sorted(inv_dir.glob("inv-*.stdin.bin")):
        m = re.match(r"inv-(\d+)\.stdin\.bin$", path.name)
        if not m:
            continue
        seq = int(m.group(1))
        try:
            raw = path.read_bytes()
            if not raw:
                continue
            items.append((seq, json.loads(raw)))
        except (OSError, ValueError):
            continue
    return items


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--raw-dir", type=Path, required=True, help="local scratch directory outside repo")
    ap.add_argument("--scenario", choices=["subagent", "simple-turn"], default="subagent")
    ap.add_argument("--agy", type=Path, default=DEFAULT_AGY)
    ap.add_argument("--agy-arg", action="append", default=[])
    ap.add_argument("--workspace", type=Path, default=REPO)
    ap.add_argument("--size", default="40x120")
    args = ap.parse_args()

    raw_dir = args.raw_dir.resolve()
    if REPO.resolve() in (raw_dir, *raw_dir.parents):
        raise SystemExit("--raw-dir must be outside the git repository so unredacted PII stays local")
    if raw_dir.exists() and any(raw_dir.iterdir()):
        raise SystemExit(f"{raw_dir} is not empty; choose a fresh directory")

    inv_dir = raw_dir / "invocations"
    screens_dir = raw_dir / "screens"
    for d in (inv_dir, screens_dir):
        d.mkdir(parents=True, exist_ok=True)

    orig_bytes = SETTINGS_PATH.read_bytes()
    orig_sha256 = hashlib.sha256(orig_bytes).hexdigest()
    (raw_dir / "settings.before.bin").write_bytes(orig_bytes)

    orig_text = orig_bytes.decode("utf-8")
    orig_json = json.loads(orig_text)
    orig_cmd = orig_json["statusLine"]["command"]
    wrapped_cmd = f"python -I {posix(SL_LOGGER)} {posix(inv_dir)} wrap -- {orig_cmd}"
    if '"' in wrapped_cmd:
        raise SystemExit(f"wrapped command must not contain double quotes: {wrapped_cmd}")
    if " " in posix(SL_LOGGER) or " " in posix(inv_dir):
        raise SystemExit("logger path and invocation directory path must not contain spaces")

    target_literal = json.dumps(orig_cmd)
    replacement_literal = json.dumps(wrapped_cmd)
    if orig_text.count(target_literal) != 1:
        raise SystemExit("could not uniquely locate statusLine.command string literal in settings.json")
    modified_text = orig_text.replace(target_literal, replacement_literal, 1)
    json.loads(modified_text)  # validate JSON syntax before writing
    modified_bytes = modified_text.encode("utf-8")

    logs_before = {p.name for p in CLI_LOG_DIR.glob("cli-*.log")} if CLI_LOG_DIR.exists() else set()
    rows, cols = (int(n) for n in args.size.split("x"))
    env = clean_user_env()
    visible_re = re.compile(rf"\b{re.escape(args.workspace.name)}\b.*\|")

    t0 = time.time()
    log = EventLog(raw_dir / "events.jsonl", t0)
    raw_pty = (raw_dir / "raw.jsonl").open("a", encoding="utf-8")
    screen = pyte.Screen(cols, rows)
    stream = pyte.Stream(screen)
    chunks: queue.Queue[str | None] = queue.Queue()
    state = {"visible": None, "dumps": 0, "first_output": False, "eof": False}
    pty = None
    stop_watch = threading.Event()
    seen_children: dict[int, str] = {}
    watcher = None

    def dump(label: str) -> str:
        state["dumps"] += 1
        text = "\n".join(line.rstrip() for line in screen.display)
        name = f"{state['dumps']:02d}-{label}.txt"
        (screens_dir / name).write_text(text + "\n", encoding="utf-8")
        return name

    def handle_chunk(chunk: str) -> None:
        raw_pty.write(json.dumps({"t": round(time.time(), 4), "data": chunk}, ensure_ascii=False) + "\n")
        if not state["first_output"]:
            state["first_output"] = True
            log.write("first_output")
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
            handle_chunk(chunk)

    def send_step(label: str, keys: str, wait_s: float) -> None:
        if keys:
            pty.write(keys)
            log.write("keys", label=label, keys=keys)
        else:
            log.write("wait", label=label)
        pump(wait_s)
        log.write(
            "step_done",
            label=label,
            shown=state["visible"],
            screen=dump(label),
            invocations=len(list(inv_dir.glob("inv-*.stdin.bin"))),
        )

    SETTINGS_PATH.write_bytes(modified_bytes)
    t_modified = time.time()
    try:
        t0 = log.t0 = time.time()
        argv = [str(args.agy), *args.agy_arg]
        pty = PtyProcess.spawn(argv, cwd=str(args.workspace), env=env, dimensions=(rows, cols))
        (raw_dir / "agy.pid").write_text(str(pty.pid), encoding="utf-8")
        log.write("spawn", pid=pty.pid, argv=argv, cwd=str(args.workspace), size=args.size, scenario=args.scenario)

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
        watcher = threading.Thread(
            target=watch_processes, args=(pty.pid, log, stop_watch, seen_children), daemon=True
        )
        watcher.start()

        if args.scenario == "simple-turn":
            send_step("boot", "", 8.0)
            send_step("prompt_typed", SIMPLE_PROMPT_TEXT, 2.0)
            inv_before_submit = len(read_invocations(inv_dir))
            pty.write("\r")
            log.write("keys", label="prompt_submit", keys="\r", inv_before_submit=inv_before_submit)

            turn_deadline = time.time() + 60.0
            saw_working = False
            while time.time() < turn_deadline and not state["eof"]:
                pump(0.5)
                invs = read_invocations(inv_dir)
                post = [(seq, p) for seq, p in invs if seq > inv_before_submit]
                if not post:
                    continue
                if any(p.get("agent_state") == "working" for _, p in post):
                    saw_working = True
                latest_seq, latest = post[-1]
                has_usage = latest.get("context_window", {}).get("current_usage") is not None
                if saw_working and latest.get("agent_state") == "idle" and has_usage:
                    log.write("turn_complete_detected", seq=latest_seq)
                    pump(6.0)
                    dump("turn_complete")
                    break
            else:
                log.write("turn_wait_timeout")
                dump("turn_timeout")
        else:
            # 1. Pure startup observation (15s, no input).
            send_step("boot", "", 15.0)

            # 2. First slash command (`/model` then Esc) without changing settings.
            send_step("slash", "/", 3.0)
            send_step("model", "model", 2.0)
            send_step("model_enter", "\r", 4.0)
            send_step("model_esc", "\x1b", 4.0)

            # 3. Submit one prompt that invokes a single `research` subagent.
            send_step("prompt_typed", PROMPT_TEXT, 2.0)
            inv_before_submit = len(read_invocations(inv_dir))
            pty.write("\r")
            log.write("keys", label="prompt_submit", keys="\r", inv_before_submit=inv_before_submit)

            turn_deadline = time.time() + 100.0
            last_approve_t = 0.0
            followup_sent = False
            idle_without_subagent_since: float | None = None

            while time.time() < turn_deadline and not state["eof"]:
                pump(0.5)
                invs = read_invocations(inv_dir)
                post = [(seq, p) for seq, p in invs if seq > inv_before_submit]
                if not post:
                    continue
                latest_seq, latest = post[-1]

                if latest.get("tool_confirmation_pending") is True and (time.time() - last_approve_t) > 3.0:
                    pump(0.8)
                    pty.write("\r")
                    last_approve_t = time.time()
                    log.write("auto_approve_tool", seq=latest_seq)
                    continue

                subagents = latest.get("subagents") or []
                has_usage = latest.get("context_window", {}).get("current_usage") is not None
                is_idle = latest.get("agent_state") == "idle"
                all_subs_done = bool(subagents) and all(s.get("status") == "completed" for s in subagents)

                if is_idle and has_usage and all_subs_done:
                    log.write("turn_complete_detected", seq=latest_seq)
                    pump(4.0)
                    dump("turn_complete")
                    break

                if is_idle and has_usage and not subagents and not followup_sent:
                    if idle_without_subagent_since is None:
                        idle_without_subagent_since = time.time()
                    elif time.time() - idle_without_subagent_since > 3.0:
                        followup_sent = True
                        send_step("followup_typed", FOLLOWUP_SUBAGENT_PROMPT, 1.5)
                        inv_before_submit = len(read_invocations(inv_dir))
                        pty.write("\r")
                        log.write("keys", label="followup_submit", keys="\r", inv_before_submit=inv_before_submit)
                else:
                    idle_without_subagent_since = None
            else:
                log.write("turn_wait_timeout")
                dump("turn_timeout")

    finally:
        if pty is not None:
            for _ in range(2):
                if pty.isalive():
                    pty.write("\x03")
                    pump(0.8)
            if pty.isalive():
                subprocess.run(["taskkill", "/PID", str(pty.pid), "/T", "/F"], capture_output=True)
                log.write("taskkill", pid=pty.pid)
            pump(1.5)
        stop_watch.set()
        if watcher is not None:
            watcher.join(timeout=2.0)

        SETTINGS_PATH.write_bytes(orig_bytes)
        t_restored = time.time()
        restored_bytes = SETTINGS_PATH.read_bytes()
        restored_sha256 = hashlib.sha256(restored_bytes).hexdigest()
        byte_identical = restored_bytes == orig_bytes

        check = {
            "settings_path": str(SETTINGS_PATH),
            "orig_len": len(orig_bytes),
            "restored_len": len(restored_bytes),
            "orig_sha256": orig_sha256,
            "restored_sha256": restored_sha256,
            "byte_identical": byte_identical,
            "modified_duration_seconds": round(t_restored - t_modified, 3),
        }
        (raw_dir / "settings_check.json").write_text(json.dumps(check, indent=2), encoding="utf-8")
        log.write("settings_restored", **check)
        raw_pty.close()
        log.close()

        if not byte_identical:
            raise SystemExit("CRITICAL: settings.json restoration check failed!")

    logs_after = {p.name for p in CLI_LOG_DIR.glob("cli-*.log")} if CLI_LOG_DIR.exists() else set()
    new_logs = sorted(logs_after - logs_before)
    if new_logs:
        (raw_dir / "new_cli_logs.json").write_text(json.dumps(new_logs, indent=2), encoding="utf-8")

    timeline = []
    for meta_path in sorted(inv_dir.glob("inv-*.meta.json")):
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        entry = {"seq": meta["seq"]}
        for key in ("t_process_created", "t_start", "t_stdin_eof", "t_end"):
            if meta.get(key) is not None:
                entry[key.replace("t_", "rel_")] = round(meta[key] - t0, 3)
        for key in ("stdin_len", "stdout_len", "exit_code", "seconds", "child_seconds", "stdout_write"):
            if key in meta:
                entry[key] = meta[key]
        stdin_path = inv_dir / f"inv-{meta['seq']:04d}.stdin.bin"
        if stdin_path.exists():
            try:
                payload = json.loads(stdin_path.read_bytes())
                entry["payload_keys"] = sorted(payload)
                entry["session_id_empty"] = payload.get("session_id") == ""
                entry["session_eq_conv"] = payload.get("session_id") == payload.get("conversation_id")
                entry["agent_state"] = payload.get("agent_state")
                entry["has_quota"] = "quota" in payload
                entry["has_plan_tier"] = "plan_tier" in payload
                entry["has_email"] = "email" in payload
                entry["has_cost"] = "cost" in payload
                entry["subagents_count"] = len(payload.get("subagents") or [])
            except ValueError as exc:
                entry["payload_error"] = str(exc)
        timeline.append(entry)

    (raw_dir / "summary.json").write_text(
        json.dumps({"t0": t0, "settings_check": check, "timeline": timeline}, indent=2),
        encoding="utf-8",
    )
    print(f"Captured {len(timeline)} invocations; settings.json byte_identical={check['byte_identical']} sha256={check['restored_sha256']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
