# agyline

[English](README.md) | [中文](README.zh-CN.md)

agyline 是给 Antigravity CLI（`agy`）用的状态栏：一个独立的 Rust 程序，自己带配置。它在 `agy` 里的位置，对应 [CCometixLine](https://github.com/Haleclipse/CCometixLine) 在 Claude Code 里的位置。

这个 Rust 程序还没写进仓库。现在能跑的是 `ref/agy-patch` 里的脚本。该目录是本机参考材料，已写进 `.gitignore`。

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

## 做成独立程序

每次刷新都要启动 PowerShell 或 Python。git status 赶在这一行印出来之前，缓存 touch 还会再开一个 shell。

换成 Rust 二进制之后，这次刷新就是该进程自己。它打印这一行时不用等 git，touch 也不再另开 shell。这个二进制是目标，仓库里还没有。

上面的环境变量是要留下的行为。布局和外观改配置文件。CCometixLine 把配置放在 `~/.claude/ccline/config.toml`，运行 `ccline` 来改。agyline 对 `agy` 用同一套做法。

## 打算做成的接口

下面这些还没有代码。实现时用这些名字。

| 部分 | 位置 |
| --- | --- |
| 二进制 | `agyline` |
| 配置目录 | `~/.gemini/agyline/`，或 `AGYLINE_HOME` 指向的目录 |
| 配置文件 | `config.toml`：预设、配色，以及每一行放哪些段。字段名和加载器在同一次改动里定下来 |
| 在终端里直接运行 | 配置器，预览和真实状态栏用同一套渲染 |
| 接到 `agy` | `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command`。写入前先把改动显示出来 |
| 默认外观 | 上面的参考行 |

官方的 `agy` 和 `agy.exe` 保持安装时的样子。`agy` 把 `agyline` 当作 `statusLine` 命令来跑。

## 仍留在参考目录里的部分

按目前的目标，`ref/agy-patch` 里这些内容继续只做参考。

| 路径 | 作用 |
| --- | --- |
| `statusline.ps1`、`statusline.py` | agyline 默认要对齐的那一行 |
| `install.ps1` | 现在如何把 `settings.json` 指到脚本 |
| `cache_relay.py` | 提示缓存保活。状态栏可以继续读它的状态文件。要不要用 Rust 重写中继，另作决定 |
| `hub-wrapper.ps1`、`hub-wrapper.sh` | 本地 CLIProxyAPI 的网关环境 |
| `patch_agy.py`、`references/` | macOS 上 `agy` 二进制的 Mach-O 补丁和 TUI 笔记 |
| `consult_gemini/` | 把多个 Claude Code 实例复用到一个 `agy acp` 进程上的 MCP 服务 |
