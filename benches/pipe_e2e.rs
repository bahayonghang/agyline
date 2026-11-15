use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agyline::{GitCacheEntry, PlatformKind, host_platform, write_git_cache};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvokeMode {
    Direct,
    ShellWrapped,
}

fn locate_agyline_binary() -> PathBuf {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_agyline") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    let cur = env::current_exe().expect("current_exe");
    let bin_name = match host_platform() {
        PlatformKind::Windows => "agyline.exe",
        PlatformKind::Unix => "agyline",
    };
    if let Some(parent) = cur.parent() {
        let sibling = parent.join(bin_name);
        if sibling.exists() {
            return sibling;
        }
        if let Some(target_dir) = parent.parent() {
            let candidate = target_dir.join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    panic!(
        "could not locate built '{bin_name}'; run 'cargo build' or 'cargo bench' on the package"
    );
}

fn push_varint(out: &mut Vec<u8>, mut val: u64) {
    loop {
        if val < 0x80 {
            out.push(val as u8);
            break;
        }
        out.push(((val & 0x7f) as u8) | 0x80);
        val >>= 7;
    }
}

fn encode_gen_metadata_blob(chat_model: &str) -> Vec<u8> {
    let mut inner = Vec::new();
    push_varint(&mut inner, (19 << 3) | 2);
    push_varint(&mut inner, chat_model.len() as u64);
    inner.extend_from_slice(chat_model.as_bytes());

    let mut outer = Vec::new();
    push_varint(&mut outer, (1 << 3) | 2);
    push_varint(&mut outer, inner.len() as u64);
    outer.extend_from_slice(&inner);
    outer
}

fn create_slow_git_shim(bin_dir: &Path) {
    fs::create_dir_all(bin_dir).expect("create fake git dir");
    match host_platform() {
        PlatformKind::Windows => {
            let bat = bin_dir.join("git.cmd");
            fs::write(
                &bat,
                "@echo off\r\nping -n 3 127.0.0.1 >nul\r\nexit /b 0\r\n",
            )
            .expect("write git.cmd");
        }
        PlatformKind::Unix => {
            let sh = bin_dir.join("git");
            fs::write(&sh, "#!/bin/sh\nsleep 2\nexit 0\n").expect("write git shim");
            let _ = Command::new("chmod").arg("+x").arg(&sh).status();
        }
    }
}

fn spawn_once(
    bin: &Path,
    mode: InvokeMode,
    home_dir: &Path,
    user_home: &Path,
    prepend_path: Option<&Path>,
    stdin_bytes: &[u8],
) -> Duration {
    let mut cmd = match mode {
        InvokeMode::Direct => Command::new(bin),
        InvokeMode::ShellWrapped => match host_platform() {
            PlatformKind::Windows => {
                let mut c = Command::new("cmd");
                c.arg("/d").arg("/s").arg("/c").arg(bin);
                c
            }
            PlatformKind::Unix => {
                let mut c = Command::new("sh");
                c.arg("-c").arg("\"$1\"").arg("sh").arg(bin);
                c
            }
        },
    };

    cmd.env("AGYLINE_HOME", home_dir)
        .env("HOME", user_home)
        .env("USERPROFILE", user_home)
        .env_remove("AGY_THEME")
        .env_remove("AGY_STATUSLINE_LINES")
        .env_remove("AGY_STATUSLINE_INDENT")
        .env_remove("AGY_SHOW_DEFAULT_MODE")
        .env_remove("AGY_CACHE_RELAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(extra) = prepend_path {
        let sep = match host_platform() {
            PlatformKind::Windows => ";",
            PlatformKind::Unix => ":",
        };
        let old_path = env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}{sep}{old_path}", extra.display()));
    }

    let t0 = Instant::now();
    let mut child = cmd.spawn().expect("spawn agyline");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(stdin_bytes);
    }
    let out = child.wait_with_output().expect("wait_with_output");
    let elapsed = t0.elapsed();
    assert!(out.status.success(), "agyline exited non-zero");
    assert!(!out.stdout.is_empty(), "expected non-empty rendered line");
    elapsed
}

fn percentile_ms(sorted: &[Duration], numer: usize, denom: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) * numer) / denom;
    sorted[idx].as_secs_f64() * 1_000.0
}

fn measure_workload<F: FnMut() -> Duration>(label: &str, warmup: usize, samples: usize, mut f: F) {
    for _ in 0..warmup {
        let _ = f();
    }
    let mut durations = Vec::with_capacity(samples);
    for _ in 0..samples {
        durations.push(f());
    }
    durations.sort_unstable();
    println!(
        "{:<42}  N={:<3}  P50={:>6.2} ms  P95={:>6.2} ms  P99={:>6.2} ms",
        label,
        samples,
        percentile_ms(&durations, 50, 100),
        percentile_ms(&durations, 95, 100),
        percentile_ms(&durations, 99, 100),
    );
}

fn parse_sample_count() -> usize {
    if let Ok(raw) = env::var("AGYLINE_BENCH_SAMPLES")
        && let Ok(n) = raw.trim().parse::<usize>()
        && n > 0
    {
        return n;
    }
    let args: Vec<String> = env::args().collect();
    let mut i = 1usize;
    while i < args.len() {
        if args[i] == "--samples"
            && let Some(val) = args.get(i + 1)
            && let Ok(n) = val.parse::<usize>()
            && n > 0
        {
            return n;
        }
        i += 1;
    }
    50
}

