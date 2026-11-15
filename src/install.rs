//! Install/uninstall engine for the `agy` statusLine hook in settings.json. Planning only reads,
//! so `--dry-run` and the TUI diff preview consume a plan without writing. Applying must do no
//! damage: keep sibling keys, write through symlinks, snapshot the prior `statusLine` for exact
//! restore, and leave the file untouched on failure. The official `agy` binary is never modified.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::cli::CliStreams;
use crate::platform::{self, PlatformKind};
use crate::state::ensure_state_dir;

// Keeps temp names unique within one process so `create_new` cannot collide across rapid writes.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

// Prior `statusLine` object, saved before the first install that replaces a foreign hook.
const SNAPSHOT_FILE_NAME: &str = "previous_statusline.json";

/// Injectable `platform::short_path_name` so tests can simulate 8.3 aliases (or volumes without
/// them) on any host; the real resolver only produces aliases on Windows.
pub type ShortPathResolver = Arc<dyn Fn(&Path) -> Option<PathBuf> + Send + Sync>;

/// Everything install reads from the host, injected rather than ambient so the sandbox install
/// test can point at fake paths and exercise Windows command rules on a Unix host.
#[derive(Clone)]
pub struct InstallEnv {
    pub settings_path: PathBuf,
    pub agyline_home: PathBuf,
    pub user_home: Option<PathBuf>,
    pub exe_path: PathBuf,
    pub path_env: Option<OsString>,
    pub platform: PlatformKind,
    pub short_path_resolver: ShortPathResolver,
}

impl fmt::Debug for InstallEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallEnv")
            .field("settings_path", &self.settings_path)
            .field("agyline_home", &self.agyline_home)
            .field("user_home", &self.user_home)
            .field("exe_path", &self.exe_path)
            .field("path_env", &self.path_env)
            .field("platform", &self.platform)
            .finish_non_exhaustive()
    }
}

impl InstallEnv {
    /// Captures the live host once; fails only when home or the exe path is unresolvable. Tests
    /// build the struct directly instead, to avoid mutating process-wide env.
    pub fn from_process() -> Result<Self, InstallError> {
        let lookup = |k: &str| std::env::var(k).ok();
        let user_home = platform::user_home_dir(&lookup);
        let Some(ref home) = user_home else {
            return Err(InstallError::MissingHomeDir);
        };
        let settings_path = home
            .join(".gemini")
            .join("antigravity-cli")
            .join("settings.json");
        let agyline_home = crate::config::resolve_home_dir(&lookup, home);
        let exe_path = std::env::current_exe().map_err(|err| InstallError::Io {
            path: PathBuf::from("<current_exe>"),
            message: err.to_string(),
        })?;
        Ok(Self {
            settings_path,
            agyline_home,
            user_home,
            exe_path,
            path_env: std::env::var_os("PATH"),
            platform: platform::host_platform(),
            short_path_resolver: Arc::new(platform::short_path_name),
        })
    }

    /// `<agyline_home>/state/previous_statusline.json`; uninstall reads it to restore exactly.
    pub fn snapshot_path(&self) -> PathBuf {
        self.agyline_home.join("state").join(SNAPSHOT_FILE_NAME)
    }
}

/// Failures carry the offending path. Writes are atomic, so an error never leaves settings.json
/// half-written; a purge error can still follow an already-applied restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    MissingHomeDir,
    NonUtf8Path { path: PathBuf },
    UnsafeCommandPath { exe_path: PathBuf, reason: String },
    InvalidSettingsJson { path: PathBuf, reason: String },
    InvalidSnapshotJson { path: PathBuf, reason: String },
    UnsafePurgePath { path: PathBuf },
    Io { path: PathBuf, message: String },
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHomeDir => {
                write!(
                    f,
                    "could not determine user home directory from environment (HOME / USERPROFILE)"
                )
            }
            Self::NonUtf8Path { path } => {
                write!(
                    f,
                    "executable path '{}' is not valid UTF-8 and cannot be encoded in settings.json",
                    path.display()
                )
            }
            Self::UnsafeCommandPath { exe_path, reason } => {
                write!(
                    f,
                    "cannot generate a safe Windows 'cmd /c' statusLine command for '{}': {reason}",
                    exe_path.display()
                )
            }
            Self::InvalidSettingsJson { path, reason } => {
                write!(
                    f,
                    "failed to parse '{}' as a JSON object: {reason}",
                    path.display()
                )
            }
            Self::InvalidSnapshotJson { path, reason } => {
                write!(
                    f,
                    "saved statusLine snapshot '{}' is invalid ({reason}); re-run with '--reset' to remove statusLine instead",
                    path.display()
                )
            }
            Self::UnsafePurgePath { path } => {
                write!(
                    f,
                    "refusing to purge unsafe AGYLINE_HOME directory '{}'",
                    path.display()
                )
            }
            Self::Io { path, message } => {
                write!(f, "I/O error on '{}': {message}", path.display())
            }
        }
    }
}

impl std::error::Error for InstallError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub yes: bool,
}

/// `reset` removes `statusLine` outright instead of restoring the snapshot (and discards it);
/// `purge` additionally deletes the whole AGYLINE_HOME directory after the settings change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UninstallOptions {
    pub dry_run: bool,
    pub yes: bool,
    pub reset: bool,
    pub purge: bool,
}

/// What `inspect_hook` found. Install may repoint any hook but snapshots a foreign one first;
/// uninstall only acts on `Connected` and leaves every other statusLine byte-for-byte untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookStatus {
    MissingSettings,
    NotConfigured,
    /// `enabled` is the effective flag; an absent key counts as enabled.
    Connected {
        command: String,
        enabled: bool,
    },
    /// Covers the legacy pwsh statusline.ps1 hook; `command` is None for non-object values.
    ForeignStatusLine {
        command: Option<String>,
    },
    InvalidSettings {
        reason: String,
    },
}

