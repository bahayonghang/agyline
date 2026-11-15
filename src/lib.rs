pub use cli::{CliStreams, run_cli, run_cli_from_process};
pub use collect::{
    CacheFrameState, FrameData, GitFrameState, NoopSpawner, RelayStatus, Spawner, SystemSpawner,
};
pub use config::{
    ColorDepth, EffectiveConfig, GlyphSet, ItemId, OverflowMode, ThemePolarity, ThemeSetting,
    ThresholdsConfig,
};
pub use input::{MAX_STDIN_BYTES, Payload, QuotaBucketData, SubagentSummary, parse_stdin_payload};
pub use render::{
    ColorToken, RenderedFrame, StyledItem, StyledLine, StyledSpan, emit_ansi, emit_plain,
};
pub use state::ensure_state_dir;
pub use statusline::{LiveOutput, RenderEnv, render_lines, run_live_frame};

mod cli;
mod collect;
mod config;
mod input;
mod items;
mod platform;
mod render;
mod scenarios;
mod state;
mod statusline;