fn main() {
    let bin = locate_agyline_binary();
    let samples = parse_sample_count();
    let warmup = 5.min(samples);

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1_700_000_000_000);
    let root = env::temp_dir().join(format!(
        "agyline_pipe_bench_{}_{now_ms}",
        std::process::id()
    ));
    let home_warm = root.join("home_warm");
    let home_cold = root.join("home_cold");
    let home_sql = root.join("home_sql");
    let home_lock = root.join("home_lock");
    let user_home = root.join("user_home");
    let conv_dir = user_home
        .join(".gemini")
        .join("antigravity-cli")
        .join("conversations");
    fs::create_dir_all(&conv_dir).expect("create conv_dir");

    let repo_cwd = root.join("workspace_repo");
    fs::create_dir_all(&repo_cwd).expect("create repo_cwd");
    let cwd_str = repo_cwd.to_string_lossy().into_owned();

    // 1. Seed warm git cache
    write_git_cache(
        &home_warm,
        &GitCacheEntry {
            version: 1,
            cwd: cwd_str.clone(),
            updated_at_ms: now_ms,
            is_repo: true,
            branch: "main".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 1,
            behind: 0,
        },
    )
    .expect("seed warm git cache");

    let warm_payload = serde_json::to_vec(&serde_json::json!({
        "session_id": "bench_warm",
        "agent_state": "idle",
        "model": { "display_name": "Gemini 2.5 Pro", "effort": "high" },
        "cwd": cwd_str,
        "context_window": {
            "current_usage": { "input_tokens": 8000, "cache_read_input_tokens": 72000 },
            "context_window_size": 200000
        }
    }))
    .expect("warm json");

    // 2. Slow git shim for cold cache non-blocking check
    let slow_git_dir = root.join("slow_git_bin");
    create_slow_git_shim(&slow_git_dir);

    // 3. Real WAL SQLite database for lookup & lock contention
    let db_lookup_path = conv_dir.join("bench_sql.db");
    {
        let conn = Connection::open(&db_lookup_path).expect("open bench_sql.db");
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);",
        )
        .expect("init bench_sql schema");
        let blob = encode_gen_metadata_blob("claude-opus-4-6");
        conn.execute(
            "INSERT INTO gen_metadata (idx, data, size) VALUES (1, ?1, ?2)",
            params![blob, blob.len() as i64],
        )
        .expect("insert gen_metadata");
    }

    let db_locked_path = conv_dir.join("bench_locked.db");
    let locked_conn = Connection::open(&db_locked_path).expect("open bench_locked.db");
    locked_conn
        .execute_batch(
            "CREATE TABLE gen_metadata (idx INTEGER PRIMARY KEY, data BLOB, size INTEGER NOT NULL DEFAULT 0);
             BEGIN EXCLUSIVE;
             INSERT INTO gen_metadata (idx, data, size) VALUES (1, X'0A0412026F6B', 6);",
        )
        .expect("acquire exclusive sqlite lock");

    println!(
        "=== agyline end-to-end pipe benchmarks (binary: {}) ===",
        bin.display()
    );

    measure_workload("warm_cache (direct)", warmup, samples, || {
        spawn_once(
            &bin,
            InvokeMode::Direct,
            &home_warm,
            &user_home,
            None,
            &warm_payload,
        )
    });

    measure_workload("warm_cache (shell-wrapped)", warmup, samples, || {
        spawn_once(
            &bin,
            InvokeMode::ShellWrapped,
            &home_warm,
            &user_home,
            None,
            &warm_payload,
        )
    });

    let mut cold_seq = 0u64;
    measure_workload(
        "cold_slow_git (direct, 2s fake git)",
        2.min(warmup),
        samples.min(20),
        || {
            cold_seq += 1;
            let fresh_cwd = root.join(format!("cold_repo_{cold_seq}"));
            let _ = fs::create_dir_all(&fresh_cwd);
            let payload = serde_json::to_vec(&serde_json::json!({
                "session_id": "bench_cold",
                "cwd": fresh_cwd.to_string_lossy(),
                "model": { "display_name": "Opus 4.6" }
            }))
            .expect("cold payload");
            spawn_once(
                &bin,
                InvokeMode::Direct,
                &home_cold,
                &user_home,
                Some(&slow_git_dir),
                &payload,
            )
        },
    );

    let mut sql_out_tok = 100_i64;
    measure_workload(
        "sqlite_lookup (direct, WAL delta_out>0)",
        warmup,
        samples,
        || {
            sql_out_tok += 25;
            let payload = serde_json::to_vec(&serde_json::json!({
                "session_id": "bench_sql",
                "agent_state": "working",
                "model": { "display_name": "Auto" },
                "context_window": {
                    "current_usage": { "output_tokens": 25 },
                    "total_output_tokens": sql_out_tok
                }
            }))
            .expect("sql payload");
            spawn_once(
                &bin,
                InvokeMode::Direct,
                &home_sql,
                &user_home,
                None,
                &payload,
            )
        },
    );

    let mut lock_out_tok = 100_i64;
    measure_workload(
        "sqlite_lock_contention (direct, EXCLUSIVE)",
        warmup,
        samples,
        || {
            lock_out_tok += 25;
            let payload = serde_json::to_vec(&serde_json::json!({
                "session_id": "bench_locked",
                "agent_state": "working",
                "model": { "display_name": "Opus 4.6" },
                "context_window": {
                    "current_usage": { "output_tokens": 25 },
                    "total_output_tokens": lock_out_tok
                }
            }))
            .expect("lock payload");
            spawn_once(
                &bin,
                InvokeMode::Direct,
                &home_lock,
                &user_home,
                None,
                &payload,
            )
        },
    );

    drop(locked_conn);
    let _ = fs::remove_dir_all(&root);
}
