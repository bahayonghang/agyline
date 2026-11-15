//! Install/uninstall contracts against a sandboxed `settings.json`: the command string must be
//! safe for the host shell, sibling keys and their order survive, a symlink stays a symlink, and
//! the prior `statusLine` is snapshotted so uninstall can restore it. Plans are pure; only
//! `apply_*` and the CLI write, and `--dry-run` writes nothing.

use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agyline::{
    CliStreams, HookStatus, InstallEnv, InstallError, InstallPlan, PlatformKind, UninstallOptions,
    UninstallPlan, apply_install, apply_uninstall, create_file_symlink, inspect_hook, plan_install,
    plan_uninstall, resolve_command_string, run_cli,
};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

/// Throwaway root for one case, removed on drop: no test here may touch the developer's real
/// `~/.gemini`.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "agyline_install_test_{}_{}_{}_{}",
            std::process::id(),
            nanos,
            id,
            label
        ));
        fs::create_dir_all(&root).expect("create sandbox dir");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// InstallEnv aimed at sandbox paths, with the platform and the 8.3 resolver injected so Windows
/// command rules can be exercised on any host.
fn make_env(
    sandbox: &Path,
    platform: PlatformKind,
    user_home: Option<PathBuf>,
    exe_path: PathBuf,
    path_env: Option<OsString>,
    short_resolver: impl Fn(&Path) -> Option<PathBuf> + Send + Sync + 'static,
) -> InstallEnv {
    InstallEnv {
        settings_path: sandbox
            .join("home")
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json"),
        agyline_home: sandbox.join("agyline-home"),
        user_home,
        exe_path,
        path_env,
        platform,
        short_path_resolver: Arc::new(short_resolver),
    }
}

// A cmd-safe path needs no quoting: in-home becomes `~/...` and separators normalize to `/` so
// cmd.exe sees a single token.
#[test]
fn windows_command_resolves_clean_home_and_non_home_paths_with_forward_slashes() {
    let sb = Sandbox::new("win_clean");
    let home = PathBuf::from(r"C:\Users\alice");

    let in_home = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(home.clone()),
        PathBuf::from(r"C:\Users\alice\.cargo\bin\agyline.exe"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&in_home).expect("clean in-home path"),
        "~/.cargo/bin/agyline.exe"
    );

    // Outside the home prefix there is no `~` form; the absolute path must still be cmd-safe.
    let outside_home = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(home),
        PathBuf::from(r"D:\tools\agyline\agyline.exe"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&outside_home).expect("clean outside-home path"),
        "D:/tools/agyline/agyline.exe"
    );
}

// Non-ASCII is not a cmd metacharacter: a Unicode path outside home is written through unchanged
// rather than mangled into an 8.3 alias.
#[test]
fn windows_command_resolves_unrelated_unicode_executable_path() {
    let sb = Sandbox::new("win_unicode");
    let env = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(PathBuf::from("C:/Users/name")),
        PathBuf::from("D:/中文中文/agyline.exe"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&env).expect("outside-home Unicode path"),
        "D:/中文中文/agyline.exe"
    );
}

// With spaces in the path, bare `agyline` is only safe when PATH resolves to this same
// executable; a namesake earlier on PATH would launch a different program.
#[test]
fn windows_command_with_spaces_uses_bare_agyline_when_path_resolves_to_same_binary() {
    let sb = Sandbox::new("win_space_path");
    let user_home = sb.path().join("Users").join("John Doe");
    let bin_dir = user_home.join(".cargo").join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    let exe_path = bin_dir.join("agyline.exe");
    fs::write(&exe_path, b"stub-binary").expect("write stub exe");

    let other_dir = sb.path().join("other");
    fs::create_dir_all(&other_dir).expect("create other dir");
    // The first PATH entry holds no agyline, so the bare name still lands on the target binary.
    let path_env = OsString::from(format!("{};{}", other_dir.display(), bin_dir.display()));

    let env = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(user_home),
        exe_path,
        Some(path_env),
        |_| Some(PathBuf::from(r"C:\Users\JOHNDO~1\.cargo\bin\agyline.exe")),
    );
    assert_eq!(
        resolve_command_string(&env).expect("PATH fallback should win first"),
        "agyline"
    );
}