/// Read-only hook report; never fails, folding parse errors into `HookStatus::InvalidSettings`.
/// `resolved_settings_path` is the symlink target a write would land on, shown in the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookInspection {
    pub settings_path: PathBuf,
    pub resolved_settings_path: PathBuf,
    pub status: HookStatus,
    pub snapshot_path: PathBuf,
    pub has_previous_snapshot: bool,
}

/// What apply must do with `<AGYLINE_HOME>/state/previous_statusline.json`, decided at plan time.
/// The file holds the exact pre-install `statusLine` object so uninstall can restore it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotAction {
    None,
    /// Captures a foreign prior hook; only planned when no snapshot file exists yet.
    SaveNew {
        path: PathBuf,
        snapshot_json: String,
    },
    /// Repeat install: keep the existing snapshot so restore still reaches the original hook.
    PreserveExisting {
        path: PathBuf,
    },
    /// Uninstall puts the saved object back, then deletes the snapshot file.
    RestoreFrom {
        path: PathBuf,
    },
    /// `--reset`: delete the snapshot without restoring; `statusLine` is removed instead.
    DiscardByReset {
        path: PathBuf,
    },
}

/// A fully computed settings.json write: exact new bytes, the diff to show first, and the
/// snapshot step. Both `--dry-run` and the TUI preview render this without applying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsMutationPlan {
    pub settings_path: PathBuf,
    pub resolved_settings_path: PathBuf,
    pub new_settings_json: String,
    pub diff: String,
    pub snapshot_action: SnapshotAction,
}

/// `AlreadyInstalled` is a no-op even without `--yes`; `WouldChange` carries everything apply
/// needs, so applying never re-reads settings.json or re-decides the command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPlan {
    AlreadyInstalled {
        settings_path: PathBuf,
        resolved_settings_path: PathBuf,
        command: String,
    },
    WouldChange {
        command: String,
        mutation: SettingsMutationPlan,
    },
}

/// `NotInstalled` and `ForeignStatusLine` never write settings.json; they may still carry a
/// `purge_dir`, which is previewed and confirmed separately from the settings change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallPlan {
    NotInstalled {
        settings_path: PathBuf,
        purge_dir: Option<PathBuf>,
    },
    ForeignStatusLine {
        settings_path: PathBuf,
        current_command: Option<String>,
        purge_dir: Option<PathBuf>,
    },
    WouldChange {
        mutation: SettingsMutationPlan,
        purge_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub updated_settings: Option<PathBuf>,
    pub snapshot_saved: Option<PathBuf>,
    pub snapshot_restored: Option<PathBuf>,
    pub purged_dir: Option<PathBuf>,
}

pub fn inspect_hook(env: &InstallEnv) -> HookInspection {
    let snapshot_path = env.snapshot_path();
    let has_previous_snapshot = snapshot_path.is_file();
    // Inspection must not fail; an unresolvable link falls back to the literal path.
    let resolved_settings_path = resolve_target_settings_path(&env.settings_path)
        .unwrap_or_else(|_| env.settings_path.clone());

    let status = match read_settings_object(&resolved_settings_path) {
        Ok((false, _, _)) => HookStatus::MissingSettings,
        Ok((true, _, root)) => match root.get("statusLine") {
            None => HookStatus::NotConfigured,
            Some(Value::Object(sl)) => {
                let cmd = sl
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let enabled = sl.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                if let Some(ref c) = cmd
                    && command_invokes_agyline(c)
                {
                    HookStatus::Connected {
                        command: c.clone(),
                        enabled,
                    }
                } else {
                    HookStatus::ForeignStatusLine { command: cmd }
                }
            }
            // A non-object statusLine is still someone's config; treat it as foreign, not absent.
            Some(
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_),
            ) => HookStatus::ForeignStatusLine { command: None },
        },
        Err(err) => HookStatus::InvalidSettings {
            reason: err.to_string(),
        },
    };

    HookInspection {
        settings_path: env.settings_path.clone(),
        resolved_settings_path,
        status,
        snapshot_path,
        has_previous_snapshot,
    }
}

/// True when the invoked program (not its arguments) is agyline: `agyline`, `agyline.exe`, or a
/// Windows 8.3 `agylin~*.exe` alias, case-insensitive. Misclassifying a foreign hook as ours
/// would let uninstall delete it, so matching stops at the program leaf.
pub fn command_invokes_agyline(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return false;
    }

    let program = extract_invoked_program(trimmed);
    if program.is_empty() {
        return false;
    }

    let leaf = program.rsplit(['/', '\\']).next().unwrap_or(program).trim();
    if leaf.eq_ignore_ascii_case("agyline") || leaf.eq_ignore_ascii_case("agyline.exe") {
        return true;
    }
    // Install's 8.3 fallback writes commands like `AGYLIN~1.EXE`; uninstall must recognize them.
    let lower = leaf.to_ascii_lowercase();
    lower.starts_with("agylin~") && lower.ends_with(".exe")
}

fn extract_invoked_program(trimmed: &str) -> &str {
    if let Some(after_quote) = trimmed.strip_prefix('\'') {
        // Parse POSIX single-quoted command, handling escaped '\'' sequences; return full unquoted leaf segment.
        if let Some(end_quote) = trimmed.rfind('\'')
            && end_quote > 0
        {
            let tail = trimmed[end_quote + 1..].trim();
            if tail.is_empty() {
                return &trimmed[1..end_quote];
            }
        }
        return after_quote.split('\'').next().unwrap_or("");
    }
    if let Some(after_dquote) = trimmed.strip_prefix('"') {
        return after_dquote.split('"').next().unwrap_or("");
    }
    // Unquoted Windows or Unix command: if there are no arguments, the whole string is the path;
    // otherwise the first whitespace-separated token is the executable.
    trimmed.split_whitespace().next().unwrap_or("")
}

