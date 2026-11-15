use std::ffi::OsStr;

pub use self::git::{
    ParsedGitStatus, parse_porcelain_v2, refresh_git_at, resolve_git_frame, run_hidden_refresh_git,
};
use crate::config::EffectiveConfig;
use crate::input::Payload;
use crate::platform;
use crate::statusline::RenderEnv;

pub mod git;

pub trait Spawner: Send + Sync {
    fn spawn_refresh_git(&self, cwd: &str);
    fn spawn_touch(&self, session_id: &str, relay_url: &str);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSpawner;

impl Spawner for NoopSpawner {
    fn spawn_refresh_git(&self, _cwd: &str) {}

    fn spawn_touch(&self, _session_id: &str, _relay_url: &str) {}
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSpawner;

impl Spawner for SystemSpawner {
    fn spawn_refresh_git(&self, cwd: &str) {
        let _ = platform::spawn_detached_self(&[
            OsStr::new("__refresh-git"),
            OsStr::new("--cwd"),
            OsStr::new(cwd),
        ]);
    }

    fn spawn_touch(&self, session_id: &str, relay_url: &str) {
        let _ = platform::spawn_detached_self(&[
            OsStr::new("__touch"),
            OsStr::new("--session"),
            OsStr::new(session_id),
            OsStr::new("--url"),
            OsStr::new(relay_url),
        ]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayStatus {
    Live,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFrameState {
    pub branch: String,
    pub dirty: bool,
    pub has_conflict: bool,
    pub ahead: i64,
    pub behind: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CacheFrameState {
    pub cache_ts: Option<i64>,
    pub relay_touches: u32,
    pub relay_status: Option<RelayStatus>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameData {
    pub payload: Payload,
    pub now_sec: i64,
    pub git: Option<GitFrameState>,
    pub cache: CacheFrameState,
    pub last_tps: Option<String>,
    pub response_model: Option<String>,
}

pub fn collect_frame(payload: Payload, config: &EffectiveConfig, env: &RenderEnv<'_>) -> FrameData {
    let now_sec = (env.now_ms / 1000) as i64;
    let delta_out = 0;
    let git = git::resolve_git_frame(&payload, config, env, delta_out);

    let mut cache = CacheFrameState::default();
    if !payload.is_pre_session() {
        let is_active_turn =
            !payload.tool_confirmation_pending && !is_non_active_agent_state(&payload.agent_state);
        if is_active_turn || payload.total_output_tokens > 0 {
            cache.cache_ts = Some(now_sec);
        }
    }

    FrameData {
        payload,
        now_sec,
        git,
        cache,
        last_tps: None,
        response_model: None,
    }
}

fn is_non_active_agent_state(state: &str) -> bool {
    let s = state.trim();
    s.is_empty()
        || s.eq_ignore_ascii_case("idle")
        || s.eq_ignore_ascii_case("authenticating")
        || s.eq_ignore_ascii_case("initializing")
}