// When PATH would launch a different binary, fall back to the 8.3 short path, which cmd.exe can
// run unquoted even though the real path has spaces.
#[test]
fn windows_command_with_spaces_falls_back_to_short_path_when_path_hits_different_binary_or_misses()
{
    let sb = Sandbox::new("win_space_short");
    let user_home = sb.path().join("Users").join("John Doe");
    let target_dir = user_home.join("My Tools");
    let wrong_dir = sb.path().join("wrong_bin");
    fs::create_dir_all(&target_dir).expect("create target dir");
    fs::create_dir_all(&wrong_dir).expect("create wrong dir");

    let target_exe = target_dir.join("agyline.exe");
    let wrong_exe = wrong_dir.join("agyline.exe");
    fs::write(&target_exe, b"target-bin").expect("write target");
    fs::write(&wrong_exe, b"wrong-bin").expect("write wrong");

    // Earlier PATH entry shadows target_exe with a different binary!
    let shadowed_path = OsString::from(format!("{};{}", wrong_dir.display(), target_dir.display()));

    let env = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(user_home),
        target_exe,
        Some(shadowed_path),
        |_| Some(PathBuf::from(r"C:\Users\JOHNDO~1\MYTOOL~1\agyline.exe")),
    );
    assert_eq!(
        resolve_command_string(&env).expect("8.3 short path fallback"),
        "C:/Users/JOHNDO~1/MYTOOL~1/agyline.exe"
    );
}

// Never write a command cmd.exe would mis-parse: spaces, `&`, `(`, `%` must end in a refusal
// that tells the user to fix PATH, not in a broken hook.
#[test]
fn windows_command_with_spaces_or_metachars_refuses_when_neither_path_nor_short_path_is_clean() {
    let sb = Sandbox::new("win_refuse");
    for dirty_dir in ["Program Files", "team&ops", "dir(1)", "100%bin"] {
        let dir = sb.path().join(dirty_dir);
        fs::create_dir_all(&dir).expect("create dirty dir");
        let exe = dir.join("agyline.exe");
        fs::write(&exe, b"bin").expect("write bin");

        // Simulate NTFS volume with 8.3 disabled (GetShortPathNameW returns original path unchanged).
        let exe_clone = exe.clone();
        let env = make_env(
            sb.path(),
            PlatformKind::Windows,
            Some(PathBuf::from(r"C:\Users\alice")),
            exe,
            Some(OsString::from(r"C:\Windows\System32")),
            move |_| Some(exe_clone.clone()),
        );
        let err = resolve_command_string(&env).expect_err("must refuse broken cmd /c command");
        let msg = err.to_string();
        assert!(
            matches!(err, InstallError::UnsafeCommandPath { .. }),
            "unexpected error variant: {err:?}"
        );
        assert!(
            msg.contains("PATH"),
            "error must guide user on PATH fix, got: {msg}"
        );
    }
}

// Unix quoting: `~` for a clean in-home path, single quotes otherwise, and `'\''` escaping so an
// apostrophe in the path cannot break out of the quoted command.
#[test]
fn unix_command_uses_tilde_for_clean_home_and_single_quotes_with_escaping_otherwise() {
    let sb = Sandbox::new("unix_cmd");
    let clean_home = PathBuf::from("/Users/alice");

    let in_home = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(clean_home.clone()),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&in_home).expect("clean unix home"),
        "~/.cargo/bin/agyline"
    );

    let outside_home = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(clean_home),
        PathBuf::from("/usr/local/bin/agyline"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&outside_home).expect("outside unix home"),
        "'/usr/local/bin/agyline'"
    );

    // Apostrophe and `$` must stay literal inside the single-quoted command.
    let quote_and_space = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/O'Reilly Family")),
        PathBuf::from("/Users/O'Reilly Family/my $tools/agyline"),
        None,
        |_| None,
    );
    assert_eq!(
        resolve_command_string(&quote_and_space).expect("escaped single quotes"),
        r"'/Users/O'\''Reilly Family/my $tools/agyline'"
    );
}