/// Reads only; the returned plan is everything apply needs. Equality is on the whole statusLine
/// object, so a hook the user disabled or edited is re-pointed and re-enabled, not skipped.
pub fn plan_install(env: &InstallEnv) -> Result<InstallPlan, InstallError> {
    let resolved_command = resolve_command_string(env)?;
    let resolved_settings_path = resolve_target_settings_path(&env.settings_path)?;
    let (old_exists, old_raw, mut root) = read_settings_object(&resolved_settings_path)?;

    let existing_sl = root.get("statusLine").cloned();
    let new_sl_map = build_installed_status_line(existing_sl.as_ref(), &resolved_command);
    let new_sl_val = Value::Object(new_sl_map);

    if old_exists && existing_sl.as_ref() == Some(&new_sl_val) {
        return Ok(InstallPlan::AlreadyInstalled {
            settings_path: env.settings_path.clone(),
            resolved_settings_path,
            command: resolved_command,
        });
    }

    let snapshot_path = env.snapshot_path();
    // Snapshot only a foreign prior hook, and never overwrite an existing snapshot: re-installs
    // must keep the path back to the original statusLine, not to our own previous command.
    let snapshot_action = match existing_sl.as_ref() {
        Some(Value::Object(prev_obj)) => {
            let prev_is_agyline = prev_obj
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(command_invokes_agyline);
            if prev_is_agyline {
                if snapshot_path.is_file() {
                    SnapshotAction::PreserveExisting {
                        path: snapshot_path,
                    }
                } else {
                    SnapshotAction::None
                }
            } else if snapshot_path.is_file() {
                SnapshotAction::PreserveExisting {
                    path: snapshot_path,
                }
            } else {
                let snap_json = serialize_json_pretty(&Value::Object(prev_obj.clone()))?;
                SnapshotAction::SaveNew {
                    path: snapshot_path,
                    snapshot_json: snap_json,
                }
            }
        }
        None
        | Some(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_),
        ) => {
            if snapshot_path.is_file() {
                SnapshotAction::PreserveExisting {
                    path: snapshot_path,
                }
            } else {
                SnapshotAction::None
            }
        }
    };

    // `Map` preserves insertion order (serde_json preserve_order), so replacing statusLine in
    // place keeps every sibling key and its position.
    root.insert("statusLine".to_string(), new_sl_val);
    let new_settings_json = serialize_json_pretty(&Value::Object(root))?;
    // Diff against normalized text so CRLF or a missing final newline is not shown as a change.
    let old_for_diff = if old_exists {
        normalize_json_for_diff(&old_raw)
    } else {
        String::new()
    };
    let diff = unified_diff(
        "settings.json",
        old_exists,
        &old_for_diff,
        &new_settings_json,
    );

    Ok(InstallPlan::WouldChange {
        command: resolved_command,
        mutation: SettingsMutationPlan {
            settings_path: env.settings_path.clone(),
            resolved_settings_path,
            new_settings_json,
            diff,
            snapshot_action,
        },
    })
}

/// Writes the plan: snapshot first, then settings.json, so a failed settings write still leaves
/// the restore path intact. `AlreadyInstalled` writes nothing, even without `--yes`.
pub fn apply_install(env: &InstallEnv, plan: &InstallPlan) -> Result<ApplyOutcome, InstallError> {
    match plan {
        InstallPlan::AlreadyInstalled { .. } => Ok(ApplyOutcome {
            updated_settings: None,
            snapshot_saved: None,
            snapshot_restored: None,
            purged_dir: None,
        }),
        InstallPlan::WouldChange {
            command: _,
            mutation,
        } => {
            let mut snapshot_saved = None;
            match &mutation.snapshot_action {
                SnapshotAction::SaveNew {
                    path,
                    snapshot_json,
                } => {
                    // Re-check at apply time: a snapshot created since planning must win.
                    if !path.exists() {
                        ensure_state_dir(&env.agyline_home).map_err(|err| InstallError::Io {
                            path: env.agyline_home.join("state"),
                            message: err.to_string(),
                        })?;
                        atomic_write_text(path, snapshot_json)?;
                        snapshot_saved = Some(path.clone());
                    }
                }
                SnapshotAction::None
                | SnapshotAction::PreserveExisting { .. }
                | SnapshotAction::RestoreFrom { .. }
                | SnapshotAction::DiscardByReset { .. } => {}
            }

            // Write the resolved target, not the link, so the user's symlink survives.
            atomic_write_text(
                &mutation.resolved_settings_path,
                &mutation.new_settings_json,
            )?;
            Ok(ApplyOutcome {
                updated_settings: Some(mutation.settings_path.clone()),
                snapshot_saved,
                snapshot_restored: None,
                purged_dir: None,
            })
        }
    }
}

/// Pure read. Only a hook that invokes agyline is changed; a foreign statusLine is reported and
/// left byte-for-byte intact. With a snapshot, restore it; `--reset` removes statusLine instead.
pub fn plan_uninstall(
    env: &InstallEnv,
    opts: UninstallOptions,
) -> Result<UninstallPlan, InstallError> {
    let purge_dir = if opts.purge {
        validate_safe_purge_path(&env.agyline_home, env.user_home.as_deref())?;
        if env.agyline_home.exists() {
            Some(env.agyline_home.clone())
        } else {
            None
        }
    } else {
        None
    };

    let resolved_settings_path = resolve_target_settings_path(&env.settings_path)?;
    let (old_exists, old_raw, mut root) = read_settings_object(&resolved_settings_path)?;
    if !old_exists {
        return Ok(UninstallPlan::NotInstalled {
            settings_path: env.settings_path.clone(),
            purge_dir,
        });
    }

    let Some(existing_sl) = root.get("statusLine") else {
        return Ok(UninstallPlan::NotInstalled {
            settings_path: env.settings_path.clone(),
            purge_dir,
        });
    };

    let Some(sl_obj) = existing_sl.as_object() else {
        return Ok(UninstallPlan::ForeignStatusLine {
            settings_path: env.settings_path.clone(),
            current_command: None,
            purge_dir,
        });
    };

    let current_cmd = sl_obj
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_agyline = current_cmd.as_deref().is_some_and(command_invokes_agyline);
    if !is_agyline {
        return Ok(UninstallPlan::ForeignStatusLine {
            settings_path: env.settings_path.clone(),
            current_command: current_cmd,
            purge_dir,
        });
    }

    let snapshot_path = env.snapshot_path();
    let (new_root, snapshot_action) = if snapshot_path.is_file() && !opts.reset {
        let restored_sl = read_snapshot_object(&snapshot_path)?;
        root.insert("statusLine".to_string(), Value::Object(restored_sl));
        (
            root,
            SnapshotAction::RestoreFrom {
                path: snapshot_path,
            },
        )
    } else {
        // Rebuild the map in order: `swap_remove` would reorder the user's remaining keys.
        let filtered: Map<String, Value> = root
            .into_iter()
            .filter(|(k, _)| k != "statusLine")
            .collect();
        let snap_action = if snapshot_path.is_file() {
            SnapshotAction::DiscardByReset {
                path: snapshot_path,
            }
        } else {
            SnapshotAction::None
        };
        (filtered, snap_action)
    };

    let new_settings_json = serialize_json_pretty(&Value::Object(new_root))?;
    let old_for_diff = normalize_json_for_diff(&old_raw);
    let diff = unified_diff("settings.json", true, &old_for_diff, &new_settings_json);

    Ok(UninstallPlan::WouldChange {
        mutation: SettingsMutationPlan {
            settings_path: env.settings_path.clone(),
            resolved_settings_path,
            new_settings_json,
            diff,
            snapshot_action,
        },
        purge_dir,
    })
}

