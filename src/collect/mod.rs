use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub use self::git::{
    ParsedGitStatus, parse_porcelain_v2, refresh_git_at, resolve_git_frame, run_hidden_refresh_git,
};
use self::sqlite::{SqliteLookup, lookup_response_model_bounded};
use crate::config::EffectiveConfig;
use crate::input::Payload;
use crate::platform;
use crate::state::relay::read_relay_state;
use crate::state::session::{
    SESSION_STATE_VERSION, SessionState, read_db_fingerprint, read_session_state,
    write_session_state,
};
use crate::statusline::RenderEnv;

pub mod git;
pub mod sqlite;
pub mod touch;

pub trait Spawner: Send + Sync {
    fn spawn_refresh_git(&self, cwd: &str);
    fn spawn_touch(&self, session_id: &str, relay_url: &str);
    fn on_sqlite_open(&self, _db_path: &Path) {}
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

    if payload.is_pre_session() {
        let git = git::resolve_git_frame(&payload, config, env, 0);
        return FrameData {
            payload,
            now_sec,
            git,
            cache: CacheFrameState::default(),
            last_tps: None,
            response_model: None,
        };
    }

    let session_id = payload
        .session_id
        .as_deref()
        .unwrap_or_default()
        .to_string();
    let cur_model_key = compute_model_key(&payload);
    let state_trimmed = payload.agent_state.trim();
    let norm_state = if state_trimmed.is_empty() {
        "idle".to_string()
    } else {
        state_trimmed.to_string()
    };

    let loaded = read_session_state(&env.home_dir, &session_id);

    let mut prev_state = "idle".to_string();
    let mut start_time_ms = env.now_ms;
    let mut base_out = payload.total_output_tokens;
    let mut last_tps = "--".to_string();
    let mut resp_model = String::new();
    let mut cache_ts: i64 = 0;
    let mut db_fingerprint = None;

    if let Some(ref s) = loaded {
        prev_state = s.prev_state.clone();
        start_time_ms = s.start_time_ms;
        base_out = s.base_out;
        last_tps = s.last_tps.clone();
        resp_model = s.resp_model.clone();
        cache_ts = s.cache_ts;
        db_fingerprint = s.db_fingerprint.clone();
        if !s.prev_model_key.is_empty() && s.prev_model_key != cur_model_key {
            resp_model.clear();
        }
    }

    let prev_active = is_active_turn_state(&prev_state);
    let cur_active = is_active_turn_state(&norm_state);
    let tot_out = payload.total_output_tokens;
    let cur_out = payload.output_tokens;

    if tot_out < base_out {
        base_out = tot_out;
    } else if !prev_active && cur_active {
        start_time_ms = env.now_ms;
        base_out = tot_out;
    } else if !prev_active && !cur_active && base_out == 0 && tot_out > 0 {
        base_out = tot_out;
    }

    let mut delta_out = tot_out - base_out;
    if delta_out > 0 {
        if cur_out > 0 && delta_out > cur_out && !prev_active {
            delta_out = cur_out;
        }
        let elapsed_sec = (env.now_ms.saturating_sub(start_time_ms) as f64) / 1000.0;
        let dt = elapsed_sec.max(0.05);
        let tps = (delta_out as f64) / dt;
        last_tps = format_tps(tps);
        start_time_ms = env.now_ms;
        base_out = tot_out;
        cache_ts = now_sec;
    } else if payload.tool_confirmation_pending {
        start_time_ms = env.now_ms;
    } else if cur_active {
        cache_ts = now_sec;
    }

    let git = git::resolve_git_frame(&payload, config, env, delta_out);

    let conv_db = session_db_path(&env.conversations_dir, &session_id);
    let current_fp = conv_db.as_deref().and_then(read_db_fingerprint);

