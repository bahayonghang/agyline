#!/usr/bin/env python3
"""Print one run as a single timeline: harness steps, status line invocations, visibility changes.

    python timeline.py <run directory> [--procs]

Times are seconds since agy was spawned. An invocation row is placed at the moment its process
was created; `ran` is how long the command took and `sid` is the payload's session_id.
`--procs` adds the start and end of every process below agy (cmd.exe, python.exe, pwsh.exe, git.exe).
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path


def rows(run: Path, procs: bool) -> list[tuple[float, str]]:
    out: list[tuple[float, str]] = []
    t0 = json.loads((run / "summary.json").read_text(encoding="utf-8"))["t0"]
    for line in (run / "events.jsonl").read_text(encoding="utf-8").splitlines():
        event = json.loads(line)
        kind = event["event"]
        if kind in ("keys", "wait", "resize"):
            detail = repr(event["keys"]) if kind == "keys" else f"{event['rows']}x{event['cols']}" if kind == "resize" else ""
            out.append((event["rel"], f"STEP {event['label']} {detail}".rstrip()))
        elif kind == "visible_change":
            out.append((event["rel"], f"        screen: {(event['shown'] or '(status line absent)').strip()}"))
        elif kind in ("taskkill", "pty_eof"):
            out.append((event["rel"], f"STEP {kind}"))
        elif procs and kind in ("proc_start", "proc_end"):
            out.append((event["rel"], f"            {kind} {event['exe']} pid={event['pid']}" + (f" parent={event['parent']}" if "parent" in event else "")))
    for meta_path in sorted((run / "invocations").glob("inv-*.meta.json")):
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        stem = meta_path.name.removesuffix(".meta.json")
        stdin_path = next((p for p in (meta_path.with_name(stem + ext) for ext in (".stdin.bin", ".stdin.json")) if p.exists()), None)
        payload = json.loads(stdin_path.read_bytes()) if stdin_path else {}
        sid = payload.get("session_id", "<absent>")
        text = (
            f"    inv #{meta['seq']}: sid={sid[:8] if sid else repr(sid)} state={payload.get('agent_state')} "
            f"width={payload.get('terminal_width')} stdin={meta.get('stdin_len')}B stdout={meta.get('stdout_len')}B "
            f"exit={meta.get('exit_code')} ran={meta.get('seconds')}s"
        )
        out.append((round((meta.get("t_process_created") or meta["t_start"]) - t0, 3), text))
    return sorted(out, key=lambda row: row[0])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run", type=Path)
    ap.add_argument("--procs", action="store_true")
    args = ap.parse_args()
    for rel, text in rows(args.run, args.procs):
        print(f"{rel:9.3f}  {text}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
