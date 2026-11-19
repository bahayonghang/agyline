#!/usr/bin/env python3
"""Copy the evidence of every run into a captures directory, redacted for publication.

    python collect.py <root>\\runs <captures directory>

Per run it keeps `events.jsonl`, `summary.json`, `settings.initial.json`, `model_server.jsonl`,
`proxy.jsonl`, `raw.jsonl`, `screens/`, `invocations/` (stdin as `.stdin.json`, stdout as
`.stdout.txt`), the run's own agy log as `agy-cli.log`, and a generated `timeline.txt`.
The isolated home, temp directory and workspace are left behind.

Redactions, applied to every copied file: the Windows user name in paths, the machine name,
e-mail addresses, the installation UUID that agy writes to its log, and anything shaped like a
credential. `stdin_len` in the meta files is the length before redaction.
"""

from __future__ import annotations

import os
import re
import shutil
import sys
from collections import Counter
from pathlib import Path

import timeline

USER = re.escape(os.environ["USERNAME"])
HOST = re.escape(os.environ.get("COMPUTERNAME", "\0"))
REDACTIONS = [
    ("user name in path", re.compile(rf"(?i)(users(?:\\\\|\\|/)){USER}(?![\w.-])"), r"\1<user>"),
    ("machine name", re.compile(rf"(?i)(?<![\w-]){HOST}(?![\w-])"), "<host>"),
    ("e-mail address", re.compile(r"[\w.+-]+@[\w-]+\.[\w.-]+"), "<email>"),
    ("installation UUID", re.compile(r"(installation UUID: )[0-9a-f-]{36}"), r"\1<installation-uuid>"),
    ("credential", re.compile(r"AIza[0-9A-Za-z_-]{20,}|ya29\.[\w.-]+|gh[pousr]_\w{20,}|(?i:bearer) [\w.~+/-]{16,}|(?i:csrf_token=|api[-_]key[=:] ?)\S+"), "<redacted>"),
]
KEEP = ["events.jsonl", "summary.json", "settings.initial.json", "model_server.jsonl", "proxy.jsonl", "raw.jsonl"]
RENAME = {".stdin.bin": ".stdin.json", ".stdout.bin": ".stdout.txt"}


def redact(text: str, counts: Counter) -> str:
    for name, pattern, replacement in REDACTIONS:
        text, n = pattern.subn(replacement, text)
        counts[name] += n
    return text


def copy_redacted(src: Path, dest: Path, counts: Counter) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    text = src.read_bytes().decode("utf-8", "surrogateescape")
    dest.write_bytes(redact(text, counts).encode("utf-8", "surrogateescape"))


def main() -> int:
    runs, captures = Path(sys.argv[1]), Path(sys.argv[2])
    counts: Counter = Counter()
    for run in sorted(p for p in runs.iterdir() if (p / "summary.json").exists()):
        dest = captures / run.name
        shutil.rmtree(dest, ignore_errors=True)
        for name in KEEP:
            if (run / name).exists():
                copy_redacted(run / name, dest / name, counts)
        for src in sorted((run / "screens").glob("*.txt")):
            copy_redacted(src, dest / "screens" / src.name, counts)
        for src in sorted((run / "invocations").iterdir()):
            name = src.name
            for old, new in RENAME.items():
                if name.endswith(old):
                    name = name[: -len(old)] + new
            copy_redacted(src, dest / "invocations" / name, counts)
        logs = sorted((run / "home" / ".gemini" / "antigravity-cli" / "log").glob("cli-*.log"))
        if logs:
            copy_redacted(logs[-1], dest / "agy-cli.log", counts)  # a continued home also holds the earlier run's log
        lines = [f"{rel:9.3f}  {text}" for rel, text in timeline.rows(dest, procs=True)]
        (dest / "timeline.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"{run.name}: {sum(1 for _ in dest.rglob('*') if _.is_file())} files")
    print("redactions:", dict(counts))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