// A missing settings.json gets exactly the three documented keys, planning writes nothing, and
// with no prior statusLine there is nothing to snapshot.
#[test]
fn fresh_install_creates_minimal_three_key_statusline_without_snapshot() {
    let sb = Sandbox::new("fresh_install");
    let home = sb.path().join("alice");
    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/alice")),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );
    let _ = home;

    let plan = plan_install(&env).expect("plan fresh install");
    let diff = match &plan {
        InstallPlan::WouldChange { command, mutation } => {
            assert_eq!(command, "~/.cargo/bin/agyline");
            mutation.diff.clone()
        }
        InstallPlan::AlreadyInstalled { .. } => panic!("expected WouldChange on missing file"),
    };
    assert!(diff.contains("statusLine"));
    assert!(diff.contains("+    \"command\": \"~/.cargo/bin/agyline\""));
    assert!(
        !env.settings_path.exists(),
        "plan_install must not touch disk"
    );

    apply_install(&env, &plan).expect("apply fresh install");
    let written = fs::read_to_string(&env.settings_path).expect("read settings.json");
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("parse written json");
    assert_eq!(
        parsed,
        serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": "~/.cargo/bin/agyline",
                "enabled": true
            }
        })
    );
    assert!(
        !env.agyline_home
            .join("state")
            .join("previous_statusline.json")
            .exists(),
        "fresh install must not write a previous_statusline.json snapshot"
    );

    // Install is idempotent: a second plan must be a no-op, not another mutation.
    let repeat = plan_install(&env).expect("repeat plan");
    assert!(matches!(repeat, InstallPlan::AlreadyInstalled { .. }));
}