/// Settings first, then snapshot cleanup, then purge: the restore is durable before the snapshot
/// is deleted, and a purge failure cannot undo an already-restored settings.json.
pub fn apply_uninstall(
    _env: &InstallEnv,
    _plan: &UninstallPlan,
) -> Result<ApplyOutcome, InstallError> {
    match _plan {
        UninstallPlan::NotInstalled {
            settings_path: _,
            purge_dir,
        }
        | UninstallPlan::ForeignStatusLine {
            settings_path: _,
            current_command: _,
            purge_dir,
        } => {
            let purged_dir = execute_purge(purge_dir.as_deref())?;
            Ok(ApplyOutcome {
                updated_settings: None,
                snapshot_saved: None,
                snapshot_restored: None,
                purged_dir,
            })
        }
        UninstallPlan::WouldChange {
            mutation,
            purge_dir,
        } => {
            atomic_write_text(
                &mutation.resolved_settings_path,
                &mutation.new_settings_json,
            )?;
            let mut snapshot_restored = None;
            // Best-effort: settings.json is already written, so cleanup must not fail the run.
            match &mutation.snapshot_action {
                SnapshotAction::RestoreFrom { path } => {
                    let _ = fs::remove_file(path);
                    snapshot_restored = Some(path.clone());
                }
                SnapshotAction::DiscardByReset { path } => {
                    let _ = fs::remove_file(path);
                }
                SnapshotAction::None
                | SnapshotAction::SaveNew { .. }
                | SnapshotAction::PreserveExisting { .. } => {}
            }
            let purged_dir = execute_purge(purge_dir.as_deref())?;
            Ok(ApplyOutcome {
                updated_settings: Some(mutation.settings_path.clone()),
                snapshot_saved: None,
                snapshot_restored,
                purged_dir,
            })
        }
    }
}

/// Prints the diff, then applies only after `--yes` or a TTY confirmation; returns the exit code.
pub fn run_install_cli(env: &InstallEnv, opts: InstallOptions, streams: &mut CliStreams<'_>) -> u8 {
    let plan = match plan_install(env) {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    };

    match &plan {
        InstallPlan::AlreadyInstalled {
            settings_path,
            resolved_settings_path: _,
            command,
        } => {
            let _ = writeln!(
                streams.stdout,
                "agyline is already configured in {} (command: {command}).",
                settings_path.display()
            );
            let _ = streams.stdout.flush();
            0
        }
        InstallPlan::WouldChange {
            command: _,
            mutation,
        } => {
            print_mutation_preview(streams.stdout, mutation, None);
            let _ = streams.stdout.flush();

            if opts.dry_run {
                let _ = writeln!(streams.stdout, "Dry run: no files were modified.");
                let _ = streams.stdout.flush();
                return 0;
            }

            if !opts.yes
                && !confirm_or_refuse(streams, &format!("'{}'", mutation.settings_path.display()))
            {
                return 1;
            }

            match apply_install(env, &plan) {
                Ok(_) => {
                    let _ = writeln!(
                        streams.stdout,
                        "Updated {}. Restart running agy sessions for changes to take effect.",
                        mutation.settings_path.display()
                    );
                    let _ = streams.stdout.flush();
                    0
                }
                Err(err) => {
                    let _ = writeln!(streams.stderr, "error: {err}");
                    1
                }
            }
        }
    }
}

/// Prints the diff, then applies only after `--yes` or a TTY confirmation; returns the exit code.
pub fn run_uninstall_cli(
    env: &InstallEnv,
    opts: UninstallOptions,
    streams: &mut CliStreams<'_>,
) -> u8 {
    let plan = match plan_uninstall(env, opts) {
        Ok(p) => p,
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            return 1;
        }
    };

    match &plan {
        UninstallPlan::NotInstalled {
            settings_path,
            purge_dir,
        } => {
            let _ = writeln!(
                streams.stdout,
                "agyline is not currently configured in {}; leaving settings.json untouched.",
                settings_path.display()
            );
            handle_purge_only_plan(env, &plan, purge_dir.as_deref(), opts, streams)
        }
        UninstallPlan::ForeignStatusLine {
            settings_path,
            current_command,
            purge_dir,
        } => {
            let cmd_desc = current_command.as_deref().unwrap_or("<non-command>");
            let _ = writeln!(
                streams.stdout,
                "statusLine in {} points to a non-agyline command ({cmd_desc}); leaving settings.json untouched.",
                settings_path.display()
            );
            handle_purge_only_plan(env, &plan, purge_dir.as_deref(), opts, streams)
        }
        UninstallPlan::WouldChange {
            mutation,
            purge_dir,
        } => {
            print_mutation_preview(streams.stdout, mutation, purge_dir.as_deref());
            let _ = streams.stdout.flush();

            if opts.dry_run {
                let _ = writeln!(streams.stdout, "Dry run: no files were modified.");
                let _ = streams.stdout.flush();
                return 0;
            }

            if !opts.yes
                && !confirm_or_refuse(streams, &format!("'{}'", mutation.settings_path.display()))
            {
                return 1;
            }

            match apply_uninstall(env, &plan) {
                Ok(outcome) => {
                    let _ = writeln!(
                        streams.stdout,
                        "Updated {}. Restart running agy sessions for changes to take effect.",
                        mutation.settings_path.display()
                    );
                    if let Some(purged) = outcome.purged_dir {
                        let _ = writeln!(streams.stdout, "Removed {}.", purged.display());
                    }
                    let _ = streams.stdout.flush();
                    0
                }
                Err(err) => {
                    let _ = writeln!(streams.stderr, "error: {err}");
                    1
                }
            }
        }
    }
}

