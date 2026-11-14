# agy 1.5.9（Windows）：从启动到第一次输入，何时调用 `statusLine.command`

对应 [bahayonghang/agyline#3](https://github.com/bahayonghang/agyline/issues/3)。观察时间 2026-11-14，`agy.exe` 1.5.9，Windows 11 build 26300。

每条结论后面标了来源：

- 【观察 `<run>`】在 `captures/<run>/` 里能看到，run 名见“场景”一节。
- 【读取 `<来源>`】从二进制字符串、参考脚本源码或日志文件里读到。
- 【推断】没有直接看到，由前两类得出。

## 回答

**agy 在启动时调用了 `statusLine.command`，而且调用了两次；状态栏不出现是因为 `statusline.ps1` 这两次都没有输出。**

1. 启动后 0.32–0.64 s 第一次调用，再过约 0.3 s 第二次调用。两次的 stdin 里 `session_id` 和 `conversation_id` 都是空字符串 `""`（键存在，值为空）。【观察 全部 18 个带状态栏的 run】
2. `statusline.ps1` 遇到空字符串的 `session_id` 就 `exit 0`，不打印任何内容；agy 把“退出码 0、输出为空”当作正常结果，不记失败、不写日志，状态栏那一行留空，也不回退到内置状态栏。【读取 `ref/agy-patch/statusline.ps1` 178–179 行】【观察 `s1-ps1`、`s3-empty`】
3. 之后 agy 不会自己重跑命令：空闲 330 s 没有任何调用。【观察 `s5-idle-long`】
4. 会话是懒创建的。第一条斜杠命令执行完、结果写进对话记录时，agy 才 `Starting new conversation`；此后的调用 `session_id` 才是 UUID，`statusline.ps1` 才第一次有输出。【观察 `s1-ps1`、`s1-ps1-direct`】

所以 issue 里的四个候选里，成立的是“调用了但没有显示，原因是空输出”。不成立的是：启动时不调用、超时、被记为失败、首帧不重绘。

置信度：**由观察确认**。同一条命令行（`pwsh … -File statusline.ps1`，不加包装）在隔离环境里复现了现象：启动后没有状态栏，`/model` 回车再 Esc 之后 1.0 s 出现。【观察 `s1-ps1-direct`、`s7-color-ps1-direct`】只改 `statusline.ps1` 那两行的副本，启动后 2.1 s 就出现状态栏。【观察 `s1-ps1-patched`】

没有直接观察的一环：用户真实环境是登录账号的会话，隔离环境用的是 API key 模式。两者的对应关系见“和真实环境的对应”。

## 场景

每个 run 用独立的 home、临时目录和工作区（一个 `git init` 过的空目录 `ws`），终端 40×120。除 `s6-onboarding` 外都预置了“已完成 onboarding”的状态。

| run | 状态栏命令 | 步骤 |
| --- | --- | --- |
| `s1-minimal` | 记录器，立即返回并打印 `AGYLINE-MARK #<序号> <时刻>` | 启动，等 60 s，`/`，`model`，回车，Esc |
| `s1-py` | 记录器包装 `python -I ref/agy-patch/statusline.py` | 启动，等 15 s，同上 |
| `s1-ps1` | 记录器包装 `pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ref/agy-patch/statusline.ps1` | 同上 |
| `s1-py-direct`、`s1-ps1-direct` | 上面两条命令本身，不加记录器 | 同上 |
| `s1-ps1-patched` | 记录器包装 `statusline.ps1` 的副本，只改空 `session_id` 的处理 | 同上 |
| `s1-none` | 不配置 `statusLine` | 同上 |
| `s2-events` | 记录器 | 会话建立之前的各种输入事件 |
| `s2-events-after` | 记录器 | 先 `/model` 建立会话，再重复同一组事件，最后空闲 60 s |
| `s3-empty`、`s3-exit1`、`s3-sleep2`、`s3-sleep8` | 记录器：空输出／退出码 1／睡 2 s／睡 8 s | 启动后反复开关斜杠菜单 |
| `s4-continue`、`s4-continue-ps1` | 记录器／包装的 `statusline.ps1` | 在 `s1-minimal` 留下的 home 和工作区上用 `agy -c` 启动 |
| `s5-idle-long` | 记录器 | 启动后空闲 330 s |
| `s6-onboarding` | 记录器 | 全新 home，停在 onboarding 第一屏 |
| `s7-color-minimal`、`s7-color-ps1-direct` | 记录器／`statusline.ps1` 本身 | 去掉环境变量 `NO_COLOR` 后重复 `s1` |

加包装的 run 能看到 payload 和退出码，但每次多一个 Python 进程（约 50 ms）。不加包装的 run 和用户的命令行一致，靠进程轮询和屏幕内容来判断调用和显示。

## 时间线：启动到第一条斜杠命令

时刻是命令进程创建的时间，单位秒，从 `agy.exe` 启动算起。“结束”一栏是退出码、stdout 字节数、命令耗时。【观察 `s1-minimal`、`s1-py`、`s1-ps1`】

| # | 触发 | `session_id` / `conversation_id` | `agent_state` | `s1-minimal` | `s1-py` | `s1-ps1` |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 启动 | `""` / `""` | `authenticating` | 0.440；0，32 B，25 ms | 0.481；0，145 B，0.22 s | 0.521；0，**0 B**，0.55 s |
| 2 | 启动（+0.3 s） | `""` / `""` | `idle` | 0.741；0，32 B，24 ms | 0.777；0，175 B，0.12 s | 0.816；0，**0 B**，0.56 s |
| — | 空闲 | | | 60 s 内无调用 | 15 s 内无调用 | 15 s 内无调用 |
| 3 | 键入 `/`（打开斜杠菜单） | `""` / `""` | `idle` | 60.119；0，32 B | 15.115；0，175 B | 15.100；0，**0 B** |
| 4 | 同上（+0.3 s） | `""` / `""` | `idle` | 60.412；0，32 B | 15.415；0，175 B | 15.399；0，**0 B** |
| — | 键入 `model`（过滤菜单） | | | 无调用 | 无调用 | 无调用 |
| 5 | 回车（打开模型选择器） | `""` / `""` | `idle` | 65.190；0，32 B | 20.189；0，175 B | 20.195；0，**0 B** |
| 6 | 同上（+0.3 s） | `""` / `""` | `idle` | 65.469；0，32 B | 20.494；0，175 B | 20.529；0，**0 B** |
| 7 | Esc（`/model` 结束） | `""` / `""` | `idle` | 69.277；0，32 B | 24.335；0，175 B | 24.374；0，**0 B** |
| 8 | 同上（+0.3 s） | UUID / 同一个 UUID | `idle` | 69.577；0，32 B | 24.632；0，175 B | 24.681；0，**175 B**，0.74 s |

状态栏第一次出现在屏幕上的时刻：

| run | 第一次可见 | 说明 |
| --- | --- | --- |
| `s1-minimal` | 0.548 s | 第 1 次调用的输出 |
| `s1-py-direct` | 0.712 s | 第 1 次调用的输出 |
| `s1-py` | 0.986 s | 第 2 次调用的输出；第 1 次在第 2 次启动前 21 ms 才结束，没有显示 |
| `s1-ps1-patched` | 2.144 s | 第 2 次调用的输出，这次耗时 1.25 s；之后各次 0.50–0.63 s |
| `s1-ps1` | 25.490 s | 第 8 次调用的输出，Esc 之后 1.27 s |
| `s1-ps1-direct` | 25.174 s | Esc（24.138 s）之后 1.04 s |
| `s7-color-ps1-direct` | 25.164 s | Esc（24.185 s）之后 0.98 s |

`s1-ps1-direct` 里 agy 同样启动了 8 次 `cmd.exe`（0.449、0.725、15.096、15.387、20.136、20.442、24.292、24.579 s），前 7 次的 `pwsh` 没有启动 `git`，第 8 次启动了。【观察 `s1-ps1-direct/timeline.txt`】

18 个带状态栏的 run 里，17 个在启动阶段是 2 次调用；`s1-py-direct` 是 3 次 `cmd.exe`（0.504、0.554、0.810 s），第一个在 50 ms 后被第二个取代。没有 payload，原因未确定。【观察 `s1-py-direct/timeline.txt`】

### 第 1 次调用的 payload

原文（用户名已替换为 `<user>`）。【观察 `s1-minimal/invocations/inv-0001.stdin.json`】

```json
{"cwd":"C:\\Users\\<user>\\AppData\\Local\\Temp\\agyline-research-fr\\runs\\s1-minimal\\ws","session_id":"","conversation_id":"","transcript_path":"C:\\Users\\<user>\\AppData\\Local\\Temp\\agyline-research-fr\\runs\\s1-minimal\\home\\.gemini\\antigravity\\brain\\.system_generated\\logs\\transcript.jsonl","model":null,"workspace":{"current_dir":"C:\\Users\\<user>\\AppData\\Local\\Temp\\agyline-research-fr\\runs\\s1-minimal\\ws","project_dir":"C:\\Users\\<user>\\AppData\\Local\\Temp\\agyline-research-fr\\runs\\s1-minimal\\ws"},"version":"1.5.9","context_window":{"total_input_tokens":0,"total_output_tokens":0,"context_window_size":0,"used_percentage":0,"remaining_percentage":0,"current_usage":null},"exceeds_200k_tokens":null,"product":"antigravity","agent_state":"authenticating","sandbox":{"enabled":false},"terminal_width":80}
```

第 2 次和它的差别：`model` 变成 `{"id":"Gemini 3.1 Pro (Low)","display_name":"Gemini 3.1 Pro (Low)","effort":"low"}`，`context_window_size` 为 `1048576`，`remaining_percentage` 为 `100`，`agent_state` 为 `idle`，多出 `"vcs":{"type":"git"}`，`terminal_width` 为 `120`（终端的真实宽度）。【观察 `inv-0002.stdin.json`】

第 8 次和第 2 次的差别只有三处：`session_id`、`conversation_id` 是同一个 UUID，`transcript_path` 的路径里多了这个 UUID 一级目录。【观察 `inv-0008.stdin.json`】

全部 157 个 payload 里：`session_id` 与 `conversation_id` 始终相等；116 个为空字符串；没有出现 `vcs.branch`、`vcs.dirty`、`cost`、`quota`、`subagents`、`task_count`、`execution_mode`；按 shift+tab 之后多出 `"cycle_mode":"accept-edits"`。这些 run 没有提交过 prompt，也没有登录账号。【观察 `captures/*/invocations/*.stdin.json`】

## 为什么第一条斜杠命令之后才出现

1. 两个脚本对空 `session_id` 的处理不同。【读取 参考脚本源码】

   `ref/agy-patch/statusline.ps1` 178–179 行：

   ```powershell
   $sessionId = [string]($d.session_id ?? $d.conversation_id ?? 'default')
   if ([string]::IsNullOrEmpty($sessionId)) { exit 0 }
   ```

   `??` 只在左边是 `$null` 时取右边，`""` 不是 `$null`，于是 `$sessionId` 是 `""`，下一行直接退出。

   `ref/agy-patch/statusline.py` 326 行：

   ```python
   session_id = str(data.get("session_id") or data.get("conversation_id") or "default")
   ```

   `or` 把 `""` 当作假，落到 `"default"`，照常输出。

   这是 `AGENTS.md` 要求先报告的那一类差异：同一个 payload，两个文件的输出不同。

2. 用户 `settings.json` 指向的 `~/.gemini/antigravity-cli/statusline.ps1` 和 `ref/agy-patch/statusline.ps1` 的 sha256 相同。【读取 两个文件的哈希，见 `captures/environment.txt`】

3. agy 在第一条斜杠命令结束时才创建会话。`s1-ps1` 里 Esc 在 24.218 s 发出，agy 日志在 24.272 s 写 `conversation_manager.go:673] Starting new conversation (agent=false)`，24.353 s 写 `Created conversation 832ff879-…`。第 7 次调用（24.374 s）的 payload 仍是空 id，第 8 次（24.681 s）带上了 UUID。【观察 `s1-ps1/agy-cli.log` 112、115 行，`s1-ps1/timeline.txt`】

   `/statusline help` 回车同样会创建会话。【观察 `s2-events`，第 19 次调用】打开斜杠菜单、shift+tab、调整窗口大小、打开快捷键面板都不会。【观察 `s2-events`】打开模型选择器也不会，要等它关闭。【观察 `s1-minimal`，第 5–7 次调用】

4. 对照：
   - 只把那两行改成和 `statusline.py` 相同的回退顺序，启动阶段的两次调用各输出 145 B 和 175 B，状态栏在 2.144 s 可见。【观察 `s1-ps1-patched`】改动内容在 `harness/observe.py` 的 `PS1_EMPTY_ID_AS_PY`，副本生成在 run 目录里，`ref/` 没有改。
   - 用 `agy -c` 续上已有会话时，第 2 次调用（0.736 s）就带 UUID，未修改的 `statusline.ps1` 输出 175 B，状态栏在 1.600 s 可见。【观察 `s4-continue-ps1`】

## 哪些输入事件会触发调用

【观察 `s2-events`（会话建立之前）、`s2-events-after`（会话建立之后）】两种状态下每个事件的调用次数相同。

| 事件 | 调用次数 |
| --- | --- |
| 启动 | 2 |
| 空输入框里键入第一个字符（`a`） | 1 |
| 再键入一个字符（`b`） | 0 |
| 退格删到空 | 1 |
| 键入 `/`，斜杠菜单打开 | 2 |
| 在斜杠菜单里继续键入（`st`、`model`） | 0 |
| 退格到空，斜杠菜单关闭 | 2 |
| 调整终端大小（120→100 列，100→120 列） | 各 2，payload 的 `terminal_width` 随之变化 |
| 键入 `?`，打开快捷键面板 | 1 |
| Esc 关闭快捷键面板 | 1 |
| shift+tab 切换模式 | 1 |
| 一次送入 `/statusline help` 这 16 个字符 | 2 |
| `/statusline help` 回车 | 2 |
| `/model` 回车，打开模型选择器 | 2 |
| 模型选择器里 Esc | 2 |
| 模型选择器里回车选定 | 2 |
| 空闲 60 s（会话已建立）、60 s 和 330 s（会话未建立） | 0 |

成对的两次调用相隔 0.261–0.346 s，平均 0.299 s（72 对）。【观察 全部 run】单次还是成对，取决于事件本身：改变输入框是否为空的按键是单次，打开或关闭面板、调整大小、执行命令是成对。【推断】间隔固定在 0.3 s 左右，像是“立即跑一次，300 ms 内的后续触发合并成一次补跑”，没有读到实现。

`?` 打开的快捷键面板是全屏的，打开期间状态栏不显示，按键也不会进入输入框，要先 Esc。【观察 `s2-events/screens/27-question-open.txt`】

## agy 怎样运行这条命令

- 命令行是 `cmd /c "<statusLine.command 原文>"`，由 `agy.exe` 直接启动。【观察 `invocations/*.meta.json` 的 `parent_command_line` 和 `ancestors`】
- stdin 是管道，写完一个 JSON 对象就关闭；stdout 不是终端。agy 没有给命令添加 `AGY_*`、`ANTIGRAVITY_*`、`GEMINI_*` 环境变量。【观察 `*.meta.json` 的 `stdin_isatty`、`stdout_isatty`、`env_names_agy`】
- 新一次调用开始时，上一次还没结束的 `cmd.exe` 会被结束，它的输出不显示。被结束的只有 `cmd.exe`，它启动的 `python.exe`、`pwsh.exe` 继续跑完。【观察 `s3-sleep8/timeline.txt`：`cmd.exe` 73136 在 0.813 s 结束，它的子进程 `python.exe` 18484 活到 8.640 s；`s3-sleep2`：屏幕上只出现过 `#0002`、`#0004`、`#0006`】
  - 对 `statusline.ps1`（单次 0.46–0.74 s）来说，每对调用的第一次必然被取代，但它的 `pwsh` 仍然跑完。【观察 `s1-ps1/timeline.txt`：`pwsh.exe` 75408 在自己的 `cmd.exe` 结束后又跑了 0.3 s】
- 超时是 5 s：`cmd.exe` 在启动后 5.0 s 被结束（0.813→5.812、8.957→13.950、20.383→25.397、34.415→39.417）。【观察 `s3-sleep8/timeline.txt`】agy 记一次失败 `command failed: exit status 1`，并在对话区加一条 `⚠ Statusline Error`。失败是在孤儿子进程退出、管道关闭之后才记下的（8.908 s，而不是 5.8 s），随后立刻又调用了一次（8.931 s）。【观察 `s3-sleep8/agy-cli.log` 106 行、`s3-sleep8/screens/01-boot.txt`】
- 退出码非 0：stdout 不显示，记一次失败。日志在第 1、10、20 次失败时各写一行 `statusline: … (failure N/30)`，第 30 次写 `statusline: auto-disabled after 30 consecutive failures`，此后不再调用。【观察 `s3-exit1/agy-cli.log` 104、109、114、119 行；`s3-exit1` 第 30 次调用之后的两个事件没有调用】
- 退出码 0、输出为空：不记失败，日志里没有任何 `statusline` 行，状态栏区域为空。未配置 `statusLine` 时，同一位置显示的是内置的 `? for shortcuts … Gemini 3.1 Pro · low`。【观察 `s3-empty`、`s1-ps1`、`s1-none/screens/01-boot.txt`】
- 慢但不超时（2 s）：命令结束时显示。【观察 `s3-sleep2`，3.010 s 可见】
- onboarding 屏幕上也会调用并显示，第 2 次的 `agent_state` 是 `initializing`。【观察 `s6-onboarding`】

二进制里和这些行为对应的字符串和符号在 `captures/binary-strings.txt`，包括 `store.(*StatusLineRunner).Run`、`recordFailure`、`store.(*Manager).triggerStatusLineUpdate`、`statusLineErrorReported`，以及一条更新日志：`statusLine` 配置有 `stack_with_default`，可以把内置状态栏和自定义输出叠在一起显示。【读取 `agy.exe`】`stack_with_default` 没有试。

## 三种命令的差别

| | 立即返回的记录器 | `statusline.py` | `statusline.ps1` |
| --- | --- | --- | --- |
| agy 的调用时刻和次数 | 相同 | 相同 | 相同 |
| 空 `session_id` 时的输出 | 32 B | 145 B（`model` 为 `null` 时模型名显示 `Model`）或 175 B | 0 B |
| 启动后第一次可见 | 0.5 s | 0.7 s（不包装）、1.0 s（包装） | 第一条斜杠命令结束后约 1 s |
| 单次耗时（含包装） | 24–35 ms | 0.10–0.23 s | 0.46–0.74 s |
| 一对调用里第一次的输出 | 显示 | 包装后常被取代，不包装时显示 | 被取代 |
| 空 id 时写的状态文件 | — | `agy_telemetry_default.dat`、`agy_vcs2_default.dat` | 不写 |

agy 对三种命令一视同仁，差别全部来自脚本。【观察 `s1-*`】`statusline.py` 在会话建立前后用的是两套状态文件（`default` 和 UUID），所以缓存倒计时在会话建立时从 `4:36` 变成 `cold`。【观察 `s1-py-direct/timeline.txt` 24.512 s、24.814 s】

## 和真实环境的对应

没有在真实配置下运行 agy，下面是读到的旁证。

- 真实日志 `~/.gemini/antigravity-cli/log/cli-20261114_024211.log`：53 行 `Launching CLI mode`（02:42:11.58），123 行 `Starting new conversation (agent=false)`（02:42:27.77）。它前面 3 ms 是 `Resolving model …`、`Propagating selected model override to backend`、`Reloading system slash commands` 三行，再往前 1.5 s 是后两行。隔离环境里，打开模型选择器写的正是后两行，选定模型写的正是这三行。【观察 `s2-events-after/agy-cli.log` 138–139、142–144 行】所以真实会话在启动后 16 s 内没有会话，第一条 `/model` 选定之后才创建，和隔离环境一致。【读取 真实日志，只读，未复制】
- 真实日志目录 290 个文件里没有一行来自 `statusline_runner.go`。这和“空输出不记失败”一致。【读取 真实日志】
- 运行的脚本和参考脚本字节相同（见上）。

## 没能确定的

- **登录账号的会话里，启动阶段的 payload 是否也是空 `session_id`。** 隔离环境用的是 `GEMINI_API_KEY` 模式，认证在 0.5 s 内完成。真实环境用 keyring 登录，认证之后还有配额、实验开关等请求，可能多出几次调用，payload 里可能多出 `quota`、`cost` 等字段。上面的真实日志说明会话同样是懒创建的，但没有看到真实 payload。要确认，需要在真实配置里临时把 `statusLine.command` 换成记录器，这一步被本次任务的约束排除。
- **成对调用的机制。** 只看到间隔约 0.3 s，没有反汇编 `triggerStatusLineUpdate`。
- **`s1-py-direct` 启动阶段多出的一次调用**是什么事件触发的。不加包装的 run 没有 payload。
- **调整终端大小后状态栏短暂消失**（约 150 ms，见 `s2-events/timeline.txt` 28.530 s）是 agy 的重绘还是 `pyte` 调整屏幕大小时的清屏，没有区分。
- **提交 prompt 之后的调用节奏**（流式输出期间、工具确认、子代理）没有观察：约束是不提交 prompt。
- **macOS 的行为**没有观察。`ref/agy-patch/references/` 里“运行器只由事件驱动、空闲不重跑”的说法，在 Windows 1.5.9 上和这里的观察一致。
- 屏幕内容是 ConPTY 输出经 `pyte` 重建的，不是真实终端的渲染。判断“是否可见”够用，颜色和字形宽度没有核对。
- **`statusline.ps1` 的耗时不能直接和日常使用比较。** 每个 run 的 `LOCALAPPDATA` 是空目录，`pwsh` 的启动缓存是冷的，而且每次调用还隔着一层记录器。这里量到 0.46–0.74 s，issue 里在真实环境量到的是 340–480 ms。调用的时刻和次数不受影响。

## 对 agyline 的含义

【推断】以下不是观察结论，是从上面得出的设计约束，定下来之前需要单独确认。

- 默认行为要决定：`session_id` 为空时是否输出。照 `statusline.ps1` 做会重现这个现象，照 `statusline.py` 做启动后就有状态栏。
- 空 id 时按会话存放的状态（`agy_telemetry_<session>.dat`）没有归属，`statusline.py` 用的是所有进程共享的 `default`。
- 一次渲染超过约 0.3 s，每对调用的第一次就白跑，而且进程不会被 agy 结束。
- 空输出等于没有状态栏，agy 不会提示。
- 没有输入就没有刷新，倒计时之类随时间变化的内容只在下一个事件时更新。

## 复现

需要 Windows、PowerShell 7、Python 3.14、`agy.exe` 1.5.9。全程不读写真实的 `~/.gemini`：`agy` 进程的 `USERPROFILE`、`HOME`、`APPDATA`、`LOCALAPPDATA`、`TEMP`、`TMP` 都指向 run 目录；模型端点是只回 500 的本机服务；其余出站请求经本机代理记录后拒绝。

```powershell
$root = "$env:TEMP\agyline-research-fr"
python -m venv "$root\venv"
& "$root\venv\Scripts\python.exe" -m pip install pywinpty pyte

cd .trellis\research\agy-first-render\harness
& .\run_scenarios.ps1 -Root $root                       # 全部场景，约 20 分钟
& .\run_scenarios.ps1 -Root $root -Only s1-ps1,s1-py    # 只跑指定场景

python timeline.py "$root\runs\s1-ps1" --procs          # 看一个 run 的时间线
python collect.py "$root\runs" ..\captures              # 脱敏后收进 captures\
Remove-Item -Recurse -Force $root                       # 清理
```

`s4-*` 依赖 `s1-minimal` 留下的 home，要先跑 `s1-minimal`。同名 run 已存在时 `observe.py` 会拒绝覆盖。

隔离方式和参考实现的区别：`capture_prompt.py`、`probe_agy.py` 用 `--app_data_dir=agy-patch/.capture-state`，这个路径相对 `~/.gemini`，会在真实目录下建子目录。这里改为重定向整个 home。`USERPROFILE` 对 `agy.exe` 1.5.9 生效：日志写的是 `CLI app data directory: <run>\home\.gemini\antigravity-cli`。【观察 `s1-minimal/agy-cli.log` 35 行】`--app_data_dir` 不在 `agy.exe --help` 里，但二进制里有 `app_data_dir for CLI settings and logs`，本机的 hub 进程也带着 `--app_data_dir=antigravity` 在跑。【读取 `agy.exe`、进程列表】

`harness/` 里的文件：

| 文件 | 作用 |
| --- | --- |
| `observe.py` | 在 ConPTY 里启动隔离的 `agy`，按步骤送键，记录事件、屏幕、子进程 |
| `sl_logger.py` | 充当 `statusLine.command`：记下时刻、stdin 原文、父进程命令行，再打印标记行或转交给真实命令 |
| `run_scenarios.ps1` | 场景表 |
| `timeline.py` | 把一个 run 的步骤、调用、可见性变化排成一条时间线 |
| `collect.py` | 把 run 的证据脱敏后复制到 `captures/` |

## `captures/` 的内容

每个 run 一个目录：

| 文件 | 内容 |
| --- | --- |
| `timeline.txt` | 步骤、每次调用、屏幕上状态栏的变化、子进程起止，按时间排列 |
| `invocations/inv-NNNN.stdin.json` | agy 写给命令的 stdin 原文 |
| `invocations/inv-NNNN.stdout.txt` | 命令的输出 |
| `invocations/inv-NNNN.meta.json` | 时刻（epoch 秒）、退出码、耗时、祖先进程、父进程命令行 |
| `screens/*.txt` | 每一步之后以及状态栏内容每次变化时重建的屏幕 |
| `events.jsonl` | harness 的事件流 |
| `summary.json` | 调用时间线，`t0` 是 `agy.exe` 启动的 epoch 时刻 |
| `agy-cli.log` | 隔离 home 里 agy 自己的日志 |
| `settings.initial.json` | 这次 run 的 `settings.json` |
| `proxy.jsonl`、`model_server.jsonl` | 被拒绝的出站请求目标；到达模型端点的请求（全部为空） |
| `raw.jsonl` | 带时间戳的终端原始输出 |

另有 `environment.txt`（版本、哈希、环境变量）和 `binary-strings.txt`（从 `agy.exe` 读出的字符串）。

脱敏：路径里的 Windows 用户名换成 `<user>`，agy 日志里的 installation UUID 换成 `<installation-uuid>`；另外扫描了邮箱、机器名和凭据形状的字符串，没有命中。`meta.json` 里的 `stdin_len` 是脱敏前的长度，每处用户名替换使文件多 3 字节。会话 UUID 属于用后即删的隔离 home，保留原样。
