use serde_json::{Map, Value};

pub const MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaBucketData {
    pub name: String,
    pub disabled: bool,
    pub remaining_fraction: Option<f64>,
    pub remaining_amount: Option<i64>,
    pub reset_in_seconds: Option<i64>,
    pub reset_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSummary {
    pub status: String,
    pub name: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub session_id: Option<String>,
    pub agent_state: String,
    pub exec_mode: String,
    pub model_display_name: String,
    pub model_id: String,
    pub model_effort: String,
    pub cwd: String,
    pub vcs_branch: String,
    pub vcs_dirty: bool,
    pub sandbox_enabled: bool,
    pub sandbox_allow_network: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub total_output_tokens: i64,
    pub context_window_size: f64,
    pub used_percentage: f64,
    pub remaining_percentage: Option<f64>,
    pub cost_usd: f64,
    pub subagent_cost_usd: f64,
    pub quota_buckets: Vec<QuotaBucketData>,
    pub subagents_total: usize,
    pub subagents_running: usize,
    pub subagents: Vec<SubagentSummary>,
    pub task_count: i64,
    pub pending_input_count: i64,
    pub tool_confirmation_pending: bool,
    pub terminal_width: Option<usize>,
    pub vim_mode: String,
    pub conversation_title: String,
    pub agent_name: String,
    pub artifact_count: i64,
    pub version: String,
    pub plan_tier: String,
}

impl Payload {
    pub fn is_pre_session(&self) -> bool {
        self.session_id.is_none()
    }
}

pub fn parse_stdin_payload(raw: &[u8]) -> Option<Payload> {
    if raw.len() > MAX_STDIN_BYTES {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    Some(extract_from_object(&obj))
}

fn extract_from_object(obj: &Map<String, Value>) -> Payload {
    let session_id = first_non_blank_trimmed(&[obj.get("session_id"), obj.get("conversation_id")]);
    let agent_state =
        first_non_blank_trimmed(&[obj.get("agent_state")]).unwrap_or_else(|| "idle".to_string());
    let exec_mode = first_non_blank_trimmed(&[obj.get("execution_mode"), obj.get("cycle_mode")])
        .unwrap_or_default();

    let model_obj = obj.get("model").and_then(Value::as_object);
    let model_display_name = model_obj
        .and_then(|m| non_blank_raw_str(m.get("display_name")))
        .unwrap_or_default();
    let model_id = model_obj
        .and_then(|m| non_blank_raw_str(m.get("id")))
        .unwrap_or_default();
    let model_effort = first_non_blank_trimmed(&[
        model_obj.and_then(|m| m.get("effort")),
        obj.get("thinkingLevel"),
    ])
    .unwrap_or_default();

    let ws_obj = obj.get("workspace").and_then(Value::as_object);
    let cwd = first_non_blank_raw(&[
        obj.get("cwd"),
        ws_obj.and_then(|w| w.get("current_dir")),
        ws_obj.and_then(|w| w.get("project_dir")),
    ])
    .unwrap_or_default();

    let vcs_obj = obj.get("vcs").and_then(Value::as_object);
    let vcs_branch = vcs_obj
        .and_then(|v| first_non_blank_trimmed(&[v.get("branch")]))
        .unwrap_or_default();
    let vcs_dirty = strict_bool(vcs_obj.and_then(|v| v.get("dirty")));

    let sbx_obj = obj.get("sandbox").and_then(Value::as_object);
    let sandbox_enabled = strict_bool(sbx_obj.and_then(|s| s.get("enabled")));
    let sandbox_allow_network = strict_bool(sbx_obj.and_then(|s| s.get("allow_network")));

    let ctx_obj = obj.get("context_window").and_then(Value::as_object);
    let cur_u = ctx_obj
        .and_then(|c| c.get("current_usage"))
        .and_then(Value::as_object);
    let input_tokens = finite_i64(cur_u.and_then(|u| u.get("input_tokens")), 0);
    let output_tokens = finite_i64(cur_u.and_then(|u| u.get("output_tokens")), 0);
    let cache_read_input_tokens =
        finite_i64(cur_u.and_then(|u| u.get("cache_read_input_tokens")), 0);
    let total_output_tokens = finite_i64(ctx_obj.and_then(|c| c.get("total_output_tokens")), 0);
    let context_window_size =
        finite_f64(ctx_obj.and_then(|c| c.get("context_window_size"))).unwrap_or(0.0);
    let used_percentage = finite_f64(ctx_obj.and_then(|c| c.get("used_percentage"))).unwrap_or(0.0);
    let remaining_percentage = finite_f64(ctx_obj.and_then(|c| c.get("remaining_percentage")));

    let (cost_usd, subagent_cost_usd) = if let Some(Value::Object(cost_map)) = obj.get("cost") {
        (
            finite_f64(cost_map.get("total_usd")).unwrap_or(0.0),
            finite_f64(cost_map.get("subagent_usd")).unwrap_or(0.0),
        )
    } else if let Some(v @ Value::Number(_)) = obj.get("cost") {
        (finite_f64(Some(v)).unwrap_or(0.0), 0.0)
    } else {
        (0.0, 0.0)
    };

    let mut quota_buckets = Vec::new();
    if let Some(Value::Object(q_map)) = obj.get("quota") {
        quota_buckets.reserve(q_map.len());
        for (name, qv) in q_map {
            let Some(q_obj) = qv.as_object() else {
                continue;
            };
            let disabled = strict_bool(q_obj.get("disabled"));
            let remaining_fraction =
                finite_f64(q_obj.get("remaining_fraction")).filter(|rf| (0.0..=1.0).contains(rf));
            let remaining_amount = q_obj
                .get("remaining_amount")
                .filter(|v| v.is_number())
                .map(|v| finite_i64(Some(v), 0));
            let reset_in_seconds = q_obj
                .get("reset_in_seconds")
                .filter(|v| v.is_number())
                .map(|v| finite_i64(Some(v), 0));
            let reset_time = first_non_blank_trimmed(&[q_obj.get("reset_time")]);
            quota_buckets.push(QuotaBucketData {
                name: name.clone(),
                disabled,
                remaining_fraction,
                remaining_amount,
                reset_in_seconds,
                reset_time,
            });
        }
    }

    let mut subagents_total = 0usize;
    let mut subagents_running = 0usize;
    let mut subagents = Vec::new();
    if let Some(Value::Array(arr)) = obj.get("subagents") {
        subagents_total = arr.len();
        subagents.reserve(arr.len());
        for entry in arr {
            if let Some(s_obj) = entry.as_object() {
                let status = first_non_blank_trimmed(&[s_obj.get("status")]).unwrap_or_default();
                let is_running = status.eq_ignore_ascii_case("running")
                    || status.eq_ignore_ascii_case("working")
                    || status.eq_ignore_ascii_case("thinking");
                if is_running {
                    subagents_running += 1;
                }
                let name = first_non_blank_trimmed(&[s_obj.get("name"), s_obj.get("role")])
                    .unwrap_or_default();
                let model = first_non_blank_trimmed(&[s_obj.get("model")]).unwrap_or_default();
                subagents.push(SubagentSummary {
                    status,
                    name,
                    model,
                });
            }
        }
    }

    let task_count = finite_i64(obj.get("task_count"), 0);
    let pending_input_count = finite_i64(obj.get("pending_input_count"), 0);
    let tool_confirmation_pending = strict_bool(obj.get("tool_confirmation_pending"));

    let terminal_width = obj
        .get("terminal_width")
        .filter(|v| v.is_number())
        .map(|v| finite_i64(Some(v), 0))
        .filter(|&w| w > 0)
        .map(|w| w as usize);

    let vim_mode = if let Some(Value::Object(vim_map)) = obj.get("vim") {
        first_non_blank_trimmed(&[vim_map.get("mode")]).unwrap_or_default()
    } else if let Some(v @ Value::String(_)) = obj.get("vim") {
        first_non_blank_trimmed(&[Some(v)]).unwrap_or_default()
    } else {
        String::new()
    };
    let conversation_title =
        first_non_blank_trimmed(&[obj.get("conversation_title")]).unwrap_or_default();
    let agent_name = if let Some(Value::Object(a_map)) = obj.get("agent") {
        first_non_blank_trimmed(&[a_map.get("name")]).unwrap_or_default()
    } else if let Some(v @ Value::String(_)) = obj.get("agent") {
        first_non_blank_trimmed(&[Some(v)]).unwrap_or_default()
    } else {
        String::new()
    };
    let artifact_count = finite_i64(obj.get("artifact_count"), 0);
    let version = first_non_blank_trimmed(&[obj.get("version")]).unwrap_or_default();
    let plan_tier = first_non_blank_trimmed(&[obj.get("plan_tier")]).unwrap_or_default();

    // Note: `email` and `battle.*` are intentionally ignored per privacy/spec boundary.
    Payload {
        session_id,
        agent_state,
        exec_mode,
        model_display_name,
        model_id,
        model_effort,
        cwd,
        vcs_branch,
        vcs_dirty,
        sandbox_enabled,
        sandbox_allow_network,
        input_tokens,
        output_tokens,
        cache_read_input_tokens,
        total_output_tokens,
        context_window_size,
        used_percentage,
        remaining_percentage,
        cost_usd,
        subagent_cost_usd,
        quota_buckets,
        subagents_total,
        subagents_running,
        subagents,
        task_count,
        pending_input_count,
        tool_confirmation_pending,
        terminal_width,
        vim_mode,
        conversation_title,
        agent_name,
        artifact_count,
        version,
        plan_tier,
    }
}

fn non_blank_raw_str(val: Option<&Value>) -> Option<String> {
    let s = val?.as_str()?;
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn first_non_blank_raw(candidates: &[Option<&Value>]) -> Option<String> {
    for &candidate in candidates {
        if let Some(s) = non_blank_raw_str(candidate) {
            return Some(s);
        }
    }
    None
}

fn first_non_blank_trimmed(candidates: &[Option<&Value>]) -> Option<String> {
    for &candidate in candidates {
        if let Some(s) = candidate.and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn strict_bool(val: Option<&Value>) -> bool {
    matches!(val, Some(Value::Bool(true)))
}

fn finite_f64(val: Option<&Value>) -> Option<f64> {
    let num = val?.as_number()?;
    let f = num.as_f64()?;
    if !f.is_finite() {
        return None;
    }
    if f == 0.0 { Some(0.0) } else { Some(f) }
}

fn finite_i64(val: Option<&Value>, default: i64) -> i64 {
    let Some(num) = val.and_then(Value::as_number) else {
        return default;
    };
    if let Some(i) = num.as_i64() {
        i
    } else if let Some(u) = num.as_u64() {
        i64::try_from(u).unwrap_or(default)
    } else if let Some(f) = num.as_f64().filter(|v| v.is_finite()) {
        f.trunc() as i64
    } else {
        default
    }
}
