//! Contract tests for `agyline update [branch]`: argument parsing, branch validation, hermetic
//! `git ls-remote` probing against a local bare repository, state/fingerprint skip & channel
//! switching, failure safety, and cross-platform executable replacement.

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agyline::{
    CliStreams, RemoteProbe, UPDATE_REPO_ENV, UpdateEnv, UpdateError, UpdateOptions, UpdateRunner,
    cleanup_stale_executable_backups, exe_filename, is_cargo_target_artifact, load_update_state,
    probe_remote_branch, replace_executable, run_update_with_runner, validate_branch_name,
};

static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_agyline")
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "agyline_update_test_{label}_{}_{nanos}_{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp test dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_bin(args: &[&str], home: &Path, extra_envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin_path());
    cmd.args(args)
        .env("AGYLINE_HOME", home)
        .env_remove(UPDATE_REPO_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn agyline")
}

struct FakeRunner {
    cargo_result: Result<(), UpdateError>,
    probes: HashMap<String, RemoteProbe>,
    build_payloads: HashMap<String, Vec<u8>>,
    build_error: Option<UpdateError>,
    verify_result: Result<String, UpdateError>,
    build_calls: AtomicUsize,
}

impl FakeRunner {
    fn new() -> Self {
        Self {
            cargo_result: Ok(()),
            probes: HashMap::new(),
            build_payloads: HashMap::new(),
            build_error: None,
            verify_result: Ok("0.3.1".to_string()),
            build_calls: AtomicUsize::new(0),
        }
    }

    fn builds(&self) -> usize {
        self.build_calls.load(Ordering::Relaxed)
    }
}

impl UpdateRunner for FakeRunner {
    fn check_cargo_available(&self) -> Result<(), UpdateError> {
        self.cargo_result.clone()
    }

    fn probe_remote_branch(&self, _repo_url: &str, branch: &str) -> RemoteProbe {
        self.probes
            .get(branch)
            .cloned()
            .unwrap_or(RemoteProbe::BranchNotFound)
    }

    fn build_staged_binary(
        &self,
        _repo_url: &str,
        branch: &str,
        staging_root: &Path,
    ) -> Result<PathBuf, UpdateError> {
        self.build_calls.fetch_add(1, Ordering::Relaxed);
        if let Some(err) = &self.build_error {
            return Err(err.clone());
        }
        let bin_dir = staging_root.join("bin");
        fs::create_dir_all(&bin_dir).expect("create staging bin dir");
        let staged = bin_dir.join(exe_filename("agyline"));
        let bytes = self
            .build_payloads
            .get(branch)
            .cloned()
            .unwrap_or_else(|| format!("binary-for-{branch}").into_bytes());
        fs::write(&staged, bytes).expect("write staged binary");
        Ok(staged)
    }

    fn verify_staged_binary(&self, _staged_bin: &Path) -> Result<String, UpdateError> {
        self.verify_result.clone()
    }
}

fn invoke_update(
    env: &UpdateEnv,
    opts: &UpdateOptions,
    runner: &dyn UpdateRunner,
) -> (u8, String, String) {
    let mut stdin = Cursor::new(Vec::<u8>::new());
    let mut stdout = Vec::<u8>::new();
    let mut stderr = Vec::<u8>::new();
    let mut streams = CliStreams {
        stdin: &mut stdin,
        stdin_is_terminal: false,
        stdout: &mut stdout,
        stdout_is_terminal: false,
        stderr: &mut stderr,
    };
    let code = run_update_with_runner(env, opts, runner, &mut streams);
    (
        code,
        String::from_utf8(stdout).expect("utf8 stdout"),
        String::from_utf8(stderr).expect("utf8 stderr"),
    )
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .stdin(Stdio::null())
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 git stdout")
}

