pub use cli::{CliStreams, run_cli, run_cli_from_process};
pub use collect::{
    CacheFrameState, FrameData, GitFrameState, NoopSpawner, ParsedGitStatus, RelayStatus, Spawner,
    SystemSpawner, parse_porcelain_v2, refresh_git_at, resolve_git_frame,
};
pub use config::{
    ColorDepth, EffectiveConfig, GlyphSet, ItemId, OverflowMode, ThemePolarity, ThemeSetting,
    ThresholdsConfig,
};
pub use input::{MAX_STDIN_BYTES, Payload, QuotaBucketData, SubagentSummary, parse_stdin_payload};
pub use install::{
    ApplyOutcome, HookInspection, HookStatus, InstallEnv, InstallError, InstallOptions,
    InstallPlan, SettingsMutationPlan, SnapshotAction, UninstallOptions, UninstallPlan,
    apply_install, apply_uninstall, command_invokes_agyline, inspect_hook, plan_install,
    plan_uninstall, resolve_command_string, run_install_cli, run_uninstall_cli,
};
pub use platform::{PlatformKind, create_file_symlink};
pub use render::{
    ColorToken, RenderedFrame, StyledItem, StyledLine, StyledSpan, emit_ansi, emit_plain,
};
pub use state::{
    GitCacheEntry, cwd_hash_hex, ensure_state_dir, git_cache_path, git_lock_path,
    maybe_sweep_stale_state, normalize_cwd_lexical, read_git_cache, write_git_cache,
};
pub use statusline::{LiveOutput, RenderEnv, render_lines, run_live_frame};

mod cli;
mod collect;
mod config;
mod input;
mod install;
mod items;
mod platform;
mod render;
mod scenarios;
mod state;
mod statusline;