/// A purge-only uninstall still previews and confirms the directory before deleting it.
fn handle_purge_only_plan(
    env: &InstallEnv,
    plan: &UninstallPlan,
    purge_dir: Option<&Path>,
    opts: UninstallOptions,
    streams: &mut CliStreams<'_>,
) -> u8 {
    let Some(dir) = purge_dir else {
        let _ = streams.stdout.flush();
        return 0;
    };

    let _ = writeln!(streams.stdout, "Purge directory: {}", dir.display());
    let _ = streams.stdout.flush();

    if opts.dry_run {
        let _ = writeln!(streams.stdout, "Dry run: no files were modified.");
        let _ = streams.stdout.flush();
        return 0;
    }

    if !opts.yes && !confirm_or_refuse(streams, &format!("'{}'", dir.display())) {
        return 1;
    }

    match apply_uninstall(env, plan) {
        Ok(outcome) => {
            if let Some(purged) = outcome.purged_dir {
                let _ = writeln!(streams.stdout, "Removed {}.", purged.display());
            }
            let _ = streams.stdout.flush();
            0
        }
        Err(err) => {
            let _ = writeln!(streams.stderr, "error: {err}");
            1
        }
    }
}

fn print_mutation_preview(
    stdout: &mut dyn Write,
    mutation: &SettingsMutationPlan,
    purge_dir: Option<&Path>,
) {
    let _ = writeln!(stdout, "Target: {}", mutation.settings_path.display());
    if mutation.resolved_settings_path != mutation.settings_path {
        let _ = writeln!(
            stdout,
            "Symlink target: {}",
            mutation.resolved_settings_path.display()
        );
    }
    match &mutation.snapshot_action {
        SnapshotAction::SaveNew { path, .. } => {
            let _ = writeln!(stdout, "Snapshot prior statusLine: {}", path.display());
        }
        SnapshotAction::RestoreFrom { path } => {
            let _ = writeln!(stdout, "Restore prior statusLine: {}", path.display());
        }
        SnapshotAction::None
        | SnapshotAction::PreserveExisting { .. }
        | SnapshotAction::DiscardByReset { .. } => {}
    }
    if let Some(dir) = purge_dir {
        let _ = writeln!(stdout, "Purge directory: {}", dir.display());
    }
    let _ = write!(stdout, "{}", mutation.diff);
}

fn confirm_or_refuse(streams: &mut CliStreams<'_>, target_desc: &str) -> bool {
    // Even a piped "y" is refused: confirmation requires a real TTY or an explicit --yes.
    if !streams.stdin_is_terminal || !streams.stdout_is_terminal {
        let _ = writeln!(
            streams.stderr,
            "error: refusing to modify {target_desc} on a non-interactive stream without '--yes' ('-y')"
        );
        return false;
    }

    let _ = write!(streams.stdout, "Apply changes? [y/N]: ");
    let _ = streams.stdout.flush();
    if read_yes_confirmation(streams.stdin) {
        true
    } else {
        let _ = writeln!(streams.stderr, "Cancelled; no changes written.");
        false
    }
}

fn read_yes_confirmation(stdin: &mut dyn Read) -> bool {
    let mut line = Vec::with_capacity(16);
    let mut byte = [0u8; 1];
    while line.len() < 256 {
        match stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    let answer = String::from_utf8_lossy(&line);
    let trimmed = answer.trim();
    trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes")
}

fn execute_purge(purge_dir: Option<&Path>) -> Result<Option<PathBuf>, InstallError> {
    let Some(dir) = purge_dir else {
        return Ok(None);
    };
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(Some(dir.to_path_buf())),
        // A directory that vanished between plan and apply is not an error.
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(InstallError::Io {
            path: dir.to_path_buf(),
            message: err.to_string(),
        }),
    }
}

