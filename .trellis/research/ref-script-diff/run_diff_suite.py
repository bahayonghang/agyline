#!/usr/bin/env python3
"""Reproducible byte-for-byte comparison suite for statusline.ps1 vs statusline.py.

Covers every stdin JSON path and AGY_* environment variable listed in AGENTS.md,
plus stateful files, git porcelain v2 states, SQLite/WAL response model lookup,
temp directory precedence, and Windows console code page interactions.

All runs use isolated temporary directories and dedicated `probe_*` session IDs;
any stray `%TEMP%/agy_*probe_*` files are verified absent on exit.
"""

from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import ctypes
import json
import os
import pathlib
import sqlite3
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
PS1 = REPO / "ref" / "agy-patch" / "statusline.ps1"
PY = REPO / "ref" / "agy-patch" / "statusline.py"


def _clean_base_env() -> dict[str, str]:
    return {
        k: v
        for k, v in os.environ.items()
        if not k.startswith("AGY_") and k != "COLORFGBG" and k != "TMPDIR"
    }


def _init_git_repo(repo_dir: pathlib.Path, branch: str = "main", mode: str = "clean") -> None:
    repo_dir.mkdir(parents=True, exist_ok=True)
    git_env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "Probe",
        "GIT_AUTHOR_EMAIL": "probe@example.com",
        "GIT_COMMITTER_NAME": "Probe",
        "GIT_COMMITTER_EMAIL": "probe@example.com",
    }
    subprocess.run(["git", "-C", str(repo_dir), "init", "-b", branch], env=git_env, capture_output=True, check=True)
    if mode == "empty_unborn":
        return
    (repo_dir / "a.txt").write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo_dir), "add", "a.txt"], env=git_env, capture_output=True, check=True)
    subprocess.run(["git", "-C", str(repo_dir), "commit", "-m", "init"], env=git_env, capture_output=True, check=True)

    if mode == "dirty":
        (repo_dir / "a.txt").write_text("modified\n", encoding="utf-8")
    elif mode == " ahead_behind" or mode == "ahead_behind":
        bare = repo_dir.parent / f"{repo_dir.name}_remote.git"
        subprocess.run(["git", "clone", "--bare", str(repo_dir), str(bare)], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "remote", "add", "origin", str(bare)], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "fetch", "origin"], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "branch", "--set-upstream-to=origin/" + branch, branch], env=git_env, capture_output=True, check=True)
        # Advance remote by 1 commit via clone2
        clone2 = repo_dir.parent / f"{repo_dir.name}_peer"
        subprocess.run(["git", "clone", str(bare), str(clone2)], env=git_env, capture_output=True, check=True)
        (clone2 / "peer.txt").write_text("peer\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(clone2), "add", "peer.txt"], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(clone2), "commit", "-m", "peer"], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(clone2), "push", "origin", branch], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "fetch", "origin"], env=git_env, capture_output=True, check=True)
        # Advance local by 2 commits
        for i in range(2):
            (repo_dir / f"local_{i}.txt").write_text(f"{i}\n", encoding="utf-8")
            subprocess.run(["git", "-C", str(repo_dir), "add", f"local_{i}.txt"], env=git_env, capture_output=True, check=True)
            subprocess.run(["git", "-C", str(repo_dir), "commit", "-m", f"l{i}"], env=git_env, capture_output=True, check=True)
    elif mode == "conflict":
        subprocess.run(["git", "-C", str(repo_dir), "checkout", "-b", "side"], env=git_env, capture_output=True, check=True)
        (repo_dir / "a.txt").write_text("side\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(repo_dir), "commit", "-am", "side"], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "checkout", branch], env=git_env, capture_output=True, check=True)
        (repo_dir / "a.txt").write_text("main_change\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(repo_dir), "commit", "-am", "main"], env=git_env, capture_output=True, check=True)
        subprocess.run(["git", "-C", str(repo_dir), "merge", "side"], env=git_env, capture_output=True)
    elif mode == "detached":
        subprocess.run(["git", "-C", str(repo_dir), "checkout", "--detach", "HEAD"], env=git_env, capture_output=True, check=True)


def _make_conv_db(
    root: pathlib.Path,
    sid: str,
    gen_rows: list[tuple[int, bytes]],
    step_blobs: list[bytes] = (),
    mtime_sec: int | None = None,
) -> pathlib.Path:
    conv_dir = root / ".gemini" / "antigravity-cli" / "conversations"
    conv_dir.mkdir(parents=True, exist_ok=True)
    dbp = conv_dir / f"{sid}.db"
    conn = sqlite3.connect(str(dbp))
    conn.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0)")
    conn.execute("CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_payload BLOB)")
    for idx, blob in gen_rows:
        conn.execute("INSERT INTO gen_metadata (idx, data, size) VALUES (?, ?, ?)", (idx, blob, len(blob)))
    for idx, blob in enumerate(step_blobs, 1):
        conn.execute("INSERT INTO steps (idx, step_payload) VALUES (?, ?)", (idx, blob))
    conn.commit()
    conn.close()
    if mtime_sec is not None:
        os.utime(dbp, (mtime_sec, mtime_sec))
    return dbp


class CaseSpec:
    def __init__(
        self,
        case_id: str,
        category: str,
        description: str,
        covers: list[str],
        payload: Any = None,
        raw_stdin: bytes | None = None,
        env: dict[str, str] | None = None,
        setup_kind: str | None = None,
        setup_args: dict[str, Any] | None = None,
        via_cmd_cp: int | None = None,
        divergence_id: str | None = None,
    ):
        self.case_id = case_id
        self.category = category
        self.description = description
        self.covers = covers
        self.payload = payload
        self.raw_stdin = raw_stdin
        self.env = env or {}
        self.setup_kind = setup_kind
        self.setup_args = setup_args or {}
        self.via_cmd_cp = via_cmd_cp
        self.divergence_id = divergence_id


def execute_case(spec: CaseSpec) -> dict[str, Any]:
    base_clean = _clean_base_env()

    for _attempt in range(8):
        with tempfile.TemporaryDirectory(prefix="agy_ps_") as td_ps, tempfile.TemporaryDirectory(
            prefix="agy_py_"
        ) as td_py, tempfile.TemporaryDirectory(prefix="agy_sh_") as td_shared:
            ps_root = pathlib.Path(td_ps)
            py_root = pathlib.Path(td_py)
            shared_root = pathlib.Path(td_shared)

            # Prepare shared read-only fixtures (like git repos) once per attempt
            cwd_override: str | None = None
            if spec.setup_kind == "git_repo":
                rname = spec.setup_args.get("dir_name", "my-project")
                branch = spec.setup_args.get("branch", "main")
                mode = spec.setup_args.get("mode", "clean")
                repo_dir = shared_root / rname
                _init_git_repo(repo_dir, branch=branch, mode=mode)
                cwd_override = str(repo_dir)
            elif spec.setup_kind == "git_junction":
                real_repo = shared_root / "real_repo"
                _init_git_repo(real_repo, branch="main", mode="clean")
                (real_repo / "sub").mkdir(exist_ok=True)
                outer = shared_root / "outer_plain"
                outer.mkdir(exist_ok=True)
                junc = outer / "linked_pkg"
                subprocess.run(
                    ["cmd.exe", "/c", "mklink", "/J", str(junc), str(real_repo / "sub")],
                    capture_output=True,
                    check=True,
                )
                cwd_override = str(junc)

            needs_clock_sync = spec.case_id in (
                "F01_stateful_tps_from_telemetry",
                "F03_db_response_model_match",
                "H08_env_cache_relay_live_badges",
            )
            if needs_clock_sync:
                while (time.time() % 1.0) > 0.20:
                    time.sleep(0.01)
            t0 = int(time.time())

            open_conns: list[sqlite3.Connection] = []
            try:
                env_ps = {**base_clean, "TEMP": str(ps_root), "TMP": str(ps_root), "USERPROFILE": str(ps_root)}
                env_py = {**base_clean, "TEMP": str(py_root), "TMP": str(py_root), "USERPROFILE": str(py_root)}

                for root, env_target in ((ps_root, env_ps), (py_root, env_py)):
                    if spec.setup_kind == "telemetry":
                        sid = spec.setup_args["sid"]
                        line = spec.setup_args["line"].format(now=t0, now_minus_120=t0 - 120, now_minus_400=t0 - 400, now_minus_2=t0 - 2)
                        (root / f"agy_telemetry_{sid}.dat").write_text(line + "\n", encoding="utf-8")
                    elif spec.setup_kind == "relay":
                        sid = spec.setup_args["sid"]
                        r_ts_mode = spec.setup_args.get("r_ts_mode", "recent")
                        r_ts = t0 - 120 if r_ts_mode == "recent" else (t0 - 260 if r_ts_mode == "warn" else t0 - 360)
                        touches = spec.setup_args.get("touches", 0)
                        status = spec.setup_args.get("status", "live")
                        (root / f"agy_cache_{sid}.dat").write_text(f"{r_ts} {touches} {status}\n", encoding="utf-8")
                    elif spec.setup_kind == "temp_vs_tmp":
                        d_temp = root / "t_temp"
                        d_tmp = root / "t_tmp"
                        d_temp.mkdir()
                        d_tmp.mkdir()
                        sid = spec.setup_args["sid"]
                        (d_temp / f"agy_telemetry_{sid}.dat").write_text(
                            f"idle {t0 - 5}.000 100 48.2 1.0s 50 - Model {t0 - 60}\n", encoding="utf-8"
                        )
                        env_target["TEMP"] = str(d_temp)
                        env_target["TMP"] = str(d_tmp)
                    elif spec.setup_kind == "db":
                        sid = spec.setup_args["sid"]
                        _make_conv_db(
                            root,
                            sid,
                            gen_rows=spec.setup_args.get("gen_rows", []),
                            step_blobs=spec.setup_args.get("step_blobs", []),
                            mtime_sec=t0 - spec.setup_args.get("mtime_age", 400),
                        )
                    elif spec.setup_kind == "db_wal_stale":
                        sid = spec.setup_args["sid"]
                        conv_dir = root / ".gemini" / "antigravity-cli" / "conversations"
                        conv_dir.mkdir(parents=True, exist_ok=True)
                        dbp = conv_dir / f"{sid}.db"
                        c = sqlite3.connect(str(dbp))
                        c.execute("PRAGMA journal_mode=WAL")
                        c.execute("CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0)")
                        c.execute("CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_payload BLOB)")
                        c.execute("INSERT INTO gen_metadata VALUES (1, ?, 20)", (b"\x9a\x01\x0fclaude-opus-4-6",))
                        c.execute("INSERT INTO steps VALUES (1, ?)", (b"y" * 24000,))
                        c.execute("INSERT INTO gen_metadata VALUES (2, ?, 20)", (b"\x9a\x01\x10claude-haiku-4-5",))
                        c.commit()
                        c.execute("PRAGMA wal_checkpoint(RESTART)")
                        c.execute("INSERT INTO gen_metadata VALUES (3, ?, 18)", (b"\x9a\x01\x0egemini-2.5-pro",))
                        c.commit()
                        open_conns.append(c)

                    env_target.update(spec.env)

                if spec.raw_stdin is not None:
                    payload_to_record: Any = spec.raw_stdin.decode("utf-8", "replace")
                    stdin_bytes = spec.raw_stdin
                else:
                    p_dict = json.loads(json.dumps(spec.payload))
                    if cwd_override is not None:
                        target_field = spec.setup_args.get("cwd_field", "cwd")
                        if target_field == "cwd":
                            p_dict["cwd"] = cwd_override
                        elif target_field == "workspace.current_dir":
                            p_dict.setdefault("workspace", {})["current_dir"] = cwd_override
                        elif target_field == "workspace.project_dir":
                            p_dict.setdefault("workspace", {})["project_dir"] = cwd_override
                    payload_to_record = p_dict
                    stdin_bytes = json.dumps(p_dict, ensure_ascii=False).encode("utf-8")

                if spec.via_cmd_cp is not None:
                    bat_ps = ps_root / "run_ps.cmd"
                    bat_py = py_root / "run_py.cmd"
                    bat_ps.write_text(
                        f'@echo off\r\nchcp {spec.via_cmd_cp} >nul\r\npwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{PS1}"\r\n',
                        encoding="ascii",
                    )
                    bat_py.write_text(
                        f'@echo off\r\nchcp {spec.via_cmd_cp} >nul\r\npython "{PY}"\r\n',
                        encoding="ascii",
                    )
                    cmd_ps = ["cmd.exe", "/d", "/c", str(bat_ps)]
                    cmd_py = ["cmd.exe", "/d", "/c", str(bat_py)]
                else:
                    cmd_ps = ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(PS1)]
                    cmd_py = ["python", str(PY)]

                proc_ps = subprocess.Popen(cmd_ps, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env_ps)
                proc_py = subprocess.Popen(cmd_py, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env_py)
                out_ps, _ = proc_ps.communicate(stdin_bytes)
                out_py, _ = proc_py.communicate(stdin_bytes)
                rc_ps, rc_py = proc_ps.returncode, proc_py.returncode
                t1 = int(time.time())
            finally:
                for c in open_conns:
                    c.close()

            if not needs_clock_sync or t1 == t0:
                if cwd_override is not None and isinstance(payload_to_record, dict):
                    for k in ("cwd",):
                        if k in payload_to_record and isinstance(payload_to_record[k], str):
                            payload_to_record[k] = "<TEMP_FIXTURE>/" + pathlib.Path(payload_to_record[k]).name
                    if isinstance(payload_to_record.get("workspace"), dict):
                        for wk in ("current_dir", "project_dir"):
                            if wk in payload_to_record["workspace"]:
                                payload_to_record["workspace"][wk] = (
                                    "<TEMP_FIXTURE>/" + pathlib.Path(payload_to_record["workspace"][wk]).name
                                )
                equal = out_ps == out_py
                return {
                    "id": spec.case_id,
                    "category": spec.category,
                    "description": spec.description,
                    "covers": spec.covers,
                    "divergence_id": spec.divergence_id,
                    "via_cmd_cp": spec.via_cmd_cp,
                    "setup": spec.setup_kind,
                    "env": spec.env,
                    "payload": payload_to_record,
                    "equal": equal,
                    "ps1": {
                        "exit_code": rc_ps,
                        "byte_len": len(out_ps),
                        "utf8": out_ps.decode("utf-8", "replace"),
                        "hex": out_ps.hex(),
                    },
                    "py": {
                        "exit_code": rc_py,
                        "byte_len": len(out_py),
                        "utf8": out_py.decode("utf-8", "replace"),
                        "hex": out_py.hex(),
                    },
                }

    raise RuntimeError(f"Timing retry exhausted for case {spec.case_id}")


def build_cases() -> list[CaseSpec]:
    cases: list[CaseSpec] = []

    # ─── A. Stdin Empty / Invalid / Top-Level JSON Shapes ─────────────────────
    cases.append(CaseSpec(
        "A01_empty_stdin", "stdin_json", "空白 stdin 输入两边均静默退出",
        ["stdin"], raw_stdin=b"   \n",
    ))
    cases.append(CaseSpec(
        "A02_invalid_json", "stdin_json", "非法 JSON 输入两边均静默退出",
        ["stdin"], raw_stdin=b"{not-json",
    ))
    cases.append(CaseSpec(
        "A03_top_level_empty_array", "stdin_json", "顶层空数组 []：ps1 管道展开为 $null 退出，py 非 dict 退出",
        ["stdin"], raw_stdin=b"[]",
    ))
    cases.append(CaseSpec(
        "A04_top_level_nonempty_array", "stdin_json", "顶层非空数组 [1]：ps1 按默认字段渲染，py 检查 dict 输出 0 B",
        ["stdin"], raw_stdin=b"[1]", divergence_id="DIV-01",
    ))
    cases.append(CaseSpec(
        "A05_top_level_scalar", "stdin_json", "顶层标量 123：ps1 按默认字段渲染，py 检查 dict 输出 0 B",
        ["stdin"], raw_stdin=b"123", divergence_id="DIV-01",
    ))
    cases.append(CaseSpec(
        "A06_case_colliding_keys", "stdin_json", "仅大小写不同的重复键：ps1 ConvertFrom-Json 报错输出 0 B，py 正常渲染",
        ["stdin", "session_id"], raw_stdin=b'{"session_id":"probe_a06","x":1,"X":2}', divergence_id="DIV-02",
    ))
    cases.append(CaseSpec(
        "A07_uppercase_field_names", "stdin_json", "JSON 键名大小写不一致（THINKINGLEVEL）：ps1 属性不区分大小写识别，py 忽略",
        ["session_id", "thinkingLevel"], payload={"session_id": "probe_a07", "THINKINGLEVEL": "high"}, divergence_id="DIV-03",
    ))

    # ─── B. Session ID, Conversation ID & Agent State ─────────────────────────
    cases.append(CaseSpec(
        "B01_missing_both_ids", "session_state", "session_id 与 conversation_id 均缺失：两边均回退到 default",
        ["session_id", "conversation_id"], payload={},
    ))
    cases.append(CaseSpec(
        "B02_conversation_id_fallback", "session_state", "session_id 缺失、conversation_id 非空：两边均回退到 conversation_id",
        ["session_id", "conversation_id"], payload={"conversation_id": "probe_conv_b02"},
    ))
    cases.append(CaseSpec(
        "B03_empty_session_id_startup", "session_state", "agy 1.5.9 启动态 session_id=\"\" & conversation_id=\"\"：ps1 输出 0 B，py 按 default 渲染",
        ["session_id", "conversation_id", "agent_state", "model.id", "model.display_name", "model.effort", "cycle_mode", "quota.*.remaining_fraction"],
        payload={
            "session_id": "",
            "conversation_id": "",
            "agent_state": "idle",
            "model": {"id": "Gemini 4 Carbon (High)", "display_name": "Gemini 4 Carbon (High)", "effort": "high"},
            "cycle_mode": "auto",
            "quota": {"3p-weekly": {"remaining_fraction": 0.49016213}, "gemini-5h": {"remaining_fraction": 0.96379286}},
        },
        divergence_id="DIV-04",
    ))
    cases.append(CaseSpec(
        "B04_empty_session_id_with_conv_id", "session_state", "session_id=\"\" 但 conversation_id 非空：ps1 的 ?? 不跨空串，仍输出 0 B；py 用 conversation_id 渲染",
        ["session_id", "conversation_id"], payload={"session_id": "", "conversation_id": "probe_conv_b04"}, divergence_id="DIV-04",
    ))
    cases.append(CaseSpec(
        "B05_agent_state_working", "session_state", "agent_state=\"working\"：两边均将缓存置为 5:00 热态",
        ["agent_state"], payload={"session_id": "probe_b05", "agent_state": "working"},
    ))
    cases.append(CaseSpec(
        "B06_agent_state_empty_string", "session_state", "agent_state=\"\"：ps1 的 ?? 保留空串且判 -ne 'idle' 显示 5:00（并写坏状态文件首列），py 归一为 idle 显示 cold",
        ["agent_state"], payload={"session_id": "probe_b06", "agent_state": ""}, divergence_id="DIV-05",
    ))

    # ─── C. Model Slug, Display Name & Effort / thinkingLevel ─────────────────
    cases.append(CaseSpec(
        "C01_display_paren_effort", "model_effort", "display_name 括号 effort（Gemini 4 Carbon (High)）与显式 effort",
        ["model.display_name", "model.id", "model.effort"],
        payload={"session_id": "probe_c01", "model": {"id": "Gemini 4 Carbon (High)", "display_name": "Gemini 4 Carbon (High)", "effort": "high"}},
    ))
    cases.append(CaseSpec(
        "C02_display_middledot_no_effort", "model_effort", "display_name 含中点 `Gemini 4 Carbon · High`（无 effort）：ps1 单引号内 `u{00b7} 不展开，颜色字节缺少灰色分隔符",
        ["model.display_name"],
        payload={"session_id": "probe_c02", "model": {"display_name": "Gemini 4 Carbon · High"}},
        divergence_id="DIV-06",
    ))
    cases.append(CaseSpec(
        "C03_display_middledot_with_effort", "model_effort", "display_name 含 `Gemini 4 Carbon · High` 且同时带 model.effort=\"high\"：ps1 未剥离后缀导致可见文字重复两次 `· High`",
        ["model.display_name", "model.effort"],
        payload={"session_id": "probe_c03", "model": {"display_name": "Gemini 4 Carbon · High", "effort": "high"}},
        divergence_id="DIV-06",
    ))
    cases.append(CaseSpec(
        "C04_display_middledot_norm_effort", "model_effort", "display_name 含 `Gemini 4 Carbon · medium`：py 拆出并规整为 Med，ps1 原样保留小写 medium",
        ["model.display_name"],
        payload={"session_id": "probe_c04", "model": {"display_name": "Gemini 4 Carbon · medium"}},
        divergence_id="DIV-06",
    ))
    cases.append(CaseSpec(
        "C05_effort_mapping_all_levels", "model_effort", "thinkingLevel 回退及全部 effort 映射（very_high -> xHigh）",
        ["model.id", "thinkingLevel"],
        payload={"session_id": "probe_c05", "model": {"id": "models/claude-opus-4-6-1m-20250929"}, "thinkingLevel": "very_high"},
    ))
    cases.append(CaseSpec(
        "C06_empty_effort_blocks_thinkinglevel", "model_effort", "model.effort=\"\" 且 thinkingLevel=\"max\"：ps1 的 ?? 停在空串丢掉 Max，py 回退渲染 Max",
        ["model.effort", "thinkingLevel"],
        payload={"session_id": "probe_c06", "model": {"id": "claude-3-5-sonnet", "effort": ""}, "thinkingLevel": "max"},
        divergence_id="DIV-04",
    ))
    cases.append(CaseSpec(
        "C07_gpt_standard_4o_32k", "model_effort", "GPT 常规与数字开头后缀（gpt-4o-32k）：两边均输出 GPT-4 O 32K",
        ["model.id"],
        payload={"session_id": "probe_c07", "model": {"id": "gpt-4o-32k"}},
    ))
    cases.append(CaseSpec(
        "C08_gpt_alnum_word_titlecase", "model_effort", "GPT 字母+数字+字母后缀（gpt-5-o4mini）：ps1 ToTitleCase 输出 O4mini，py .title() 输出 O4Mini",
        ["model.id"],
        payload={"session_id": "probe_c08", "model": {"id": "gpt-5-o4mini"}},
        divergence_id="DIV-07",
    ))
    cases.append(CaseSpec(
        "C09_gpt_double_hyphen", "model_effort", "GPT 连续连字符（gpt-4--mini）：ps1 只去单个前导连字符输出双空格 `GPT-4  Mini`，py 输出 `GPT-4 Mini`",
        ["model.id"],
        payload={"session_id": "probe_c09", "model": {"id": "gpt-4--mini"}},
        divergence_id="DIV-08",
    ))
    cases.append(CaseSpec(
        "C10_uppercase_prefix_suffix_slug", "model_effort", "大写前缀/后缀（MODELS/gemini-2.5-pro-PREVIEW）：ps1 -replace 不区分大小写剥离，py 区分大小写保留原串",
        ["model.id"],
        payload={"session_id": "probe_c10", "model": {"id": "MODELS/gemini-2.5-pro-PREVIEW"}},
        divergence_id="DIV-09",
    ))
    cases.append(CaseSpec(
        "C11_generic_kebab_slug", "model_effort", "通用 kebab-case 美化（gemini-4-carbon-v2 -> Gemini 4 Carbon V2）",
        ["model.id"],
        payload={"session_id": "probe_c11", "model": {"id": "gemini-4-carbon-v2-preview-05-06"}},
    ))

    # ─── D. Workspace / CWD & VCS / Git ───────────────────────────────────────
    cases.append(CaseSpec(
        "D01_workspace_fallbacks", "cwd_vcs", "cwd 缺失时依次回退 workspace.current_dir / workspace.project_dir",
        ["cwd", "workspace.current_dir", "workspace.project_dir"],
        payload={"session_id": "probe_d01", "workspace": {"project_dir": r"D:\NonExistent\agyline-demo\\"}},
    ))
    cases.append(CaseSpec(
        "D02_empty_cwd_blocks_workspace", "cwd_vcs", "cwd=\"\" 且 workspace.current_dir 非空：ps1 的 ?? 停在空串丢掉目录段与 Git 段，py 回退正常渲染",
        ["cwd", "workspace.current_dir"],
        payload={"session_id": "probe_d02", "cwd": ""},
        setup_kind="git_repo", setup_args={"dir_name": "ws-repo", "branch": "main", "mode": "clean", "cwd_field": "workspace.current_dir"},
        divergence_id="DIV-04",
    ))
    cases.append(CaseSpec(
        "D03_drive_root_cwd", "cwd_vcs", "盘符根目录 cwd=\"D:\\\\\"：两边均回退显示 D:",
        ["cwd"], payload={"session_id": "probe_d03", "cwd": "D:\\"},
    ))
    cases.append(CaseSpec(
        "D04_git_clean_repo", "cwd_vcs", "干净 Git 仓库：两边均通过 git status --porcelain=v2 渲染 main ✓",
        ["cwd", "vcs.branch", "vcs.dirty"],
        payload={"session_id": "probe_d04", "vcs": {"type": "git"}},
        setup_kind="git_repo", setup_args={"dir_name": "clean-repo", "branch": "main", "mode": "clean"},
    ))
    cases.append(CaseSpec(
        "D05_git_dirty_ahead_behind", "cwd_vcs", "Git ahead/behind 与长分支截断（>24 字符截为 23+…）及 payload vcs.dirty=true",
        ["cwd", "vcs.branch", "vcs.dirty"],
        payload={"session_id": "probe_d05", "vcs": {"dirty": True}},
        setup_kind="git_repo", setup_args={"dir_name": "ab-repo", "branch": "feature/twenty-five-chars", "mode": "ahead_behind"},
    ))
    cases.append(CaseSpec(
        "D06_git_conflict_and_detached", "cwd_vcs", "Git 合并冲突状态（红色 ⚠）",
        ["cwd"],
        payload={"session_id": "probe_d06"},
        setup_kind="git_repo", setup_args={"dir_name": "conf-repo", "branch": "main", "mode": "conflict"},
    ))
    cases.append(CaseSpec(
        "D07_git_detached_head", "cwd_vcs", "Git detached HEAD 显示 7 位短 commit SHA",
        ["cwd"],
        payload={"session_id": "probe_d07"},
        setup_kind="git_repo", setup_args={"dir_name": "det-repo", "branch": "main", "mode": "detached"},
    ))
    cases.append(CaseSpec(
        "D08_git_junction_into_repo", "cwd_vcs", "目录 Junction 指向仓库内子目录：ps1 GetFullPath 纯词法上溯漏掉 .git，py Path.resolve() 解析真实路径渲染 Git 段",
        ["cwd"],
        payload={"session_id": "probe_d08"},
        setup_kind="git_junction",
        divergence_id="DIV-10",
    ))
    cases.append(CaseSpec(
        "D09_branch_emoji_utf16_vs_codepoint", "cwd_vcs", "含 Emoji 的分支名（24 码点 = 25 UTF-16 码元，且第 23 UTF-16 码元切在代理对中间）：ps1 截断并产生损坏替换符 ，py 不截断",
        ["vcs.branch"],
        payload={"session_id": "probe_d09", "vcs": {"branch": "a" * 22 + "🚀b"}},
        divergence_id="DIV-11",
    ))
    cases.append(CaseSpec(
        "D10_vcs_dirty_nonbool_int", "cwd_vcs", "布尔字段传整数 1（vcs.dirty=1, sandbox.enabled=1）：ps1 -eq $true 判真，py is True 判假",
        ["vcs.branch", "vcs.dirty", "sandbox.enabled"],
        payload={"session_id": "probe_d10", "vcs": {"branch": "main", "dirty": 1}, "sandbox": {"enabled": 1}},
        divergence_id="DIV-12",
    ))
    cases.append(CaseSpec(
        "D11_cp936_non_ascii_stdin", "cwd_vcs", "简体中文默认控制台代码页 CP936 下含中文路径的 UTF-8 stdin：ps1 未设 InputEncoding 致 GBK 吞引号 JSON 解析失败输出 0 B，py 正常输出",
        ["cwd"],
        payload={"session_id": "probe_d11", "cwd": r"D:\中文目录\项目名", "model": {"display_name": "Gemini 4 (High)"}},
        via_cmd_cp=936,
        divergence_id="DIV-13",
    ))
    cases.append(CaseSpec(
        "D12_cp437_non_ascii_git_branch", "cwd_vcs", "CP437 控制台代码页下含中文 Git 分支名：ps1 未设 StandardOutputEncoding 输出乱码，py UTF-8 解码正常",
        ["cwd"],
        payload={"session_id": "probe_d12"},
        setup_kind="git_repo", setup_args={"dir_name": "cn-branch-repo", "branch": "feat/中文分支", "mode": "clean"},
        via_cmd_cp=437,
        divergence_id="DIV-13",
    ))

    # ─── E. Context Window, Numbers, Rounding, Cost & Quota ───────────────────
    cases.append(CaseSpec(
        "E01_ctx_tokens_and_cache_hit", "numbers_budget", "上下文窗口正常态（<50%）、k 缩写与缓存命中率",
        [
            "context_window.current_usage.input_tokens",
            "context_window.current_usage.output_tokens",
            "context_window.current_usage.cache_read_input_tokens",
            "context_window.context_window_size",
            "agent_state",
        ],
        payload={
            "session_id": "probe_e01",
            "agent_state": "working",
            "context_window": {
                "context_window_size": 1048576,
                "current_usage": {"input_tokens": 12000, "output_tokens": 500, "cache_read_input_tokens": 198000},
            },
        },
    ))
    cases.append(CaseSpec(
        "E02_ctx_warn_and_crit_thresholds", "numbers_budget", "上下文窗口 50% 告警色与 >=80% 危险色及 M 缩写（1,250,000 -> 1.2M，验证两边均为 IEEE-754 ties-to-even）",
        ["context_window.current_usage.input_tokens", "context_window.context_window_size"],
        payload={
            "session_id": "probe_e02",
            "context_window": {
                "context_window_size": 1500000,
                "current_usage": {"input_tokens": 1250000},
            },
        },
    ))
    cases.append(CaseSpec(
        "E03_rounding_midpoints_ties_to_even", "numbers_budget", "核验 .NET 7+ F0/F1/F2/F3 与 Python 舍入一致性：10.5%->10%、$2.675->$2.67、quota 0.445->44%（全部零分歧）",
        ["context_window.used_percentage", "context_window.context_window_size", "cost", "quota.*.remaining_fraction"],
        payload={
            "session_id": "probe_e03",
            "context_window": {"used_percentage": 10.5, "context_window_size": 200000},
            "cost": 2.675,
            "quota": {"a": {"remaining_fraction": 0.445}, "b": {"remaining_fraction": 0.01, "disabled": True}},
        },
    ))
    cases.append(CaseSpec(
        "E04_rounding_small_pct_and_subdollar_cost", "numbers_budget", "核验 <10% 的 F1（0.25%->0.2%）与 <$1 的 F3（$0.0625->$0.062）及 cost.total_usd 对象读法",
        ["context_window.used_percentage", "cost", "quota.*.remaining_fraction"],
        payload={
            "session_id": "probe_e04",
            "context_window": {"used_percentage": 0.25},
            "cost": {"total_usd": 0.0625},
            "quota": {"3p-5h": {"remaining_fraction": 0.15}},
        },
    ))
    cases.append(CaseSpec(
        "E05_cost_top_level_string", "numbers_budget", "cost 为顶层数字字符串 \"0.42\"：ps1 解析渲染 $0.420，py 只接受 dict/int/float 忽略之",
        ["cost"],
        payload={"session_id": "probe_e05", "cost": "0.42"},
        divergence_id="DIV-14",
    ))
    cases.append(CaseSpec(
        "E06_quota_fraction_gt_one", "numbers_budget", "quota.remaining_fraction > 1.0（如 1.2）：ps1 有 <=1.0 上限不显示配额段，py 无上限直接渲染 120% quota",
        ["quota.*.remaining_fraction"],
        payload={"session_id": "probe_e06", "quota": {"b": {"remaining_fraction": 1.2}}},
        divergence_id="DIV-15",
    ))
    cases.append(CaseSpec(
        "E07_numeric_string_underscore_and_inf", "numbers_budget", "数字字符串含 \"inf\"：py 的 _to_int 抛未捕获 OverflowError 退出码 1 输出 0 B，ps1 回退为 0 正常输出",
        ["task_count"],
        payload={"session_id": "probe_e07", "task_count": "inf"},
        divergence_id="DIV-16",
    ))

    # ─── F. Stateful TPS, Cache Relay, Temp Precedence & Response Model DB ────
    cases.append(CaseSpec(
        "F01_stateful_tps_from_telemetry", "stateful_db", "从 agy_telemetry_<sid>.dat 读取上一次 tok/s 与热缓存倒计时",
        ["context_window.total_output_tokens"],
        payload={"session_id": "probe_f01", "context_window": {"total_output_tokens": 500}},
        setup_kind="telemetry",
        setup_args={"sid": "probe_f01", "line": "idle {now_minus_2}.000 500 64.5 1.2s 80 - Model {now_minus_120}"},
    ))
    cases.append(CaseSpec(
        "F02_temp_vs_tmp_precedence", "stateful_db", "Windows 下 TEMP 与 TMP 指向不同目录：ps1 GetTempPath 读 TMP，py gettempdir 读 TEMP，状态文件互不可见",
        ["session_id"],
        payload={"session_id": "probe_f02"},
        setup_kind="temp_vs_tmp",
        setup_args={"sid": "probe_f02"},
        divergence_id="DIV-17",
    ))
    cases.append(CaseSpec(
        "F03_db_response_model_match", "stateful_db", "会话库 gen_metadata 含常规模型（claude-opus-4-6）：两边均把 Auto 替换为 Opus 4.6",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f03", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f03", "gen_rows": [(1, b"\x08\x01\x9a\x01\x0fclaude-opus-4-6")], "mtime_age": 60},
    ))
    cases.append(CaseSpec(
        "F04_db_gt256kb_steps_tail", "stateful_db", "真实会话高频场景：steps 表追加 >256 KB 使 gen_metadata 距文件尾超 256 KB，ps1 漏检保留 Auto，py 查 SQLite 命中 Opus 4.6",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f04", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f04", "gen_rows": [(1, b"\x9a\x01\x0fclaude-opus-4-6")], "step_blobs": [b"x" * 300_000]},
        divergence_id="DIV-18",
    ))
    cases.append(CaseSpec(
        "F05_db_wal_stale_frame", "stateful_db", "SQLite WAL RESTART 检查点后新帧覆写头部、旧帧留存尾部：ps1 倒扫读到旧模型 Opus 4.6，py 读到最新模型 Gemini 2.5 Pro",
        ["model.display_name", "agent_state", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f05", "model": {"display_name": "Auto"}, "agent_state": "working", "context_window": {"total_output_tokens": 20}},
        setup_kind="db_wal_stale",
        setup_args={"sid": "probe_f05"},
        divergence_id="DIV-18",
    ))
    cases.append(CaseSpec(
        "F06_db_steps_table_pollution", "stateful_db", "steps 表字节中含 \\x9a\\x01 标记：ps1 不区分表扫出 Gemini 2.5 Pro，py 只查 gen_metadata 返回 Opus 4.6",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f06", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f06", "gen_rows": [(1, b"\x9a\x01\x0fclaude-opus-4-6")], "step_blobs": [b"log \x9a\x01\x0egemini-2.5-pro"]},
        divergence_id="DIV-18",
    ))
    cases.append(CaseSpec(
        "F07_db_trailing_nonmodel_field19", "stateful_db", "gen_metadata.data 内模型后跟非模型 field 19（end_turn）：py 正则过宽误把 end_turn 当模型，ps1 前缀过滤命中 Opus 4.6",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f07", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f07", "gen_rows": [(1, b"\x9a\x01\x0fclaude-opus-4-6\x9a\x01\x08end_turn")]},
        divergence_id="DIV-18",
    ))
    cases.append(CaseSpec(
        "F08_db_gt600b_trailing_in_blob", "stateful_db", "gen_metadata.data 模型标记距 blob 末尾 >600 B：py 因 substr(data,-600) 截断漏检，ps1 256 KB 扫描命中 Opus 4.6",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f08", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f08", "gen_rows": [(1, b"\x9a\x01\x0fclaude-opus-4-6" + b"\x00" * 700)]},
        divergence_id="DIV-18",
    ))
    cases.append(CaseSpec(
        "F09_db_short_and_nonwhitelist_models", "stateful_db", "短模型名 o3（长 2 < 4）或两段非白名单名 mistral-large：ps1 长度/前缀过滤拒收，py 命中 o3",
        ["model.display_name", "context_window.total_output_tokens"],
        payload={"session_id": "probe_f09", "model": {"display_name": "Auto"}, "context_window": {"total_output_tokens": 20}},
        setup_kind="db",
        setup_args={"sid": "probe_f09", "gen_rows": [(1, b"\x9a\x01\x02o3")]},
        divergence_id="DIV-18",
    ))

    # ─── G. Line 2 Runtime Indicators (Mode, Subagents, Tasks, Queue, Sandbox) ─
    cases.append(CaseSpec(
        "G01_line2_all_active_runtime", "line2_runtime", "第 2 行全要素同时开启：execution_mode(yolo)、运行中子代理、单数 task、queued、sandbox+net",
        [
            "execution_mode",
            "subagents",
            "task_count",
            "pending_input_count",
            "sandbox.enabled",
            "sandbox.allow_network",
        ],
        payload={
            "session_id": "probe_g01",
            "execution_mode": "yolo",
            "subagents": [{"status": "running"}, {"status": "completed"}, {"status": "thinking"}],
            "task_count": 1,
            "pending_input_count": 2,
            "sandbox": {"enabled": True, "allow_network": True},
        },
    ))
    cases.append(CaseSpec(
        "G02_line2_confirm_pending_and_idle_agents", "line2_runtime", "tool_confirmation_pending 覆盖模式显示 approval required，外加全部完成子代理、复数 tasks、无网 sandbox",
        [
            "tool_confirmation_pending",
            "cycle_mode",
            "subagents",
            "task_count",
            "sandbox.enabled",
            "sandbox.allow_network",
        ],
        payload={
            "session_id": "probe_g02",
            "tool_confirmation_pending": True,
            "cycle_mode": "auto",
            "subagents": [{"status": "completed"}, {"status": "done"}],
            "task_count": 3,
            "sandbox": {"enabled": True, "allow_network": False},
        },
    ))
    cases.append(CaseSpec(
        "G03_empty_exec_mode_blocks_cycle_mode", "line2_runtime", "execution_mode=\"\" 且 cycle_mode=\"plan\"：ps1 的 ?? 停在空串不渲染第 2 行，py 回退渲染 plan mode on",
        ["execution_mode", "cycle_mode"],
        payload={"session_id": "probe_g03", "execution_mode": "", "cycle_mode": "plan"},
        divergence_id="DIV-04",
    ))
    cases.append(CaseSpec(
        "G04_subagent_status_uppercase", "line2_runtime", "subagents[].status 大写 \"RUNNING\"：ps1 -in 不区分大小写显示 1/1 agents running，py 区分大小写显示 1 agents",
        ["subagents"],
        payload={"session_id": "probe_g04", "subagents": [{"status": "RUNNING"}]},
        divergence_id="DIV-19",
    ))

    # ─── H. Environment Variables (`AGY_*` + `COLORFGBG`) ─────────────────────
    cases.append(CaseSpec(
        "H01_env_theme_light", "env_vars", "AGY_THEME=light 切换浅色 SGR 调色板",
        ["AGY_THEME"],
        payload={"session_id": "probe_h01", "cycle_mode": "accept-edits"},
        env={"AGY_THEME": "light"},
    ))
    cases.append(CaseSpec(
        "H02_env_colorfgbg_auto_light_and_dark_override", "env_vars", "COLORFGBG=0;15 自动切浅色，且被 AGY_THEME=dark 显式覆盖回深色",
        ["AGY_THEME", "COLORFGBG"],
        payload={"session_id": "probe_h02", "cycle_mode": "best-of-n"},
        env={"AGY_THEME": "dark", "COLORFGBG": "0;15"},
    ))
    cases.append(CaseSpec(
        "H03_env_theme_trailing_space", "env_vars", "AGY_THEME=\"light \" 含尾空格（如 cmd set AGY_THEME=light && ...）：py strip() 切浅色，ps1 原样比较落回深色",
        ["AGY_THEME"],
        payload={"session_id": "probe_h03"},
        env={"AGY_THEME": "light "},
        divergence_id="DIV-20",
    ))
    cases.append(CaseSpec(
        "H04_env_lines_single_mode_pill", "env_vars", "AGY_STATUSLINE_LINES=1 合并为单行并追加紧凑模式胶囊 `▸▸ auto`",
        ["AGY_STATUSLINE_LINES", "cycle_mode"],
        payload={"session_id": "probe_h04", "cycle_mode": "auto", "task_count": 2},
        env={"AGY_STATUSLINE_LINES": "1"},
    ))
    cases.append(CaseSpec(
        "H05_env_lines_single_confirm", "env_vars", "AGY_STATUSLINE_LINES=1 且 tool_confirmation_pending=true 追加 `⚠ confirm`",
        ["AGY_STATUSLINE_LINES", "tool_confirmation_pending"],
        payload={"session_id": "probe_h05", "tool_confirmation_pending": True},
        env={"AGY_STATUSLINE_LINES": "1"},
    ))
    cases.append(CaseSpec(
        "H06_env_indent_custom_and_empty", "env_vars", "AGY_STATUSLINE_INDENT 自定义缩进 \"    \"（两边一致）",
        ["AGY_STATUSLINE_INDENT"],
        payload={"session_id": "probe_h06", "cycle_mode": "auto"},
        env={"AGY_STATUSLINE_INDENT": "    "},
    ))
    cases.append(CaseSpec(
        "H07_env_show_default_mode", "env_vars", "AGY_SHOW_DEFAULT_MODE=1 在未知/默认模式下渲染 `▸▸ ask mode (shift+tab to cycle)`",
        ["AGY_SHOW_DEFAULT_MODE", "cycle_mode"],
        payload={"session_id": "probe_h07", "cycle_mode": "default"},
        env={"AGY_SHOW_DEFAULT_MODE": "1"},
    ))
    cases.append(CaseSpec(
        "H08_env_cache_relay_live_badges", "env_vars", "AGY_CACHE_RELAY 开启并读取 agy_cache_<sid>.dat 渲染保活徽标 `↻2` 与命中率",
        ["AGY_CACHE_RELAY", "context_window.current_usage.input_tokens", "context_window.current_usage.cache_read_input_tokens"],
        payload={
            "session_id": "probe_h08",
            "context_window": {"current_usage": {"input_tokens": 4000, "cache_read_input_tokens": 36000}},
        },
        env={"AGY_CACHE_RELAY": "http://127.0.0.1:9"},
        setup_kind="relay",
        setup_args={"sid": "probe_h08", "r_ts_mode": "recent", "touches": 2, "status": "live"},
    ))
    cases.append(CaseSpec(
        "H09_env_cache_relay_uppercase_OFF", "env_vars", "AGY_CACHE_RELAY=\"OFF\"（大写）：ps1 -eq 'off' 关闭保活忽略状态文件，py == 'off' 区分大小写仍当作开启渲染 `✗`",
        ["AGY_CACHE_RELAY"],
        payload={"session_id": "probe_h09", "agent_state": "working"},
        env={"AGY_CACHE_RELAY": "OFF"},
        setup_kind="relay",
        setup_args={"sid": "probe_h09", "r_ts_mode": "recent", "touches": 1, "status": "fail"},
        divergence_id="DIV-21",
    ))

    return cases


