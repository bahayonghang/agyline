# Journal - lyh (Part 1)

> AI development session journal
> Started: 2026-11-14

---



## Session 1: 确定 agyline 的产品边界
<!-- trellis-session: v=2 fp=93c945758737a7ae -->

**Date**: 2026-11-14
**Task**: 确定 agyline 的产品边界
**Branch**: `main`

### Summary

仓库目标是把 ref/agy-patch 里的 agy 状态栏做成类似 CCometixLine 的独立 Rust 程序，带配置文件和配置器。README.md 与 README.zh-CN.md 分开写了现有脚本行为和尚未实现的接口；Mach-O 补丁、hub wrapper、consult_gemini 与 cache relay 重写仍只作参考。AGENTS.md 在 trellis 块之后写了实现约束。Rust crate 尚未开始。
Topics: [README.md](../../../README.md), [README.zh-CN.md](../../../README.zh-CN.md)

### Git Commits

(No commits - planning session)

### Status

[OK] **Completed**

### Next Steps

- 按 README 的接口开始 Rust crate，默认行对齐 statusline.ps1。


## Session 2: 完成 agyline v1 实施与 Windows 验收
<!-- trellis-session: v=2 fp=48d0638233eaab18 -->

**Date**: 2026-11-15
**Task**: 完成 agyline v1 实施与 Windows 验收
**Branch**: `feat/agyline-v1`

### Summary

合入 #28 配置器，完成双轴审查与 TDD 修复。普通 CI 的 70 项集成测试、沙箱安装生命周期、原生 PTY 和宿主备份还原均通过；正式延迟记录在验收文档。所有提交保留本地，未 push。
Topics: [docs/verification/agyline-v1-windows.md](../../../docs/verification/agyline-v1-windows.md)

### Git Commits

| Hash | Message |
|------|---------|
| `a757c9f` | Fix v1 review crashes, configurator visibility, and validation |

### Status

[OK] **Completed**


## Session 3: Record the English comment standard and annotate src/ and tests/
<!-- trellis-session: v=2 fp=3c2ca1354b64f5a6 -->

**Date**: 2026-11-15
**Task**: Record the English comment standard and annotate src/ and tests/
**Branch**: `dev`

### Summary

Added .trellis/spec/code-comments.md (three layers: //! module contract, /// public-item contract, // only for the reason) plus a top-level spec index, and pointed the AGENTS.md Code bullet at it instead of duplicating. Then ran a comment-only pass over all 44 Rust files: 1644 lines added, zero deleted, verified by git diff audit. just ci green (fmt, clippy -D warnings, 70 tests, bench --no-run). Agents corrected two inaccurate claims in the brief: evaluate_slot_dual exists so narrowing does not re-evaluate (File-Only preview is apply_env_overrides), and git_cache.rs uses the shared RAII ScopedLock (the real orphan is a .tmp file on write error, not a lock).
Topics: [.trellis/spec/code-comments.md](../../spec/code-comments.md)

### Main Changes

- New spec: .trellis/spec/code-comments.md and .trellis/spec/index.md
- AGENTS.md Code bullet links the spec; no duplicated rule text
- 1644 comment lines across src/ and tests/, pure additions

### Git Commits

(No commits - planning session)

### Testing

- [OK] cargo fmt --all -- --check: pass
- [OK] cargo clippy --all-targets -- -D warnings: pass
- [OK] cargo test: 70 passed, 0 failed
- [OK] cargo bench --no-run: pass
- [OK] git diff audit: 0 deleted lines in all 44 Rust files

### Status

[OK] **Completed**

### Next Steps

- Unverified defect leads are recorded in project memory; none were fixed in this pass


## Session 4: Keep the empty-mode ask pill: intentional agy 1.5.9 adaptation, not a bug
<!-- trellis-session: v=2 fp=a8632fa00748cad5 -->

**Date**: 2026-11-15
**Task**: Keep the empty-mode ask pill: intentional agy 1.5.9 adaptation, not a bug
**Branch**: `dev`

### Summary

The comment pass first reported eval_mode's ask pill on an empty execution_mode as a verified parity bug. That was wrong: golden fixture I03_empty_mode_show_default deliberately pins it, because agy 1.5.9 has no execution_mode field and omits cycle_mode until the first shift+tab, so the reference scripts' non-empty gate would make AGY_SHOW_DEFAULT_MODE=1 a permanent no-op. Evidence: agy-statusline-payload/notes.md:236, agy-first-render/notes.md:93, ref-script-diff DIV-04. User confirmed keeping the behavior. Added the reason as a comment at the show_default gate in src/items/runtime.rs and recorded the decision in ref-script-diff/notes.md. Lesson: check for a pinning fixture and its description before calling a reference divergence a bug.
Topics: [.trellis/research/ref-script-diff/notes.md](../../research/ref-script-diff/notes.md)

### Main Changes

- Comment the deliberate divergence at src/items/runtime.rs show_default gate
- Record the decision and evidence in .trellis/research/ref-script-diff/notes.md

### Git Commits

(No commits - planning session)

### Testing

- [OK] cargo fmt --check, clippy -D warnings, 70 tests, bench --no-run all pass

### Status

[OK] **Completed**

### Next Steps

- Do not align the empty-mode ask pill to the reference scripts without a new decision
