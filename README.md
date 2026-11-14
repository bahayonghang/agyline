# agyline

[English](README.md) | [中文](README.zh-CN.md)

agyline is a status line for the Antigravity CLI (`agy`): one Rust program, with its own configuration, in the place [CCometixLine](https://github.com/Haleclipse/CCometixLine) occupies for Claude Code.

The Rust program is not in this tree yet. The line that runs today is the script in `ref/agy-patch`. That directory is local reference material and is listed in `.gitignore`.

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

## A separate program

Each refresh starts PowerShell or Python. Git status runs before the line is printed, and a cache touch starts another shell.

Once that command is a Rust binary, the refresh is that process. It prints the line without waiting for git, and a touch does not start another shell. That binary is the goal; it is not in the tree yet.

The environment variables above are the behavior to keep. People change layout and appearance in a config file. CCometixLine stores that file at `~/.claude/ccline/config.toml` and edits it by running `ccline`. agyline uses the same arrangement for `agy`.

## Intended interface

No code creates the pieces below. Build against these names.

| Piece | Place |
| --- | --- |
| Binary | `agyline` |
| Config home | `~/.gemini/agyline/`, or the directory in `AGYLINE_HOME` |
| Config file | `config.toml`: preset, palette, and which segments sit on which line. Field names are fixed in the same change as the loader. |
| Terminal run | a configurator, whose preview uses the same renderer as the live line |
| `agy` hook | `statusLine.command` in `~/.gemini/antigravity-cli/settings.json`. Show the edit before writing it. |
| Default look | the reference line above |

The official `agy` and `agy.exe` binaries stay as installed. `agy` runs `agyline` as the `statusLine` command.

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