def _set_console_utf8() -> None:
    if os.name == "nt":
        kernel32 = ctypes.windll.kernel32
        kernel32.SetConsoleCP(65001)
        kernel32.SetConsoleOutputCP(65001)


def main() -> int:
    cases = build_cases()
    normal_cases = [c for c in cases if c.via_cmd_cp is None]
    cp_cases = [c for c in cases if c.via_cmd_cp is not None]

    t_start = time.perf_counter()
    _set_console_utf8()
    results_by_id: dict[str, dict[str, Any]] = {}
    try:
        with ThreadPoolExecutor(max_workers=3) as pool:
            for r in pool.map(execute_case, normal_cases):
                results_by_id[r["id"]] = r
        for c in cp_cases:
            results_by_id[c.case_id] = execute_case(c)
    finally:
        _set_console_utf8()

    results = [results_by_id[c.case_id] for c in cases]
    elapsed = time.perf_counter() - t_start

    # Clean up any accidental probe_* files in real %TEMP% just in case
    sys_temp = pathlib.Path(tempfile.gettempdir())
    cleaned = 0
    for pat in ("agy_telemetry_probe_*", "agy_cache_probe_*", "agy_vcs2_probe_*"):
        for fp in sys_temp.glob(pat):
            fp.unlink(missing_ok=True)
            cleaned += 1

    out_path = HERE / "fixtures.json"
    summary = {
        "total_cases": len(results),
        "matching_cases": sum(1 for r in results if r["equal"]),
        "diverging_cases": sum(1 for r in results if not r["equal"]),
        "elapsed_seconds": round(elapsed, 2),
        "stray_system_temp_files_cleaned": cleaned,
        "cases": results,
    }
    out_path.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"Completed {summary['total_cases']} cases in {summary['elapsed_seconds']}s: "
        f"{summary['matching_cases']} identical, {summary['diverging_cases']} diverging "
        f"(stray temp files cleaned: {cleaned})."
    )
    unexpected = []
    for r in results:
        expected_div = r["divergence_id"] is not None
        actual_div = not r["equal"]
        if expected_div != actual_div:
            unexpected.append((r["id"], r["divergence_id"], r["equal"]))
        tag = "DIFF" if actual_div else "SAME"
        print(f"  [{tag}] {r['id']:38s} div={str(r['divergence_id']):7s} bytes=({r['ps1']['byte_len']},{r['py']['byte_len']})")

    if unexpected:
        print("UNEXPECTED MISMATCHES:", unexpected)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
