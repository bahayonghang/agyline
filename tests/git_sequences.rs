//! Git contracts driven as sequences: porcelain-v2 parsing, cache-file identity from the
//! lexical cwd, and the fresh / stale-while-revalidate / expired lifecycle as the clock advances.
//! The print path only reads the cache and requests detached `__refresh-git` workers; the worker
//! cases run the real binary to pin lock ownership, link resolution, and pipe EOF under slow git.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agyline::{
    EffectiveConfig, GitCacheEntry, GitFrameState, NoopSpawner, ParsedGitStatus, RenderEnv,
    Spawner, cwd_hash_hex, git_cache_path, git_lock_path, maybe_sweep_stale_state,
    normalize_cwd_lexical, parse_porcelain_v2, parse_stdin_payload, read_git_cache, refresh_git_at,
    resolve_git_frame, run_live_frame, write_git_cache,
};

static SANDBOX_SEQ: AtomicU64 = AtomicU64::new(0);

/// Unique per-test sandbox under the OS temp dir, removed on drop so parallel tests never share
/// state and a failing case leaves no artifacts behind.
struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let seq = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "agyline_test_{label}_{}_{nanos}_{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp sandbox dir");
        Self { path: dir }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// Reference parity for every porcelain-v2 shape: which lines count as dirty, how a detached or
