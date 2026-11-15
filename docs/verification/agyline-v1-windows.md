# agyline v1 Windows 验收记录

日期：2026-11-15。分支：`feat/agyline-v1`。代码验收提交：`a757c9f2d55c214ebbd5e830890003b24718986f`。

范围：[Spec #22](https://github.com/bahayonghang/agyline/issues/22) 的六个切片：[#23](https://github.com/bahayonghang/agyline/issues/23) 默认渲染、[#24](https://github.com/bahayonghang/agyline/issues/24) 会话状态/SQLite/Touch、[#25](https://github.com/bahayonghang/agyline/issues/25) 后台 Git 缓存、[#26](https://github.com/bahayonghang/agyline/issues/26) 配置/20 项目录/收窄/preview、[#27](https://github.com/bahayonghang/agyline/issues/27) 安装与 justfile、[#28](https://github.com/bahayonghang/agyline/issues/28) 终端配置器。

本次只验证 Windows x86_64 原生环境，使用 rustc/cargo 1.99.0、官方未修改的 agy 1.5.9。其他平台及预编译分发没有在本次验收。提交均保留本地，没有 push。

## Standards

对 `git diff dev...HEAD` 独立审查，基线 `dev = 522e9a2b77ce1a0d5a2ef73eb82148468a694475`。没有发现硬性规范违规；两条启发式建议已处理：

- Git 锁复用现有 `ScopedLock`，保留锁名称、所有权和 30 秒过期策略，统一写入失败时的清理行为。
- 五个入口共用一个纯配置目录解析函数；安装器仍保留宿主 home 缺失时的明确错误。

单 Cargo 包、平台条件编译只在 `src/platform/`、Layer 2 无 TUI 库依赖、导入在顶部等检查通过。修复差异的独立合入检查没有发现新的阻断问题。

## Spec

独立审查发现六项 P2 问题，均已修复：

1. Unicode relay URL 导致隐藏 Touch worker 和 Runtime 页崩溃。
2. 异常 quota 重置时间导致 live render 崩溃。
3. 与 home 无关的 Unicode 可执行文件路径导致 Windows 命令解析崩溃。
4. Catalog/Active Rows 选中项不可见、滚动后的鼠标映射和小尺寸 tab 栏问题。
5. 原慢 Git 基准缺少 `.git`，没有真正调用休眠 Git。
6. 两份 README 的 preview 参数和 palette 明暗行为描述错误。

前三项、列表可见性和 tab 栏均先通过公共接口复现失败，再修到通过。另修复了 Git 进程测试继承 `NO_COLOR=1` 导致的非确定性。没有把 Git dirty 的既有 OR 合并规则改为覆盖规则；它与参考脚本一致。

## 自动验证

主工作区普通 `just ci` 退出 0，保留执行环境的 `NO_COLOR=1 / TERM=dumb`。包含格式检查、所有 target 的 Clippy（警告视为错误）、70 项集成测试和基准编译；单元测试和 doc-test 均为 0 项。

| 测试套件 | 通过数 |
| --- | ---: |
| config_and_items | 9 |
| git_sequences | 7 |
| golden | 4 |
| install_settings | 15 |
| preview_cli | 5 |
| process_contract | 9 |
| session_sequences | 9 |
| tui_smoke | 12 |
| 合计 | 70 |

Golden 套件包含 72 个 Default Line fixture，配置/Item fixture 为 12 个；fixture 数不等于 Rust 测试函数数。序列与黑盒检查覆盖会话隔离、SQLite 锁回退、慢 Git/relay 下 stdout EOF 和进程退出码。Windows 脱离进程的 `CreateProcessW` 仍使用 `b_inherit_handles = 0`。

`sandbox-install-test` 生命周期退出 0：隔离目录内 cargo install、dry-run 不写入、非 TTY 无确认拒绝、参考脚本迁移快照、幂等安装、pipe render、卸载精确还原、重新安装、reset/purge、cargo uninstall。修复后的运行使用仓库原 recipe 的外部副本，只在递归清理前增加绝对路径校验，仓库 `justfile` 未改。`ci`、`install`、`uninstall` 和 `sandbox-install-test` recipe 均保留。

## 原生终端与真实宿主

- 最终 release 配置器在 Windows PTY 中启动成功；80 列下四个 tab 均可见，Catalog 经 Tab + Down×19 滚动显示 Plan，直接退出为 0 且不创建配置文件。TestBackend 另覆盖 80×22、100×28 的列表与鼠标行为，以及小于最低尺寸后的恢复。
- 官方 agy 1.5.9 通过临时 `--settings` 覆盖接入编译后的命令。未发送消息或 slash command，启动即显示 Pre-session 状态栏，随后显示 Git 和 quota 更新；Ctrl+C 退出正常。隔离状态目录只有 Git 缓存与清理标记，没有会话文件、数据库或 email 字段。
- 对真实 `settings.json` 先备份，再执行最终 release 的 dry-run、install、幂等 install、经实际配置命令的 `cmd /c` pipe render、uninstall。其他键及顺序保持不变，快照内容正确，渲染退出 0 且 stderr 为空。卸载后 JSON 和字节均与原文件一致，最后再次还原备份并核验 SHA256。
- 最终真实 settings 仍指向原 `statusline.ps1`。SHA256：`81A072A65C7F88CBEF2BEF2E567DE7A059B48CD538CECF6D8663E997D6CD9262`。
- 官方 `agy.exe` 验证前后 SHA256 均为 `8803C289CAC04D6E53F2211592D9981C2D5760BC0D9B4A7F222EF6ECC7ABB38A`。`ref/` 的 211 个文件路径及 SHA256 与初始清单完全一致。

## Release 延迟

先构建最终 release，再依次运行 `cargo bench --bench render_micro` 和 `AGYLINE_BENCH_SAMPLES=50 cargo bench --bench pipe_e2e`，采样时 CI 已结束。数值为本机经验分位数，进程采样包含启动和 stdout EOF，不能外推为跨机器延迟上限。

| 内存基准（每项 1000 次） | P50 μs | P95 μs | P99 μs |
| --- | ---: | ---: | ---: |
| parse + 默认 render + ANSI | 11.70 | 12.70 | 15.60 |
| dashboard render + ANSI | 13.10 | 14.40 | 16.60 |
| 74 列收窄 + ANSI | 55.20 | 60.00 | 71.40 |

| 进程基准 | 样本 | P50 ms | P95 ms | P99 ms |
| --- | ---: | ---: | ---: | ---: |
| 热缓存，直接启动 | 50 | 21.73 | 75.72 | 95.66 |
| 热缓存，经 cmd /c | 50 | 55.11 | 129.02 | 135.90 |
| 冷缓存，Git 休眠 2 秒 | 20 | 39.25 | 115.28 | 115.28 |
| SQLite WAL 查询 | 50 | 27.07 | 30.43 | 100.85 |
| SQLite EXCLUSIVE 锁竞争 | 50 | 46.58 | 48.25 | 63.65 |

慢 Git 使用原生编译替身，每个 cwd 带 `.git`，22/22 个 worker（2 次预热 + 20 次测量）均有调用标记。标记检查与 worker 清理等待不计入渲染时间。热缓存通过 TTL 防止测量中途过期；子进程颜色环境已隔离。

## 本地收尾

配置器原始提交为 `8fa9d95`，审查修复为 `a757c9f`。两个实施 worktree 和临时分支已清理。原 worktree 的未跟踪研究资料移到系统临时目录备存，主工作区原有的两处未跟踪资料保持原样：`.trellis/research/configurator-prototype.html` 和 `.trellis/research/ref-script-diff/`；不纳入提交。