// Migrating off the reference script must keep it on disk, keep unrelated keys, order and extra
// statusLine subkeys, and snapshot the original once so uninstall can restore it.
#[test]
fn migrate_from_statusline_ps1_preserves_key_order_extra_subkeys_snapshot_and_restores_on_uninstall()
 {
    let sb = Sandbox::new("migrate_restore");
    let cli_dir = env_settings_parent(sb.path());
    fs::create_dir_all(&cli_dir).expect("create cli dir");
    let legacy_ps1 = cli_dir.join("statusline.ps1");
    fs::write(&legacy_ps1, "# legacy reference script\n").expect("write legacy ps1");

    let initial_json = r#"{
  "colorScheme": "colorblind-friendly dark",
  "model": "Gemini 4 Carbon (High)",
  "statusLine": {
    "type": "command",
    "command": "pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ~/.gemini/antigravity-cli/statusline.ps1",
    "enabled": true,
    "padding": 1
  },
  "trustedWorkspaces": [
    "D:\\Documents\\Code\\Web\\Exp\\agyline"
  ],
  "verbosity": "high"
}
"#;
    let env1 = make_env(
        sb.path(),
        PlatformKind::Windows,
        Some(PathBuf::from(r"C:\Users\alice")),
        PathBuf::from(r"C:\Users\alice\.cargo\bin\agyline.exe"),
        None,
        |_| None,
    );
    fs::write(&env1.settings_path, initial_json).expect("seed settings.json");

    let plan1 = plan_install(&env1).expect("plan1");
    match &plan1 {
        InstallPlan::WouldChange { mutation, .. } => {
            assert!(mutation.diff.contains("statusline.ps1"));
            assert!(mutation.diff.contains("~/.cargo/bin/agyline.exe"));
        }
        InstallPlan::AlreadyInstalled { .. } => panic!("expected migration change"),
    }
    apply_install(&env1, &plan1).expect("apply1");

    // Verify legacy script was NEVER deleted.
    assert!(legacy_ps1.exists(), "must never delete statusline.ps1");

    // Verify exact top-level key order and preserved `padding: 1`.
    let after_install = fs::read_to_string(&env1.settings_path).expect("read after install");
    let root_after: serde_json::Value = serde_json::from_str(&after_install).expect("json");
    let keys_after: Vec<&str> = root_after
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys_after,
        vec![
            "colorScheme",
            "model",
            "statusLine",
            "trustedWorkspaces",
            "verbosity"
        ]
    );
    assert_eq!(root_after["statusLine"]["padding"], serde_json::json!(1));
    assert_eq!(
        root_after["statusLine"]["command"],
        serde_json::json!("~/.cargo/bin/agyline.exe")
    );

    // Snapshot file exists and holds the original statusline.ps1 object.
    let snapshot_path = env1
        .agyline_home
        .join("state")
        .join("previous_statusline.json");
    assert!(snapshot_path.exists());
    let snap_before_reinstall = fs::read_to_string(&snapshot_path).expect("read snapshot");

    // Re-installing from a relocated agyline binary updates command WITHOUT overwriting snapshot!
    let mut env2 = env1.clone();
    env2.exe_path = PathBuf::from(r"D:\bin\agyline.exe");
    let plan2 = plan_install(&env2).expect("plan2");
    assert!(matches!(plan2, InstallPlan::WouldChange { .. }));
    apply_install(&env2, &plan2).expect("apply2");
    assert_eq!(
        fs::read_to_string(&snapshot_path).expect("snapshot intact"),
        snap_before_reinstall,
        "repeat install must NOT overwrite previous_statusline.json"
    );

    // Simulate user modifying another top-level key (`verbosity`: "medium") before uninstalling.
    let modified = fs::read_to_string(&env2.settings_path)
        .expect("read")
        .replace("\"verbosity\": \"high\"", "\"verbosity\": \"medium\"");
    fs::write(&env2.settings_path, modified).expect("write user edit");

    let insp = inspect_hook(&env2);
    assert!(matches!(insp.status, HookStatus::Connected { .. }));
    assert!(insp.has_previous_snapshot);

    // Uninstall restores prior statusLine and removes snapshot, keeping user's `verbosity: "medium"` and key order!
    let uplan = plan_uninstall(&env2, UninstallOptions::default()).expect("plan uninstall");
    assert!(matches!(uplan, UninstallPlan::WouldChange { .. }));
    apply_uninstall(&env2, &uplan).expect("apply uninstall");

    assert!(
        !snapshot_path.exists(),
        "snapshot should be cleaned up after restore"
    );
    let restored_raw = fs::read_to_string(&env2.settings_path).expect("read restored");
    let restored_val: serde_json::Value = serde_json::from_str(&restored_raw).expect("parse");
    let keys_restored: Vec<&str> = restored_val
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys_restored,
        vec![
            "colorScheme",
            "model",
            "statusLine",
            "trustedWorkspaces",
            "verbosity"
        ]
    );
    assert_eq!(restored_val["verbosity"], serde_json::json!("medium"));
    assert_eq!(restored_val["statusLine"]["padding"], serde_json::json!(1));
    assert!(
        restored_val["statusLine"]["command"]
            .as_str()
            .expect("cmd")
            .contains("statusline.ps1")
    );
}