// unborn HEAD becomes a branch label, and that non-UTF-8 output degrades instead of failing.
#[test]
fn porcelain_v2_parses_clean_dirty_ignored_conflict_ahead_behind_detached_and_utf8() {
    // 1. Clean repository with ignored file (`!` must NOT mark dirty)
    let clean_with_ignored = b"# branch.oid 0123456789abcdef0123456789abcdef01234567\r\n\
# branch.head main\r\n\
# branch.upstream origin/main\r\n\
# branch.ab +0 -0\r\n\
! target/debug/agyline.exe\r\n";
    assert_eq!(
        parse_porcelain_v2(clean_with_ignored),
        ParsedGitStatus {
            branch: "main".to_string(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        }
    );

    // 2. Dirty tracked + untracked + ahead/behind (`D05` shape)
    let dirty_ab = b"# branch.oid 89abcdef0123456789abcdef0123456789abcdef\n\
# branch.head feature/twenty-five-chars\n\
# branch.upstream origin/feature/twenty-five-chars\n\
# branch.ab +2 -1\n\
1 .M N... 100644 100644 100644 1111111 2222222 src/lib.rs\n\
? scratch.txt\n";
    assert_eq!(
        parse_porcelain_v2(dirty_ab),
        ParsedGitStatus {
            branch: "feature/twenty-five-chars".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 2,
            behind: 1,
        }
    );

    // 3. Unmerged conflict (`u ` sets both has_conflict and dirty, `D06` shape)
    let conflict = b"# branch.oid fedcba9876543210fedcba9876543210fedcba98\n\
# branch.head main\n\
u UU N... 100644 100644 100644 100644 1111111 2222222 3333333 README.md\n";
    assert_eq!(
        parse_porcelain_v2(conflict),
        ParsedGitStatus {
            branch: "main".to_string(),
            dirty: true,
            has_conflict: true,
            ahead: 0,
            behind: 0,
        }
    );

    // 4. Initial unborn repository (`# branch.oid (initial)`)
    let unborn_named = b"# branch.oid (initial)\n# branch.head main\n? README.md\n";
    assert_eq!(
        parse_porcelain_v2(unborn_named),
        ParsedGitStatus {
            branch: "main".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        }
    );

    // 5. Detached HEAD with commit SHA (`D07` shape -> 7-char short SHA) and unborn detached (`HEAD`)
    let detached_sha =
        b"# branch.oid fb7464b99887766554433221100ffeeddccbbaa9\n# branch.head (detached)\n";
    assert_eq!(
        parse_porcelain_v2(detached_sha),
        ParsedGitStatus {
            branch: "fb7464b".to_string(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        }
    );

    let detached_initial = b"# branch.oid (initial)\n# branch.head (detached)\n";
    assert_eq!(
        parse_porcelain_v2(detached_initial),
        ParsedGitStatus {
            branch: "HEAD".to_string(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        }
    );

    // 6. Emoji & CJK UTF-8 branch names (`D09` & `D12`) + lossy UTF-8 resilience (`DIV-13`)
    let cjk_branch = "# branch.oid 1234567890abcdef\n# branch.head feat/中文分支\n".as_bytes();
    assert_eq!(parse_porcelain_v2(cjk_branch).branch, "feat/中文分支");

    let emoji_branch =
        "# branch.oid 1234567890abcdef\n# branch.head aaaaaaaaaaaaaaaaaaaaaa🚀b\n".as_bytes();
    assert_eq!(
        parse_porcelain_v2(emoji_branch).branch,
        "aaaaaaaaaaaaaaaaaaaaaa🚀b"
    );

    let mut non_utf8 = b"# branch.oid 1234567890abcdef\n# branch.head main\n? bad_".to_vec();
    non_utf8.push(0xff);
    non_utf8.extend_from_slice(b".bin\n");
    let lossy_parsed = parse_porcelain_v2(&non_utf8);
    assert_eq!(lossy_parsed.branch, "main");
    assert!(lossy_parsed.dirty);
}

// Cache identity is the normalized cwd plus the embedded cwd check: an untrusted or wrong-format
// file is a miss, never a panic, and the next atomic write heals it.
#[test]
fn git_cache_store_normalizes_lexical_cwd_verifies_embedded_key_and_self_heals() {
    // Pure lexical normalization: preserve roots, strip trailing separators on subpaths
    assert_eq!(normalize_cwd_lexical("D:\\"), "D:\\");
    assert_eq!(normalize_cwd_lexical("D:\\\\"), "D:\\");
    assert_eq!(normalize_cwd_lexical("C:/"), "C:/");
    assert_eq!(normalize_cwd_lexical("/"), "/");
    assert_eq!(normalize_cwd_lexical("///"), "/");
    assert_eq!(
        normalize_cwd_lexical("\\\\server\\share"),
        "\\\\server\\share"
    );
    assert_eq!(
        normalize_cwd_lexical("\\\\server\\share\\"),
        "\\\\server\\share"
    );
    assert_eq!(
        normalize_cwd_lexical("\\\\wsl.localhost\\Ubuntu\\home\\dev\\repo\\"),
        "\\\\wsl.localhost\\Ubuntu\\home\\dev\\repo"
    );
    assert_eq!(
        normalize_cwd_lexical("D:\\Work\\agyline\\\\"),
        "D:\\Work\\agyline"
    );
    assert_eq!(
        normalize_cwd_lexical("/Users/dev/agyline/"),
        "/Users/dev/agyline"
    );

    // Deterministic 64-bit FNV-1a known vector ("foobar" -> 85944171f73967e8)
    assert_eq!(cwd_hash_hex("foobar"), "85944171f73967e8");

    let sandbox = TempDirGuard::new("cache_store");
    let home = &sandbox.path;

    let entry = GitCacheEntry {
        version: 1,
        cwd: normalize_cwd_lexical("D:\\Work\\agyline\\"),
        updated_at_ms: 1_700_000_000_500,
        is_repo: true,
        branch: "main".to_string(),
        dirty: true,
        has_conflict: false,
        ahead: 3,
        behind: 1,
    };
    write_git_cache(home, &entry).expect("atomic write git cache");

    // Trailing separator variants resolve to the exact same cache file and pass embedded cwd check
    assert_eq!(
        read_git_cache(home, "D:\\Work\\agyline"),
        Some(entry.clone())
    );
    assert_eq!(
        read_git_cache(home, "D:\\Work\\agyline\\\\"),
        Some(entry.clone())
    );

    let cache_file = git_cache_path(home, "D:\\Work\\agyline").expect("cache path");
    assert!(cache_file.is_file());

    // No leftover .tmp. files after atomic rename
    let git_dir = home.join("state").join("git");
    for dir_entry in fs::read_dir(&git_dir).expect("read git dir") {
        let name = dir_entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .to_string();
        assert!(!name.contains(".tmp."), "unexpected temp leftover: {name}");
    }

    // Embedded cwd mismatch (e.g. hash collision) must be treated as miss
    let colliding = serde_json::json!({
        "version": 1,
        "cwd": "D:\\Other\\different_cwd",
        "updated_at_ms": 1_700_000_000_500_u64,
        "is_repo": true,
        "branch": "hacked",
        "dirty": false,
        "has_conflict": false,
        "ahead": 0,
        "behind": 0
    });
    fs::write(&cache_file, colliding.to_string()).expect("write mismatch");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), None);

    // Wrong version, missing required fields, and truncated JSON must all return None
    let wrong_ver = serde_json::json!({
        "version": 2,
        "cwd": "D:\\Work\\agyline",
        "updated_at_ms": 1_700_000_000_500_u64,
        "is_repo": true,
        "branch": "main",
        "dirty": false,
        "has_conflict": false,
        "ahead": 0,
        "behind": 0
    });
    fs::write(&cache_file, wrong_ver.to_string()).expect("write v2");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), None);

    let missing_branch = serde_json::json!({
        "version": 1,
        "cwd": "D:\\Work\\agyline",
        "updated_at_ms": 1_700_000_000_500_u64,
        "is_repo": true
    });
    fs::write(&cache_file, missing_branch.to_string()).expect("write missing branch");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), None);

    fs::write(&cache_file, b"{\"version\":1,\"cwd\":\"truncated").expect("write truncated");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), None);

    // Minimal negative cache JSON (`is_repo: false`) parses cleanly
    let minimal_neg = serde_json::json!({
        "version": 1,
        "cwd": "D:\\Work\\agyline",
        "updated_at_ms": 1_700_000_001_000_u64,
        "is_repo": false
    });
    fs::write(&cache_file, minimal_neg.to_string()).expect("write neg");
    let parsed_neg = read_git_cache(home, "D:\\Work\\agyline").expect("neg hit");
    assert!(!parsed_neg.is_repo);
    assert!(parsed_neg.branch.is_empty());

    // Atomic overwrite self-heals broken file
    fs::write(&cache_file, b"not json at all").expect("corrupt");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), None);
    write_git_cache(home, &entry).expect("self-heal write");
    assert_eq!(read_git_cache(home, "D:\\Work\\agyline"), Some(entry));
}