// `update --help` prints usage to stdout with exit 0; unknown flags, extra args, and unsafe refs
// exit 2 with zero stdout and a `--help` hint on stderr.
#[test]
fn cli_update_help_and_invalid_arguments() {
    let home = TempDir::new("cli_help");
    for args in [&["update", "-h"][..], &["update", "--help"][..]] {
        let out = run_bin(args, home.path(), &[]);
        assert_eq!(out.status.code(), Some(0), "args {args:?}");
        assert!(out.stderr.is_empty(), "args {args:?}");
        let text = String::from_utf8(out.stdout).expect("utf8");
        assert!(text.contains("agyline update"));
        assert!(text.contains("main"));
        assert!(text.contains("dev"));
        assert!(text.contains("--check"));
        assert!(text.contains("--force"));
        assert!(text.contains(UPDATE_REPO_ENV));
    }

    let invalid_cases: &[&[&str]] = &[
        &["update", "--unknown"],
        &["update", "-x"],
        &["update", "main", "extra"],
        &["update", "dev", "main"],
        &["update", "../evil"],
        &["update", "foo..bar"],
        &["update", "dev.lock"],
        &["update", "has space"],
        &["update", "--", "-leading-dash"],
        &["update", "@"],
        &["update", "dev@{0}"],
    ];
    for args in invalid_cases {
        let out = run_bin(args, home.path(), &[]);
        assert_eq!(out.status.code(), Some(2), "args {args:?} must exit 2");
        assert!(
            out.stdout.is_empty(),
            "args {args:?} must emit empty stdout"
        );
        let err = String::from_utf8(out.stderr).expect("utf8 stderr");
        assert!(
            err.contains("--help"),
            "args {args:?} stderr must mention --help, got: {err:?}"
        );
    }
}

// Branch validation allows normal channel/feature branch names and rejects git-unsafe refs.
#[test]
fn validate_branch_name_accepts_channels_and_rejects_unsafe_refs() {
    for good in [
        "main",
        "dev",
        "feat/v0.4-update",
        "release/1.0",
        "v0.3_hotfix",
    ] {
        assert_eq!(validate_branch_name(good).as_deref(), Ok(good));
    }
    for bad in [
        "",
        "-b",
        "--branch",
        "/main",
        "main/",
        "a//b",
        ".hidden",
        "end.",
        "a..b",
        "a/.b",
        "main.lock",
        "a.lock/b",
        "@",
        "a@{1}",
        "a b",
        "a~1",
        "a^1",
        "a:b",
        "a?b",
        "a*b",
        "a[0]",
        "a\\b",
        "a\nb",
    ] {
        assert!(
            validate_branch_name(bad).is_err(),
            "expected '{bad}' to be rejected"
        );
    }
}