/// Guards `--purge`: refuses an empty path, a filesystem root, or the user's home (compared
/// canonically, so a symlink to home is caught too). `remove_dir_all` on those is unrecoverable.
fn validate_safe_purge_path(
    agyline_home: &Path,
    user_home: Option<&Path>,
) -> Result<(), InstallError> {
    if agyline_home.as_os_str().is_empty() || agyline_home.parent().is_none() {
        return Err(InstallError::UnsafePurgePath {
            path: agyline_home.to_path_buf(),
        });
    }
    if let Some(home) = user_home {
        if agyline_home == home {
            return Err(InstallError::UnsafePurgePath {
                path: agyline_home.to_path_buf(),
            });
        }
        if let (Ok(ca), Ok(ch)) = (fs::canonicalize(agyline_home), fs::canonicalize(home))
            && ca == ch
        {
            return Err(InstallError::UnsafePurgePath {
                path: agyline_home.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Repoints only type/command/enabled, preserving extra statusLine subkeys like `padding`.
/// Existing canonical keys keep their positions; otherwise the three lead and extras follow.
fn build_installed_status_line(existing: Option<&Value>, command: &str) -> Map<String, Value> {
    if let Some(Value::Object(prev)) = existing {
        if prev.contains_key("type") && prev.contains_key("command") && prev.contains_key("enabled")
        {
            let mut sl = prev.clone();
            sl.insert("type".to_string(), Value::String("command".to_string()));
            sl.insert("command".to_string(), Value::String(command.to_string()));
            sl.insert("enabled".to_string(), Value::Bool(true));
            return sl;
        }
        let mut sl = Map::new();
        sl.insert("type".to_string(), Value::String("command".to_string()));
        sl.insert("command".to_string(), Value::String(command.to_string()));
        sl.insert("enabled".to_string(), Value::Bool(true));
        for (k, v) in prev {
            if k != "type" && k != "command" && k != "enabled" {
                sl.insert(k.clone(), v.clone());
            }
        }
        return sl;
    }

    let mut sl = Map::new();
    sl.insert("type".to_string(), Value::String("command".to_string()));
    sl.insert("command".to_string(), Value::String(command.to_string()));
    sl.insert("enabled".to_string(), Value::Bool(true));
    sl
}

/// Resolves settings.json to the file a write must land on, so the symlink itself is never
/// replaced. Handles dangling links (returns the ultimate target) and caps chains at 32 hops.
fn resolve_target_settings_path(settings_path: &Path) -> Result<PathBuf, InstallError> {
    match fs::symlink_metadata(settings_path) {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(settings_path.to_path_buf()),
        Err(err) => Err(InstallError::Io {
            path: settings_path.to_path_buf(),
            message: err.to_string(),
        }),
        Ok(meta) => {
            if settings_path.exists()
                && let Ok(canon) = fs::canonicalize(settings_path)
            {
                return Ok(clean_canonical_path(canon));
            }
            if meta.file_type().is_symlink() {
                // canonicalize fails on a dangling link; walk manually, capped against loops.
                let mut cur = settings_path.to_path_buf();
                for _ in 0..32 {
                    match fs::symlink_metadata(&cur) {
                        Ok(m) if m.file_type().is_symlink() => {
                            let target = fs::read_link(&cur).map_err(|err| InstallError::Io {
                                path: cur.clone(),
                                message: err.to_string(),
                            })?;
                            cur = if target.is_absolute() {
                                target
                            } else {
                                cur.parent().unwrap_or_else(|| Path::new(".")).join(target)
                            };
                        }
                        Ok(_) => return Ok(cur),
                        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(cur),
                        Err(err) => {
                            return Err(InstallError::Io {
                                path: cur,
                                message: err.to_string(),
                            });
                        }
                    }
                }
                return Err(InstallError::Io {
                    path: settings_path.to_path_buf(),
                    message: "too many levels of symbolic links".to_string(),
                });
            }
            Ok(settings_path.to_path_buf())
        }
    }
}

/// Strips the `\\?\` verbatim prefix from Windows `canonicalize` output so previews and writes
/// use the conventional path form.
fn clean_canonical_path(path: PathBuf) -> PathBuf {
    if let Some(s) = path.to_str() {
        let stripped = strip_windows_verbatim(s);
        if stripped.len() != s.len() {
            return PathBuf::from(stripped);
        }
    }
    path
}

/// Returns (exists, raw body, parsed object). A missing file is a valid empty settings, not an
/// error; anything that is not a JSON object is rejected so a malformed file is never rewritten.
fn read_settings_object(path: &Path) -> Result<(bool, String, Map<String, Value>), InstallError> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Ok((false, String::new(), Map::new()));
        }
        Err(err) => {
            return Err(InstallError::Io {
                path: path.to_path_buf(),
                message: err.to_string(),
            });
        }
    };
    // serde_json rejects a leading UTF-8 BOM, which Windows editors may add; strip it first.
    let body = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let val: Value =
        serde_json::from_str(body).map_err(|err| InstallError::InvalidSettingsJson {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
    match val {
        Value::Object(map) => Ok((true, body.to_string(), map)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err(InstallError::InvalidSettingsJson {
                path: path.to_path_buf(),
                reason: format!(
                    "expected top-level JSON object, found {}",
                    json_type_name(&val)
                ),
            })
        }
    }
}

/// A corrupt snapshot aborts uninstall (`InvalidSnapshotJson` suggests `--reset`) rather than
/// restoring garbage over the user's current hook.
fn read_snapshot_object(path: &Path) -> Result<Map<String, Value>, InstallError> {
    let raw = fs::read_to_string(path).map_err(|err| InstallError::Io {
        path: path.to_path_buf(),
        message: err.to_string(),
    })?;
    let body = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let val: Value =
        serde_json::from_str(body).map_err(|err| InstallError::InvalidSnapshotJson {
            path: path.to_path_buf(),
            reason: err.to_string(),
        })?;
    match val {
        Value::Object(map) => Ok(map),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err(InstallError::InvalidSnapshotJson {
                path: path.to_path_buf(),
                reason: format!("expected JSON object, found {}", json_type_name(&val)),
            })
        }
    }
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn serialize_json_pretty(val: &Value) -> Result<String, InstallError> {
    let mut s = serde_json::to_string_pretty(val).map_err(|err| InstallError::Io {
        path: PathBuf::from("<json>"),
        message: err.to_string(),
    })?;
    // Match `normalize_json_for_diff` so the final newline never shows up as a diff hunk.
    s.push('\n');
    Ok(s)
}

/// Normalizes CRLF and the final newline so the diff shows only real content changes.
fn normalize_json_for_diff(raw: &str) -> String {
    let mut normalized = raw.replace("\r\n", "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized
}

/// tmp-file + rename so readers (agy) never see partial JSON and a failure leaves the original
/// intact. Preserves the target's permissions and retries transient Windows rename denials.
fn atomic_write_text(target_path: &Path, content: &str) -> Result<(), InstallError> {
    let parent = target_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| InstallError::Io {
        path: parent.to_path_buf(),
        message: err.to_string(),
    })?;

    // Copy the old permissions onto the tmp so the rename cannot silently change the file's mode.
    let existing_perms = fs::metadata(target_path).ok().map(|m| m.permissions());
    let file_name = target_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = parent.join(format!(
        ".{file_name}.agyline.tmp.{}.{}.{}",
        std::process::id(),
        nanos,
        seq
    ));

    {
        // `create_new` refuses to reuse a colliding tmp name instead of truncating someone's file.
        let mut tmp_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|err| InstallError::Io {
                path: tmp_path.clone(),
                message: err.to_string(),
            })?;
        // sync_all before the rename so a power loss cannot swap in a truncated settings.json.
        if let Err(err) = tmp_file
            .write_all(content.as_bytes())
            .and_then(|()| tmp_file.sync_all())
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(InstallError::Io {
                path: tmp_path,
                message: err.to_string(),
            });
        }
    }

    if let Some(perms) = existing_perms {
        let _ = fs::set_permissions(&tmp_path, perms);
    }

    let mut last_err = None;
    // Brief retries: Windows antivirus or a concurrent reader can transiently deny the rename.
    for attempt in 0..4 {
        match fs::rename(&tmp_path, target_path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                let retryable = err.kind() == ErrorKind::PermissionDenied;
                last_err = Some(err);
                if !retryable || attempt == 3 {
                    break;
                }
                thread::sleep(Duration::from_millis(4));
            }
        }
    }

    let _ = fs::remove_file(&tmp_path);
    let message = last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "atomic rename failed".to_string());
    Err(InstallError::Io {
        path: target_path.to_path_buf(),
        message,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Hand-rolled unified diff (3 context lines) so the preview needs no external `diff` binary.
fn unified_diff(label: &str, old_exists: bool, old_text: &str, new_text: &str) -> String {
    let new_lines: Vec<&str> = split_diff_lines(new_text);
    if !old_exists {
        let mut out = format!("--- /dev/null\n+++ b/{label}\n");
        out.push_str(&format!("@@ -0,0 +1,{} @@\n", new_lines.len()));
        for line in new_lines {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
        return out;
    }

    let old_lines: Vec<&str> = split_diff_lines(old_text);
    let ops = compute_lcs_ops(&old_lines, &new_lines);
    let context = 3usize;

    let mut change_indices = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        match op {
            DiffOp::Equal(_) => {}
            DiffOp::Delete(_) | DiffOp::Insert(_) => change_indices.push(idx),
        }
    }
    if change_indices.is_empty() {
        return String::new();
    }

    let mut groups: Vec<(usize, usize)> = Vec::new();
    for idx in change_indices {
        let start = idx.saturating_sub(context);
        let end = (idx + context + 1).min(ops.len());
        if let Some(last) = groups.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            groups.push((start, end));
        }
    }

    let mut out = format!("--- a/{label}\n+++ b/{label}\n");
    for (start, end) in groups {
        let mut old_line_no = 1usize;
        let mut new_line_no = 1usize;
        for op in &ops[..start] {
            match op {
                DiffOp::Equal(_) => {
                    old_line_no += 1;
                    new_line_no += 1;
                }
                DiffOp::Delete(_) => old_line_no += 1,
                DiffOp::Insert(_) => new_line_no += 1,
            }
        }
        let mut old_count = 0usize;
        let mut new_count = 0usize;
        for op in &ops[start..end] {
            match op {
                DiffOp::Equal(_) => {
                    old_count += 1;
                    new_count += 1;
                }
                DiffOp::Delete(_) => old_count += 1,
                DiffOp::Insert(_) => new_count += 1,
            }
        }
        let old_start = if old_count == 0 {
            old_line_no.saturating_sub(1)
        } else {
            old_line_no
        };
        let new_start = if new_count == 0 {
            new_line_no.saturating_sub(1)
        } else {
            new_line_no
        };
        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for op in &ops[start..end] {
            match op {
                DiffOp::Equal(line) => {
                    out.push(' ');
                    out.push_str(line);
                    out.push('\n');
                }
                DiffOp::Delete(line) => {
                    out.push('-');
                    out.push_str(line);
                    out.push('\n');
                }
                DiffOp::Insert(line) => {
                    out.push('+');
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn split_diff_lines(text: &str) -> Vec<&str> {
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('\n').collect()
    }
}

fn compute_lcs_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp<'a>> {
    let m = old.len();
    let n = new.len();
    let cols = n + 1;
    let mut dp = vec![0usize; (m + 1) * cols];

    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * cols + j] = if old[i] == new[j] {
                dp[(i + 1) * cols + (j + 1)] + 1
            } else {
                dp[(i + 1) * cols + j].max(dp[i * cols + (j + 1)])
            };
        }
    }

    let mut ops = Vec::with_capacity(m + n);
    let mut i = 0;
    let mut j = 0;
    while i < m && j < n {
        if old[i] == new[j] {
            ops.push(DiffOp::Equal(old[i]));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)] {
            ops.push(DiffOp::Delete(old[i]));
            i += 1;
        } else {
            ops.push(DiffOp::Insert(new[j]));
            j += 1;
        }
    }
    while i < m {
        ops.push(DiffOp::Delete(old[i]));
        i += 1;
    }
    while j < n {
        ops.push(DiffOp::Insert(new[j]));
        j += 1;
    }
    ops
}

/// The command agy will run. Windows: clean exe path (tilde-shortened), else bare `agyline` if
/// PATH resolves to this same binary, else a clean 8.3 short path, else refuse. Unix: `~/...`
/// when clean, otherwise a single-quoted path with `'\''` escapes.
pub fn resolve_command_string(env: &InstallEnv) -> Result<String, InstallError> {
    match env.platform {
        PlatformKind::Windows => resolve_windows_command(env),
        PlatformKind::Unix => resolve_unix_command(env),
    }
}

fn resolve_windows_command(env: &InstallEnv) -> Result<String, InstallError> {
    let Some(raw_exe) = env.exe_path.to_str() else {
        return Err(InstallError::NonUtf8Path {
            path: env.exe_path.clone(),
        });
    };
    let exe_norm = normalize_windows_slashes(strip_windows_verbatim(raw_exe));

    if is_clean_windows_cmd_path(&exe_norm) {
        // Prefer `~/...` like the reference hook; only when both exe and home are cmd-clean.
        if let Some(raw_home) = env.user_home.as_deref().and_then(Path::to_str) {
            let home_slashes = normalize_windows_slashes(strip_windows_verbatim(raw_home));
            let home_norm = trim_trailing_slash_windows(&home_slashes);
            if let Some(rel) = strip_home_prefix_windows(&exe_norm, home_norm)
                && is_clean_windows_cmd_path(home_norm)
            {
                return Ok(format!("~/{rel}"));
            }
        }
        return Ok(exe_norm);
    }

    // Bare name only if PATH's first agyline hit is this exact binary, not a shadowing older copy.
    if bare_agyline_resolves_to_same_exe(env) {
        return Ok("agyline".to_string());
    }

    // 8.3 aliases have no spaces, so cmd.exe runs them unquoted; last resort before refusing.
    if let Some(short_pb) = (env.short_path_resolver)(&env.exe_path)
        && let Some(short_str) = short_pb.to_str()
    {
        let short_norm = normalize_windows_slashes(strip_windows_verbatim(short_str));
        if !short_norm.is_empty() && is_clean_windows_cmd_path(&short_norm) {
            return Ok(short_norm);
        }
    }

    let parent_hint = env
        .exe_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<agyline-bin-dir>".to_string());
    Err(InstallError::UnsafeCommandPath {
        exe_path: env.exe_path.clone(),
        reason: format!(
            "path contains spaces or cmd.exe metacharacters, bare 'agyline' is not first on PATH, and no clean 8.3 short path is available. Add '{parent_hint}' to PATH (then restart your terminal), move agyline.exe to a directory without spaces/special characters, or enable 8.3 short names on the volume."
        ),
    })
}

fn resolve_unix_command(env: &InstallEnv) -> Result<String, InstallError> {
    let Some(exe_str) = env.exe_path.to_str() else {
        return Err(InstallError::NonUtf8Path {
            path: env.exe_path.clone(),
        });
    };

    if let Some(raw_home) = env.user_home.as_deref().and_then(Path::to_str) {
        let home = raw_home.trim_end_matches('/');
        if !home.is_empty()
            && let Some(rest) = exe_str.strip_prefix(home)
            && let Some(rel) = rest.strip_prefix('/')
            && !rel.is_empty()
            && is_clean_unix_sh_path(exe_str)
        {
            return Ok(format!("~/{rel}"));
        }
    }

    // Single quotes literalize everything but `'`; `'\''` closes, escapes one quote, reopens.
    Ok(format!("'{}'", exe_str.replace('\'', "'\\''")))
}

/// Strip `\\?\` only for drive-letter paths; `\\?\UNC\` needs the slash-normalizer instead.
fn strip_windows_verbatim(path: &str) -> &str {
    if let Some(rest) = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return rest;
    }
    path
}