// `--reset` drops the statusLine key without reordering the rest; `--purge` removes AGYLINE_HOME
// including config.toml, so the two scopes stay distinguishable.
#[test]
fn uninstall_reset_and_purge_remove_statusline_preserving_remaining_key_order_and_clean_home() {
    let sb = Sandbox::new("reset_purge");
    let cli_dir = env_settings_parent(sb.path());
    fs::create_dir_all(&cli_dir).expect("create cli dir");

    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/alice")),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );
    let initial = r#"{
  "colorScheme": "dark",
  "model": "Gemini 4",
  "statusLine": {
    "type": "command",
    "command": "python3 ~/.gemini/antigravity-cli/statusline.py",
    "enabled": true
  },
  "trustedWorkspaces": ["/work"],
  "verbosity": "high"
}
"#;
    fs::write(&env.settings_path, initial).expect("seed");

    let iplan = plan_install(&env).expect("plan install");
    apply_install(&env, &iplan).expect("apply install");

    // Also create a config.toml inside AGYLINE_HOME to verify --purge deletes everything.
    fs::create_dir_all(&env.agyline_home).expect("agyline home");
    fs::write(env.agyline_home.join("config.toml"), "version = 1\n").expect("write config.toml");

    let uplan = plan_uninstall(
        &env,
        UninstallOptions {
            dry_run: false,
            yes: true,
            reset: true,
            purge: true,
        },
    )
    .expect("plan uninstall --reset --purge");
    apply_uninstall(&env, &uplan).expect("apply uninstall --reset --purge");

    let after_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&env.settings_path).expect("read")).expect("json");
    let keys: Vec<&str> = after_val
        .as_object()
        .expect("obj")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["colorScheme", "model", "trustedWorkspaces", "verbosity"],
        "removing statusLine must preserve exact order of remaining keys (no swap_remove!)"
    );
    assert!(
        !env.agyline_home.exists(),
        "--purge must remove AGYLINE_HOME"
    );
}

// A statusLine we do not own is left byte-for-byte alone, and unparseable settings must error
// rather than be rewritten into something the user never wrote.
#[test]
fn uninstall_leaves_foreign_statusline_untouched_and_malformed_json_errors() {
    let sb = Sandbox::new("foreign_and_malformed");
    let cli_dir = env_settings_parent(sb.path());
    fs::create_dir_all(&cli_dir).expect("create cli dir");

    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/alice")),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );

    let foreign = "{\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"python3 /custom/bar.py\",\n    \"enabled\": true\n  }\n}\n";
    fs::write(&env.settings_path, foreign).expect("write foreign");

    let uplan = plan_uninstall(&env, UninstallOptions::default()).expect("plan foreign uninstall");
    assert!(matches!(uplan, UninstallPlan::ForeignStatusLine { .. }));
    apply_uninstall(&env, &uplan).expect("apply foreign noop");
    assert_eq!(
        fs::read_to_string(&env.settings_path).expect("read"),
        foreign,
        "foreign statusLine must remain byte-for-byte untouched"
    );

    // Malformed / non-object JSON must return InvalidSettingsJson on both install & uninstall.
    for bad in ["{ broken json", "[1, 2, 3]", "\"just a string\""] {
        fs::write(&env.settings_path, bad).expect("write bad json");
        assert!(matches!(
            plan_install(&env),
            Err(InstallError::InvalidSettingsJson { .. })
        ));
        assert!(matches!(
            plan_uninstall(&env, UninstallOptions::default()),
            Err(InstallError::InvalidSettingsJson { .. })
        ));
    }
}

// Dotfiles-managed settings.json is a symlink: the atomic swap must write through it and leave
// the link itself in place.
#[test]
fn install_follows_symlink_without_replacing_link_with_regular_file() {
    let sb = Sandbox::new("symlink_follow");
    let dotfiles_dir = sb.path().join("dotfiles");
    fs::create_dir_all(&dotfiles_dir).expect("dotfiles dir");
    let real_settings = dotfiles_dir.join("settings.json");
    fs::write(&real_settings, "{\n  \"verbosity\": \"high\"\n}\n").expect("seed real settings");

    let cli_dir = env_settings_parent(sb.path());
    fs::create_dir_all(&cli_dir).expect("cli dir");
    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/alice")),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );

    if let Err(err) = create_file_symlink(&real_settings, &env.settings_path) {
        // Unprivileged Windows hosts without Developer Mode return OS error 1314 (ERROR_PRIVILEGE_NOT_HELD).
        if err.raw_os_error() == Some(1314) {
            return;
        }
        panic!("failed to create test symlink: {err}");
    }

    let plan = plan_install(&env).expect("plan symlink install");
    apply_install(&env, &plan).expect("apply symlink install");

    let link_meta = fs::symlink_metadata(&env.settings_path).expect("symlink metadata");
    assert!(
        link_meta.file_type().is_symlink(),
        "settings.json symlink must remain a symlink after atomic update"
    );
    let target_val: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&real_settings).expect("read target"))
            .expect("json");
    assert_eq!(
        target_val["statusLine"]["command"],
        serde_json::json!("~/.cargo/bin/agyline")
    );
    assert_eq!(target_val["verbosity"], serde_json::json!("high"));
}

