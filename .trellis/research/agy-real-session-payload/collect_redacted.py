#!/usr/bin/env python3
"""Copy and redact real-session capture artifacts into `.trellis/research/agy-real-session-payload/captures/`.

Asserts after copying that no raw e-mail address, Windows user name, or machine name
remains anywhere in the target directory.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sys
from collections import Counter
from pathlib import Path

HARNESS_DIR = Path(__file__).resolve().parents[1] / "agy-first-render" / "harness"
sys.path.insert(0, str(HARNESS_DIR))
import timeline  # noqa: E402

USER = os.environ["USERNAME"]
HOST = os.environ.get("COMPUTERNAME", "")
CLI_LOG_DIR = Path.home() / ".gemini" / "antigravity-cli" / "log"

REDACTIONS: list[tuple[str, re.Pattern[str], str]] = [
    ("e-mail address", re.compile(r"[\w.+-]+@[\w-]+\.[\w.-]+"), "<email>"),
    ("plan tier", re.compile(r'("plan_tier"\s*:\s*)"[^"]+"'), r'\1"<plan-tier>"'),
    ("plan tier screen", re.compile(r"(<email>\s*\()[^)]+(\))"), r"\1<plan-tier>\2"),
    ("user name in path", re.compile(rf"(?i)(users(?:\\\\|\\|/)){re.escape(USER)}(?![\w.-])"), r"\1<user>"),
    ("machine name", re.compile(rf"(?i)(?<![\w-]){re.escape(HOST)}(?![\w-])") if HOST else re.compile(r"\0"), "<host>"),
    ("installation UUID", re.compile(r"(installation UUID: )[0-9a-f-]{36}"), r"\1<installation-uuid>"),
    ("gcp project", re.compile(r"gen-lang-client-\d+"), "<gcp-project>"),
    (
        "credential",
        re.compile(
            r"AIza[0-9A-Za-z_-]{20,}|ya29\.[\w.-]+|gh[pousr]_\w{20,}|(?i:bearer) [\w.~+/-]{16,}|(?i:csrf_token=|api[-_]key[=:] ?)\S+"
        ),
        "<redacted>",
    ),
]

KEEP_FILES = ["events.jsonl", "summary.json", "settings_check.json"]
RENAME_SUFFIX = {".stdin.bin": ".stdin.json", ".stdout.bin": ".stdout.txt"}


def redact_text(text: str, counts: Counter[str]) -> str:
    for name, pattern, replacement in REDACTIONS:
        text, n = pattern.subn(replacement, text)
        counts[name] += n
    return text


def copy_redacted(src: Path, dest: Path, counts: Counter[str]) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    text = src.read_bytes().decode("utf-8", "surrogateescape")
    dest.write_bytes(redact_text(text, counts).encode("utf-8", "surrogateescape"))


def find_session_log(raw_run: Path) -> Path | None:
    pid_file = raw_run / "agy.pid"
    new_logs_file = raw_run / "new_cli_logs.json"
    if not pid_file.exists() or not new_logs_file.exists():
        return None
    target_pid = pid_file.read_text(encoding="utf-8").strip()
    needle = f"Starting language server process with pid {target_pid}"
    for name in json.loads(new_logs_file.read_text(encoding="utf-8")):
        candidate = CLI_LOG_DIR / name
        if candidate.exists() and needle in candidate.read_text(encoding="utf-8", errors="replace"):
            return candidate
    return None


def verify_clean(captures_dir: Path, extra_Emails: set[str]) -> None:
    forbidden = [USER.lower()]
    if HOST:
        forbidden.append(HOST.lower())
    for e in extra_Emails:
        forbidden.append(e.lower())
        local_part = e.split("@", 1)[0].lower()
        if len(local_part) >= 4:
            forbidden.append(local_part)

    for path in sorted(captures_dir.rglob("*")):
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace").lower()
        for token in forbidden:
            if token and token in text:
                raise SystemExit(f"Redaction check failed: found forbidden token {token!r} in {path}")
        if re.search(r"[\w.+-]+@[\w-]+\.[\w.-]+", text):
            raise SystemExit(f"Redaction check failed: found email pattern in {path}")


def main() -> int:
    raw_root = Path(sys.argv[1]).resolve()
    captures_dir = Path(sys.argv[2]).resolve()
    counts: Counter[str] = Counter()
    seen_emails: set[str] = set()

    for raw_run in sorted(p for p in raw_root.iterdir() if p.is_dir() and (p / "summary.json").exists()):
        for stdin_bin in (raw_run / "invocations").glob("inv-*.stdin.bin"):
            try:
                data = json.loads(stdin_bin.read_bytes())
                if data.get("email"):
                    seen_emails.add(str(data["email"]))
            except (OSError, ValueError):
                pass

        dest = captures_dir / raw_run.name
        shutil.rmtree(dest, ignore_errors=True)
        for fname in KEEP_FILES:
            if (raw_run / fname).exists():
                copy_redacted(raw_run / fname, dest / fname, counts)
        for src in sorted((raw_run / "screens").glob("*.txt")):
            copy_redacted(src, dest / "screens" / src.name, counts)
        for src in sorted((raw_run / "invocations").iterdir()):
            out_name = src.name
            for old, new in RENAME_SUFFIX.items():
                if out_name.endswith(old):
                    out_name = out_name[: -len(old)] + new
            copy_redacted(src, dest / "invocations" / out_name, counts)

        log_path = find_session_log(raw_run)
        if log_path is not None:
            copy_redacted(log_path, dest / "agy-cli.log", counts)

        lines = [f"{rel:9.3f}  {text}" for rel, text in timeline.rows(dest, procs=True)]
        (dest / "timeline.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"{raw_run.name}: wrote {sum(1 for f in dest.rglob('*') if f.is_file())} redacted files")

    verify_clean(captures_dir, seen_emails)
    print("Redaction counts:", dict(counts))
    print("Verification passed: 0 raw emails, usernames, or hostnames in captures.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
