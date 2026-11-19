//! Canned frames for `agyline preview` and the configurator's preview pane: six hand-built
//! scenarios that exercise the catalog with no session, repo or relay behind them. All of it is
//! deterministic (fixed anchor clock, no I/O, no state writes), so repeat runs and goldens agree.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::collect::{CacheFrameState, FrameData, GitFrameState, RelayStatus};
use crate::input::{Payload, QuotaBucketData, SubagentSummary};

/// Fixed `now_sec` (Unix seconds) for every built-in frame, so cache countdowns and quota reset
/// timers do not drift with the wall clock. Configurator tests pin the same instant in ms.
pub const PREVIEW_ANCHOR_SEC: i64 = 1_700_000_300;

/// One canned frame. `name` is the CLI selector, `label` the configurator's display name, and
/// `default_width` the simulated width the configurator starts at (a starting point, not a cap).
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub default_width: usize,
    pub frame: FrameData,
}

impl Scenario {
    /// The frame with the caller's `AGY_CACHE_RELAY` applied, so preview matches the live line
    /// in the same environment. Blank or `off` clears it; unset leaves it as authored.
    pub fn frame_for_env(&self, env_vars: &BTreeMap<String, String>) -> FrameData {
        let mut out = self.frame.clone();
        if let Some(raw_relay) = env_vars.get("AGY_CACHE_RELAY").map(|s| s.trim())
            && (raw_relay.is_empty() || raw_relay.eq_ignore_ascii_case("off"))
        {
            // Off drops the badge and touch count only; the 5-minute countdown still renders.
            out.cache.relay_status = None;
            out.cache.relay_touches = 0;
        }
        out
    }
}

/// The six canonical scenarios, built once and shared. Order is the `--all` and configurator cycle
/// order, so it is part of the observable output, not an implementation detail.
pub fn builtin_scenarios() -> &'static [Scenario] {
    static SCENARIOS: OnceLock<Vec<Scenario>> = OnceLock::new();
    SCENARIOS.get_or_init(build_scenarios).as_slice()
}

/// Case-insensitive lookup on a trimmed name; `None` for an unknown scenario, which the CLI turns
/// into a usage error listing `preview --list`.
pub fn find_scenario(name: &str) -> Option<&'static Scenario> {
    let target = name.trim();
    builtin_scenarios()
        .iter()
        .find(|sc| sc.name.eq_ignore_ascii_case(target))
}

/// A preview frame from a captured payload with no persisted session state: no telemetry, git
/// cache, relay file or SQLite is read and nothing is written, so preview stays repeatable and
/// side-effect free. Cache, speed and response model stay cold; `now_sec` is Unix seconds.
pub fn stateless_frame_from_payload(payload: Payload, now_sec: i64) -> FrameData {
    // Ahead/behind and conflicts would need a git call, which preview never makes: only the
    // payload's own branch and dirty flag are shown.
    let git = if payload.vcs_branch.trim().is_empty() {
        None
    } else {
        Some(GitFrameState {
            branch: payload.vcs_branch.clone(),
            dirty: payload.vcs_dirty,
            has_conflict: false,
            ahead: 0,
            behind: 0,
        })
    };
    FrameData {
        payload,
        now_sec,
        git,
        cache: CacheFrameState::default(),
        last_tps: None,
        response_model: None,
    }
}

/// Every field at its parse default; each scenario overrides only what it exercises. "Model" is
/// the reference's fallback label when neither `display_name` nor `id` is present.
fn empty_payload() -> Payload {
    Payload {
        session_id: None,
        agent_state: "idle".to_string(),
        exec_mode: String::new(),
        model_display_name: "Model".to_string(),
        model_id: String::new(),
        model_effort: String::new(),
        cwd: String::new(),
        vcs_branch: String::new(),
        vcs_dirty: false,
        sandbox_enabled: false,
        sandbox_allow_network: false,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        total_output_tokens: 0,
        context_window_size: 0.0,
        used_percentage: 0.0,
        remaining_percentage: None,
        cost_usd: 0.0,
        subagent_cost_usd: 0.0,
        quota_buckets: Vec::new(),
        subagents_total: 0,
        subagents_running: 0,
        subagents: Vec::new(),
        task_count: 0,
        pending_input_count: 0,
        tool_confirmation_pending: false,
        terminal_width: None,
        vim_mode: String::new(),
        conversation_title: String::new(),
        agent_name: String::new(),
        artifact_count: 0,
        version: String::new(),
        plan_tier: String::new(),
    }
}

