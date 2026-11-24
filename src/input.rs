//! One untrusted `agy` stdin frame -> a normalized `Payload`, via typed streaming serde
//! deserialization with zero intermediate `Value` tree allocations. Every field is optional and
//! lenient against unexpected JSON types; defaults and fallback chains mirror `ref/agy-patch`.

use std::borrow::Cow;
use std::fmt;

use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// Hard cap on piped stdin (4 MiB). The reference scripts read stdin unbounded; this bound stops a
/// hostile or runaway frame from being buffered and parsed. Over-limit input is rejected, not cut.
pub const MAX_STDIN_BYTES: usize = 4 * 1024 * 1024;

/// A `quota.<name>` bucket. `remaining_fraction` is 0.0..=1.0, `None` when absent or out of range;
/// `reset_in_seconds` counts seconds from now, `reset_time` is an unparsed timestamp string.
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

/// The normalized frame every later stage reads. Missing data becomes "" / 0 / false, never an
/// error; `Option` marks fields whose absence changes the line, not merely the JSON shape.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Payload {
    /// `session_id`, else `conversation_id`; `None` when both are missing or blank (pre-session).
    pub session_id: Option<String>,
    /// As sent, defaulting to "idle", which is not an active turn for speed or cache anchoring.
    pub agent_state: String,
    /// `execution_mode`, else `cycle_mode`; "" when neither is present.
    pub exec_mode: String,
    pub model_display_name: String,
    pub model_id: String,
    /// Raw `model.effort`, else `thinkingLevel`; items normalize it to Low/Med/High/xHigh/Max.
    pub model_effort: String,
    /// `cwd`, else `workspace.current_dir`, else `workspace.project_dir`, kept verbatim.
    pub cwd: String,
    pub vcs_type: String,
    pub vcs_branch: String,
    pub vcs_dirty: bool,
    pub sandbox_enabled: bool,
    pub sandbox_allow_network: bool,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    /// Window size in tokens; f64 because agy may send the number as a float.
    pub context_window_size: f64,
    /// Reported percent (0-100), used only when token counts or the window size are unavailable.
    pub used_percentage: f64,
    /// `None` when absent or non-finite; only read for a `show_remaining` context slot.
    pub remaining_percentage: Option<f64>,
    /// USD. `cost` may be an object (`total_usd`) or a bare number; both shapes are accepted.
    pub cost_usd: f64,
    /// USD attributed to subagents; 0 unless `cost` is an object that carries it.
    pub subagent_cost_usd: f64,
    pub cost_estimated: bool,
    /// One entry per `quota.<name>` object; empty when `quota` is absent or not an object.
    pub quota_buckets: Vec<QuotaBucketData>,
    pub subagents_total: usize,
    /// Entries whose status is running, working or thinking, as the reference counts them.
    pub subagents_running: usize,
    pub subagents: Vec<SubagentSummary>,
    pub task_count: i64,
    pub pending_input_count: i64,
    pub tool_confirmation_pending: bool,
    /// Columns agy reports; `None` when absent or non-positive, which leaves the line un-narrowed.
    pub terminal_width: Option<usize>,
    pub transcript_path: String,
    // Session/identity extras: the reference scripts never read these, agyline's catalog does.
    pub vim_mode: String,
    pub conversation_title: String,
    pub agent_name: String,
    pub artifact_count: i64,
    pub version: String,
    pub plan_tier: String,
    /// Optional credit balance used as `cr:<n>` fallback in inline quota display.
    pub ai_credits: Option<f64>,
}

impl Payload {
    /// True when no usable session id arrived: `collect` then skips session telemetry, cache
    /// anchoring, the relay and the SQLite lookup, and renders a cold startup line.
    pub fn is_pre_session(&self) -> bool {
        self.session_id.is_none()
    }
}