fn env_settings_parent(sandbox: &Path) -> PathBuf {
    sandbox.join("home").join(".gemini").join("antigravity-cli")
}

/// Runs the real binary with HOME/USERPROFILE/AGYLINE_HOME redirected into the sandbox, so CLI
/// cases exercise process-level behavior without touching the developer's settings.
fn run_sandboxed_bin(sandbox: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let user_home = sandbox.join("home");
    let agyline_home = sandbox.join("agyline-home");
    fs::create_dir_all(&user_home).expect("create sandbox home");

    // Copy the compiled test binary into sandbox/home/bin/agyline(.exe) so its path has zero spaces
    // even if a developer's workspace path ever had spaces.
    let bin_src = Path::new(env!("CARGO_BIN_EXE_agyline"));
    let bin_name = bin_src.file_name().expect("bin name");
    let sandbox_bin_dir = user_home.join(".cargo").join("bin");
    fs::create_dir_all(&sandbox_bin_dir).expect("create sandbox bin dir");
    let sandbox_bin = sandbox_bin_dir.join(bin_name);
    if !sandbox_bin.exists() {
        fs::copy(bin_src, &sandbox_bin).expect("copy agyline binary into sandbox");
    }

    let mut child = Command::new(&sandbox_bin)
        .args(args)
        .env("USERPROFILE", &user_home)
        .env("HOME", &user_home)
        .env("AGYLINE_HOME", &agyline_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sandboxed agyline");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        let _ = stdin.write_all(stdin_bytes);
    }

    child.wait_with_output().expect("wait sandboxed agyline")
}

// Help exits 0 on stdout; any unknown flag or stray argument exits 2 with a `--help` hint on
// stderr and nothing on stdout.
#[test]
fn cli_install_and_uninstall_help_and_unknown_flags_contract() {
    let sb = Sandbox::new("cli_flags");

    for sub in ["install", "uninstall"] {
        for flag in ["-h", "--help"] {
            let out = run_sandboxed_bin(sb.path(), &[sub, flag], b"");
            assert_eq!(out.status.code(), Some(0));
            assert!(out.stderr.is_empty());
            assert!(String::from_utf8_lossy(&out.stdout).contains(sub));
        }
    }

    // `--reset`/`--purge` are uninstall-only; accepting them on install would widen its scope.
    let bad_cases: &[&[&str]] = &[
        &["install", "--unknown"],
        &["install", "--reset"],
        &["install", "--purge"],
        &["install", "extra-arg"],
        &["uninstall", "--unknown"],
        &["uninstall", "-x"],
        &["uninstall", "extra-arg"],
    ];
    for args in bad_cases {
        let out = run_sandboxed_bin(sb.path(), args, b"");
        assert_eq!(
            out.status.code(),
            Some(2),
            "args {args:?} must exit 2 on usage error"
        );
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--help"));
    }
}

