//! Public surface of the crate. Pipeline: `input` (one stdin JSON frame) -> `collect` (session
//! state, git cache, relay, bounded SQLite) -> `items` (slots) -> `render` (layout, ANSI/plain),
//! over `config`/`state`/`platform`. `cli`, `statusline` and `tui` share a renderer whose default
//! must reproduce `ref/agy-patch`; the printing path spawns no shell and waits on no `git`.

pub use cli::{CliStreams, run_cli, run_cli_from_process};
pub use collect::{
    CacheFrameState, CollectionNeeds, FrameData, GitFrameState, NoopSpawner, ParsedGitStatus,
    RelayStatus, Spawner, SystemSpawner, TranscriptTotals, parse_porcelain_v2,
    read_transcript_start_sec_bounded, refresh_git_at, resolve_git_frame,
};
pub use config::{
    ActiveEnvOverride, ColorDepth, ConfigDoc, ConfigLoadResult, ConfigSyntaxError,
    DEFAULT_COMPACT_AT, EffectiveConfig, GlyphSet, ItemId, ItemSlot, ItemStyle, OverflowMode,
    PaletteOverrides, PresetScope, ThemePolarity, ThemeSetting, ThresholdsConfig,
    inspect_env_overrides, load_config_doc, parse_config_doc, parse_config_doc_with_home,
    presets::{PresetInfo, apply_preset, build_preset_doc, builtin_presets, discover_presets},
    resolve_separator, serialize_config_doc,
};
pub use input::{MAX_STDIN_BYTES, Payload, QuotaBucketData, SubagentSummary, parse_stdin_payload};
pub use install::{
    ApplyOutcome, HookInspection, HookStatus, InstallEnv, InstallError, InstallOptions,
    InstallPlan, SettingsMutationPlan, SnapshotAction, UninstallOptions, UninstallPlan,
    apply_install, apply_uninstall, command_invokes_agyline, inspect_hook, plan_install,
    plan_uninstall, resolve_command_string, run_install_cli, run_uninstall_cli,
};
pub use items::{
    EvaluatedItem, ItemCatalogEntry, ItemCategory, ItemOptionKind, ItemOptionSpec, catalog,
    evaluate_slot, evaluate_slot_dual,
};
pub use platform::{PlatformKind, create_file_symlink, host_platform, stdout_terminal_columns};
pub use render::{
    Ansi16Color, BarStyle, ColorSpec, ColorToken, EffectiveColorDepth, PaletteInfo,
    PalettePolarity, RenderedFrame, ResolvedColor, ResolvedPalette, SlotOutcome, StyledItem,
    StyledLine, StyledSpan, TextStyle, builtin_palettes, discover_palettes, emit_ansi, emit_plain,
    parse_color_spec, render_usage_bar, resolve_effective_color_depth, resolve_palette,
    str_display_width,
};
pub use scenarios::{
    PREVIEW_ANCHOR_SEC, Scenario, builtin_scenarios, find_scenario, stateless_frame_from_payload,
};
pub use state::{
    GitCacheEntry, cwd_hash_hex, ensure_state_dir, git_cache_path, git_lock_path,
    maybe_sweep_stale_state, normalize_cwd_lexical, read_git_cache, write_git_cache,
};
pub use statusline::{
    LiveOutput, RenderEnv, render_lines, render_lines_with_width, run_live_frame,
    run_live_frame_ansi,
};
pub use tui::{
    ConfiguratorApp, ConfiguratorEnv, ConfiguratorExit, TuiTab, render_tui_frame,
    rendered_frame_to_ratatui_lines, run_configurator,
};

mod cli;
mod collect;
mod config;
mod input;
mod install;
pub mod items;
mod platform;
mod render;
mod scenarios;
mod state;
mod statusline;
mod tui;