/// cmd.exe accepts forward slashes; UNC verbatim paths become the `//server/share` form.
fn normalize_windows_slashes(path: &str) -> String {
    if let Some(unc_rest) = path
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| path.strip_prefix("//?/UNC/"))
    {
        return format!("//{}", unc_rest.replace('\\', "/"));
    }
    path.replace('\\', "/")
}

fn trim_trailing_slash_windows(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    // Keep the slash on `C:/`: bare `C:` means the drive's current directory, not the root.
    if trimmed.len() == 2 && trimmed.as_bytes().get(1) == Some(&b':') {
        return path;
    }
    trimmed
}

fn strip_home_prefix_windows<'a>(exe_norm: &'a str, home_norm: &str) -> Option<&'a str> {
    if home_norm.is_empty() || exe_norm.len() <= home_norm.len() {
        return None;
    }
    let (prefix, rest) = exe_norm.split_at_checked(home_norm.len())?;
    if !prefix.eq_ignore_ascii_case(home_norm) {
        return None;
    }
    // Require a `/` boundary so a home of `alice` never prefixes a sibling like `alicia`.
    let rel = rest.strip_prefix('/')?;
    if rel.is_empty() {
        return None;
    }
    Some(rel)
}

/// Rejects anything cmd.exe would parse: whitespace, quotes, metacharacters, `%`/`!` expansion.
fn is_clean_windows_cmd_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().all(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(
                    c,
                    '&' | '|' | '<' | '>' | '^' | '(' | ')' | '%' | '!' | '"' | ';' | ',' | '='
                )
        })
}