/// `pct` is a percentage (0-100), stored as the 0.0-1.0 fraction the payload carries; `reset_secs`
/// is seconds until reset, which the quota item prefers over an absolute `reset_time`.
fn bucket(name: &str, pct: f64, reset_secs: i64) -> QuotaBucketData {
    QuotaBucketData {
        name: name.to_string(),
        disabled: false,
        remaining_fraction: Some(pct / 100.0),
        remaining_amount: None,
        reset_in_seconds: Some(reset_secs),
        reset_time: None,
    }
}

fn build_scenarios() -> Vec<Scenario> {
    let now_sec = PREVIEW_ANCHOR_SEC;

    // 1. normal
    let mut p_normal = empty_payload();
    p_normal.session_id = Some("preview_normal".to_string());
    p_normal.agent_state = "working".to_string();
    p_normal.exec_mode = "auto".to_string();
    p_normal.model_display_name = "Gemini 2.5 Pro".to_string();
    p_normal.model_effort = "High".to_string();
    p_normal.cwd = "/workspace/agyline".to_string();
    p_normal.sandbox_enabled = true;
    p_normal.input_tokens = 9_600;
    p_normal.cache_read_input_tokens = 230_400;
    p_normal.context_window_size = 1_000_000.0;
    p_normal.used_percentage = 24.0;
    p_normal.quota_buckets = vec![
        bucket("gemini-5h", 68.0, 3 * 3600 + 12 * 60),
        bucket("3p-5h", 53.0, 3600 + 45 * 60),
        bucket("gemini-weekly", 84.0, 4 * 86_400),
        bucket("3p-weekly", 71.0, 4 * 86_400),
    ];
    p_normal.conversation_title = "Design Rust statusline configurator".to_string();
    p_normal.artifact_count = 1;
    p_normal.version = "1.5.9".to_string();
    p_normal.plan_tier = "Pro".to_string();

    let sc_normal = Scenario {
        name: "normal",
        label: "normal (Active Turn)",
        description: "Standard active turn in git repo with warm Prompt Cache and quota.",
        default_width: 112,
        frame: FrameData {
            payload: p_normal,
            now_sec,
            git: Some(GitFrameState {
                branch: "main".to_string(),
                dirty: true,
                has_conflict: false,
                ahead: 2,
                behind: 0,
            }),
            cache: CacheFrameState {
                // `300 - 222` reads as "222 s left": 300 s is the reference prompt-cache window.
                cache_ts: Some(now_sec - (300 - 222)),
                relay_touches: 0,
                relay_status: None,
            },
            last_tps: Some("52.4".to_string()),
            response_model: None,
        },
    };

    // 2. pre_session
    let mut p_pre = empty_payload();
    p_pre.session_id = None;
    p_pre.exec_mode = "auto".to_string();
    p_pre.model_display_name = "Opus 4.6".to_string();
    p_pre.model_effort = "High".to_string();
    p_pre.cwd = "/workspace/agyline".to_string();
    p_pre.quota_buckets = vec![
        bucket("gemini-5h", 91.0, 4 * 3600 + 58 * 60),
        bucket("3p-5h", 82.0, 4 * 3600 + 40 * 60),
    ];
    p_pre.version = "1.5.9".to_string();
    p_pre.plan_tier = "Pro".to_string();

    let sc_pre = Scenario {
        name: "pre_session",
        label: "pre_session (Startup 0ms)",
        description: "agy startup before first conversation (empty session_id) with cold cache and no speed.",
        default_width: 112,
        frame: FrameData {
            payload: p_pre,
            now_sec,
            git: Some(GitFrameState {
                branch: "main".to_string(),
                dirty: false,
                has_conflict: false,
                ahead: 0,
                behind: 0,
            }),
            cache: CacheFrameState::default(),
            last_tps: None,
            response_model: None,
        },
    };

    // 3. cache_warm_touch
    let mut p_touch = empty_payload();
    p_touch.session_id = Some("preview_cache_touch".to_string());
    p_touch.exec_mode = "accept-edits".to_string();
    p_touch.model_display_name = "Sonnet 4.5".to_string();
    p_touch.model_effort = "High".to_string();
    p_touch.cwd = "/workspace/agyline".to_string();
    p_touch.sandbox_enabled = true;
    p_touch.sandbox_allow_network = true;
    p_touch.input_tokens = 1_120;
    p_touch.cache_read_input_tokens = 110_880;
    p_touch.context_window_size = 200_000.0;
    p_touch.used_percentage = 56.0;
    p_touch.cost_usd = 0.384;
    p_touch.subagent_cost_usd = 0.092;
    p_touch.quota_buckets = vec![
        bucket("3p-5h", 41.0, 48 * 60),
        bucket("gemini-5h", 75.0, 2 * 3600 + 10 * 60),
    ];
    p_touch.subagents_total = 2;
    p_touch.subagents_running = 0;
    p_touch.task_count = 1;
    p_touch.vim_mode = "NORMAL".to_string();
    p_touch.conversation_title = "Keepalive touch native HTTP worker".to_string();
    p_touch.artifact_count = 2;
    p_touch.version = "1.5.9".to_string();
    p_touch.plan_tier = "Pro".to_string();

    let sc_touch = Scenario {
        name: "cache_warm_touch",
        label: "cache_warm_touch (Relay Live)",
        description: "Last 60s of 5-min Prompt Cache window with Cache Relay keepalive touch badge.",
        default_width: 112,
        frame: FrameData {
            payload: p_touch,
            now_sec,
            git: Some(GitFrameState {
                branch: "feat/cache-touch".to_string(),
                dirty: true,
                has_conflict: false,
                ahead: 1,
                behind: 0,
            }),
            cache: CacheFrameState {
                // 44 s left: inside the reference's last-60 s touch window, 2 of 4 touches used.
                cache_ts: Some(now_sec - (300 - 44)),
                relay_touches: 2,
                relay_status: Some(RelayStatus::Live),
            },
            last_tps: Some("74.8".to_string()),
            response_model: None,
        },
    };

    // 4. busy_subagents
    let mut p_busy = empty_payload();
    p_busy.session_id = Some("preview_busy".to_string());
    p_busy.agent_state = "working".to_string();
    p_busy.exec_mode = "plan".to_string();
    p_busy.model_display_name = "Gemini 2.5 Pro".to_string();
    p_busy.model_effort = "xHigh".to_string();
    p_busy.cwd = "/workspace/agyline".to_string();
    p_busy.sandbox_enabled = true;
    p_busy.input_tokens = 25_200;
    p_busy.cache_read_input_tokens = 394_800;
    p_busy.context_window_size = 1_000_000.0;
    p_busy.used_percentage = 42.0;
    p_busy.cost_usd = 1.42;
    p_busy.subagent_cost_usd = 0.64;
    p_busy.quota_buckets = vec![
        bucket("gemini-5h", 62.0, 2 * 3600 + 30 * 60),
        bucket("3p-5h", 58.0, 3 * 3600 + 5 * 60),
    ];
    p_busy.subagents_total = 4;
    p_busy.subagents_running = 2;
    p_busy.subagents = vec![
        SubagentSummary {
            status: "running".to_string(),
            name: "research".to_string(),
            model: "Flash".to_string(),
        },
        SubagentSummary {
            status: "working".to_string(),
            name: "coder".to_string(),
            model: "Sonnet 4.5".to_string(),
        },
        SubagentSummary {
            status: "idle".to_string(),
            name: "reviewer".to_string(),
            model: "Pro".to_string(),
        },
        SubagentSummary {
            status: "idle".to_string(),
            name: "verifier".to_string(),
            model: "Haiku 4.5".to_string(),
        },
    ];
    p_busy.task_count = 3;
    p_busy.pending_input_count = 2;
    p_busy.vim_mode = "INSERT".to_string();
    p_busy.conversation_title = "Prototype 3 variants for issue 14 configurator".to_string();
    p_busy.agent_name = "coordinator".to_string();
    p_busy.artifact_count = 4;
    p_busy.version = "1.5.9".to_string();
    p_busy.plan_tier = "Ultra".to_string();

    let sc_busy = Scenario {
        name: "busy_subagents",
        label: "busy_subagents (Parallel Swarm)",
        description: "Multi-agent session with running subagents, background tasks, queued input, vim, and artifacts.",
        default_width: 116,
        frame: FrameData {
            payload: p_busy,
            now_sec,
            git: Some(GitFrameState {
                branch: "feat/tui-prototype".to_string(),
                dirty: true,
                has_conflict: false,
                ahead: 4,
                behind: 1,
            }),
            cache: CacheFrameState {
                // 275 s left: relay live, but outside the touch window, so no keepalive is due.
                cache_ts: Some(now_sec - (300 - 275)),
                relay_touches: 0,
                relay_status: Some(RelayStatus::Live),
            },
            last_tps: Some("118".to_string()),
            response_model: None,
        },
    };

    // 5. warn_crit
    let mut p_warn = empty_payload();
    p_warn.session_id = Some("preview_warn_crit".to_string());
    p_warn.agent_state = "working".to_string();
    p_warn.exec_mode = "yolo".to_string();
    p_warn.tool_confirmation_pending = true;
    p_warn.model_display_name = "Opus 4.6 1M".to_string();
    p_warn.model_effort = "Max".to_string();
    p_warn.cwd = "/workspace/agyline".to_string();
    p_warn.input_tokens = 103_200;
    p_warn.cache_read_input_tokens = 756_800;
    p_warn.context_window_size = 1_000_000.0;
    p_warn.used_percentage = 86.0;
    p_warn.cost_usd = 4.85;
    p_warn.subagent_cost_usd = 1.20;
    p_warn.quota_buckets = vec![
        bucket("3p-5h", 14.0, 19 * 60),
        bucket("gemini-5h", 33.0, 55 * 60),
    ];
    p_warn.subagents_total = 1;
    p_warn.subagents_running = 1;
    p_warn.subagents = vec![SubagentSummary {
        status: "running".to_string(),
        name: "runner".to_string(),
        model: "Haiku 4.5".to_string(),
    }];
    p_warn.task_count = 1;
    p_warn.pending_input_count = 1;
    p_warn.vim_mode = "VISUAL".to_string();
    p_warn.conversation_title = "Resolve conflict in settings.json installer".to_string();
    p_warn.agent_name = "debugger".to_string();
    p_warn.artifact_count = 1;
    p_warn.version = "1.5.9".to_string();
    p_warn.plan_tier = "Pro".to_string();

    let sc_warn = Scenario {
        name: "warn_crit",
        label: "warn_crit (Alerts & Approval)",
        description: "Critical context (86%), critical quota (14%), failed relay badge, git conflict, and tool confirmation pending.",
        default_width: 112,
        frame: FrameData {
            payload: p_warn,
            now_sec,
            git: Some(GitFrameState {
                branch: "hotfix/merge-conflict".to_string(),
                dirty: true,
                has_conflict: true,
                ahead: 0,
                behind: 3,
            }),
            cache: CacheFrameState {
                // 25 s left with a failed relay: the badge reads Fail, not a live keepalive.
                cache_ts: Some(now_sec - (300 - 25)),
                relay_touches: 1,
                relay_status: Some(RelayStatus::Fail),
            },
            last_tps: Some("31.6".to_string()),
            response_model: None,
        },
    };

    // 6. narrow_stress
    let mut p_narrow = empty_payload();
    p_narrow.session_id = Some("preview_narrow".to_string());
    p_narrow.agent_state = "working".to_string();
    p_narrow.exec_mode = "ask".to_string();
    p_narrow.model_display_name = "Claude Opus 4.6 Extended".to_string();
    p_narrow.model_effort = "xHigh".to_string();
    p_narrow.cwd = "/workspace/antigravity-statusline-rs".to_string();
    p_narrow.sandbox_enabled = true;
    p_narrow.sandbox_allow_network = true;
    p_narrow.input_tokens = 18_900;
    p_narrow.cache_read_input_tokens = 611_100;
    p_narrow.context_window_size = 1_000_000.0;
    p_narrow.used_percentage = 63.0;
    p_narrow.cost_usd = 0.95;
    p_narrow.quota_buckets = vec![bucket("3p-5h", 39.0, 3600 + 12 * 60)];
    p_narrow.subagents_total = 3;
    p_narrow.subagents_running = 2;
    p_narrow.subagents = vec![
        SubagentSummary {
            status: "running".to_string(),
            name: "explore".to_string(),
            model: "Flash".to_string(),
        },
        SubagentSummary {
            status: "running".to_string(),
            name: "proto".to_string(),
            model: "Pro".to_string(),
        },
        SubagentSummary {
            status: "idle".to_string(),
            name: "verify".to_string(),
            model: "Haiku 4.5".to_string(),
        },
    ];
    p_narrow.task_count = 2;
    p_narrow.pending_input_count = 1;
    // Narrowing budget when no `--width` is passed: 74 columns, matching `default_width` below.
    p_narrow.terminal_width = Some(74);
    p_narrow.vim_mode = "NORMAL".to_string();
    p_narrow.conversation_title =
        "Verify grapheme cluster safe truncation width budget".to_string();
    p_narrow.agent_name = "wayfinder".to_string();
    p_narrow.artifact_count = 5;
    p_narrow.version = "1.5.9".to_string();
    p_narrow.plan_tier = "Enterprise".to_string();

    let sc_narrow = Scenario {
        name: "narrow_stress",
        label: "narrow_stress (74-col Narrowing)",
        description: "Long model/branch/dir names at 74 columns to stress-test Full -> Compact -> Drop -> Ellipsis.",
        default_width: 74,
        frame: FrameData {
            payload: p_narrow,
            now_sec,
            git: Some(GitFrameState {
                branch: "feat/narrowing-three-stage-pipeline".to_string(),
                dirty: true,
                has_conflict: false,
                ahead: 12,
                behind: 3,
            }),
            cache: CacheFrameState {
                cache_ts: Some(now_sec - (300 - 185)),
                relay_touches: 1,
                relay_status: Some(RelayStatus::Live),
            },
            last_tps: Some("64.5".to_string()),
            response_model: None,
        },
    };

    vec![sc_normal, sc_pre, sc_touch, sc_busy, sc_warn, sc_narrow]
}