// The whole CLI lifecycle in one sandbox: preview, non-TTY refusal, apply, idempotence, restore.
// Nothing may mutate settings.json without consent, and restore must be byte-exact.
#[test]
fn cli_dry_run_non_tty_guard_and_end_to_end_subprocess_lifecycle() {
    let sb = Sandbox::new("cli_e2e");
    let cli_dir = env_settings_parent(sb.path());
    fs::create_dir_all(&cli_dir).expect("create cli dir");
    let settings_path = cli_dir.join("settings.json");
    let agyline_home = sb.path().join("agyline-home");

    let initial = "{\n  \"colorScheme\": \"dark\",\n  \"statusLine\": {\n    \"type\": \"command\",\n    \"command\": \"pwsh -NoLogo -File ~/.gemini/antigravity-cli/statusline.ps1\",\n    \"enabled\": true\n  },\n  \"verbosity\": \"high\"\n}\n";
    fs::write(&settings_path, initial).expect("seed settings");

    // 1. --dry-run prints diff, exits 0, touches neither settings.json nor AGYLINE_HOME.
    let dry = run_sandboxed_bin(sb.path(), &["install", "--dry-run"], b"");
    assert_eq!(dry.status.code(), Some(0));
    assert!(dry.stderr.is_empty());
    let dry_out = String::from_utf8(dry.stdout).expect("utf8");
    assert!(dry_out.contains("settings.json"));
    assert!(dry_out.contains("-    \"command\": \"pwsh"));
    assert_eq!(fs::read_to_string(&settings_path).expect("read"), initial);
    assert!(!agyline_home.exists());

    // 2. Non-TTY without -y prints diff to stdout, error to stderr, exits 1, does not mutate!
    let refused = run_sandboxed_bin(sb.path(), &["install"], b"y\n");
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stdout).contains("--- a/settings.json"));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert_eq!(fs::read_to_string(&settings_path).expect("read"), initial);

    // 3. install -y applies atomically, snapshots prior statusLine, reminds to restart agy.
    let installed = run_sandboxed_bin(sb.path(), &["install", "-y"], b"");
    assert_eq!(installed.status.code(), Some(0));
    assert!(installed.stderr.is_empty());
    let installed_out = String::from_utf8(installed.stdout).expect("utf8");
    assert!(installed_out.contains("--- a/settings.json"));
    assert!(installed_out.to_ascii_lowercase().contains("restart"));
    assert!(
        agyline_home
            .join("state")
            .join("previous_statusline.json")
            .exists()
    );

    // 4. Repeat install (even on non-TTY without -y!) is idempotent and exits 0.
    let idem = run_sandboxed_bin(sb.path(), &["install"], b"");
    assert_eq!(idem.status.code(), Some(0));
    assert!(idem.stderr.is_empty());
    assert!(String::from_utf8_lossy(&idem.stdout).contains("already"));

    // 5. Non-TTY uninstall without -y prints diff to stdout, error to stderr, exits 1.
    let urefused = run_sandboxed_bin(sb.path(), &["uninstall"], b"y\n");
    assert_eq!(urefused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&urefused.stdout).contains("--- a/settings.json"));
    assert!(!urefused.stderr.is_empty());

    // 6. uninstall --dry-run previews restore without modifying.
    let udry = run_sandboxed_bin(sb.path(), &["uninstall", "--dry-run"], b"");
    assert_eq!(udry.status.code(), Some(0));
    assert!(udry.stderr.is_empty());
    assert!(
        agyline_home
            .join("state")
            .join("previous_statusline.json")
            .exists()
    );

    // 7. uninstall -y restores exact initial settings.json!
    let uninst = run_sandboxed_bin(sb.path(), &["uninstall", "-y"], b"");
    assert_eq!(uninst.status.code(), Some(0));
    assert!(uninst.stderr.is_empty());
    assert_eq!(fs::read_to_string(&settings_path).expect("read"), initial);
}