/// Parses one frame, or returns `None` for over-limit, non-UTF-8, blank, malformed or non-object
/// input. Never panics and never logs; the live caller prints nothing and exits 0 on `None`.
pub fn parse_stdin_payload(raw: &[u8]) -> Option<Payload> {
    if raw.len() > MAX_STDIN_BYTES {
        return None;
    }
    let text = std::str::from_utf8(raw).ok()?;
    let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
    let trimmed = without_bom.trim_start();
    if trimmed.is_empty() || !trimmed.starts_with('{') {
        return None;
    }
    serde_json::from_str::<RawRoot>(trimmed)
        .ok()
        .map(RawRoot::into_payload)
}

/// Zero-copy JSON object key: borrows unescaped keys directly from input without allocating.
struct JsonKey<'de>(Cow<'de, str>);

impl<'de> de::Deserialize<'de> for JsonKey<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = JsonKey<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON string key")
            }

            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(JsonKey(Cow::Borrowed(v)))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(JsonKey(Cow::Owned(v.to_string())))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(JsonKey(Cow::Owned(v)))
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}

fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

macro_rules! ignore_non_primitive {
    ($ret:expr) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq)?;
            Ok($ret)
        }
    };
}

struct LenientTrimmed(Option<String>);

impl<'de> de::Deserialize<'de> for LenientTrimmed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientTrimmed;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("optional trimmed string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let t = v.trim();
                Ok(LenientTrimmed((!t.is_empty()).then(|| t.to_string())))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientTrimmed(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientTrimmed(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientTrimmed(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientTrimmed(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientTrimmed(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(LenientTrimmed(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(LenientTrimmed(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientRawStr(Option<String>);

impl<'de> de::Deserialize<'de> for LenientRawStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientRawStr;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("optional raw string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LenientRawStr((!v.trim().is_empty()).then(|| v.to_string())))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(LenientRawStr((!v.trim().is_empty()).then_some(v)))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientRawStr(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientRawStr(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientRawStr(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientRawStr(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientRawStr(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(LenientRawStr(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(LenientRawStr(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientBool(Option<bool>);

impl<'de> de::Deserialize<'de> for LenientBool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientBool;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("strict JSON boolean")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(LenientBool(Some(v)))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(LenientBool(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(LenientBool(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

fn norm_finite_f64(v: f64) -> Option<f64> {
    if !v.is_finite() {
        return None;
    }
    Some(if v == 0.0 { 0.0 } else { v })
}

struct LenientF64(Option<f64>);

impl<'de> de::Deserialize<'de> for LenientF64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientF64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("finite JSON number")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(LenientF64(norm_finite_f64(v as f64)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(LenientF64(norm_finite_f64(v as f64)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(LenientF64(norm_finite_f64(v)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientF64(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientF64(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientF64(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(LenientF64(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(LenientF64(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientI64(Option<i64>);

impl<'de> de::Deserialize<'de> for LenientI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientI64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("finite JSON integer or truncatable float")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(LenientI64(Some(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(LenientI64(Some(i64::try_from(v).unwrap_or(0))))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(LenientI64(v.is_finite().then_some(v.trunc() as i64)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(LenientI64(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(LenientI64(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientObject<T>(Option<T>);

impl<'de, T: Default + VisitObject<'de>> de::Deserialize<'de> for LenientObject<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Default + VisitObject<'de>> Visitor<'de> for ObjVisitor<T> {
            type Value = LenientObject<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("optional JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                Ok(LenientObject(Some(T::visit_object(map)?)))
            }

            ignore_non_primitive!(LenientObject(None));
        }
        deserializer.deserialize_any(ObjVisitor(std::marker::PhantomData))
    }
}

trait VisitObject<'de>: Sized {
    fn visit_object<A: MapAccess<'de>>(map: A) -> Result<Self, A::Error>;
}

#[derive(Default)]
struct RawModel {
    display_name: Option<String>,
    id: Option<String>,
    effort: Option<String>,
}

impl<'de> VisitObject<'de> for RawModel {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "display_name" => out.display_name = map.next_value::<LenientRawStr>()?.0,
                "id" => out.id = map.next_value::<LenientRawStr>()?.0,
                "effort" => out.effort = map.next_value::<LenientTrimmed>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawVcs {
    vcs_type: Option<String>,
    branch: Option<String>,
    dirty: Option<bool>,
}

impl<'de> VisitObject<'de> for RawVcs {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "type" => out.vcs_type = map.next_value::<LenientTrimmed>()?.0,
                "branch" => out.branch = map.next_value::<LenientTrimmed>()?.0,
                "dirty" => out.dirty = map.next_value::<LenientBool>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawSandbox {
    enabled: Option<bool>,
    allow_network: Option<bool>,
}

impl<'de> VisitObject<'de> for RawSandbox {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "enabled" => out.enabled = map.next_value::<LenientBool>()?.0,
                "allow_network" => out.allow_network = map.next_value::<LenientBool>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawWorkspace {
    current_dir: Option<String>,
    project_dir: Option<String>,
    vcs: Option<RawVcs>,
    sandbox: Option<RawSandbox>,
}

impl<'de> VisitObject<'de> for RawWorkspace {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "current_dir" => out.current_dir = map.next_value::<LenientRawStr>()?.0,
                "project_dir" => out.project_dir = map.next_value::<LenientRawStr>()?.0,
                "vcs" => out.vcs = map.next_value::<LenientObject<RawVcs>>()?.0,
                "sandbox" => out.sandbox = map.next_value::<LenientObject<RawSandbox>>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawCurrentUsage {
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
}

impl<'de> VisitObject<'de> for RawCurrentUsage {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "input_tokens" => out.input_tokens = map.next_value::<LenientI64>()?.0.unwrap_or(0),
                "output_tokens" => {
                    out.output_tokens = map.next_value::<LenientI64>()?.0.unwrap_or(0)
                }
                "cache_read_input_tokens" => {
                    out.cache_read_input_tokens = map.next_value::<LenientI64>()?.0.unwrap_or(0);
                }
                "cache_creation_input_tokens" => {
                    out.cache_creation_input_tokens = map.next_value::<LenientI64>()?.0;
                }
                "cache_creation_tokens" => {
                    out.cache_creation_tokens = map.next_value::<LenientI64>()?.0;
                }
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawContextWindow {
    current_usage: Option<RawCurrentUsage>,
    total_input_tokens: i64,
    total_output_tokens: i64,
    context_window_size: f64,
    used_percentage: f64,
    remaining_percentage: Option<f64>,
}

impl<'de> VisitObject<'de> for RawContextWindow {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "current_usage" => {
                    out.current_usage = map.next_value::<LenientObject<RawCurrentUsage>>()?.0;
                }
                "total_input_tokens" => {
                    out.total_input_tokens = map.next_value::<LenientI64>()?.0.unwrap_or(0);
                }
                "total_output_tokens" => {
                    out.total_output_tokens = map.next_value::<LenientI64>()?.0.unwrap_or(0);
                }
                "context_window_size" => {
                    out.context_window_size = map.next_value::<LenientF64>()?.0.unwrap_or(0.0);
                }
                "used_percentage" => {
                    out.used_percentage = map.next_value::<LenientF64>()?.0.unwrap_or(0.0);
                }
                "remaining_percentage" => {
                    out.remaining_percentage = map.next_value::<LenientF64>()?.0;
                }
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawCost(f64, f64, bool);

impl<'de> de::Deserialize<'de> for RawCost {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawCost;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("cost number or object")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(RawCost(
                    norm_finite_f64(v as f64).unwrap_or(0.0),
                    0.0,
                    false,
                ))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RawCost(
                    norm_finite_f64(v as f64).unwrap_or(0.0),
                    0.0,
                    false,
                ))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(RawCost(norm_finite_f64(v).unwrap_or(0.0), 0.0, false))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawCost::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawCost::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawCost::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(RawCost::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut total_usd = None;
                let mut total_cost_usd = None;
                let mut sub_usd = None;
                let mut sub_cost_usd = None;
                let mut estimated = false;
                while let Some(k) = map.next_key::<JsonKey<'de>>()? {
                    match k.0.as_ref() {
                        "total_usd" => total_usd = map.next_value::<LenientF64>()?.0,
                        "total_cost_usd" => total_cost_usd = map.next_value::<LenientF64>()?.0,
                        "subagent_usd" => sub_usd = map.next_value::<LenientF64>()?.0,
                        "subagent_cost_usd" => sub_cost_usd = map.next_value::<LenientF64>()?.0,
                        "estimated" => estimated = map.next_value::<LenientBool>()?.0 == Some(true),
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(RawCost(
                    total_usd.or(total_cost_usd).unwrap_or(0.0),
                    sub_usd.or(sub_cost_usd).unwrap_or(0.0),
                    estimated,
                ))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Default)]
struct RawQuotaBucket {
    disabled: bool,
    remaining_fraction: Option<f64>,
    remaining_amount: Option<i64>,
    reset_in_seconds: Option<i64>,
    reset_time: Option<String>,
    reset_at: Option<String>,
}

impl<'de> VisitObject<'de> for RawQuotaBucket {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "disabled" => out.disabled = map.next_value::<LenientBool>()?.0 == Some(true),
                "remaining_fraction" => {
                    // Only 0.0..=1.0 is usable; statusline.ps1 skips out-of-range buckets in the minimum.
                    out.remaining_fraction = map
                        .next_value::<LenientF64>()?
                        .0
                        .filter(|rf| (0.0..=1.0).contains(rf));
                }
                "remaining_amount" => out.remaining_amount = map.next_value::<LenientI64>()?.0,
                "reset_in_seconds" => out.reset_in_seconds = map.next_value::<LenientI64>()?.0,
                "reset_time" => out.reset_time = map.next_value::<LenientTrimmed>()?.0,
                "reset_at" => out.reset_at = map.next_value::<LenientTrimmed>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

struct RawQuotaList(Vec<QuotaBucketData>);

impl<'de> de::Deserialize<'de> for RawQuotaList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawQuotaList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("quota object map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((name, q_opt)) =
                    map.next_entry::<String, LenientObject<RawQuotaBucket>>()?
                {
                    if let Some(q) = q_opt.0 {
                        out.push(QuotaBucketData {
                            name,
                            disabled: q.disabled,
                            remaining_fraction: q.remaining_fraction,
                            remaining_amount: q.remaining_amount,
                            reset_in_seconds: q.reset_in_seconds,
                            reset_time: q.reset_time.or(q.reset_at),
                        });
                    }
                }
                Ok(RawQuotaList(out))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawQuotaList(Vec::new()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                drain_seq(seq)?;
                Ok(RawQuotaList(Vec::new()))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Default)]
struct RawSubagentEntry {
    status: String,
    name: String,
    model: String,
}

impl<'de> VisitObject<'de> for RawSubagentEntry {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "status" => out.status = map.next_value::<LenientTrimmed>()?.0.unwrap_or_default(),
                // Never fall back to `role`: in agy 1.5.9 `role` holds the full multi-line prompt.
                "name" => out.name = map.next_value::<LenientTrimmed>()?.0.unwrap_or_default(),
                "model" => out.model = map.next_value::<LenientTrimmed>()?.0.unwrap_or_default(),
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

struct RawSubagentList {
    total: usize,
    running: usize,
    entries: Vec<SubagentSummary>,
}

impl<'de> de::Deserialize<'de> for RawSubagentList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawSubagentList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("subagents array")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut total = 0usize;
                let mut running = 0usize;
                let mut entries = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(entry_opt) = seq.next_element::<LenientObject<RawSubagentEntry>>()? {
                    total += 1;
                    if let Some(entry) = entry_opt.0 {
                        // Exactly the three statuses the reference counts as running; no others qualify.
                        if entry.status.eq_ignore_ascii_case("running")
                            || entry.status.eq_ignore_ascii_case("working")
                            || entry.status.eq_ignore_ascii_case("thinking")
                        {
                            running += 1;
                        }
                        entries.push(SubagentSummary {
                            status: entry.status,
                            name: entry.name,
                            model: entry.model,
                        });
                    }
                }
                Ok(RawSubagentList {
                    total,
                    running,
                    entries,
                })
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(RawSubagentList {
                    total: 0,
                    running: 0,
                    entries: Vec::new(),
                })
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct RawStrOrField(Option<String>);

fn deserialize_str_or_object_field<'de, D: Deserializer<'de>>(
    deserializer: D,
    field_name: &'static str,
) -> Result<RawStrOrField, D::Error> {
    struct V(&'static str);
    impl<'de> Visitor<'de> for V {
        type Value = RawStrOrField;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("string or object")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            let t = v.trim();
            Ok(RawStrOrField((!t.is_empty()).then(|| t.to_string())))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut found = None;
            while let Some(k) = map.next_key::<JsonKey<'de>>()? {
                if k.0.as_ref() == self.0 {
                    found = map.next_value::<LenientTrimmed>()?.0;
                } else {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(RawStrOrField(found))
        }

        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(RawStrOrField(None))
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(RawStrOrField(None))
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(RawStrOrField(None))
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(RawStrOrField(None))
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(RawStrOrField(None))
        }
        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq)?;
            Ok(RawStrOrField(None))
        }
    }
    deserializer.deserialize_any(V(field_name))
}

struct RawVimField(Option<String>);
impl<'de> de::Deserialize<'de> for RawVimField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_str_or_object_field(deserializer, "mode").map(|r| Self(r.0))
    }
}

struct RawAgentField(Option<String>);
impl<'de> de::Deserialize<'de> for RawAgentField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_str_or_object_field(deserializer, "name").map(|r| Self(r.0))
    }
}

struct RawArtifactsCount(Option<i64>);

impl<'de> de::Deserialize<'de> for RawArtifactsCount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawArtifactsCount;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("artifact count or array")
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(Some(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(Some(i64::try_from(v).unwrap_or(0))))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(v.is_finite().then_some(v.trunc() as i64)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut count = 0_i64;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    count += 1;
                }
                Ok(RawArtifactsCount(Some(count)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawArtifactsCount(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                drain_map(map)?;
                Ok(RawArtifactsCount(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Default)]
struct RawAccount {
    plan_tier: Option<String>,
    plan: Option<String>,
    tier: Option<String>,
}

impl<'de> VisitObject<'de> for RawAccount {
    fn visit_object<A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut out = Self::default();
        while let Some(k) = map.next_key::<JsonKey<'de>>()? {
            match k.0.as_ref() {
                "plan_tier" => out.plan_tier = map.next_value::<LenientTrimmed>()?.0,
                "plan" => out.plan = map.next_value::<LenientTrimmed>()?.0,
                "tier" => out.tier = map.next_value::<LenientTrimmed>()?.0,
                _ => {
                    let _ = map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(out)
    }
}

#[derive(Default)]
struct RawRoot {
    session_id: Option<String>,
    conversation_id: Option<String>,
    agent_state: Option<String>,
    execution_mode: Option<String>,
    cycle_mode: Option<String>,
    model: Option<RawModel>,
    thinking_level: Option<String>,
    cwd: Option<String>,
    workspace: Option<RawWorkspace>,
    vcs: Option<RawVcs>,
    sandbox: Option<RawSandbox>,
    context_window: Option<RawContextWindow>,
    cost: RawCost,
    quota_buckets: Vec<QuotaBucketData>,
    subagents: Option<RawSubagentList>,
    task_count: i64,
    pending_input_count: i64,
    tool_confirmation_pending: bool,
    terminal_width: Option<usize>,
    transcript_path: Option<String>,
    vim: Option<String>,
    vim_mode: Option<String>,
    conversation_title: Option<String>,
    title: Option<String>,
    session_title: Option<String>,
    agent: Option<String>,
    agent_name: Option<String>,
    artifact_count: Option<i64>,
    artifacts: Option<i64>,
    version: Option<String>,
    agy_version: Option<String>,
    cli_version: Option<String>,
    plan_tier: Option<String>,
    plan: Option<String>,
    tier: Option<String>,
    account: Option<RawAccount>,
    ai_credits: Option<f64>,
}

impl<'de> de::Deserialize<'de> for RawRoot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RootVisitor;
        impl<'de> Visitor<'de> for RootVisitor {
            type Value = RawRoot;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("top-level agy JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut root = RawRoot::default();
                while let Some(k) = map.next_key::<JsonKey<'de>>()? {
                    match k.0.as_ref() {
                        "session_id" => root.session_id = map.next_value::<LenientTrimmed>()?.0,
                        "conversation_id" => {
                            root.conversation_id = map.next_value::<LenientTrimmed>()?.0;
                        }
                        "agent_state" => root.agent_state = map.next_value::<LenientTrimmed>()?.0,
                        "execution_mode" => {
                            root.execution_mode = map.next_value::<LenientTrimmed>()?.0;
                        }
                        "cycle_mode" => root.cycle_mode = map.next_value::<LenientTrimmed>()?.0,
                        "model" => root.model = map.next_value::<LenientObject<RawModel>>()?.0,
                        "thinkingLevel" => {
                            root.thinking_level = map.next_value::<LenientTrimmed>()?.0;
                        }
                        "cwd" => root.cwd = map.next_value::<LenientRawStr>()?.0,
                        "workspace" => {
                            root.workspace = map.next_value::<LenientObject<RawWorkspace>>()?.0;
                        }
                        "vcs" => root.vcs = map.next_value::<LenientObject<RawVcs>>()?.0,
                        "sandbox" => {
                            root.sandbox = map.next_value::<LenientObject<RawSandbox>>()?.0;
                        }
                        "context_window" => {
                            root.context_window =
                                map.next_value::<LenientObject<RawContextWindow>>()?.0;
                        }
                        "cost" => root.cost = map.next_value::<RawCost>()?,
                        "quota" => root.quota_buckets = map.next_value::<RawQuotaList>()?.0,
                        "subagents" => root.subagents = Some(map.next_value::<RawSubagentList>()?),
                        "task_count" => {
                            root.task_count = map.next_value::<LenientI64>()?.0.unwrap_or(0);
                        }
                        "pending_input_count" => {
                            root.pending_input_count =
                                map.next_value::<LenientI64>()?.0.unwrap_or(0);
                        }
                        "tool_confirmation_pending" => {
                            root.tool_confirmation_pending =
                                map.next_value::<LenientBool>()?.0 == Some(true);
                        }
                        "terminal_width" => {
                            root.terminal_width = map
                                .next_value::<LenientI64>()?
                                .0
                                .filter(|&w| w > 0)
                                .map(|w| w as usize);
                        }
                        "transcript_path" => {
                            root.transcript_path = map.next_value::<LenientRawStr>()?.0;
                        }
                        "vim" => root.vim = map.next_value::<RawVimField>()?.0,
                        "vim_mode" => root.vim_mode = map.next_value::<LenientTrimmed>()?.0,
                        "conversation_title" => {
                            root.conversation_title = map.next_value::<LenientTrimmed>()?.0;
                        }
                        "title" => root.title = map.next_value::<LenientTrimmed>()?.0,
                        "session_title" => {
                            root.session_title = map.next_value::<LenientTrimmed>()?.0;
                        }
                        "agent" => root.agent = map.next_value::<RawAgentField>()?.0,
                        "agent_name" => root.agent_name = map.next_value::<LenientTrimmed>()?.0,
                        "artifact_count" => {
                            root.artifact_count = map.next_value::<LenientI64>()?.0;
                        }
                        "artifacts" => root.artifacts = map.next_value::<RawArtifactsCount>()?.0,
                        "version" => root.version = map.next_value::<LenientTrimmed>()?.0,
                        "agy_version" => root.agy_version = map.next_value::<LenientTrimmed>()?.0,
                        "cli_version" => root.cli_version = map.next_value::<LenientTrimmed>()?.0,
                        "plan_tier" => root.plan_tier = map.next_value::<LenientTrimmed>()?.0,
                        "plan" => root.plan = map.next_value::<LenientTrimmed>()?.0,
                        "tier" => root.tier = map.next_value::<LenientTrimmed>()?.0,
                        "account" => {
                            root.account = map.next_value::<LenientObject<RawAccount>>()?.0;
                        }
                        "ai_credits" => {
                            root.ai_credits = map.next_value::<LenientF64>()?.0;
                        }
                        // Note: `email`, `account.email`, `battle.*`, etc. are ignored in-place.
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(root)
            }
        }
        deserializer.deserialize_map(RootVisitor)
    }
}

impl RawRoot {
    fn into_payload(self) -> Payload {
        let model = self.model.unwrap_or_default();
        let ws = self.workspace.unwrap_or_default();
        let vcs = self.vcs.unwrap_or_default();
        let ws_vcs = ws.vcs.unwrap_or_default();
        let sbx = self.sandbox.unwrap_or_default();
        let ws_sbx = ws.sandbox.unwrap_or_default();
        let ctx = self.context_window.unwrap_or_default();
        let cur_u = ctx.current_usage.unwrap_or_default();
        let subs = self.subagents.unwrap_or(RawSubagentList {
            total: 0,
            running: 0,
            entries: Vec::new(),
        });
        let acc = self.account.unwrap_or_default();

        Payload {
            session_id: self.session_id.or(self.conversation_id),
            agent_state: self.agent_state.unwrap_or_else(|| "idle".to_string()),
            exec_mode: self.execution_mode.or(self.cycle_mode).unwrap_or_default(),
            model_display_name: model.display_name.unwrap_or_default(),
            model_id: model.id.unwrap_or_default(),
            model_effort: model.effort.or(self.thinking_level).unwrap_or_default(),
            cwd: self
                .cwd
                .or(ws.current_dir)
                .or(ws.project_dir)
                .unwrap_or_default(),
            vcs_type: vcs.vcs_type.or(ws_vcs.vcs_type).unwrap_or_default(),
            vcs_branch: vcs.branch.or(ws_vcs.branch).unwrap_or_default(),
            vcs_dirty: vcs.dirty.or(ws_vcs.dirty).unwrap_or(false),
            sandbox_enabled: sbx.enabled.or(ws_sbx.enabled).unwrap_or(false),
            sandbox_allow_network: sbx.allow_network.or(ws_sbx.allow_network).unwrap_or(false),
            input_tokens: cur_u.input_tokens,
            output_tokens: cur_u.output_tokens,
            cache_read_input_tokens: cur_u.cache_read_input_tokens,
            cache_creation_input_tokens: cur_u
                .cache_creation_input_tokens
                .or(cur_u.cache_creation_tokens)
                .unwrap_or(0),
            total_input_tokens: ctx.total_input_tokens,
            total_output_tokens: ctx.total_output_tokens,
            context_window_size: ctx.context_window_size,
            used_percentage: ctx.used_percentage,
            remaining_percentage: ctx.remaining_percentage,
            cost_usd: self.cost.0,
            subagent_cost_usd: self.cost.1,
            cost_estimated: self.cost.2,
            quota_buckets: self.quota_buckets,
            subagents_total: subs.total,
            subagents_running: subs.running,
            subagents: subs.entries,
            task_count: self.task_count,
            pending_input_count: self.pending_input_count,
            tool_confirmation_pending: self.tool_confirmation_pending,
            terminal_width: self.terminal_width,
            transcript_path: self.transcript_path.unwrap_or_default(),
            vim_mode: self.vim.or(self.vim_mode).unwrap_or_default(),
            conversation_title: self
                .conversation_title
                .or(self.title)
                .or(self.session_title)
                .unwrap_or_default(),
            agent_name: self.agent.or(self.agent_name).unwrap_or_default(),
            artifact_count: self.artifact_count.or(self.artifacts).unwrap_or(0),
            version: self
                .version
                .or(self.agy_version)
                .or(self.cli_version)
                .unwrap_or_default(),
            plan_tier: self
                .plan_tier
                .or(self.plan)
                .or(self.tier)
                .or(acc.plan_tier)
                .or(acc.plan)
                .or(acc.tier)
                .unwrap_or_default(),
            ai_credits: self.ai_credits,
        }
    }
}
