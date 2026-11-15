<!-- TRELLIS:START -->
# mini-trellis

This project uses mini-trellis as a memory layer. Working knowledge lives under `.trellis/`:

- `.trellis/spec/` — durable contracts (short markdown; read `index.md` first)
- `.trellis/research/` — topic inbox (`<topic>.md`; `README.md` is not a topic)
- `.trellis/workspace/` — per-developer journals

Record a session with `/mini-trellis:remember` (Claude/OpenCode), `/mini-trellis-remember` (Pi/OMP/Kiro), `$mini-trellis-remember` (Codex/ZCode), or the `mini-trellis-remember` skill (Cursor and DeepSeek Harness). When earlier decisions or relevant project rules are needed, use `mini-trellis mem recall "<topic or path>"`; expand evidence with `mini-trellis mem read "<ref>"`. No search is required for unrelated work. Promote lasting boundaries into spec with `mini-trellis-update-spec`.

Record only confirmed decisions, reusable research findings, or the session/compact handoff. Skip ordinary turns and unconfirmed guesses. New installs write notes without an automatic git commit; use `mini-trellis doctor` to check host wiring and `mini-trellis refresh --dry-run` before refreshing assets.

Keep confirmed rules short and faithful. Unconfirmed scope, exceptions, and implementation choices stay open or are labeled as inference, not added to the contract.

For a confirmed update, follow this short order: read the existing topic; edit that file; read it back and check the new rule, limits, and negations; run `python3 ./.trellis/scripts/add_session.py --title "..." --summary "..." --topic <actual-topic-path>`; read the journal and check its topic link. Journal-only success is not topic success; report partial writes accurately. New notes follow the requested or discussion language; updates preserve the document language.

When recall excerpts already answer the question with source and qualifiers, cite them and stop. Expand only missing context; do not repeat a lookup or fall back to dialogue-only `search` merely to reconfirm sufficient evidence.

ZCode, Kiro and OMP conversation histories are not parsed; use project knowledge or supply Markdown/text exports. For an explicit request to organize selected past sessions or supplied Markdown/text exports into project knowledge, use `mini-trellis-session-insight`; update existing documents and preserve sources.

mini-trellis does not create or drive tasks.

Managed by mini-trellis. Edits outside this block are preserved.
<!-- TRELLIS:END -->

# agyline

The product boundary is [README.md](README.md) and [README.zh-CN.md](README.zh-CN.md). When a boundary changes, update both. Keep shipped behavior and intended behavior in different sections. Do not copy the same description into `.trellis/spec/`.

## Status

`agyline` is implemented as the single Rust crate in this repository. `ref/agy-patch` is the local gitignored reference baseline that `agyline`'s default line reproduces.

`ref/` is gitignored local reference. Read it when checking parity. Implement changes in the crate, and leave the files under `ref/` unchanged.

## Default line

Match `ref/agy-patch/statusline.ps1` for default segments, labels, colors, JSON fields, and thresholds. `statusline.py` is the cross-platform companion. If the two files disagree on output, stop and report the difference before choosing one.

Stdin fields the scripts already read:

`session_id`, `conversation_id`, `agent_state`, `execution_mode`, `cycle_mode`, `model.display_name`, `model.id`, `model.effort`, `thinkingLevel`, `cwd`, `workspace.current_dir`, `workspace.project_dir`, `vcs.branch`, `vcs.dirty`, `sandbox.enabled`, `sandbox.allow_network`, `context_window` (input, output, cache read, total output, window size, used percentage), `cost`, `quota.*.remaining_fraction`, `subagents`, `task_count`, `pending_input_count`, `tool_confirmation_pending`.

## Integration

`agy` runs agyline through `statusLine` in `~/.gemini/antigravity-cli/settings.json` (`type` is `command`, `enabled` is true). The official `agy` and `agy.exe` binaries stay as installed.

`patch_agy.py`, the prompt templates, `hub-wrapper`, and `consult_gemini/` stay in the reference. Porting them into the crate needs its own decision.

The status line may read the cache relay's existing state file. Rewriting `cache_relay.py` needs its own decision.

## Configuration

`AGY_THEME`, `AGY_STATUSLINE_LINES`, `AGY_STATUSLINE_INDENT`, `AGY_SHOW_DEFAULT_MODE`, and `AGY_CACHE_RELAY` are behavior to preserve. New settings go in `~/.gemini/agyline/config.toml`. `AGYLINE_HOME` moves that directory. Once a TOML key is chosen, the loader and both READMEs use that key.

A terminal stdin opens the configurator. Any other stdin is a render. The preview and the live line share one renderer. The default config reproduces the reference line. Segment order and colors come from the config.

## Performance

The render that prints the line must not start a shell and must not wait on `git`. In the reference, `statusline.py` limits `git status` to 1.5 seconds, and `statusline.ps1` reads git output on the render path before its 1.5 second wait. Move git and conversation-database scans off the printing path, or bound them so a slow read cannot hold the line.

Measure a change that claims to be faster. A cache that can change the printed text needs a fixture that covers the changed text.

## Code

- One Cargo package until a split has a reason.
- Imports stay at the top of the file.
- A `match` on a union or enum ends with a `never` check.
- Comments and commit messages are in English.
- Commit only when asked. No Cursor trailer and no `Co-authored-by` for Cursor. Commit with local `git commit -F`.

## Agent skills

### Issue tracker

Issues, specs, and wayfinder maps are GitHub issues in `bahayonghang/agyline`, a public repo. See `docs/agents/issue-tracker.md`.