// On a TTY the default is No: only an explicit yes applies, and a cancel must leave no file.
#[test]
fn cli_interactive_tty_prompt_cancels_unless_yes_and_applies_on_yes() {
    let sb = Sandbox::new("cli_tty_prompt");
    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(PathBuf::from("/Users/alice")),
        PathBuf::from("/Users/alice/.cargo/bin/agyline"),
        None,
        |_| None,
    );

    // Test interactive cancellation ("n\n") and acceptance ("yes\n") via run_install_cli / run_cli.
    // Using agyline::run_install_cli directly with simulated TTY streams keeps process env thread-safe!
    let mut cancel_in = Cursor::new(b"n\n".to_vec());
    let mut cancel_out = Vec::new();
    let mut cancel_err = Vec::new();
    let code_cancel = {
        let mut streams = CliStreams {
            stdin: &mut cancel_in,
            stdin_is_terminal: true,
            stdout: &mut cancel_out,
            stdout_is_terminal: true,
            stderr: &mut cancel_err,
        };
        agyline::run_install_cli(&env, agyline::InstallOptions::default(), &mut streams)
    };
    assert_eq!(code_cancel, 1);
    assert!(
        String::from_utf8_lossy(&cancel_out).contains("Apply changes? [y/N]: "),
        "must prompt [y/N] on TTY"
    );
    assert!(!cancel_err.is_empty(), "cancellation must report to stderr");
    assert!(!env.settings_path.exists());

    let mut ok_in = Cursor::new(b"yes\r\n".to_vec());
    let mut ok_out = Vec::new();
    let mut ok_err = Vec::new();
    let code_ok = {
        let mut streams = CliStreams {
            stdin: &mut ok_in,
            stdin_is_terminal: true,
            stdout: &mut ok_out,
            stdout_is_terminal: true,
            stderr: &mut ok_err,
        };
        let _ = run_cli;
        agyline::run_install_cli(&env, agyline::InstallOptions::default(), &mut streams)
    };
    assert_eq!(code_ok, 0);
    assert!(ok_err.is_empty());
    assert!(env.settings_path.exists());
}

// Uninstall must recognize its own escaped command, and `--purge` must refuse when AGYLINE_HOME
// is the user's home rather than a dedicated directory.
#[test]
fn unix_single_quoted_command_roundtrips_through_install_and_uninstall_and_guards_unsafe_purge() {
    let sb = Sandbox::new("quote_roundtrip_and_purge_guard");
    let user_home = sb.path().join("O'Reilly Family");
    let env = make_env(
        sb.path(),
        PlatformKind::Unix,
        Some(user_home.clone()),
        user_home.join("my $tools").join("agyline"),
        None,
        |_| None,
    );

    let iplan = plan_install(&env).expect("plan quoted install");
    apply_install(&env, &iplan).expect("apply quoted install");

    let uplan = plan_uninstall(&env, UninstallOptions::default()).expect("plan quoted uninstall");
    assert!(
        matches!(uplan, UninstallPlan::WouldChange { .. }),
        "escaped single-quoted agyline command must be recognized by uninstall"
    );
    apply_uninstall(&env, &uplan).expect("apply quoted uninstall");

    // Guard against misconfigured AGYLINE_HOME == user_home on --purge.
    let mut unsafe_env = env.clone();
    unsafe_env.agyline_home = user_home;
    let err = plan_uninstall(
        &unsafe_env,
        UninstallOptions {
            dry_run: false,
            yes: true,
            reset: false,
            purge: true,
        },
    )
    .expect_err("must refuse purging user_home");
    assert!(matches!(err, InstallError::UnsafePurgePath { .. }));

    // Also exercise host process short_path_resolver without touching real settings.json.
    if let Ok(proc_env) = InstallEnv::from_process() {
        let spaced_dir = sb.path().join("Dir With Spaces");
        fs::create_dir_all(&spaced_dir).expect("create spaced dir");
        let spaced_exe = spaced_dir.join("agyline.exe");
        fs::write(&spaced_exe, b"stub").expect("write stub");
        let _ = (proc_env.short_path_resolver)(&spaced_exe);
        assert!(
            (proc_env.short_path_resolver)(&sb.path().join("does_not_exist_12345.exe")).is_none()
        );
    }
}