// Sweeping is bounded work: at most one scan per 24h, and each artifact class keeps its own TTL
// (sessions 7d, git cache 24h, tmp/lock 10m) so a live worker's files survive.
#[test]
fn stale_state_sweep_throttles_to_24h_and_cleans_only_expired_artifacts() {
    let sandbox = TempDirGuard::new("sweep");
    let home = &sandbox.path;
    let state_dir = home.join("state");
    let sessions_dir = state_dir.join("sessions");
    let git_dir = state_dir.join("git");
    let locks_dir = state_dir.join("locks");
    fs::create_dir_all(&sessions_dir).expect("mkdir sessions");
    fs::create_dir_all(&git_dir).expect("mkdir git");
    fs::create_dir_all(&locks_dir).expect("mkdir locks");

    let created_wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_millis() as u64;

    // Seed files created at `created_wall_ms`:
    // Suppose sweep runs at `sweep_now = created_wall_ms + 8 days`? Wait: let's test both fresh and expired
    // simultaneously in a single sweep!
    // For git/*.json and locks/*.lock, embedded timestamps can represent different ages, and for sessions/*.json
    // and *.tmp.*, OS mtime determines age. Let's check mtime-based expiry cleanly!
    let old_git = git_dir.join("old.json");
    let fresh_git = git_dir.join("fresh.json");
    let sweep_t1 = created_wall_ms + 8 * 24 * 60 * 60 * 1000; // +8 days from file creation mtime!

    // File whose mtime is `created_wall_ms` (8 days before `sweep_t1`):
    let old_session = sessions_dir.join("old-session.json");
    fs::write(&old_session, b"{\"version\":1}").expect("write old session");
    let old_tmp = git_dir.join("abc.json.tmp.999.123");
    fs::write(&old_tmp, b"partial").expect("write old tmp");
    let old_lock = locks_dir.join("git-old.lock");
    fs::write(&old_lock, format!("{created_wall_ms} 999\n")).expect("write old lock");
    let fresh_lock = locks_dir.join("git-fresh.lock");
    fs::write(&fresh_lock, format!("{} 999\n", sweep_t1 - 60_000)).expect("write fresh lock");

    fs::write(
        &old_git,
        serde_json::json!({
            "version": 1,
            "cwd": "D:\\old",
            "updated_at_ms": sweep_t1 - (25 * 60 * 60 * 1000),
            "is_repo": false
        })
        .to_string(),
    )
    .expect("write old git");
    fs::write(
        &fresh_git,
        serde_json::json!({
            "version": 1,
            "cwd": "D:\\fresh",
            "updated_at_ms": sweep_t1 - (2 * 60 * 60 * 1000),
            "is_repo": false
        })
        .to_string(),
    )
    .expect("write fresh git");

    // First sweep at `sweep_t1` runs (no `last_sweep` yet)
    maybe_sweep_stale_state(home, sweep_t1);

    assert!(!old_session.exists(), "session > 7d must be swept");
    assert!(!old_git.exists(), "git cache > 24h must be swept");
    assert!(fresh_git.exists(), "git cache < 24h must be retained");
    assert!(!old_tmp.exists(), ".tmp. > 10m must be swept");
    assert!(!old_lock.exists(), ".lock > 10m must be swept");
    assert!(fresh_lock.exists(), ".lock < 10m must be retained");
    assert!(state_dir.join("last_sweep").is_file());

    // Plant another expired temp file, then run sweep only +1 hour later (< 24h throttle): must NOT scan!
    let throttled_tmp = git_dir.join("throttled.json.tmp.999.456");
    fs::write(&throttled_tmp, b"partial").expect("write throttled tmp");
    maybe_sweep_stale_state(home, sweep_t1 + 60 * 60 * 1000);
    assert!(
        throttled_tmp.exists(),
        "sweep must be throttled for 24h after last_sweep"
    );

    // Advance >= 24h past `sweep_t1`: sweep runs again and cleans `throttled_tmp`!
    maybe_sweep_stale_state(home, sweep_t1 + 24 * 60 * 60 * 1000);
    assert!(
        !throttled_tmp.exists(),
        "sweep must run once 24h has elapsed since last_sweep"
    );
}

/// Records `spawn_refresh_git` requests instead of launching processes, so a test can assert the
/// print path only asked for work. Relay touches are out of scope here and stay a no-op.
#[derive(Default)]
struct RecordingSpawner {
    git_cwds: Mutex<Vec<String>>,
}

