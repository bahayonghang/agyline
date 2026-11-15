# agyline

`agyline` is a configurable status line for the Antigravity CLI (`agy`) that renders session, workspace, context, cache, quota, and runtime state under the conversation view.

## Language

**Reference Line**:
The existing two-line status line shipped as local scripts prior to `agyline`, serving as the behavioral benchmark for default segments, labels, colors, and thresholds.
_Avoid_: Legacy line, old statusline

**Default Line**:
The status line output produced by `agyline` when run with an unmodified default configuration.
_Avoid_: Built-in statusline

**Pre-session**:
The lifecycle phase after `agy` starts and before a conversation is created or bound on resume, during which no conversation identifier is attached yet.
_Avoid_: Cold start, empty session, unauthenticated state

**Picker Model**:
The model selection reported by `agy` from the user's active model picker choice, which may name a concrete model or an automatic routing alias.
_Avoid_: Display model, configured model

**Response Model**:
The actual upstream model that fulfilled the most recent turn, distinct from the Picker Model when `agy` or a gateway routes the request dynamically.
_Avoid_: Backend model, real model, DB model

**Prompt Cache**:
The short-lived upstream provider cache of a conversation's prompt prefix, whose remaining validity window and hit rate appear on the status line.
_Avoid_: Context cache, bare "cache" (when local state or Git caches are also in scope)

**Cache Relay**:
An optional local intermediary that observes session requests and can replay the latest prompt prefix to keep the Prompt Cache warm during idle periods.
_Avoid_: Keepalive daemon, gateway

**Keepalive Touch**:
A non-advancing replay trigger sent to the Cache Relay near the end of the Prompt Cache validity window to extend its lifetime without adding a conversation turn.
_Avoid_: Cache ping, warm-up request

**Item**:
A single configurable display unit placed on a status line row that formats one piece of session or workspace state and hides itself when empty.
_Avoid_: Segment, widget, block, module

**Quota Bucket**:
An independent upstream usage pool scoped to a model family and reset window, reporting its own remaining allowance and reset time.
_Avoid_: Quota tier, rate limit

**Preset**:
A named starting bundle of row layout and visual style defaults used to initialize or reset status line configuration.
_Avoid_: Theme, template

**Palette**:
A named mapping from semantic and category roles to terminal colors.
_Avoid_: Theme, color scheme

**Theme**:
The terminal background polarity (`dark`, `light`, or `auto`) used to select dark- or light-background color variants.
_Avoid_: Palette, preset

**Scenario**:
A named sample session state used by preview and the configurator to render representative status line conditions without reading or mutating live session state.
_Avoid_: Preset, test fixture
