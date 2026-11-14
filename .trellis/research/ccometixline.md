# CCometixLine 2.x 的 crate、config.toml 和配置器

问题来源：[bahayonghang/agyline#5](https://github.com/bahayonghang/agyline/issues/5)。本文只记录 CCometixLine 的事实，不为 agyline 给设计建议。

## 读的是哪个版本

| 项 | 值 |
| --- | --- |
| 仓库 | <https://github.com/Haleclipse/CCometixLine> |
| 主要依据 | tag `v2.0.1`，commit `56849931745e05d2d4e506af2bedbc8d1f0616d4`（2026-10-25，`chore: release v2.0.1 (#140)`） |
| 对照 1.x | tag `v1.1.2`，commit `a73b166557662b2e79b83ea005fb297003748fb0` |
| 阅读时的 `master` | `1749ff85b52d8d10e67e0a04d247ceab768f380f`（2026-11-02），比 `v2.0.1` 多一个提交 `fix(cmd): keep cmd output per session (#141)`，只改 `src/cli.rs`、`src/main.rs`、`src/metrics/custom.rs`、`src/state/cmd_cache.rs`（隐藏子命令 `refresh-cmd` 多一个 `--session`） |
| npm | `@cometix/ccline` 的 `dist-tags.latest` 是 `2.0.1`（<https://registry.npmjs.org/@cometix/ccline>，2026-11-14 查询） |
| 本机安装 | npm 全局包 `@cometix/ccline` 2.0.1；`ccline --version` 输出 `ccline 2.0.1`；`~/.claude/ccline/ccline.exe` 与包 `@cometix/ccline-win32-x64` 里的 `ccline.exe` SHA-256 相同 |
| 阅读日期 | 2026-11-14 |

下文的路径都相对上面的 `v2.0.1` commit，格式是 `路径:行号`，可拼成 `https://github.com/Haleclipse/CCometixLine/blob/56849931745e05d2d4e506af2bedbc8d1f0616d4/<路径>#L<行号>`。标了 `v1.1.2:` 的路径在 1.x 的 tag 上。标“（推断）”的句子是从代码读出来的推论，没有运行验证。

## 1. crate 的目录、模块和依赖

### 包

一个 Cargo 包，没有 workspace，没有 feature。包名 `ccometixline`，`edition = "2024"`，`rust-version = "1.88"`（`Cargo.toml:1-12`）。`src/lib.rs` 声明 21 个公开模块，`src/main.rs` 是二进制入口并只通过库调用（`src/lib.rs:1-21`，`src/main.rs:1-14`）。构建产物名是 `ccometixline`，打包时才改名为 `ccline`（`.github/workflows/release.yml:83-97`）。

### 模块

| 路径 | 职责（依据各文件的模块注释） |
| --- | --- |
| `src/main.rs` | 解析参数，分发子命令，区分渲染和配置器 |
| `src/cli.rs` | clap 的参数和子命令定义 |
| `src/input.rs` | Claude Code 写到 stdin 的 JSON：`Input`、`SubagentInput`、`Task`；每个字段都可缺，形状不对的字段单独丢弃（`src/input.rs:1-18`） |
| `src/config/mod.rs` | `Config` 及其加载、保存 |
| `src/presets.rs` | 内置预设（TOML 字符串）、应用预设、用户预设库 `Library` |
| `src/paths.rs` | 数据目录、各文件路径、`write_atomic` |
| `src/statusline.rs` | 渲染管线：`config + input → metrics → widgets → layout → lines`（`src/statusline.rs:1`） |
| `src/metrics/` | 数据源。`mod.rs` 有 `Metric`、`MetricMeta`、`Reading`、`Ctx`、`catalog()`；其余按类分文件：`identity`、`usage`、`projection`、`code`、`custom`、`live`、`task`，`fmt` 是格式化辅助 |
| `src/widgets/` | 把 `Reading` 变成带角色的文本片段：`mod.rs`（`text`/`bar`/`gauge`/`sparkline`/`pace`）和 `template.rs`（文本模板） |
| `src/layout.rs` | 按可用宽度换行、降级、丢弃 |
| `src/style/` | `color`（颜色与色深）、`glyphs`（图标和图形字符）、`palette`（调色板和 token）、`piece`（`Piece`、`Role`、`Severity`）、`skin`（皮肤、上色、拼接） |
| `src/render/` | `span`（输出用的中间表示 `Span`/`Line`）、`ansi`（写成 ANSI）、`width`（显示宽度、截断） |
| `src/state/` | `~/.claude/ccline/state/` 下的持久状态：`session`、`ledger`、`models_seen`、`git_cache`、`cmd_cache`、`swr`、`update`、`bridge`；`mod.rs` 有 `observe`、`spawn_detached`、文件锁、清理 |
| `src/git.rs`、`src/process.rs` | 一次 `git status` 及其解析；带超时的子进程 |
| `src/models/` | 模型注册表（`models.toml`）和内置的 Claude 模型识别 |
| `src/env.rs` | `COLUMNS`、色深探测、读取 Claude Code 的 settings 文件 |
| `src/cc_settings.rs` | 修改 Claude Code 的 `settings.json` |
| `src/companion.rs` | 伴随 mod（Claude Code 插件）的文件和安装；文件本体在仓库根的 `mod/`，用 `include_str!` 编进二进制（`src/companion.rs:19-35`） |
| `src/scenarios.rs` | 内置样例会话，给 `ccline preview`、配置器、快照测试共用（`src/scenarios.rs:1-5`） |
| `src/version.rs` | Claude Code 版本号和各字段的最低版本 |
| `src/tui/` | 配置器：`mod.rs`（启动和事件循环）、`app.rs`、`doc.rs`、`preview.rs`、`convert.rs`、`nerd.rs`、`ui/`、`pages/`（`home`、`presets`、`layout/`、`style`、`models`、`claude`）、`dialogs/`（`color`、`commands`、`companion`、`fonts`、`form`、`help`、`preset`、`prompts`、`rule`、`settings`、`setup`） |
| `tests/snapshots.rs`、`tests/snapshots/*.snap` | insta 快照测试 |
| `npm/`、`mod/`、`assets/nerd-glyphs.txt`、`scripts/nerd-glyphs.mjs` | npm 包装、伴随 mod、图标选择器的数据表（`src/tui/nerd.rs:7`）及其生成脚本 |

### 依赖

`Cargo.toml:14-27`，版本号取自 `Cargo.lock`。

| crate | 用途 |
| --- | --- |
| `serde`（`derive`） | 所有配置、输入、状态的序列化 |
| `serde_json`（`preserve_order`） | 解析 stdin、读写 `state/*.json`、改 `settings.json`。保序是为了改 `settings.json` 时键的顺序不变（`src/cc_settings.rs:1-4`） |
| `toml` 1.0 | `config.toml`、预设、调色板、`models.toml`，以及每个 item 的 `options` 表（`src/metrics/mod.rs:27`） |
| `clap` 4.5（`derive`） | 只在 `src/cli.rs` |
| `chrono` | 时间戳和本地时区 |
| `dirs` | 家目录 |
| `regex` | 模型规则和内置模型识别；清掉 `cmd` 输出里的转义序列（`src/state/cmd_cache.rs:57-63`） |
| `unicode-width`、`unicode-segmentation` | 显示宽度和截断（`src/render/width.rs`） |
| `ratatui` 0.30（关掉默认 feature，开 `crossterm`、`layout-cache`；锁文件里是 0.30.2） | 配置器。crossterm 0.29.0 通过 `ratatui::crossterm` 使用，没有单独列为依赖 |
| `insta` 1（dev，锁文件 1.49.0） | 快照测试 |

没有 HTTP 客户端、没有 git 库、没有异步运行时：更新检查调用外部 `curl`（`src/state/update.rs:1-5`、`:101-117`），git 调用外部 `git`（`src/git.rs:50-60`）。release 配置是 `lto = true`、`codegen-units = 1`、`strip = true`（`Cargo.toml:29-32`）。

## 2. config.toml 的结构

### 位置

数据目录是 `$CCLINE_HOME`，没设时是 `~/.claude/ccline`，不跟随 `CLAUDE_CONFIG_DIR`（`src/paths.rs:3-15`）。目录里有：

| 路径 | 内容 |
| --- | --- |
| `config.toml` | 本节 |
| `models.toml` | 模型注册表（`src/models/mod.rs:50-57`） |
| `presets/<name>.toml` | 用户保存的预设（`src/paths.rs:25-28`） |
| `palettes/<name>.toml` | 用户调色板（`src/style/palette.rs:213-224`） |
| `state/` | 会话历史、git 缓存、命令缓存、每日花费、更新检查结果（`src/state/mod.rs:1-5`） |
| `bridge/`、`mod/` | 伴随 mod 写入的数据和 mod 自身的文件 |

### 顶层键

结构体 `Config`（`src/config/mod.rs:182-209`）。整个结构体带 `#[serde(default)]`，缺的键取默认值；没有 `deny_unknown_fields`，不认识的键被忽略（测试 `ignores_the_removed_overflow_setting`，同文件）。

| 键 | 类型和取值 | 默认 | 含义 |
| --- | --- | --- | --- |
| `version` | 整数 | `2` | 配置格式版本（`:11`） |
| `preset` | 字符串，可缺 | 无 | 这份配置从哪个预设来，只用于显示“已修改” |
| `palette` | 调色板名 | `"terminal"` | |
| `skin` | `plain`、`bracket`、`pill`、`powerline` | `plain` | `src/style/skin.rs:13-23` |
| `fill` | `solid`、`tinted` | `solid` | 填充型皮肤在调色板没给段颜色时怎么上色（`skin.rs:43-49`） |
| `glyphs` | `nerd`、`unicode`、`ascii` | `nerd` | 字体能显示哪一档字符（`src/style/glyphs.rs:8-16`） |
| `color_depth` | `auto`、`truecolor`、`"256"`、`"16"`、`none` | `auto` | `src/config/mod.rs:14-26` |
| `separator` | 字符串，可缺 | 无 | `plain` 皮肤的分隔符 |
| `[powerline]` | `join`、`end`：`flat`、`arrow`、`round`、`slant`、`backslant`、`flame`、`pixel` | 都是 `flat` | powerline 段之间和行尾的形状（`skin.rs:110-165`） |
| `[layout]` | `width`、`padding`：`"auto"` 或列数；`reserve`：列数 | `auto`、`auto`、`0` | `src/config/mod.rs:40-77` |
| `[palette_overrides]` | `token = "颜色"` | 空 | 在调色板之上改单个 token |
| `[git]` | `cache_ms` | `2000` | 见第 5 节 |
| `[update]` | `check`、`interval_hours` | `true`、`24` | 更新检查 |
| `[subagent]` | `left`、`right` | 无 | 每个后台任务那一行；缺省用内置的一行（`src/presets.rs:179-194`） |
| `[[line]]` | `left`、`right` | 见下 | 状态行的每一行 |

### 段、顺序、启用

2.x 里没有“segment”这个配置概念，对应的是行上的 item。每个 `[[line]]` 有 `left` 和 `right` 两个数组，`right` 靠右对齐（`src/config/mod.rs:143-149`）。

- 行的顺序是 `[[line]]` 出现的顺序，item 的顺序是数组内的顺序。
- 没有 `enabled` 开关：item 在数组里就是启用，不在就是没有。
- item 对当前输入没有数据时在渲染时跳过，不占位置（`src/statusline.rs:252-263`）。

item 有两种写法（`ItemSpec`，`src/config/mod.rs:79-85`）：一个 metric id 字符串，或者一个表。表的键（`ItemConfig`，`:99-141`）：

| 键 | 含义 |
| --- | --- |
| `metric` | metric id，必填 |
| `widget` | `text`、`bar`、`gauge`、`sparkline`、`pace` |
| `bar`、`cells` | 条形图的字符集（`blocks`、`eighths`、`shapes`、`dots`、`braille`、`ascii`）和格数 |
| `format`、`compact`、`minimal` | 三档宽度各自的文本模板 |
| `icon` | 图标字符，`""` 表示不要图标 |
| `color` | item 的颜色；填充型皮肤里用作背景 |
| `fg` | 文字颜色 |
| `icon_color` | 图标颜色 |
| `levels` | 阈值，如 `[[50, "warn"], [80, "crit"]]` |
| `coloring` | `steps` 或 `gradient` |
| `priority` | 1 到 5，空间不够时先降级或丢弃数值小的 |
| `options` | metric 自己的选项表，如 `dir` 的 `style`、`cmd` 的 `run` |

文本模板的语法：`{field}` 插入字段，`[ … ]` 是可选组，组内任一字段缺失就整组丢掉，`\` 转义下一个字符（`src/widgets/template.rs:1-3`）。

### 预设和主题

2.x 没有“theme”。预设是“布局加外观”，应用预设就是把它拷进用户的配置（`src/presets.rs:1-3`）。

- 内置 8 个：`cometix`、`minimal`、`dashboard`、`budget`、`cache-aware`、`focus`、`zen`、`powerline`，各自是源码里的一段 TOML 字符串（`src/presets.rs:23-177`）。默认是 `cometix`（`:21`）。
- `apply(config, preset, parts)` 有三种范围：`All`、`Layout`（只换 `[[line]]`）、`Style`（换 `palette`、`skin`、`fill`、`glyphs`、`color_depth`、`separator`、`powerline`、`palette_overrides`）。`layout`、`git`、`update`、`subagent` 不随预设变（`:204-237`）。
- 顶层的 `preset` 键只记来源名。是否“已修改”由 `matches` 把预设重新套一遍后比较得出（`:239-242`）。
- 用户预设是 `presets/<name>.toml`，一个预设一个文件。文件内容是 `version`、`summary`、`palette`、`skin`、`fill`、`glyphs`、`color_depth`、`separator`、`powerline`、`palette_overrides`、`[[line]]`，开头带两行注释（`:313-355`）。与内置同名的文件会顶替内置的那个（`:482-494`）。名字是小写字母数字加连字符，最长 32 个字符（`:249-281`）。

### 调色板和颜色

- `palette = "<name>"`。加载时先找 `palettes/<name>.toml`，没有再找内置（`src/style/palette.rs:212-224`）。名字找不到时用 `terminal`（`src/statusline.rs:31`）。
- 内置 13 个：`terminal`、`catppuccin-mocha`、`catppuccin-latte`、`dracula`、`nord`、`tokyo-night`、`rose-pine`、`gruvbox`、`anthropic-clay`、`powerline-dark`、`powerline-light`、`powerline-rose-pine`、`powerline-tokyo-night`（`palette.rs:271-285`）。
- 用户调色板文件：`light = false` 加一个 `[colors]` 表，键是 token 名（`palette.rs:226-247`）。调色板没定义的 token 回落到 `terminal` 的（`:197-206`）。
- token 名（`palette.rs:138-163`）：语义色 `ok`、`warn`、`crit`、`accent`、`muted`、`separator`、`text`、`bg`、`on_fill`；类别色 `identity`、`workspace`、`vcs`、`context`、`quota`、`cost`、`cache`、`session`、`timing`、`activity`、`system`；每个类别（再加 `custom`）还有 `<类别>.bg` 和 `<类别>.fg`，给填充型皮肤用（`:40-56`）。
- `terminal` 调色板全部是 ANSI 16 色，`text` 和 `bg` 不设，用终端自己的；注释写明类别色沿用 1.x 的 cometix 主题（`palette.rs:338-367`）。
- 颜色写法：`#rrggbb`、`#rgb`、`256:n`、`16:n`，或 ANSI 色名如 `red`、`bright_blue`（`src/style/color.rs:67-85`）。item 的 `color`、`fg`、`icon_color` 还可以写 token 名（`src/statusline.rs:308-311`）。

### 皮肤、图标、分隔符

- 皮肤是顶层的 `skin`。`pill` 和 `powerline` 在 `glyphs` 不是 `nerd` 时退成 `bracket`（`src/style/skin.rs:25-32`）。
- 图标没有独立的配置表。`glyphs` 选一档，每个 metric 的默认图标从内置表 `IconKey → (nerd, unicode)` 取，`ascii` 档没有图标（`src/style/glyphs.rs:102-142`）。要换单个图标，在 item 上写 `icon`。
- item 之间的分隔符是顶层 `separator`，只对 `plain` 皮肤有效；不设时是 ` │ `，`ascii` 档是 ` | `（`skin.rs:68-74`）。颜色取 `separator` token。
- item 内部的分隔符写在模板里。模板字面量中的 ` · `、` | `、` │ `、` / ` 会被单独切成 `Role::Sep` 片段，用分隔符颜色画（`src/widgets/template.rs:8-10`、`:58-75`）。

### 默认文件怎样生成

渲染不会生成文件。

- `config.toml` 不存在时，`load_from` 在内存里返回 `presets::default_config()`，并标记 `LoadNote::Missing`，注释写明 “nothing is written”（`src/config/mod.rs:260-266`）。
- 文件解析失败时同样用默认预设，并在第一行末尾加一个 ` ! ccline config` 标记（`src/config/mod.rs:267-275`，`src/main.rs:91-104`）。
- 文件能解析但没有 `line` 键时，其余设置照用，行取默认预设的；写了 `line = []` 则保持空（`src/config/mod.rs:276-285`）。
- 配置器打开时也不写文件（`src/tui/doc.rs:70-71`）。文件在配置器保存时才出现：`Config::save` 用 `toml::to_string_pretty` 序列化，再经 `write_atomic` 先写同目录临时文件后改名（`src/config/mod.rs:246-252`，`src/paths.rs:42-53`）。
- 保存只写改动过的文件（`src/tui/doc.rs:167-199`）。所以首次设置时如果选的全是默认值，`config.toml` 不会被创建，之后继续用内置默认（推断；依据是 `Doc::save` 的 `config_dirty()` 判断和同文件测试 `saves_only_changed_files_and_reads_them_back`）。

默认预设经配置器保存后的完整形式如下。这段是在该 commit 上调用 `presets::default_config().to_toml()` 得到的输出：

```toml
version = 2
preset = "cometix"
palette = "terminal"
skin = "plain"
fill = "solid"
glyphs = "nerd"
color_depth = "auto"
separator = " | "

[powerline]
join = "flat"
end = "flat"

[layout]
width = "auto"
padding = "auto"
reserve = 0

[git]
cache_ms = 2000

[update]
check = true
interval_hours = 24

[[line]]
left = [
    "model",
    { metric = "dir", icon_color = "bright_yellow" },
    "git",
    { metric = "context", widget = "text", format = "{pct}%[ · {tokens} tokens]" },
    "cache",
]
right = ["update"]
```

`[palette_overrides]` 为空时不写出，`[subagent]` 没设时不写出。设了的话，同样的序列化输出是：

```toml
[palette_overrides]
warn = "#ff8800"

[[subagent.left]]
metric = "task.label"
icon = ""
priority = 4

[[subagent.right]]
metric = "task.trend"
priority = 1
```

手写的文件可以只写一部分键。本机的 `~/.claude/ccline/config.toml` 就只有 `preset`、`palette`、`skin`、`glyphs`、`separator` 和一个 `[[line]]`，内容与 `cometix` 预设一致。

## 3. 配置器

### 库和骨架

- ratatui 0.30，后端是 crossterm。`ratatui::run` 负责进入和恢复终端，另外开了 bracketed paste（`src/tui/mod.rs:26-38`）。
- 事件循环：画一帧，`event::poll` 最多等 500 ms，只处理 `KeyEventKind::Press` 和粘贴事件（`src/tui/mod.rs:22-54`）。
- 终端小于 80×24 时只显示一句提示（`src/tui/app.rs:19-20`、`:100-109`）。
- `App` 持有 `Doc`（被编辑的 `config.toml` 和 `models.toml`）、`Env`、六个页面、一个对话框栈、`Preview`（`src/tui/app.rs:30-47`）。页面实现 `Page` trait（`src/tui/pages/mod.rs:123-165`），对话框实现 `Dialog` trait（`src/tui/dialogs/mod.rs:67-75`）。

### 页面

六个页面，顺序固定（`src/tui/pages/mod.rs:31-63`）。

| 页面 | 内容 | 分栏 |
| --- | --- | --- |
| Home | 环境检查（Claude Code 版本、`statusLine` 是否指向 ccline、`config.toml`、`models.toml`、更新），每个问题给一个修复键；入口列表；用最近一次真实会话画出的状态行（`src/tui/pages/home.rs:1-3`、`:72-128`） | 无 |
| Presets | 预设陈列，每张卡片实时渲染 | 无 |
| Layout | 从 metric 库里挑 item，排到行和左右两侧，调整选中 item 的各项 | `Library`、`Canvas`、`Inspector`（`src/tui/pages/layout/mod.rs:34-41`） |
| Style | 皮肤、调色板、字符档和色深、token 覆盖 | `Skin`、`Palette`、`Look`、`Tokens`（`src/tui/pages/style.rs:57-65`） |
| Models | 模型注册表：没认出的 id、规则、上下文修饰、测试框 | `Unrecognized`、`Rules`、`Modifiers`、`Tester`（`src/tui/pages/models.rs:25-31`） |
| Claude Code | `settings.json` 里的 `statusLine`、`subagentStatusLine`、`refreshInterval`，更新检查，伴随 mod | `StatusLine`、`Mod`、`Agents`、`Updates`（`src/tui/pages/claude.rs:21-31`） |

对话框（`src/tui/dialogs/mod.rs:4-31`）：颜色选择、命令面板、伴随 mod 的安装步骤、字体检查、表单、按键帮助、预设命名、退出确认和冲突确认、模型规则编辑、`settings.json` 的 diff、首次设置。Layout 页另有自己的对话框文件 `src/tui/pages/layout/dialogs.rs`。

首次设置在 `config.toml` 不存在时自动弹出（`src/tui/mod.rs:27-30`），四步：`Detect`、`Fonts`、`Preset`、`Connect`（`src/tui/dialogs/setup.rs:20-21`）。

### 按键

按键的去向依次是：最上层的对话框；带 Ctrl 的键归全局；焦点在标签行时 `←` `→` 换页、`↓` 或 `Enter` 进入页面；否则交给页面，页面不处理的再落到全局键；页面对 `↑` 或 `Esc` 不处理时焦点回到标签行（`src/tui/app.rs:244-293`）。

全局键（`src/tui/app.rs:320-345`，帮助表在 `src/tui/dialogs/help.rs:13-26`）：

| 键 | 作用 |
| --- | --- |
| `^S` | 保存 |
| `^Z`、`^Y` | 撤销、重做 |
| `^P` | 命令面板 |
| `^C`、`q` | 退出 |
| `?` | 当前页的按键帮助 |
| `[`、`]` | 预览用的样例会话 |
| `-`、`+`（或 `=`） | 预览宽度，每次 4 列 |
| `{`、`}` | 预览的色深、字符档 |
| `R` | 在样例和最近一次真实会话之间切换 |

各页在页脚显示的键（各页的 `keys()`）：

| 页面 | 键 | 来源 |
| --- | --- | --- |
| Home | `↑↓` 移动，`Enter` 打开，字母键修复（`s` 连接 `statusLine`、`i` 设 `refreshInterval`、`m` 去 Models、`u` 复制升级命令） | `src/tui/pages/home.rs:246-275` |
| Presets | `←→↑↓` 选择，`Enter` 应用，`L` 只用布局，`S` 只用外观，`N` 存为预设，`Tab` 过滤；在自己的预设上还有 `W` 覆盖、`E` 改名、`X` 删除 | `src/tui/pages/presets.rs:338-351` |
| Layout（Library） | `Tab` 换栏，`Enter` 加入或移除，`←→` 折叠，`/` 搜索，`t` 切换编辑状态行还是后台任务行 | `src/tui/pages/layout/mod.rs:546-557` |
| Layout（Canvas） | `Tab`，`Alt+方向键` 移动，`x` 移除，`n` 新行，`t`，`a` 添加 | `:558-572` |
| Layout（Inspector） | `Tab`，`←→` 改值，`Enter` 编辑，`Del` 恢复默认 | `:573-578` |
| Style | `Tab`，`↑↓` 移动，`Enter` 或 `←→` 选取；Tokens 栏里 `Enter` 改颜色，`Del` 复位 | `src/tui/pages/style.rs:586-596` |
| Models | `Tab`，`↑↓`，`Enter` 编辑，`A` 从没认出的 id 添加；Tester 栏里 `Enter` 复制环境变量行 | `src/tui/pages/models.rs:582-596` |
| Claude Code | `Tab`，`←→` 改值，`Enter` 设定，`a` 写入 `settings.json` | `src/tui/pages/claude.rs:576-583` |

上游 README 写页面用 `1`–`6` 切换（`README.md:120`），`v2.0.1` 的源码里没有数字键的处理，测试 `navigates_pages_from_the_tab_row` 里还有一句 “Digits no longer switch pages”（`src/tui/app.rs` 的测试模块）。这一点以源码为准。

### 预览怎样和真实渲染共用代码

共用的是整条管线，只有最后一步不同。

- 真实渲染：`statusline::lines(&input, &config, &registry, frame)` 得到 `Vec<Line>`，其中 `Line` 是 `Vec<Span>`，再由 `render::ansi::emit` 写成 ANSI（`src/main.rs:90`、`:109`）。
- 预览：`Preview::render` 调用同一个 `statusline::lines`，再由 `tui::convert::line` 把 `Span` 直接转成 ratatui 的 `Line`，不经过 ANSI（`src/tui/preview.rs:290-309`，`src/tui/convert.rs:1-2`、`:33-40`）。
- 后台任务行同理，两边都调用 `statusline::subagent_lines`（`src/main.rs:133`，`src/tui/preview.rs:274-287`）。
- Layout 页显示“哪个 item 被降级、丢弃、没有数据”，用的是 `statusline::report` 和 `statusline::item_levels`，也在同一个文件里（`src/statusline.rs:114-213`，`src/tui/preview.rs:311-354`）。

两边的差别在送进管线的 `Frame`（`src/statusline.rs:18-28`）：

| 字段 | 真实渲染（`src/main.rs:79-89`） | 预览（`src/tui/preview.rs:220-263`） |
| --- | --- | --- |
| 输入 | stdin | 内置样例 `scenarios::SCENARIOS`（9 个），或最近一次真实会话的 `last_input` |
| `now` | 当前时间 | 样例用固定的 `BASE`；回放用该会话最后保存的时间 |
| `git` | `GitSource::Cached(cache_ms)` | `GitSource::Fixed(...)`；回放时在打开配置器那一刻读一次 git，限时 800 ms（`:66-73`） |
| `commands` | `Commands::Run`，真的执行 `cmd` item | `Commands::Show`，只显示命令行或 `sample` 选项 |
| `state` | `state::observe` 读写出来的历史 | 样例自带的历史，或会话里存的样本 |
| `width` | 由 `COLUMNS`、`[layout]`、`statusLine.padding` 算出 | 预览栏的宽度，可用 `-` `+` 调 |

预览还能临时覆盖色深和字符档而不改配置（`src/tui/preview.rs:28-40`、`:209-216`）。Presets 页上，预览显示的是光标下的预设而不是正在编辑的配置（`src/tui/pages/mod.rs:150-154`，`src/tui/app.rs:137-143`）。

### 保存

保存是显式的，没有自动保存。

- 编辑先进内存里的 `Doc`，每次改动是一步撤销，最多留 200 步（`src/tui/doc.rs:12-13`、`:112-125`）。有未保存改动时标题栏显示 `*`（`src/tui/app.rs:121`）。
- `^S` 触发 `Doc::save(force = false)`：只写改过的文件（`config.toml`、`models.toml`）；文件的修改时间和加载时不一样就拒绝，弹出冲突对话框，`O` 覆盖、`R` 重新加载、`Esc` 取消（`src/tui/doc.rs:167-199`，`src/tui/dialogs/prompts.rs:60-101`）。
- 有未保存改动时退出会问：`S` 保存并退出，`D` 丢弃，`Esc` 留下（`src/tui/app.rs:388-395`，`src/tui/dialogs/prompts.rs:15-57`）。
- 首次设置走完最后一步也触发保存（`src/tui/dialogs/setup.rs:236-242`）。
- 用户预设不走这条路：新建、覆盖、改名、删除都立即写文件，不进撤销历史（`src/tui/doc.rs:49-51`，`src/presets.rs:402-403`）。
- 对 Claude Code `settings.json` 的改动只是草稿，看过 diff 按 `Y` 才写；`^S` 在写完 ccline 自己的文件后会把还没应用的 diff 弹出来（`src/tui/app.rs:432-449`，`src/tui/dialogs/settings.rs:97-109`）。

## 4. 入口：渲染还是配置器，子命令和参数

判断顺序（`src/main.rs:16-52`）：

1. clap 解析参数。有子命令就执行并返回。
2. 带了 `-c`/`--config`，或者 stdin 是终端，就开配置器。此时 stdout 不是终端则报 `ccline: the configuration TUI needs a terminal.` 并以 1 退出。
3. 其余情况是渲染：读 stdin。

渲染分两种（`src/main.rs:54-139`）：

- stdin 的 JSON 里有 `tasks` 数组时当作 `subagentStatusLine` 的输入，每个任务输出一行 JSON `{"id": …, "content": …}`，内容为空的任务不输出（`src/input.rs:307-316`）。带了 `--subagent` 但输入不是任务列表时什么都不画。
- 否则按状态行输入解析。解析失败时输出一行 `ccline: could not read the status line input (…)`。

子命令和参数（`src/cli.rs:3-88`；与本机 2.0.1 的 `ccline --help` 输出一致）：

| 命令 | 参数 | 作用 |
| --- | --- | --- |
| `ccline` | `-c`/`--config`，`--subagent`，`-h`/`--help`，`-V`/`--version` | 见上 |
| `ccline preview` | `-s`/`--scenario <名字或 JSON 文件>`（默认 `normal`），`--all`，`--list`，`--subagent`，`-w`/`--width <列数>`（默认取 `$COLUMNS`，否则 100），`--color <auto|truecolor|256|16|none>` | 用自己的配置渲染内置样例或一份抓下来的 JSON；不记录任何状态（`src/main.rs:211-305`） |
| `ccline update` | 无 | 立刻查一次有没有新版本，只提示升级命令 |
| `ccline mod [status|install|uninstall]` | 无；不带动作时是 `status` | 伴随 mod |
| `ccline refresh-git <dir>` | 隐藏 | 后台刷新某目录的 git 缓存，由渲染进程启动 |
| `ccline update-check` | 隐藏 | 后台更新检查 |
| `ccline refresh-cmd [--cwd <dir>] --timeout-ms <n> -- <命令>` | 隐藏 | 后台执行一个 `cmd` item 并缓存输出 |

代码里读的环境变量：`CCLINE_HOME`、`CLAUDE_CONFIG_DIR`（`src/paths.rs`）；`COLUMNS`、`NO_COLOR`、`COLORTERM`、`TERM`、`CLAUDE_PROJECT_DIR`、`DISABLE_COMPACT`、`DISABLE_AUTO_COMPACT`、`CLAUDE_CODE_AUTO_COMPACT_WINDOW`、`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`（`src/env.rs`）；`GIT_DIR`（`src/git.rs:33-35`）；`CCLINE_CLAUDE`（`src/companion.rs:131-138`）。npm 的安装脚本另读 `CCLINE_SKIP_POSTINSTALL`。

## 5. 慢操作：git、命令、更新检查

原则写在 `src/state/swr.rs:1-4`：Claude Code 会取消跑过下一次刷新的渲染，所以渲染不等慢操作；用存下来的值，哪怕过期，刷新交给一个脱离的 ccline 进程。

### git

- 只跑一条命令：`git --no-optional-locks -c core.quotePath=false status --porcelain=v2 --branch`，并设 `GIT_OPTIONAL_LOCKS=0`（`src/git.rs:50-60`）。分支、上游、ahead/behind、各类改动数都从这一次输出解析（`:70-109`）。
- 启动 git 之前先看当前目录或祖先目录有没有 `.git`（或设了 `GIT_DIR`），没有就不启动（`src/git.rs:31-49`）。
- 渲染里等 git 最多 800 ms（`src/git.rs:8-9`）。超时由 `process::run` 实现：每 2 ms 轮询 `try_wait`，到点 `kill`；stdout 在另一个线程读（`src/process.rs:19-76`）。
- 一次渲染里 git 最多取一次，而且只有某个 metric 要用时才取（`OnceCell`，`src/metrics/mod.rs:232-243`）。

缓存在 `state/git/<cwd 的 FNV-1a 哈希>.json`，带写入时间。`[git] cache_ms`（默认 2000）决定新鲜期（`src/state/git_cache.rs:31-58`，`src/state/swr.rs:38-49`）：

| 缓存状态 | 渲染怎么做 |
| --- | --- |
| 比 `cache_ms` 新 | 直接用 |
| 过了新鲜期但不到 10 分钟 | 用旧值，同时启动后台刷新 |
| 没有，或超过 10 分钟 | 渲染自己同步跑 git（限 800 ms）；成功就存，超时就启动后台刷新，这次不显示 git |
| `cache_ms = 0` | 每次渲染都同步跑 git（限 800 ms） |

后台刷新是同一个可执行文件的 `ccline refresh-git <dir>`，限时 10 秒（`src/state/git_cache.rs:13-14`、`:60-71`）。启动前用 `create_new` 建锁文件抢占，锁超过 30 秒视为遗弃（`src/state/swr.rs:13-14`，`src/state/mod.rs:97-120`）。进程脱离方式：Unix 上新进程组，Windows 上 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`，三个标准流都接空（`src/state/mod.rs:122-148`）。

上游 README 写 “git runs in the background, never on the render path”（`README.md:22`）。按上表，缓存缺失或超过 10 分钟时，以及 `cache_ms = 0` 时，git 是在渲染路径上同步跑的，只是有 800 ms 上限。

### `cmd` item

同一套机制（`src/state/cmd_cache.rs`）：

- 输出在 `cache_s` 秒内算新鲜（默认 10），最久用到 1 小时前的旧值（`:20-21`，`src/metrics/custom.rs:13-14`）。
- 没有可用缓存时渲染同步等，每条命令最多 150 ms，一次渲染所有命令合计最多 500 ms，还受该 item 的 `timeout_ms` 限制；等不到就转后台（`:15-19`、`:136-164`）。
- 后台是 `ccline refresh-cmd`，限时 `timeout_ms`（默认 2000）。
- 命令经 shell 执行：Windows 上 `cmd /C`，其他平台 `sh -c`（`:69-82`）。只取第一行非空输出，去掉转义序列，最多 200 个字符（`:56-67`）。

默认预设里没有 `cmd` item，所以默认的渲染不启动 shell。

### 更新检查

渲染只读 `state/update.json`。到期时（默认 24 小时一次，上次失败则 1 小时后重试）启动脱离的 `ccline update-check`，它依次用 `curl -fsSL --max-time 8` 问 npm registry、npmmirror、GitHub releases，把结果写回文件（`src/state/update.rs:1-5`、`:18-36`、`:76-93`、`:100-134`）。不下载任何东西。

### 渲染路径上其余的同步 I/O

这些没有超时，都是本地小文件（`src/main.rs:73-78`，`src/state/mod.rs:174-236`）：

- 读 `config.toml`、`models.toml`，以及最多三个 Claude Code settings 文件（项目的 `settings.local.json`、`settings.json`，用户的 `settings.json`；`src/env.rs:34-53`）。
- 读本会话的 `state/sessions/<id>.json`；有新样本或距上次保存满 5 秒时写回（`src/state/session.rs:10-15`、`:94-127`）。
- 更新当日花费账本、记录见过的模型 id。
- 读伴随 mod 的 `bridge/<session>.json`；碰上写到一半的文件，等 3 ms 再读一次（`src/state/bridge.rs:136-151`）。

### 性能数字

上游的说法是 2.0.0 比 1.1.2 快 7–12 倍（`CHANGELOG.md:38`），一次渲染比进程启动多几毫秒（`README.md:22`）。仓库的文件列表里没有 `benches/` 目录或基准脚本，这些数字我没有复现。

## 6. 分发，以及怎样改 Claude Code 的设置

### npm

- 主包 `@cometix/ccline`：`bin` 把 `ccline` 指到 `bin/ccline.js`，`postinstall` 跑 `scripts/postinstall.js`，七个平台包列在 `optionalDependencies`，`engines.node >= 14`（`npm/main/package.json`）。主包的 tarball 只有 4 个文件，二进制都在平台包里（npm registry 的 `dist.fileCount`）。
- 平台包：`@cometix/ccline-darwin-x64`、`-darwin-arm64`、`-linux-x64`、`-linux-x64-musl`、`-linux-arm64`、`-linux-arm64-musl`、`-win32-x64`（`npm/platforms/*/package.json`）。没有 Windows arm64 包；`win32-ia32` 映射到 x64 包。
- `postinstall.js` 把平台包里的二进制放到 `~/.claude/ccline/ccline`（Windows 是 `ccline.exe`）：Windows 上复制，其他平台先试硬链接，不行再复制，然后 `chmod 755`。Linux 上用 `ldd --version` 判断 libc，musl 或 glibc 低于 2.35 时选 musl 包。`npm_config_loglevel=silent` 或 `CCLINE_SKIP_POSTINSTALL=1` 时不打印；出错不让安装失败。脚本不碰 `settings.json`，目标目录写死为 `~/.claude/ccline`，不看 `CCLINE_HOME`（`npm/main/scripts/postinstall.js`）。
- `bin/ccline.js` 是个转发器：`~/.claude/ccline/ccline` 存在就运行它，否则运行平台包里的二进制，`spawnSync` 且 `stdio: 'inherit'`（`npm/main/bin/ccline.js`）。

### 发布流程

`.github/workflows/release.yml`，推 `v*` tag 时触发：

- 构建 7 个目标：`x86_64-unknown-linux-gnu`（在 ubuntu-22.04 上）、`x86_64-unknown-linux-musl`、`aarch64-unknown-linux-gnu` 和 `aarch64-unknown-linux-musl`（用 `cargo-zigbuild`）、`x86_64-pc-windows-gnu`（在 Ubuntu 上用 mingw-w64 交叉编译）、`x86_64-apple-darwin`、`aarch64-apple-darwin`（`:18-42`）。
- 打包成 GitHub Release 的 7 个附件：`ccline-linux-x64.tar.gz`、`ccline-linux-x64-static.tar.gz`、`ccline-linux-arm64.tar.gz`、`ccline-linux-arm64-static.tar.gz`、`ccline-macos-x64.tar.gz`、`ccline-macos-arm64.tar.gz`、`ccline-windows-x64.zip`。`v2.0.1` 的 release 上正是这 7 个（<https://github.com/Haleclipse/CCometixLine/releases/tag/v2.0.1>）。发布说明由 git-cliff 按 `cliff.toml` 生成。
- 然后发 npm：`npm/scripts/prepare-packages.js` 把 tag 里的版本号写进各 `package.json`，先发平台包，等 30 秒，再发主包；用 npm trusted publishing（OIDC），不用 token（`:138-233`）。

### cargo

crates.io 上没有 `ccometixline` 这个 crate（2026-11-14 查询 <https://crates.io/api/v1/crates/ccometixline>，返回 “does not exist”）。README 给的源码安装方式是 `cargo build --release` 后把 `target/release/ccometixline` 复制成 `~/.claude/ccline/ccline`（`README.md:66-77`）。

### 升级

`ccline update` 和后台检查都只提示，升级靠 `npm update -g @cometix/ccline`（`src/main.rs:195-209`）。CHANGELOG 记载 1.1.2 已经删掉了自更新的下载和安装代码（`CHANGELOG.md:61-64`）。

### 改 `settings.json`

只有配置器会改，命令行没有对应的子命令或参数（`src/cli.rs`）。逻辑在 `src/cc_settings.rs`：

- 目标文件是用户级的 `settings.json`：`$CLAUDE_CONFIG_DIR/settings.json`，否则 `~/.claude/settings.json`（`:93-96`，`src/paths.rs:34-40`）。
- 文件必须是一个纯 JSON 对象，否则拒绝编辑并提示手工修（`:98-117`）。
- `set_status_line` 写 `statusLine` 的 `type`（固定 `"command"`）、`command`、`padding`、`refreshInterval`（至少 1）、`hideVimModeIndicator`；`statusLine` 里它不认识的键保留（`:54-61`、`:165-194`）。`set_subagent` 写 `subagentStatusLine` 的 `type` 和 `command`（`:196-209`）。其余顶层键不动，顺序不变。
- 写之前一定先给 diff：只列有变化的顶层键，按行对比（`:236-259`、`:293-324`）。
- 保存时（`:267-290`）：文件自读取后在磁盘上变过就拒绝；旧内容先存成同目录的 `settings.json.ccline-bak`；符号链接会写到它指向的文件；写入是原子的；格式是两空格缩进的 JSON 加一个换行。
- 建议的 `command`：`~/.claude/ccline/ccline` 存在就用它，否则用当前可执行文件的路径；非 Windows 上把家目录写成 `~/`（`:68-90`）。判断现有命令是不是 ccline，看字符串里有没有 `ccline` 或 `ccometixline`（`:63-66`）。

触发写入的三个地方，每处都先显示 diff：首次设置的第四步按 `Y`（`src/tui/dialogs/setup.rs:296-305`）；Home 页的 `s` 和 `i`（`src/tui/pages/home.rs:157-177`）；Claude Code 页的 `a`（`src/tui/pages/claude.rs:130-143`）。

渲染路径只读这些 settings 文件，取 `statusLine.padding` 和自动压缩相关的设置（`src/env.rs:66-110`）。

伴随 mod 的安装是另一条路：`ccline mod install` 把 mod 的文件写到 `~/.claude/ccline/mod/` 作为一个本地插件市场，然后调用 Claude Code 自己的 `claude plugin marketplace add …` 和 `claude plugin install cometix@ccline --scope user`，安装记录由 Claude Code 自己写（`src/companion.rs:1-5`、`:164-207`、`:284-291`）。

## 7. 段模型

### 没有 trait

2.x 里“段”拆成三层：数据（metric）、形态（widget）、外观（skin 和 palette）。CHANGELOG 的说法是 “Data, form and look are separate layers”（`CHANGELOG.md:15`）。行上的一个 item 是三者的组合，没有哪个类型实现 segment trait。

metric 是一个普通结构体加一个函数指针（`src/metrics/mod.rs:136-139`）：

```rust
pub struct Metric {
    pub meta: MetricMeta,
    pub compute: fn(&Ctx, &Options) -> Option<Reading>,
}
```

所有 metric 由各子模块的 `metrics()` 汇进一张静态表 `catalog()`，按 id 字符串查找（`src/metrics/mod.rs:271-288`）。`v2.0.1` 里共 37 个 id：

| 文件 | id |
| --- | --- |
| `src/metrics/identity.rs` | `model`、`output_style`、`vim`、`agent`、`session_name`、`cc_version`、`update` |
| `src/metrics/usage.rs` | `context`、`quota.5h`、`quota.7d`、`quota.spend`、`quota.eta`、`cost`、`cache`、`duration` |
| `src/metrics/projection.rs` | `context.trend`、`context.to_compact`、`cost.rate`、`cost.today`、`cache.cold_cost` |
| `src/metrics/code.rs` | `dir`、`git`、`repo`、`pr`、`worktree`、`lines` |
| `src/metrics/custom.rs` | `cmd` |
| `src/metrics/live.rs` | `context.breakdown`、`activity.tools`、`activity.agents`、`timing.ttft`、`timing.tps` |
| `src/metrics/task.rs` | `task.label`、`task.name`、`task.elapsed`、`task.status`、`task.trend` |

`MetricMeta` 是每个 metric 的静态描述（`src/metrics/mod.rs:115-134`）：`id`、`category`、`name`、`summary`、默认图标 `icon`、默认 `widget`、三档默认模板 `templates`、模板可用的 `fields`、默认阈值 `levels`、所需的最低 Claude Code 版本 `min_cc`、是否需要伴随 mod `needs_mod`、选项说明 `options`（配置器据此生成编辑控件）、`stateful`、默认 `priority`。

### metric 收到什么

`compute` 的两个参数（`src/metrics/mod.rs:27`、`:162-180`）：

- `Ctx`：解析后的 stdin `input`；模型注册表 `registry`；`now` 和时区 `tz`；字符档 `glyphs`；Claude Code 版本 `cc`；`state`（本会话的历史样本、当日花费、已知的最新版本、伴随 mod 的数据）；自动压缩设置 `compact`；`commands`（真的执行还是只展示）；后台任务行对应的 `task`。另有两个按需计算并缓存的方法 `git()` 和 `model()`。
- `Options`：这个 item 在配置里的 `options` 表。

返回 `Option<Reading>`，`None` 表示这个 item 这次不显示。`Reading`（`src/metrics/mod.rs:35-51`）里有：命名的文本字段 `fields`（给模板用，每个字段可带一个 `Role`）、`ratio`（条和表盘的填充比例）、`elapsed`（时间窗已过的比例）、`series`（迷你折线的历史）、`severity`、`icon`（替换默认图标）、`link`。metric 不知道自己会被怎样显示（`src/metrics/mod.rs:1-3`）。

### 从 `Reading` 到一行

`statusline::lay_item`（`src/statusline.rs:275-321`）对每个 item 做四件事：

1. 查到 metric，调用 `compute`。
2. 把 item 配置盖在 `MetricMeta` 的默认值上，得到 `widgets::Props`：widget 种类、条的字符集、格数、三档模板、图标、阈值、着色方式。
3. 对 `Full`、`Compact`、`Minimal` 三档各调用一次 `widgets::render`，得到 `Vec<Piece>`。`Piece` 是一段文本加一个 `Role`（`Icon`、`Text`、`Strong`、`Muted`、`Sep`、`Fill`、`Gradient`、`Empty`、`Good`、`Bad`、`Pace`），还可带链接（`src/style/piece.rs:14-52`）。
4. 组成 `LaidItem`：三档片段、类别、严重程度、解析好的 `color`、`fg`、`icon_color`、优先级（`src/layout.rs:11-22`）。

之后 `layout.rs` 决定每个 item 用哪一档、放在哪一行：宽度已知时状态行在 item 之间换行；必须保持一行的（后台任务行，或一个 item 就比终端宽）按优先级从低到高逐档降级，最后丢弃（`src/layout.rs:1-4`、`:112-248`）。

### 颜色和图标怎样确定

widget 不选颜色，只标角色；上色在 `style::skin::paint` 里按角色、调色板、皮肤完成（`src/style/piece.rs:1-3`，`src/style/skin.rs:340-429`）。

非填充型皮肤（`plain`、`bracket`）里（`skin.rs:346-384`）：

| 角色 | 前景色 |
| --- | --- |
| 基色 | item 的 `color`，否则是 metric 类别对应的调色板 token（`custom` 类别用 `accent`）（`skin.rs:177-179`，`palette.rs:166-183`） |
| `Icon` | `icon_color`，否则基色 |
| `Text`、`Strong` | 严重程度的颜色（`ok`、`warn`、`crit`），否则 `fg`，否则基色；`Strong` 加粗 |
| `Muted`、`Empty` | `muted` |
| `Sep` | `separator` |
| `Fill(sev)` | 该严重程度的颜色，否则基色 |
| `Gradient(t)` | 沿 `ok → warn → crit` 的插值；调色板不全是 RGB 时分三段 |
| `Good`、`Bad` | `ok`、`crit` |

填充型皮肤（`pill`、`powerline`）里，每个 item 先由 `segment()` 定出背景和文字色：item 没有自己的 `color` 时先用调色板的 `<类别>.bg` 和 `<类别>.fg`，没有再按 `fill` 推出来，推出来的颜色会调到对比度不低于 4.5；`warn` 或 `crit` 时把文字改成该颜色，读不清就把背景改成该颜色（`skin.rs:238-296`）。

严重程度来自 `Reading.severity`，没有就用 `ratio` 对 `levels` 阈值比较（`src/widgets/mod.rs:60-75`）。

图标：item 写了 `icon` 就用它；否则用 `Reading.icon` 或 `MetricMeta.icon` 这个 `IconKey` 在当前字符档下的字符（`src/statusline.rs:292-294`）。

色深在最后输出时才处理：`Span` 里存的是具体颜色，`emit` 按 `color_depth` 把 RGB 降到 256 色或 16 色（`src/render/ansi.rs:70-92`，`src/style/color.rs:105-120`）。输出时每行前后各加一个复位序列，没有可见文字的行不输出，文本里的控制字符全部去掉（`src/render/ansi.rs:1-6`、`:29-36`、`:94-98`）。

### 测试怎样覆盖渲染

- 单元测试写在各模块末尾的 `#[cfg(test)]` 里。按 `#[test]` 属性计数，`src/` 下 198 个，`tests/` 下 2 个。
- 快照测试 `tests/snapshots.rs` 用 insta：8 个内置预设 × 9 个样例会话 × 宽度 60、100、140，经 `statusline::lines` 和 `ansi::emit` 输出成纯文本，每行末尾加一个 `⏎` 标出右边界；另有 `cometix` 和 `dashboard` 两个预设在宽度 40、60、100 下的后台任务行（`tests/snapshots.rs:14-85`）。快照文件 10 个，在 `tests/snapshots/`。
- 夹具是 `src/scenarios.rs`：每个样例是一段内联 JSON，加上 git 状态、历史样本、伴随 mod 数据；时间固定为 `BASE = 1_790_812_800`，时区 UTC（`src/scenarios.rs:11-28`、`:169`）。测试用 `GitSource::Fixed`、`Commands::Show`、`Registry::default()`，不启动 git，不执行命令。
- 快照用 `EmitOptions::plain()`，不含颜色（`src/render/ansi.rs:20-27`）。颜色由单元测试覆盖：`src/style/skin.rs`、`src/style/palette.rs`、`src/render/ansi.rs`、`src/tui/convert.rs` 的测试模块。
- 配置器的测试用 ratatui 的 `TestBackend` 把整屏画成文本，再断言包含某些字符串；按键通过 `handle_key` 直接送入（`src/tui/app.rs` 测试模块里的 `screen`、`key`、`ctrl`）。
- 改 `settings.json` 的测试在临时目录里进行（`src/cc_settings.rs` 测试模块）。伴随 mod 的测试在没设 `CCLINE_CLAUDE` 时拒绝运行 `claude`（`src/companion.rs:253-257`）。
- CI 跑 `cargo test --verbose`、`cargo fmt -- --check`、`cargo clippy -- -D warnings`，另在 Ubuntu、Windows、macOS 上各做一次 release 构建（`.github/workflows/ci.yml`）。`src/process.rs` 的测试只在 Unix 上编译（`:78`）。
- `statusline::theme` 加载调色板时会先看用户目录的 `palettes/<name>.toml`，所以快照测试的输出并不完全独立于运行测试的机器上的 `CCLINE_HOME`（推断；`src/statusline.rs:30-31`，`src/style/palette.rs:212-224`）。

测试套件我没有在本机运行。

## 2.x 和 1.x 的差别

CHANGELOG 的 `2.0.0` 一节称这是一次重写，配置格式是新的，1.x 的不读（`CHANGELOG.md:13-46`）。对照 `v1.1.2` 的源码：

| 方面 | 1.1.2 | 2.0.1 |
| --- | --- | --- |
| 目录 | `src/config/{defaults,loader,models,types}.rs`、`src/core/{statusline.rs,segments/*}`、`src/ui/{app,events,layout,main_menu,components/*,themes/*}`、`src/updater.rs`、`src/utils/{claude_code_patcher,credentials}.rs` | 见第 1 节 |
| 段的接口 | `trait Segment { fn collect(&self, input: &InputData) -> Option<SegmentData>; fn id(&self) -> SegmentId; }`，`SegmentData` 是 `primary`、`secondary`、`metadata`（`v1.1.2:src/core/segments/mod.rs`） | 结构体加函数指针，返回 `Reading`；显示形态和颜色不在段里 |
| 段的种类 | 9 个固定的 `SegmentId`：`model`、`directory`、`git`、`context_window`、`usage`、`cost`、`session`、`output_style`、`update`（`v1.1.2:src/config/types.rs`） | 37 个 metric id |
| 配置的键 | `theme`、`[style]`（`mode`：`plain`、`nerd_font`、`powerline`；`separator`）、`[[segments]]`（`id`、`enabled`、`icon.plain`、`icon.nerd_font`、`colors.icon`、`colors.text`、`colors.background`、`styles.text_bold`、`options`）（同上） | 见第 2 节 |
| 顺序和启用 | `[[segments]]` 的数组顺序，加每段的 `enabled` | `[[line]]` 里 `left`、`right` 的数组顺序；在不在数组里 |
| 颜色写法 | `{ c16 = n }`、`{ c256 = n }`、`{ r, g, b }` | 字符串：token 名、色名、`#rrggbb`、`256:n`、`16:n` |
| 主题 | 9 个内置主题：`cometix`、`default`、`minimal`、`gruvbox`、`nord`、`powerline-dark`、`powerline-light`、`powerline-rose-pine`、`powerline-tokyo-night`。每个主题是一份完整的配置，存为 `~/.claude/ccline/themes/<name>.toml`；每次 `Config::load()` 都会补写缺失的内置主题文件；`--theme` 可临时指定（`v1.1.2:src/ui/themes/presets.rs`、`v1.1.2:src/config/loader.rs`） | 没有主题；预设和调色板分开，内置的都在二进制里，渲染不写这些文件 |
| 入口 | `-c` 开配置器，`--theme`，`--patch`；stdin 是终端时先出一个主菜单（`v1.1.2:src/main.rs`、`v1.1.2:src/cli.rs`） | 见第 4 节；stdin 是终端直接进配置器，多了子命令 |
| 输入解析 | `model`、`workspace`、`transcript_path` 是必填字段，整体 `serde_json::from_reader`（`v1.1.2:src/config/types.rs`、`v1.1.2:src/main.rs`） | 所有字段可缺，逐字段宽容解析 |
| git | 同步执行多条 git 命令（`rev-parse --git-dir`、`branch --show-current`、`status --porcelain`、`rev-list --count`、`rev-parse --short`），该文件里没有超时和缓存（`v1.1.2:src/core/segments/git.rs`） | 一条命令，800 ms 上限，缓存加后台刷新 |
| 依赖 | 另有 `crossterm`（直接依赖）、`ansi_term`、`ansi-to-tui`、`ureq`、`semver`、`tree-sitter`、`tree-sitter-javascript`（`v1.1.2:Cargo.toml`） | 这些都去掉了 |
| 去掉的功能 | — | 主题和 `--theme`；Claude Code 补丁器 `--patch`；向 OAuth 用量接口发请求（`CHANGELOG.md:44-47`） |
| 保留的 | — | `models.toml` 的 1.x 格式仍然能读（`CHANGELOG.md:42`） |

1.x 的预览依赖 `ansi-to-tui`，应是先生成 ANSI 再转回 TUI 文本（推断；依据是该依赖和 2.x `src/tui/convert.rs:1-2` 里 “no ANSI round trip” 的注释，1.x 的预览组件我没有读）。

## 和 agyline README 的对照

agyline 的 `README.md` 写参考行跟随 “CCometixLine 2.0's `cometix` preset, `terminal` palette, and `plain` skin: items separated by ` | `, model and effort separated by ` · `”。这几个名字在 `v2.0.1` 的源码里都存在，写法一致：

| README 里的说法 | 源码 |
| --- | --- |
| `cometix` preset | `src/presets.rs:21`、`:24-44`，是默认预设 |
| `terminal` palette | `src/style/palette.rs:272`、`:341-367`；`cometix` 预设里 `palette = "terminal"` |
| `plain` skin | `src/style/skin.rs:13-16`；`cometix` 预设里 `skin = "plain"` |
| item 之间是 ` \| ` | `cometix` 预设里 `separator = " \| "`（`src/presets.rs:32`） |
| 模型和 effort 之间是 ` · ` | `model` 的默认模板 `{name}[ · {effort}]`（`src/metrics/identity.rs:49`） |
| effort 的写法 `Low`、`Med`、`High`、`xHigh`、`Max` | `src/metrics/identity.rs:160-181` |

没有发现不一致。两点补充：`cometix` 预设还有第四个外观设置 `glyphs = "nerd"`，README 没有提到；配置文件的位置可以用 `CCLINE_HOME` 挪动，且文件只在配置器保存时才写出。

## 没能确定的

- 性能数字（快 7–12 倍、每次渲染几毫秒）是上游的说法，没有复现，仓库里也没有找到基准。
- 没有运行上游的测试套件。
- 配置器里以下文件没有逐行读完：`src/tui/pages/layout/` 下的 `canvas.rs`、`inspector.rs`、`library.rs`、`edit.rs`、`dialogs.rs`，`src/tui/pages/style.rs`、`models.rs`、`presets.rs`，以及 `src/tui/dialogs/` 里除 `help`、`setup`、`settings`、`prompts`、`commands` 之外的对话框。第 3 节各页的按键取自 `keys()` 返回的页脚提示，页面内部可能还处理了没有列在提示里的键。
- `mod/cometix/hooks/ccline.ts`（伴随 mod 的实现）没有读。
- `src/input.rs` 里 `MAX_INPUT_BYTES` 的数值没有记录。
- “选的全是默认值时不创建 `config.toml`”是推断，没有实际跑一遍首次设置。
- crates.io 只查了 `ccometixline` 这一个名字。
- `README.zh.md` 没有和 `README.md` 逐段比对。
- `.gitignore` 里写着 `/docs/` 是 “Design notes stay local”，上游的设计笔记不在仓库里，设计取舍的理由只能从代码注释和 CHANGELOG 看到。