    if cache_ts == 0 && tot_out > 0 {
        if let Some(ref fp) = current_fp
            && let Some(ref db_stamp) = fp.db
        {
            let mtime_sec = (db_stamp.mtime_ms / 1000) as i64;
            if mtime_sec > 0 {
                cache_ts = mtime_sec;
            }
        }
        if cache_ts == 0 {
            cache_ts = now_sec;
        }
    }

    let relay_url = config
        .cache_relay_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("off"));

    let mut relay_touches: u32 = 0;
    let mut relay_status: Option<RelayStatus> = None;
    if relay_url.is_some()
        && let Some(relay_state) = read_relay_state(&env.temp_dir, &session_id)
    {
        if let Some(anchor_ts) = relay_state.anchor_ts {
            cache_ts = anchor_ts;
        }
        relay_touches = relay_state.touches;
        relay_status = relay_state.status;
    }

    let db_fingerprint_changed = db_fingerprint != current_fp;
    let should_query_sqlite =
        delta_out > 0 || (resp_model.is_empty() && tot_out > 0 && db_fingerprint_changed);

    if should_query_sqlite
        && current_fp.is_some()
        && let Some(ref db_path) = conv_db
    {
        env.spawner.on_sqlite_open(db_path);
        match lookup_response_model_bounded(db_path) {
            SqliteLookup::Found(model) => {
                resp_model = model;
                db_fingerprint = current_fp;
            }
            SqliteLookup::Empty => {
                db_fingerprint = current_fp;
            }
            SqliteLookup::Unavailable => {}
        }
    }

    let next_state = SessionState {
        version: SESSION_STATE_VERSION,
        session_id: session_id.clone(),
        prev_state: norm_state,
        start_time_ms,
        base_out,
        last_tps: last_tps.clone(),
        resp_model: resp_model.clone(),
        prev_model_key: cur_model_key,
        cache_ts,
        db_fingerprint,
    };

    if loaded.as_ref() != Some(&next_state) {
        write_session_state(&env.home_dir, &next_state);
    }

    if let Some(url) = relay_url
        && cache_ts > 0
        && relay_status == Some(RelayStatus::Live)
    {
        let cache_rem = 300_i64.saturating_sub(now_sec.saturating_sub(cache_ts));
        let tot_prompt = payload
            .input_tokens
            .max(0)
            .saturating_add(payload.cache_read_input_tokens.max(0));
        if (1..=60).contains(&cache_rem) && tot_prompt >= 32_000 && relay_touches < 4 {
            env.spawner.spawn_touch(&session_id, url);
        }
    }

    let cache = CacheFrameState {
        cache_ts: (cache_ts > 0).then_some(cache_ts),
        relay_touches,
        relay_status,
    };

    FrameData {
        payload,
        now_sec,
        git,
        cache,
        last_tps: (!last_tps.is_empty() && last_tps != "--").then_some(last_tps),
        response_model: (!resp_model.is_empty()).then_some(resp_model),
    }
}

fn is_active_turn_state(state: &str) -> bool {
    let s = state.trim();
    !s.is_empty()
        && !s.eq_ignore_ascii_case("idle")
        && !s.eq_ignore_ascii_case("authenticating")
        && !s.eq_ignore_ascii_case("initializing")
}

fn compute_model_key(payload: &Payload) -> String {
    let source = if !payload.model_display_name.is_empty() {
        payload.model_display_name.as_str()
    } else if !payload.model_id.is_empty() {
        payload.model_id.as_str()
    } else {
        "Model"
    };
    let mut out = String::with_capacity(source.len());
    let mut in_ws = false;
    for ch in source.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push('_');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    out
}

fn format_tps(tps: f64) -> String {
    if tps >= 100.0 {
        format!("{:.0}", tps.round_ties_even())
    } else {
        format!("{tps:.1}")
    }
}

fn session_db_path(conversations_dir: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || session_id == "."
        || session_id == ".."
        || session_id.contains(['/', '\\', ':', '\0'])
    {
        return None;
    }
    Some(conversations_dir.join(format!("{session_id}.db")))
}