// Hermetic offline test against a real local bare Git repository with distinct `main` and `dev`
// commits: verifies `probe_remote_branch` and real `agyline update [dev] --check` subprocesses.
#[test]
fn probe_remote_branch_and_cli_check_against_local_bare_repo() {
    let root = TempDir::new("local_bare");
    let bare = root.path().join("remote.git");
    let work = root.path().join("work");
    let home = root.path().join("agyline-home");
    fs::create_dir_all(&bare).expect("create bare dir");
    fs::create_dir_all(&work).expect("create work dir");

    run_git(&bare, &["init", "--bare"]);
    run_git(&work, &["init", "-b", "main"]);
    fs::write(work.join("README.md"), "main\n").expect("write readme");
    run_git(&work, &["add", "README.md"]);
    run_git(&work, &["commit", "-m", "main commit"]);
    let main_sha = run_git(&work, &["rev-parse", "HEAD"]).trim().to_string();

    run_git(&work, &["checkout", "-b", "dev"]);
    fs::write(work.join("README.md"), "dev\n").expect("write dev readme");
    run_git(&work, &["add", "README.md"]);
    run_git(&work, &["commit", "-m", "dev commit"]);
    let dev_sha = run_git(&work, &["rev-parse", "HEAD"]).trim().to_string();
    assert_ne!(main_sha, dev_sha);

    let bare_str = bare.to_string_lossy().into_owned();
    run_git(&work, &["remote", "add", "origin", &bare_str]);
    run_git(&work, &["push", "origin", "main", "dev"]);

    assert_eq!(
        probe_remote_branch(&bare_str, "main", Duration::from_secs(10)),
        RemoteProbe::Commit(main_sha.clone())
    );
    assert_eq!(
        probe_remote_branch(&bare_str, "dev", Duration::from_secs(10)),
        RemoteProbe::Commit(dev_sha.clone())
    );
    assert_eq!(
        probe_remote_branch(&bare_str, "no-such-branch", Duration::from_secs(10)),
        RemoteProbe::BranchNotFound
    );

    // Default `agyline update --check` resolves `main`; `agyline update dev --check` resolves `dev`.
    let out_main = run_bin(
        &["update", "--check"],
        &home,
        &[(UPDATE_REPO_ENV, &bare_str)],
    );
    assert_eq!(out_main.status.code(), Some(0));
    let stdout_main = String::from_utf8(out_main.stdout).expect("utf8");
    assert!(
        stdout_main.contains("Target branch:  main"),
        "{stdout_main}"
    );
    assert!(stdout_main.contains(&main_sha[..7]), "{stdout_main}");
    assert!(stdout_main.contains("Dry run: no files were modified."));

    let out_dev = run_bin(
        &["update", "dev", "--dry-run"],
        &home,
        &[(UPDATE_REPO_ENV, &bare_str)],
    );
    assert_eq!(out_dev.status.code(), Some(0));
    let stdout_dev = String::from_utf8(out_dev.stdout).expect("utf8");
    assert!(stdout_dev.contains("Target branch:  dev"), "{stdout_dev}");
    assert!(stdout_dev.contains(&dev_sha[..7]), "{stdout_dev}");
    assert!(
        !home.exists(),
        "--check/--dry-run must not create state dir"
    );

    let out_missing = run_bin(
        &["update", "does-not-exist", "--check"],
        &home,
        &[(UPDATE_REPO_ENV, &bare_str)],
    );
    assert_eq!(out_missing.status.code(), Some(1));
    let stderr_missing = String::from_utf8(out_missing.stderr).expect("utf8");
    assert!(
        stderr_missing.contains("not found"),
        "expected branch-not-found diagnostic, got: {stderr_missing}"
    );
}

// Verifies initial update, same-commit skip, switching main -> dev even with identical SemVer,
// external binary modification fingerprint invalidation, and `--force` override.
#[test]
fn update_flow_main_skip_switch_dev_fingerprint_and_force() {
    let dir = TempDir::new("flow");
    let home = dir.path().join("home");
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    let target_exe = bin_dir.join(exe_filename("agyline"));
    fs::write(&target_exe, b"initial-binary").expect("seed target exe");

    let env = UpdateEnv {
        agyline_home: home.clone(),
        target_exe: target_exe.clone(),
        repo_url: "https://example.invalid/agyline.git".to_string(),
        current_version: "0.3.0".to_string(),
    };

    let main_sha = "1111111111111111111111111111111111111111".to_string();
    let dev_sha = "2222222222222222222222222222222222222222".to_string();
    let mut runner = FakeRunner::new();
    runner
        .probes
        .insert("main".to_string(), RemoteProbe::Commit(main_sha.clone()));
    runner
        .probes
        .insert("dev".to_string(), RemoteProbe::Commit(dev_sha.clone()));
    runner
        .build_payloads
        .insert("main".to_string(), b"built-main-binary".to_vec());
    runner
        .build_payloads
        .insert("dev".to_string(), b"built-dev-binary-longer".to_vec());

    // 1. Default update -> main.
    let (code, stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &runner);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(runner.builds(), 1);
    assert_eq!(
        fs::read(&target_exe).expect("read exe"),
        b"built-main-binary"
    );
    assert!(stdout.contains("main @ 1111111"), "{stdout}");
    let state = load_update_state(&env.state_path()).expect("state written");
    assert_eq!(state.branch, "main");
    assert_eq!(state.commit.as_deref(), Some(main_sha.as_str()));
    assert_eq!(state.version, "0.3.1");

    // 2. Repeat default update -> skipped because commit + binary fingerprint match.
    let (code, stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &runner);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(runner.builds(), 1);
    assert!(stdout.contains("already up to date"), "{stdout}");

    // 3. Switch to `dev` (same SemVer 0.3.1, different commit SHA) -> rebuilds & swaps.
    let dev_opts = UpdateOptions {
        branch: "dev".to_string(),
        check: false,
        force: false,
    };
    let (code, stdout, stderr) = invoke_update(&env, &dev_opts, &runner);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(runner.builds(), 2);
    assert_eq!(
        fs::read(&target_exe).expect("read exe"),
        b"built-dev-binary-longer"
    );
    assert!(stdout.contains("dev @ 2222222"), "{stdout}");
    let state = load_update_state(&env.state_path()).expect("dev state written");
    assert_eq!(state.branch, "dev");
    assert_eq!(state.commit.as_deref(), Some(dev_sha.as_str()));

    // 4. External binary replacement invalidates fingerprint -> rebuilds on next `update dev`.
    fs::write(&target_exe, b"manual-cargo-path-build").expect("simulate manual install");
    let (code, _stdout, stderr) = invoke_update(&env, &dev_opts, &runner);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(runner.builds(), 3);
    assert_eq!(
        fs::read(&target_exe).expect("read exe"),
        b"built-dev-binary-longer"
    );

    // 5. `--force` bypasses up-to-date short-circuit.
    let force_opts = UpdateOptions {
        branch: "dev".to_string(),
        check: false,
        force: true,
    };
    let (code, _stdout, stderr) = invoke_update(&env, &force_opts, &runner);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(runner.builds(), 4);
}

