# agyline

[English](README.md) | [中文](README.zh-CN.md)

agyline is a status line for the Antigravity CLI (`agy`): one Rust program, with its own configuration, in the place [CCometixLine](https://github.com/Haleclipse/CCometixLine) occupies for Claude Code.

The `agyline` Rust crate lives in this repository. `ref/agy-patch` is the gitignored local reference script baseline whose default look and `AGY_*` behavior `agyline` reproduces.

Agent instructions are in [AGENTS.md](AGENTS.md).

## The reference line

`statusline.ps1` is the Windows line. `statusline.py` is the same line in Python. Both follow CCometixLine 2.0's `cometix` preset, `terminal` palette, and `plain` skin: items separated by ` | `, model and effort separated by ` · `.

`agy` reads `statusLine` from `~/.gemini/antigravity-cli/settings.json`, writes one JSON object to the command's stdin, and shows the command's stdout under the conversation. `install.ps1` points that command at the script:

```json
{
  "statusLine": {
    "type": "command",
    "command": "pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ~/.gemini/antigravity-cli/statusline.ps1",
    "enabled": true
  }
}
```

The first line shows:

- model and effort (`Low`, `Med`, `High`, `xHigh`, `Max`)
- workspace directory
- git branch, dirty or conflict state, and ahead/behind counts
- context window, as a percent and a token count
- prompt-cache time left, counted as five minutes from the last accepted turn, plus hit rate
- a keepalive mark when the cache relay state file says the relay is live
- output speed, shown from the first measured output and kept afterwards
- remaining quota and cost, when the JSON includes them

The keepalive mark comes from `agy_cache_<session>.dat` in the temp directory. A touch runs only in the last 60 seconds of that five-minute window, for prompts of at least 32K tokens, and at most four times in one idle stretch. `AGY_CACHE_RELAY=off` skips the relay file, the keepalive mark, and the touch. The five-minute countdown still follows `agy_telemetry_<session>.dat`.

The second line shows execution mode, running subagents, tasks, queued input, and sandbox state. An empty item is omitted. `AGY_STATUSLINE_LINES=1` keeps a single line.

Other environment variables on the script: `AGY_THEME` (`light` or `dark`; otherwise `COLORFGBG`), `AGY_STATUSLINE_INDENT` (default two spaces), and `AGY_SHOW_DEFAULT_MODE=1` to print the default ask mode.

Per session, the script stores speed and the cache clock in `agy_telemetry_<session>.dat` under the temp directory. When the picker name and the upstream response model differ, it reads the response model from `~/.gemini/antigravity-cli/conversations/<session_id>.db`.

Git status runs while the line is drawn. `statusline.py` stops that call after 1.5 seconds and reuses a two-second cache. `statusline.ps1` reads git on the same path.

## Shipped in this repository

Unlike PowerShell or Python status line scripts, `agyline` runs as one compiled Rust binary: the printing path never spawns a shell and never waits on `git`. Stale local Git caches and prompt-cache relay touches run asynchronously in detached background worker modes (`__refresh-git`, `__touch`).

| Piece | Shipped behavior |
| --- | --- |
| Binary & `just` recipes | `agyline` single crate (`cargo install --path .`), plus `just ci`, `just install`, `just uninstall`, and `just sandbox-install-test` |
| Live pipe render | `agy` pipes one JSON frame to `agyline`'s stdin; `agyline` renders 1 or 2 ANSI/plain lines to stdout with priority-based width narrowing (`overflow = "fit"` / `"none"`) |
| 20-item catalog | Workspace (`model`, `dir`, `git`), Usage (`context`, `cache`, `speed`, `budget`, repeatable `quota`, `cost`), Runtime (`mode`, `subagents`, `tasks`, `queue`, `sandbox`), and Session/Identity (`vim`, `title`, `agent`, `artifacts`, `version`, `plan`) |
| Presets & palettes | Built-in presets (`cometix`, `minimal`, `dashboard`, `cache-focus`) + custom `~/.gemini/agyline/presets/*.toml`; 8 built-in palettes (`terminal`, `catppuccin-mocha`, `catppuccin-latte`, `tokyo-night`, `nord`, `gruvbox`, `rose-pine`, `dracula`) + custom `palettes/*.toml`; `terminal` supports both polarities, while the seven RGB palettes have a fixed polarity and fall back to `terminal` on a mismatch |
| Config home & file | `~/.gemini/agyline/config.toml` (or `AGYLINE_HOME`): `version`, `preset`, `palette`, `theme` (`auto`/`dark`/`light`), `glyphs` (`nerd`/`unicode`/`ascii`), `color_depth` (`auto`/`truecolor`/`256`/`16`/`none`), `separator`, `indent`, `single_line`, `overflow`; `[thresholds]`, `[git]`, `[cache_relay]`, `[palette_overrides]`, and `[[line]]`. Explicit `AGY_*` variables override `config.toml`; `COLORFGBG` applies when `theme = "auto"`. |
| Runtime state | `state/` under `AGYLINE_HOME` (per-session telemetry, bounded SQLite response-model lookup, Git cache, background locks, and saved `previous_statusline.json` snapshot; `cache_relay.py` state stays read-only in the OS temp directory) |
| Interactive configurator | `agyline` (on a TTY) or `agyline -c` / `--config`: 4-tab terminal workbench (`Layout`, `Style & Presets`, `Thresholds & Runtime`, `agy Hook`) sharing the live renderer without ANSI re-parsing, with 100-step undo/redo, live `TOML` drawer, `.bak` broken-TOML recovery, external `mtime` conflict guard, and standalone `settings.json` hook diff preview |
| CLI subcommands | `agyline preview` (`--scenario`, `--all`, `--list`, `--input`, `--width`, `--plain`), `agyline install` (`--dry-run`, `--yes`), `agyline uninstall` (`--dry-run`, `--yes`, `--reset`, `--purge`) |
| `agy` hook | `statusLine.command` in `~/.gemini/antigravity-cli/settings.json` (preserving symlinks and sibling keys, showing a unified diff before writing, and restoring the prior `statusLine` on uninstall) |

The official `agy` and `agy.exe` binaries stay as installed. `agy` runs `agyline` as the `statusLine` command.

## Planned distribution

Prebuilt release archives via GitHub Releases (Windows `msvc`, macOS, and static `musl` Linux across `x86_64` and `arm64`, matching official `agy` targets; same-OS invocation only, with no npm wrapper) are planned separately from local `cargo install --path .` builds.

## What stays in the reference

These parts of `ref/agy-patch` remain reference material under the current goal.

| Path | Role |
| --- | --- |
| `statusline.ps1`, `statusline.py` | Default line for agyline to match |
| `install.ps1` | How `settings.json` is pointed at the script today |
| `cache_relay.py` | Prompt-cache keepalive. The line may keep reading its state file. A Rust relay is a separate decision. |
| `hub-wrapper.ps1`, `hub-wrapper.sh` | Gateway environment for a local CLIProxyAPI |
| `patch_agy.py`, `references/` | Mach-O patches and TUI notes for the macOS `agy` binary |
| `consult_gemini/` | MCP server that multiplexes Claude Code instances onto one `agy acp` process |
