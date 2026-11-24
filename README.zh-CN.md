# agyline

[English](README.md) | [中文](README.zh-CN.md)

agyline 是给 Antigravity CLI（`agy`）用的状态栏：一个独立的 Rust 程序，自己带配置。它在 `agy` 里的位置，对应 [CCometixLine](https://github.com/Haleclipse/CCometixLine) 在 Claude Code 里的位置。

本仓库已实现 `agyline` Rust crate。`cometix` 预设沿用 `ref/agy-patch` 中的本机参考脚本布局（已写入 `.gitignore`）。

写代码时要遵守的约定在 [AGENTS.md](AGENTS.md)。

## 参考实现里的这一行

Windows 上是 `statusline.ps1`，另一份是 Python 的 `statusline.py`。外观对齐 CCometixLine 2.0 的 `cometix` 预设、`terminal` 配色和 `plain` 皮肤：段与段之间用 ` | `，模型和思考档位之间用 ` · `。

`agy` 读取 `~/.gemini/antigravity-cli/settings.json` 里的 `statusLine`，把一段 JSON 写进该命令的标准输入，再把标准输出画在对话下面。`install.ps1` 把命令指到脚本：

```json
{
  "statusLine": {
    "type": "command",
    "command": "pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ~/.gemini/antigravity-cli/statusline.ps1",
    "enabled": true
  }
}
```

第一行包括：

- 模型和思考档位（`Low`、`Med`、`High`、`xHigh`、`Max`）
- 当前目录
- git 分支、未提交或冲突，以及领先、落后的提交数
- 上下文窗口，百分比和 token 数
- 提示缓存还剩多久，从上一轮被接受的请求起算五分钟，另加命中率
- 缓存中继的状态文件标明中继在线时的保活标记
- 生成速度，从第一次测到输出 token 起一直显示
- JSON 里带了剩余额度或费用时，把它们画出来

保活标记读临时目录里的 `agy_cache_<session>.dat`。只有五分钟窗口的最后 60 秒、提示词至少 32K token、且这一段空闲里还没满 4 次时，才会向中继发 touch。`AGY_CACHE_RELAY=off` 时不读中继状态文件，不画保活标记，也不发 touch。五分钟倒计时仍按 `agy_telemetry_<session>.dat` 走。

第二行是执行模式、正在跑的子代理、任务、排队输入和沙箱。没有内容的项不画。`AGY_STATUSLINE_LINES=1` 时收成一行。

脚本还认这些环境变量：`AGY_THEME`（`light` 或 `dark`，否则看 `COLORFGBG`）、`AGY_STATUSLINE_INDENT`（默认两个空格），以及 `AGY_SHOW_DEFAULT_MODE=1`（画出默认的 ask 模式）。

每个会话在临时目录的 `agy_telemetry_<session>.dat` 里记生成速度和缓存计时。选择器上的名字和上游实际返回的模型不一致时，从 `~/.gemini/antigravity-cli/conversations/<session_id>.db` 读返回的模型名。

画这一行的过程里会跑 git status。`statusline.py` 把这次调用限在 1.5 秒，并复用两秒缓存。`statusline.ps1` 在同一条路径上读 git。

## 仓库已实现功能

`default`（HUD）预设使用 ` │ ` 分隔符渲染 3 行状态栏，默认启用 Nerd/Unicode 语义小图标与纯前景 `line` 细轨进度条（`━╸─`，无需背景色块即可自适应深浅终端）：

1. **身份与工作区**：状态徽章（`status`）、简洁模式徽章（`mode` 的 `style = "badge"`）、斜体模型名与档位（`model` 的 `style = "hud"`）、上下文进度条（`context` 的 `style = "bar"`）、多级路径（`dir` 的 `max_levels = 2`）以及细粒度 Git 计数（`git` 的 `style = "counts"`）。
2. **模型家族额度组**：同屏展示 `Gemini` 与 `3P`（`quota_group`）的 5 小时与每周已用百分比进度条及紧凑重置倒计时（如 `2h44m`、`5d4h`，并带按紧迫度变化的电量图标；空家族自动隐藏）。
3. **运行期遥测**：沙箱状态（`sandbox` 开启 `show_when_off = true`）、累计 token 与输入/输出拆解（`tokens`）、缓存命中率（`cache` 的 `style = "hit_rate"`）、会话时长（`duration`），以及非零时显示的 `subagents`、`tasks`、`cost`、`title`。

在配置器中选择 `classic`（或设置 `preset = "classic"`）可恢复原 2 行 `@model` 默认布局；选择 `cometix`（`preset = "cometix"`）则与 `ref/agy-patch` 保持 100% 字节级一致。

自定义 `quota` 项仍支持 `bucket = "@model/5h"` 或 `bucket = "@model/weekly"`；`quota_group` 与 `context` 进度条均支持 `bar_style`（默认 `"line"`，可选 `"solid"`、`"block"`、`"ascii"`）和 `bar_width`（`4..=20`），其中 `quota_group` 还支持 `family`（`"gemini"`、`"3p"` 或 `"@model"`）、`show_used`、`show_bar` 与 `show_reset`。若在未设置 `COLORFGBG` 的浅色终端显式选用 `bar_style = "solid"`，建议配置 `theme = "light"`（或 `AGY_THEME=light`）以启用浅色轨道底色。

不同于每次刷新都启动 PowerShell 或 Python 的脚本，`agyline` 是编译后的单一 Rust 二进制：渲染输出路径绝不启动 shell，绝不阻塞等待 `git`，且仅用 `<= 4 KiB` 的 O(1) 头部微读获取会话起始时间。本地 Git 缓存更新、缓存中继 touch 与可选的完整 transcript token 统计均通过后台分离进程（`__refresh-git`、`__touch`、`__refresh-transcript`）异步完成。

| 部分 | 已交付行为 |
| --- | --- |
| 二进制与 `just` 命令 | 单一 `agyline` crate（`cargo install --path .`，需要 rustc 1.88），提供 `just ci`、`just install`、`just uninstall` 与 `just sandbox-install-test` |
| 实时管道渲染 | `agy` 将单帧 JSON 写入 `agyline` 标准输入；`agyline` 输出多行 ANSI/纯文本状态栏，并支持按优先级的窄屏自适应压缩（`overflow = "fit"` / `"none"`） |
| 24 项状态目录 | 工作区（`model`、`dir`、`git`）、用量（`context`、`tokens`、`cache`、`speed`、`budget`、可重复添加多桶的 `quota` 与 `quota_group`、`cost`）、运行态（`status`、`mode`、`duration`、`subagents`、`tasks`、`queue`、`sandbox`）与会话标识（`vim`、`title`、`agent`、`artifacts`、`version`、`plan`） |
| 预设与调色板 | 内置预设（`default`、`hud`、`classic`、`cometix`、`minimal`、`dashboard`、`cache-focus`）及自定义 `~/.gemini/agyline/presets/*.toml`；内置 8 套调色板（`terminal`、`catppuccin-mocha`、`catppuccin-latte`、`tokyo-night`、`nord`、`gruvbox`、`rose-pine`、`dracula`）及自定义 `palettes/*.toml`；`terminal` 支持明暗双极性，其余 7 套 RGB 调色板各有固定极性，不匹配时回退到 `terminal` |
| 配置目录与文件 | `~/.gemini/agyline/config.toml`（或 `AGYLINE_HOME`）：`version`、`preset`、`palette`、`theme`（`auto`/`dark`/`light`）、`glyphs`（`nerd`/`unicode`/`ascii`）、`color_depth`（`auto`/`truecolor`/`256`/`16`/`none`）、`separator`、`indent`、`single_line`、`overflow`；子表 `[thresholds]`、`[git]`、`[cache_relay]`、`[palette_overrides]` 与 `[[line]]` 槽位选项（`style`、`max_levels`、`show_when_off`、`show_used`、`show_bar`、`bar_style`、`bar_width`、`family`、`show_ttl`、`show_breakdown`、`show_pending` 及通用 `fg`/`icon`/`priority`/`label`/`bucket`）。显式 `AGY_*` 环境变量优先于 `config.toml`；`COLORFGBG` 在 `theme = "auto"` 时生效 |
| 运行期状态 | `AGYLINE_HOME` 下的 `state/`（每会话遥测与 transcript 缓存、有界 SQLite 响应模型查询、基于 `git status --porcelain=v2 --branch --show-stash` 的本地 Git 缓存、后台锁与 `previous_statusline.json` 快照；`cache_relay.py` 状态文件仍从系统临时目录只读） |
| 交互式终端配置器 | 在终端直接运行 `agyline` 或 `agyline -c` / `--config`：四标签工作台（`Layout`、`Style & Presets`、`Thresholds & Runtime`、`agy Hook`），与实时状态栏无损共用渲染管线（零 ANSI 反解析），支持 100 步撤销/重做、实时只读 `TOML` 抽屉、损坏 `config.toml` 的 `.bak` 备份恢复、外部 `mtime` 冲突保护以及独立的 `settings.json` diff 预览确认 |
| 公开子命令 | `agyline preview`（`--scenario`、`--all`、`--list`、`--input`、`--width`、`--plain`）、`agyline install`（`--dry-run`、`--yes`）、`agyline uninstall`（`--dry-run`、`--yes`、`--reset`、`--purge`）。`--purge` 只删除绝对路径上的专用 agyline 目录；即使用了 `-y`，也会拒绝用户主目录、`~/.gemini`、settings 所在目录、文件系统根、符号链接，以及混有其他文件的目录。 |
| 接到 `agy` | `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command`（保留符号链接与其余键，写入前展示 unified diff，卸载时恢复原有 `statusLine`） |

官方的 `agy` 和 `agy.exe` 保持安装时的样子。`agy` 把 `agyline` 当作 `statusLine` 命令来跑。

## 后续发布规划

通过 GitHub Releases 分发预编译包（Windows `msvc`、macOS 与纯静态 `musl` Linux，覆盖 `x86_64` 与 `arm64` 对齐官方 `agy` 发行目标；需同侧运行，不做 npm 包装）独立于本仓库本地 `cargo install --path .` 构建进行规划。

## 仍留在参考目录里的部分

按目前的目标，`ref/agy-patch` 里这些内容继续只做参考。

| 路径 | 作用 |
| --- | --- |
| `statusline.ps1`、`statusline.py` | `cometix` 预设的布局基线 |
| `install.ps1` | 现在如何把 `settings.json` 指到脚本 |
| `cache_relay.py` | 提示缓存保活。状态栏可以继续读它的状态文件。要不要用 Rust 重写中继，另作决定 |
| `hub-wrapper.ps1`、`hub-wrapper.sh` | 本地 CLIProxyAPI 的网关环境 |
| `patch_agy.py`、`references/` | macOS 上 `agy` 二进制的 Mach-O 补丁和 TUI 笔记 |
| `consult_gemini/` | 把多个 Claude Code 实例复用到一个 `agy acp` 进程上的 MCP 服务 |