// Failure paths never corrupt the existing binary or persist misleading state; transport errors
// during `git ls-remote` fail `--check` but degrade cleanly into Cargo on real update.
#[test]
fn update_failure_modes_and_target_artifact_guard() {
    let dir = TempDir::new("failures");
    let home = dir.path().join("home");
    let bin_dir = dir.path().join("bin");
    fs::create_dir_all(&bin_dir).expect("create bin dir");
    let target_exe = bin_dir.join(exe_filename("agyline"));
    fs::write(&target_exe, b"original-good-binary").expect("seed target");

    let env = UpdateEnv {
        agyline_home: home.clone(),
        target_exe: target_exe.clone(),
        repo_url: "https://example.invalid/agyline.git".to_string(),
        current_version: "0.3.0".to_string(),
    };

    // Missing cargo exits 1 before building.
    let mut no_cargo = FakeRunner::new();
    no_cargo.cargo_result = Err(UpdateError::CargoNotFound);
    no_cargo.probes.insert(
        "main".to_string(),
        RemoteProbe::Commit("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
    );
    let (code, _stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &no_cargo);
    assert_eq!(code, 1);
    assert_eq!(no_cargo.builds(), 0);
    assert!(stderr.contains("cargo"), "{stderr}");
    assert_eq!(
        fs::read(&target_exe).expect("read"),
        b"original-good-binary"
    );

    // Cargo build error leaves target exe and state untouched.
    let mut build_fail = FakeRunner::new();
    build_fail.probes.insert(
        "main".to_string(),
        RemoteProbe::Commit("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
    );
    build_fail.build_error = Some(UpdateError::CargoFailed("exit status 101".to_string()));
    let (code, _stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &build_fail);
    assert_eq!(code, 1);
    assert!(stderr.contains("cargo install failed"), "{stderr}");
    assert_eq!(
        fs::read(&target_exe).expect("read"),
        b"original-good-binary"
    );
    assert!(load_update_state(&env.state_path()).is_none());

    // Verification failure prevents swapping a broken binary into place.
    let mut verify_fail = FakeRunner::new();
    verify_fail.probes.insert(
        "main".to_string(),
        RemoteProbe::Commit("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()),
    );
    verify_fail.verify_result = Err(UpdateError::VerificationFailed("bad output".to_string()));
    let (code, _stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &verify_fail);
    assert_eq!(code, 1);
    assert!(stderr.contains("verify"), "{stderr}");
    assert_eq!(
        fs::read(&target_exe).expect("read"),
        b"original-good-binary"
    );

    // Git transport error fails `--check` but warns and falls through to Cargo on live update.
    let mut transport_err = FakeRunner::new();
    transport_err.probes.insert(
        "main".to_string(),
        RemoteProbe::TransportError("could not resolve proxy".to_string()),
    );
    let check_opts = UpdateOptions {
        check: true,
        ..UpdateOptions::default()
    };
    let (code, _stdout, _stderr) = invoke_update(&env, &check_opts, &transport_err);
    assert_eq!(code, 1);
    assert_eq!(transport_err.builds(), 0);

    let (code, stdout, stderr) = invoke_update(&env, &UpdateOptions::default(), &transport_err);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(transport_err.builds(), 1);
    assert!(stderr.contains("continuing with cargo"), "{stderr}");
    assert!(stdout.contains("Updated agyline"), "{stdout}");

    // Local Cargo `target/debug` artifact guard blocks accidental overwrite unless `--force`.
    let fake_repo = dir.path().join("repo");
    let debug_dir = fake_repo.join("target").join("debug");
    fs::create_dir_all(&debug_dir).expect("create fake target/debug");
    fs::write(
        fake_repo.join("Cargo.toml"),
        "[package]\nname = \"agyline\"\n",
    )
    .expect("Cargo.toml");
    let debug_exe = debug_dir.join(exe_filename("agyline"));
    fs::write(&debug_exe, b"debug-bin").expect("debug exe");
    assert!(is_cargo_target_artifact(&debug_exe));
    assert!(!is_cargo_target_artifact(&target_exe));

    let artifact_env = UpdateEnv {
        agyline_home: home,
        target_exe: debug_exe.clone(),
        repo_url: "https://example.invalid/agyline.git".to_string(),
        current_version: "0.3.0".to_string(),
    };
    let mut ok_runner = FakeRunner::new();
    ok_runner.probes.insert(
        "main".to_string(),
        RemoteProbe::Commit("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()),
    );
    let (code, _stdout, stderr) =
        invoke_update(&artifact_env, &UpdateOptions::default(), &ok_runner);
    assert_eq!(code, 1);
    assert!(stderr.contains("cargo build artifact"), "{stderr}");
    assert_eq!(ok_runner.builds(), 0);

    let (code, _stdout, stderr) = invoke_update(
        &artifact_env,
        &UpdateOptions {
            force: true,
            ..UpdateOptions::default()
        },
        &ok_runner,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(ok_runner.builds(), 1);
}

// Exercises `platform::replace_executable` across separate directories and verifies
// `cleanup_stale_executable_backups` removes only `<exe>.old.*` siblings.
#[test]
fn platform_replace_executable_and_stale_backup_cleanup() {
    let dir = TempDir::new("platform_swap");
    let staging = dir.path().join("staging");
    let install_dir = dir.path().join("install");
    fs::create_dir_all(&staging).expect("staging dir");
    fs::create_dir_all(&install_dir).expect("install dir");

    let exe_name = exe_filename("agyline");
    let target_exe = install_dir.join(&exe_name);
    let staged_bin = staging.join(&exe_name);
    let stale_a = install_dir.join(format!("{exe_name}.old.100.0"));
    let stale_b = install_dir.join(format!("{exe_name}.old.200.1"));
    let unrelated = install_dir.join("keep.txt");

    fs::write(&target_exe, b"old-bytes").expect("write target");
    fs::write(&staged_bin, b"new-bytes-payload").expect("write staged");
    fs::write(&stale_a, b"stale1").expect("write stale a");
    fs::write(&stale_b, b"stale2").expect("write stale b");
    fs::write(&unrelated, b"keep").expect("write unrelated");

    cleanup_stale_executable_backups(&target_exe);
    assert!(!stale_a.exists(), "stale backup A must be removed");
    assert!(!stale_b.exists(), "stale backup B must be removed");
    assert!(target_exe.exists(), "target exe must survive backup sweep");
    assert!(
        unrelated.exists(),
        "unrelated sibling must survive backup sweep"
    );

    replace_executable(&staged_bin, &target_exe).expect("replace_executable");
    assert_eq!(
        fs::read(&target_exe).expect("read new exe"),
        b"new-bytes-payload"
    );

    let leftover_tmp: Vec<_> = fs::read_dir(&install_dir)
        .expect("list install dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".new.") || name.contains(".old."))
        .collect();
    assert!(
        leftover_tmp.is_empty(),
        "no leftover temp/old files expected for non-running target, found: {leftover_tmp:?}"
    );
}
