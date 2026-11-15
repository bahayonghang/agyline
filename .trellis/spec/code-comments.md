# Code comments

Scope: `src/` and `tests/` of the `agyline` crate. Comments are written in English
([AGENTS.md](../../AGENTS.md) `## Code`).

## Rule

Comment the reason, not the statement. Three layers, each with a job:

| Layer | Where | Job |
|-------|-------|-----|
| `//!` | every module, top of file | the module's role in the pipeline and the contract it must honor |
| `///` | public items whose name does not carry the contract | units, valid ranges, when `None`/empty is returned, required call order |
| `//` | inside a body, only where the reason is not visible | see below |

An inline `//` is earned by:

- A constant or threshold that reproduces `ref/agy-patch` behavior — the 300s prompt-cache
  window, the last-60s touch window, the 32K token floor, the 4-touch cap, the 1.5s git bound.
- A state-machine transition or ordering constraint — the output-rate baseline reset, the
  `cache_ts` fallback chain, lock acquisition before cache write.
- A guard against a specific failure — path traversal in a session id, the Windows console
  key double-fire, lock cleanup when a write fails.
- A performance contract — the printing path spawns no shell and waits on no `git`.

An inline `//` is not earned by restating the identifier or the next line, narrating control
flow, or naming the task, PR, or issue that introduced the code. Keep it to one short line,
two at most; no multi-paragraph blocks.

In `tests/`, state the contract the case pins. Keep the existing scenario IDs (`D05`,
`DIV-13`, and the like) when a case maps to one.

## Editing constraint

Adding comments must not change code. A comment-only pass adds lines and deletes none, and
`just ci` (`cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, `cargo bench --no-run`) stays green.