/// Rejects shell metacharacters and glob/expansion chars so the unquoted `~/...` form is safe.
fn is_clean_unix_sh_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().all(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(
                    c,
                    '\\' | '\''
                        | '"'
                        | '`'
                        | '$'
                        | '&'
                        | '|'
                        | ';'
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '['
                        | ']'
                        | '*'
                        | '?'
                        | '!'
                        | '#'
                        | '~'
                        | '%'
                )
        })
}

/// True only when PATH's first agyline candidate is this same binary, guarding against a shadowing
/// older copy that a bare `agyline` command would otherwise run.
fn bare_agyline_resolves_to_same_exe(env: &InstallEnv) -> bool {
    let Some(ref raw_path) = env.path_env else {
        return false;
    };
    let path_lossy = raw_path.to_string_lossy();
    // Mirror cmd.exe's PATHEXT order: .com, .exe, .bat, .cmd, then extensionless.
    let candidates = [
        "agyline.com",
        "agyline.exe",
        "agyline.bat",
        "agyline.cmd",
        "agyline",
    ];
    for entry in path_lossy.split(';') {
        let dir_str = entry.trim().trim_matches('"');
        if dir_str.is_empty() {
            continue;
        }
        let dir = Path::new(dir_str);
        for name in candidates {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return same_executable_file(&candidate, &env.exe_path);
            }
        }
    }
    false
}

/// Canonicalize to equate symlinks/8.3 aliases; fall back to case-insensitive text when it fails.
fn same_executable_file(a: &Path, b: &Path) -> bool {
    if let (Ok(ca), Ok(cb)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return ca == cb;
    }
    let na = normalize_windows_slashes(strip_windows_verbatim(&a.to_string_lossy()));
    let nb = normalize_windows_slashes(strip_windows_verbatim(&b.to_string_lossy()));
    na.eq_ignore_ascii_case(&nb)
}