impl RecordingSpawner {
    /// Drains the log so each assertion counts only the spawns caused by the frame under test.
    fn take_git_spawns(&self) -> Vec<String> {
        std::mem::take(&mut *self.git_cwds.lock().expect("lock spawner"))
    }
}

impl Spawner for RecordingSpawner {
    fn spawn_refresh_git(&self, cwd: &str) {
        self.git_cwds
            .lock()
            .expect("lock spawner")
            .push(cwd.to_string());
    }

    fn spawn_touch(&self, _session_id: &str, _relay_url: &str) {}
}

// One cache entry walked through fresh, SWR, expired and negative ages: a single frame cannot
// show that both the served text and the spawn count depend on age and on `delta_out`.
#[test]
fn git_cache_lifecycle_swr_negative_cache_and_vcs_merge_sequence() {
    let sandbox = TempDirGuard::new("git_seq");
    let home = &sandbox.path;
    let spawner = RecordingSpawner::default();

    // Non-existent workspace directory proves zero workspace disk access on the print path!
    let repo_cwd = "D:\\VirtualWorkspace\\clean-repo";
    let payload_bytes = serde_json::to_vec(&serde_json::json!({
        "session_id": "",
        "cwd": repo_cwd
    }))
    .expect("json");

    // Injected clock: cache age comes from `now_ms`, so the lifecycle needs no real sleeps.
    let make_env = |now_ms: u64| RenderEnv {
        home_dir: home.clone(),
        temp_dir: home.join("temp"),
        conversations_dir: home.join("conversations"),
        env_vars: BTreeMap::new(),
        now_ms,
        spawner: &spawner,
    };

    // 1. Cold miss (even in Pre-session): omits Git item and spawns 1 refresh
    let out_cold = run_live_frame(&payload_bytes, &make_env(10_000));
    assert_eq!(
        out_cold.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.take_git_spawns(), vec![repo_cwd.to_string()]);

    // Seed positive cache at t = 10_000
    write_git_cache(
        home,
        &GitCacheEntry {
            version: 1,
            cwd: repo_cwd.to_string(),
            updated_at_ms: 10_000,
            is_repo: true,
            branch: "main".to_string(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        },
    )
    .expect("seed positive cache");

    // 2. Warm hit (< 2000 ms, t = 11_999): renders Git item and spawns 0 refreshes
    let out_warm = run_live_frame(&payload_bytes, &make_env(11_999));
    assert_eq!(
        out_warm.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f02a2} main \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert!(spawner.take_git_spawns().is_empty());

    // 3. Fresh cache (< 2000 ms) WITH delta_out > 0: serves cached Git AND triggers 1 SWR refresh!
    let parsed_payload = parse_stdin_payload(&payload_bytes).expect("parse");
    let cfg = EffectiveConfig::default();
    let delta_git = resolve_git_frame(&parsed_payload, &cfg, &make_env(11_500), 25);
    assert_eq!(
        delta_git,
        Some(GitFrameState {
            branch: "main".to_string(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        })
    );
    assert_eq!(spawner.take_git_spawns(), vec![repo_cwd.to_string()]);

    // 4. Stale-while-revalidate (2000 ms ..= 600_000 ms): serves cached Git AND spawns 1 refresh
    for swr_now in [12_000_u64, 300_000_u64, 610_000_u64] {
        let out_swr = run_live_frame(&payload_bytes, &make_env(swr_now));
        assert_eq!(
            out_swr.plain,
            "  \u{ef23} Model | \u{f024b} clean-repo | \u{f02a2} main \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n",
            "SWR at now_ms={swr_now} must still render cached Git"
        );
        assert_eq!(
            spawner.take_git_spawns(),
            vec![repo_cwd.to_string()],
            "SWR at now_ms={swr_now} must spawn 1 refresh"
        );
    }

    // 5. Expired (> 600_000 ms, t = 610_001): omits Git item AND spawns 1 refresh
    let out_exp = run_live_frame(&payload_bytes, &make_env(610_001));
    assert_eq!(
        out_exp.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.take_git_spawns(), vec![repo_cwd.to_string()]);

    // 6. Negative cache (`is_repo: false`): suppresses repeat spawns while fresh (even if delta_out > 0!)
    write_git_cache(
        home,
        &GitCacheEntry {
            version: 1,
            cwd: repo_cwd.to_string(),
            updated_at_ms: 700_000,
            is_repo: false,
            branch: String::new(),
            dirty: false,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        },
    )
    .expect("seed negative cache");

    let out_neg_fresh = run_live_frame(&payload_bytes, &make_env(701_200));
    assert_eq!(
        out_neg_fresh.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert!(spawner.take_git_spawns().is_empty());

    assert_eq!(
        resolve_git_frame(&parsed_payload, &cfg, &make_env(701_200), 100),
        None
    );
    assert!(
        spawner.take_git_spawns().is_empty(),
        "fresh negative cache must suppress spawn even when delta_out > 0"
    );

    let out_neg_stale = run_live_frame(&payload_bytes, &make_env(702_500));
    assert_eq!(
        out_neg_stale.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.take_git_spawns(), vec![repo_cwd.to_string()]);

    // 7. Per-field merge with payload `vcs.branch` & `vcs.dirty` without short-circuiting refresh
    write_git_cache(
        home,
        &GitCacheEntry {
            version: 1,
            cwd: repo_cwd.to_string(),
            updated_at_ms: 800_000,
            is_repo: true,
            branch: "cached-branch".to_string(),
            dirty: false,
            has_conflict: true,
            ahead: 4,
            behind: 2,
        },
    )
    .expect("seed merge cache");

    let merge_payload = serde_json::to_vec(&serde_json::json!({
        "session_id": "s_merge",
        "cwd": repo_cwd,
        "vcs": {
            "branch": "payload-override",
            "dirty": false
        }
    }))
    .expect("json");

    // At t = 805_000 (SWR): payload branch wins, conflict/ahead/behind come from cache, AND refresh still spawns!
    let out_merge_swr = run_live_frame(&merge_payload, &make_env(805_000));
    assert_eq!(
        out_merge_swr.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f02a2} payload-override \u{26a0} \u{2191}4 \u{2193}2 | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(
        spawner.take_git_spawns(),
        vec![repo_cwd.to_string()],
        "vcs.branch must NEVER short-circuit background Git refresh"
    );

    // Expired cache + payload `vcs.branch`: renders payload branch cleanly while dropping stale conflict/ahead/behind
    let out_merge_expired = run_live_frame(&merge_payload, &make_env(1_500_000));
    assert_eq!(
        out_merge_expired.plain,
        "  \u{ef23} Model | \u{f024b} clean-repo | \u{f02a2} payload-override \u{2713} | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.take_git_spawns(), vec![repo_cwd.to_string()]);
}

/// Real repo with one commit on `branch`: worker cases need genuine git output, and an unborn
/// repo would report `(initial)` instead of a branch name.
fn init_real_git_repo(repo_dir: &Path, branch: &str) {
    fs::create_dir_all(repo_dir).expect("create repo dir");
    let run_git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_dir)
            .args(args)
            .output()
            .expect("invoke git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&["init", "-b", branch]);
    run_git(&["config", "user.email", "test@agyline.local"]);
    run_git(&["config", "user.name", "agyline-test"]);
    fs::write(repo_dir.join("README.md"), b"hello\n").expect("write readme");
    run_git(&["add", "README.md"]);
    run_git(&["commit", "-m", "initial"]);
}

/// Directory link the host can create unprivileged: a junction on Windows, a symlink on Unix.
fn create_dir_link(link_path: &Path, target_dir: &Path) {
    let out = if cfg!(windows) {
        Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link_path)
            .arg(target_dir)
            .output()
            .expect("mklink /J")
    } else {
        Command::new("ln")
            .arg("-s")
            .arg(target_dir)
            .arg(link_path)
            .output()
            .expect("ln -s")
    };
    assert!(
        out.status.success(),
        "directory link failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// Only the detached worker resolves the filesystem: it must find the repo behind a link or a
// worktree `.git` file, while the rendered `dir` item keeps the literal cwd leaf.
#[test]
fn refresh_git_worker_resolves_junction_or_symlink_dash_cwd_and_negative_cache() {
    let sandbox = TempDirGuard::new("junction_d08");
    let home_dir = sandbox.path.join("agyline_home");
    let real_repo = sandbox.path.join("real_repo");
    init_real_git_repo(&real_repo, "main");
    let sub_pkg = real_repo.join("packages").join("inner");
    fs::create_dir_all(&sub_pkg).expect("create sub_pkg");

    // Outer non-Git directory containing a junction/symlink `linked_pkg` -> `real_repo/packages/inner` (`D08` / `DIV-10`)
    let outer_plain = sandbox.path.join("outer_plain");
    fs::create_dir_all(&outer_plain).expect("create outer_plain");
    let linked_pkg = outer_plain.join("linked_pkg");
    create_dir_link(&linked_pkg, &sub_pkg);

    let linked_cwd_str = linked_pkg.to_string_lossy().to_string();
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": "probe_d08",
        "cwd": linked_cwd_str
    }))
    .expect("json");

    let spawner = RecordingSpawner::default();
    let now_before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_millis() as u64;

    // Cold frame before worker runs: shows literal leaf `linked_pkg`, omits Git, requests 1 background refresh
    let env_cold = RenderEnv {
        home_dir: home_dir.clone(),
        temp_dir: sandbox.path.join("temp"),
        conversations_dir: sandbox.path.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: now_before,
        spawner: &spawner,
    };
    let cold_out = run_live_frame(&payload, &env_cold);
    assert_eq!(
        cold_out.plain,
        "  \u{ef23} Model | \u{f024b} linked_pkg | \u{f49b} 0% | \u{f1c0} cold\n"
    );
    assert_eq!(spawner.take_git_spawns(), vec![linked_cwd_str.clone()]);

    // Run real compiled `agyline __refresh-git --cwd <linked_pkg>`
    let bin = env!("CARGO_BIN_EXE_agyline");
    let worker_out = Command::new(bin)
        .arg("__refresh-git")
        .arg("--cwd")
        .arg(&linked_pkg)
        .env("AGYLINE_HOME", &home_dir)
        .output()
        .expect("run __refresh-git");
    // Worker contract: exit 0 and stay silent, so it can never pollute a printed status line.
    assert_eq!(worker_out.status.code(), Some(0));
    assert!(worker_out.stdout.is_empty());
    assert!(worker_out.stderr.is_empty());

    // Lock must be cleaned up on completion and `last_sweep` recorded
    let lock_file = git_lock_path(&home_dir, &linked_cwd_str).expect("lock path");
    assert!(!lock_file.exists(), "worker must remove its lock on exit");
    assert!(home_dir.join("state").join("last_sweep").is_file());

    // Warm frame after worker finishes: `dir` shows literal `linked_pkg` while `git` shows resolved `main ✓` (`D08`)!
    let cached = read_git_cache(&home_dir, &linked_cwd_str).expect("cached entry must exist");
    assert!(cached.is_repo);
    assert_eq!(cached.branch, "main");
    assert!(!cached.dirty);

    let env_warm = RenderEnv {
        home_dir: home_dir.clone(),
        temp_dir: sandbox.path.join("temp"),
        conversations_dir: sandbox.path.join("conv"),
        env_vars: BTreeMap::new(),
        now_ms: cached.updated_at_ms + 200,
        spawner: &NoopSpawner,
    };
    let warm_out = run_live_frame(&payload, &env_warm);
    assert_eq!(
        warm_out.ansi,
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m linked_pkg\x1b[0;37m | \x1b[0;94m\u{f02a2} main \u{2713}\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
    );

    // Non-repo plain directory (`outer_plain` itself) must write `is_repo: false` negative cache
    let outer_str = outer_plain.to_string_lossy().to_string();
    let neg_out = Command::new(bin)
        .arg("__refresh-git")
        .arg("--cwd")
        .arg(&outer_plain)
        .env("AGYLINE_HOME", &home_dir)
        .output()
        .expect("run __refresh-git on non-repo");
    assert_eq!(neg_out.status.code(), Some(0));
    assert!(neg_out.stdout.is_empty());
    assert!(neg_out.stderr.is_empty());
    let neg_cached = read_git_cache(&home_dir, &outer_str).expect("negative cache entry");
    assert!(!neg_cached.is_repo);

    // Relative `<dir>` starting with `-` (`-dash-repo`) passed after `--cwd` preserves exact OsString!
    let dash_parent = sandbox.path.join("dash_parent");
    let dash_repo = dash_parent.join("-dash-repo");
    init_real_git_repo(&dash_repo, "feat/dash");
    let dash_out = Command::new(bin)
        .current_dir(&dash_parent)
        .arg("__refresh-git")
        .arg("--cwd")
        .arg("-dash-repo")
        .env("AGYLINE_HOME", &home_dir)
        .output()
        .expect("run __refresh-git on -dash-repo");
    assert_eq!(dash_out.status.code(), Some(0));
    assert!(dash_out.stdout.is_empty());
    assert!(dash_out.stderr.is_empty());
    let dash_cached = read_git_cache(&home_dir, "-dash-repo").expect("dash repo cache entry");
    assert!(dash_cached.is_repo);
    assert_eq!(dash_cached.branch, "feat/dash");

    // Worktree `.git` regular file (not directory) upward discovery from nested subdirectory
    let wt_dir = sandbox.path.join("worktree_leaf");
    let wt_add = Command::new("git")
        .arg("-C")
        .arg(&real_repo)
        .args(["worktree", "add", "-b", "wt/feature"])
        .arg(&wt_dir)
        .output()
        .expect("git worktree add");
    assert!(
        wt_add.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&wt_add.stderr)
    );
    assert!(
        wt_dir.join(".git").is_file(),
        "worktree .git must be a file"
    );
    let wt_nested = wt_dir.join("nested_sub");
    fs::create_dir_all(&wt_nested).expect("mkdir wt_nested");
    let wt_nested_str = wt_nested.to_string_lossy().to_string();
    let wt_out = Command::new(bin)
        .arg("__refresh-git")
        .arg("--cwd")
        .arg(&wt_nested)
        .env("AGYLINE_HOME", &home_dir)
        .output()
        .expect("run __refresh-git on worktree subdir");
    assert_eq!(wt_out.status.code(), Some(0));
    let wt_cached = read_git_cache(&home_dir, &wt_nested_str).expect("worktree cache entry");
    assert!(wt_cached.is_repo);
    assert_eq!(wt_cached.branch, "wt/feature");
}

// Lock ownership, not just existence: a live foreign lock blocks the refresh and must survive,
// and only a lock this worker owns may be removed on exit.
#[test]
fn refresh_git_worker_enforces_30s_lock_takeover_and_ownership() {
    let sandbox = TempDirGuard::new("lock_takeover");
    let home_dir = sandbox.path.join("home");
    let repo_dir = sandbox.path.join("repo");
    init_real_git_repo(&repo_dir, "main");
    let repo_str = repo_dir.to_string_lossy().to_string();

    let lock_path = git_lock_path(&home_dir, &repo_str).expect("lock path");
    fs::create_dir_all(lock_path.parent().expect("locks dir")).expect("mkdir locks");

    // 1. Fresh lock held by another worker (< 30_000 ms): must skip without touching the foreign lock!
    let foreign_marker = "100000 999999\n";
    fs::write(&lock_path, foreign_marker).expect("plant active lock");
    refresh_git_at(
        &home_dir,
        OsStr::new(&repo_str),
        120_000, // age = 20_000 ms < 30_000 ms
        Duration::from_secs(5),
    );
    assert_eq!(
        read_git_cache(&home_dir, &repo_str),
        None,
        "must not refresh while active lock is held"
    );
    assert_eq!(
        fs::read_to_string(&lock_path).expect("foreign lock intact"),
        foreign_marker,
        "must never delete another live worker's lock"
    );

    // 2. Stale lock (>= 30_000 ms): must take over, refresh cache, and release its own lock on exit!
    refresh_git_at(
        &home_dir,
        OsStr::new(&repo_str),
        130_000, // age = 30_000 ms >= 30_000 ms takeover threshold!
        Duration::from_secs(5),
    );
    let cached =
        read_git_cache(&home_dir, &repo_str).expect("stale lock takeover must write cache");
    assert!(cached.is_repo);
    assert_eq!(cached.branch, "main");
    assert_eq!(cached.updated_at_ms, 130_000);
    assert!(
        !lock_path.exists(),
        "taken-over lock owned by current worker must be removed on completion"
    );
}

// The performance contract end to end: a 3s git must neither delay the printed line nor hold its
// pipe open, and the recorded PIDs prove the worker really ran, so a pass is never vacuous.
#[test]
fn pipe_eof_zero_handle_leak_returns_under_800ms_when_fake_git_sleeps_3s() {
    let sandbox = TempDirGuard::new("pipe_eof_slow_git");
    let fake_bin_dir = sandbox.path.join("fake_bin");
    let pid_dir = sandbox.path.join("pids");
    let home_dir = sandbox.path.join("agyline_home");
    fs::create_dir_all(&fake_bin_dir).expect("mkdir fake_bin");
    fs::create_dir_all(&pid_dir).expect("mkdir pids");

    // Compile a standalone fake `git` binary that records its PID and sleeps 3 seconds.
    let fake_src = sandbox.path.join("fake_git.rs");
    fs::write(
        &fake_src,
        r#"fn main() {
    if let Ok(dir) = std::env::var("AGYLINE_TEST_PID_DIR") {
        let pid = std::process::id();
        let _ = std::fs::write(format!("{dir}/{pid}.pid"), pid.to_string());
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
}
"#,
    )
    .expect("write fake_git.rs");

    let fake_git_bin = if cfg!(windows) {
        fake_bin_dir.join("git.exe")
    } else {
        fake_bin_dir.join("git")
    };
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compile_out = Command::new(rustc)
        .arg(&fake_src)
        .arg("-O")
        .arg("-o")
        .arg(&fake_git_bin)
        .output()
        .expect("compile fake git with rustc");
    assert!(
        compile_out.status.success(),
        "rustc fake_git failed: {}",
        String::from_utf8_lossy(&compile_out.stderr)
    );

    // The worker inherits this PATH, so the fake git must win lookup over the real one.
    let mut path_entries = vec![fake_bin_dir.clone()];
    if let Some(existing_path) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&existing_path));
    }
    let test_path = std::env::join_paths(path_entries).expect("join PATH");

    // 1. Cold repo with `.git` directory: `agyline` spawns detached `__refresh-git`, which invokes 3s fake `git`.
    let cold_repo = sandbox.path.join("cold_slow_repo");
    fs::create_dir_all(cold_repo.join(".git")).expect("mkdir cold .git");
    let cold_payload = serde_json::to_vec(&serde_json::json!({
        "session_id": "eof_cold",
        "cwd": cold_repo.to_string_lossy()
    }))
    .expect("json");

    let bin = env!("CARGO_BIN_EXE_agyline");
    let t0 = Instant::now();
    let mut child = Command::new(bin)
        .env("AGYLINE_HOME", &home_dir)
        .env("PATH", &test_path)
        .env("AGYLINE_TEST_PID_DIR", &pid_dir)
        .env_remove("AGY_THEME")
        .env_remove("AGY_STATUSLINE_LINES")
        .env_remove("AGY_STATUSLINE_INDENT")
        .env_remove("AGY_SHOW_DEFAULT_MODE")
        .env_remove("AGY_CACHE_RELAY")
        .env_remove("COLORFGBG")
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .env_remove("TERM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agyline cold");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        stdin.write_all(&cold_payload).expect("write stdin");
    }
    let cold_output = child.wait_with_output().expect("wait agyline cold");
    let cold_elapsed = t0.elapsed();

    assert_eq!(cold_output.status.code(), Some(0));
    assert!(cold_output.stderr.is_empty(), "stderr must be 0 B");
    assert_eq!(
        String::from_utf8(cold_output.stdout).expect("utf8"),
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m cold_slow_repo\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
    );
    assert!(
        cold_elapsed < Duration::from_millis(800),
        "cold render must return pipe EOF in < 800 ms without waiting 3 s for background git, took {cold_elapsed:?}"
    );

    // 2. Stale SWR repo (`age > 2s`): serves cached `main ●` immediately while detached worker runs 3s fake `git`.
    let stale_repo = sandbox.path.join("stale_slow_repo");
    fs::create_dir_all(stale_repo.join(".git")).expect("mkdir stale .git");
    let stale_cwd_str = stale_repo.to_string_lossy().to_string();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("epoch")
        .as_millis() as u64;
    write_git_cache(
        &home_dir,
        &GitCacheEntry {
            version: 1,
            cwd: stale_cwd_str.clone(),
            updated_at_ms: now_ms - 15_000,
            is_repo: true,
            branch: "main".to_string(),
            dirty: true,
            has_conflict: false,
            ahead: 1,
            behind: 0,
        },
    )
    .expect("seed stale cache");

    let stale_payload = serde_json::to_vec(&serde_json::json!({
        "session_id": "eof_stale",
        "cwd": stale_cwd_str
    }))
    .expect("json");

    let t1 = Instant::now();
    // agy runs the line through a host shell; that wrapper must not keep the pipe open either.
    let mut stale_cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/d", "/s", "/c", bin]);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(format!("'{}'", bin.replace('\'', "'\\''")));
        c
    };
    let mut stale_child = stale_cmd
        .env("AGYLINE_HOME", &home_dir)
        .env("PATH", &test_path)
        .env("AGYLINE_TEST_PID_DIR", &pid_dir)
        .env_remove("AGY_THEME")
        .env_remove("AGY_STATUSLINE_LINES")
        .env_remove("AGY_STATUSLINE_INDENT")
        .env_remove("AGY_SHOW_DEFAULT_MODE")
        .env_remove("AGY_CACHE_RELAY")
        .env_remove("COLORFGBG")
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .env_remove("TERM")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agyline stale via host shell wrapper");
    {
        let mut stdin = stale_child.stdin.take().expect("stdin");
        stdin.write_all(&stale_payload).expect("write stdin");
    }
    let stale_output = stale_child.wait_with_output().expect("wait agyline stale");
    let stale_elapsed = t1.elapsed();

    assert_eq!(stale_output.status.code(), Some(0));
    assert!(stale_output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(stale_output.stdout).expect("utf8"),
        "  \x1b[0m\x1b[0;96m\u{ef23} Model\x1b[0;37m | \x1b[0;93m\u{f024b}\x1b[0;92m stale_slow_repo\x1b[0;37m | \x1b[0;94m\u{f02a2} main \u{25cf} \u{2191}1\x1b[0;37m | \x1b[0;95m\u{f49b} 0%\x1b[0;37m | \x1b[0;34m\u{f1c0}\x1b[0;90m cold\x1b[0m\n"
    );
    assert!(
        stale_elapsed < Duration::from_millis(800),
        "stale SWR render must return pipe EOF in < 800 ms without waiting 3 s, took {stale_elapsed:?}"
    );

    // Confirm detached background worker actually launched `fake_git` (non-vacuous check), then clean up sleeping PIDs.
    let wait_pid_start = Instant::now();
    let mut spawned_pids = Vec::new();
    while wait_pid_start.elapsed() < Duration::from_millis(800) {
        if let Ok(entries) = fs::read_dir(&pid_dir) {
            for entry in entries.flatten() {
                if let Ok(txt) = fs::read_to_string(entry.path()) {
                    let trimmed = txt.trim().to_string();
                    if !trimmed.is_empty() && !spawned_pids.contains(&trimmed) {
                        spawned_pids.push(trimmed);
                    }
                }
            }
        }
        if spawned_pids.len() >= 2 {
            break;
        }
        thread::sleep(Duration::from_millis(15));
    }
    assert!(
        !spawned_pids.is_empty(),
        "detached __refresh-git must have spawned sleeping fake_git in background"
    );

    for pid in &spawned_pids {
        if cfg!(windows) {
            let _ = Command::new("taskkill").args(["/F", "/PID", pid]).output();
        } else {
            let _ = Command::new("kill").args(["-9", pid]).output();
        }
    }
    thread::sleep(Duration::from_millis(50));
}
