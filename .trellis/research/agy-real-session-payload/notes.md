# 登录账号的真实会话里，状态栏 payload 是什么样

对应 [登录账号的真实会话里，payload 是什么样](https://github.com/bahayonghang/agyline/issues/18)。观察日期 2026-11-14，`agy.exe` 1.5.9（SHA256 `8803C289CAC04D6E53F2211592D9981C2D5760BC0D9B4A7F222EF6ECC7ABB38A`），Windows 11，工作目录为真实 git 仓库 `D:\Documents\Code\Web\Exp\agyline`（分支 `dev`，有已修改跟踪文件和未跟踪目录）。

证据标记：

- 【观察 `run1`/`run2`】见同目录脱敏后的 `captures/run1/`、`captures/run2/`（原始未脱敏文件留在本机会话 scratch 目录，不在仓库工作树内）。
- 【读取 `<来源>`】读自真实 CLI 日志、磁盘路径核对或 `agy.exe` 反汇编。

## 结论摘要

1. **启动阶段 `session_id` 同样全部是空字符串 `""`。** 无输入的 15 秒启动期共触发 **10 次**调用（0.338–5.585 s），每次的 `session_id` 和 `conversation_id` 都是 `""`，被包装的真实 `statusline.ps1` 全部输出 `0 B`、退出码 `0`，屏幕上不显示状态栏。按 `/model` 再按 `Esc` 关闭选择器后，agy 才懒创建会话（24.578 s 第 16 次调用带上 UUID，`statusline.ps1` 输出 `275 B`，25.126 s 状态栏首次可见）。“启动后要等第一条斜杠命令才出现状态栏”在真实登录会话里完整闭环。【观察 `run1`】
2. **启动阶段比隔离环境多出 8 次调用（共 10 次），陆续填入账号与服务端字段。** 隔离 API key 模式下启动只有 2 次调用；真实登录会话随 keyring 认证、模型列表拉取、配额刷新、Mendel 实验开关刷新分批触发，依次多出 `email`（第 2 次）、`model.effort` 与 `context_window_size`（第 3 次）、`quota`（第 4 次）、`cycle_mode: "auto"`（第 6 次，实验开关刷新后自动设置，未按 Shift+Tab）、`plan_tier`（第 7 次）。5.585 s 之后到 15.078 s 空闲期 0 次调用。【观察 `run1`】【读取 `captures/run1/agy-cli.log`】
3. **提交过 prompt 之后的待确认项：**
   - **`quota`**：键名有 4 个，按字典序为 `3p-5h`、`3p-weekly`、`gemini-5h`、`gemini-weekly`；每个桶含 `remaining_fraction`（如 `1`、`0.49016213`、`0.96379286`、`0.8028002`）、`reset_time`（RFC 3339 UTC 字符串）和 `reset_in_seconds`（每次构造 payload 时按当前时刻重算的整数秒），未出现 `remaining_amount` 和 `disabled`。【观察 `run1`、`run2`】
   - **`cost`**：两轮共 **65 个 payload 里始终缺失**（包括模型步骤完成后、子代理完成后、以及 `run2` 整轮对话自然结束回到 `idle` 后）。反汇编确认成本累计受服务端实验开关 `enable-cost-accumulator` 控制（`(*CascadeManager).shouldEnableCostAccounting`，`0x142844860`），当前登录的订阅账号下不开启，因此 `CostSummary` 恒为 nil、`cost` 被省略。【观察 `run1`、`run2`】【读取 `agy.exe`】
   - **`used_percentage` 的小数位**：**不取整**，直接按 Go `float64` 最短精确十进制输出，实测最长 **16 位小数**（如 `20890 / 1048576 * 100` 输出 `1.9922256469726562`，`21839 / 1048576 * 100` 输出 `2.0827293395996094`；分母 `1048576 = 2^20` 能整除更多 2 的因子时位数变短，如 `22800` 输出 14 位 `2.17437744140625`）。分子确认为 `total_input_tokens`（即 `EstimatedTokensUsed`），不含 `total_output_tokens`。【观察 `run1` 第 26/45 次、`run2` 第 18 次】
   - **`subagents[].role` 的内容**：静态分析读到的“优先取 `Prompt`，为空才取 `Role`”**两个分支在真实会话里都出现了**——发起子代理的当次会话（`run1` 第 29/37 次）里，传入 `Role="ProbeRoleLabel"`、`Prompt="Say PONG only and call no tools."`，payload 的 `subagents[0].role` 是完整的 prompt 字符串 `"Say PONG only and call no tools."`；用 `--conversation` 恢复同一条会话（`run2` 第 6 次）时，内存里没有 `Prompt`，`subagents[0].role` 变成 `"ProbeRoleLabel"`。【观察 `run1` 第 29/37 次、`run2` 第 6 次】
   - **`vcs`**：在有真实分支（`dev`）、真实提交、已修改跟踪文件和未跟踪文件的仓库里，全程 63 个带 `vcs` 的 payload **仍然只有 `{"type":"git"}`**，从不出现 `branch`、`dirty`、`client`（屏幕上显示的 `dev ●` 来自 `statusline.ps1` 自己跑的 `git`）。【观察 `run1`、`run2`】
4. **顺带确认的两项事实（回应 [agy 1.5.9 状态栏 payload 有哪些字段](https://github.com/bahayonghang/agyline/issues/4) 的待确认清单）：**
   - **`product` 与 `transcript_path` 路径错位**：`product` 是 `"antigravity"`，payload 给出的 `transcript_path` 是 `C:\Users\<user>\.gemini\antigravity\brain\<id>\.system_generated\logs\transcript.jsonl`，**磁盘上不存在**；CLI 实际写出的转录文件在 `C:\Users\<user>\.gemini\antigravity-cli\brain\<id>\.system_generated\logs\transcript.jsonl`（磁盘核对为真）。【观察 `run1` 第 16 次】【读取 本机路径核对】
   - **`model` 与 `conversation_title`**：`settings.json` 已配 `"model": "Gemini 4 Carbon (High)"` 时，第 1–2 次调用 `model` 非 `null`，含 `id` 和 `display_name`（同为 `"Gemini 4 Carbon (High)"`）但无 `effort`；第 3 次拉完模型列表后补上 `"effort": "high"`。子代理传 `Model="flash_lite"` 时，`subagents[0].model` 是显示名 `"Gemini 3.5 Flash Lite"`。`conversation_title` 在恢复会话时出现（`"Invoke Subagent For Research Task"`）。【观察 `run1` 第 1–3/29 次、`run2` 第 2 次】

## 采集方式与 `settings.json` 校验

在 ConPTY（`40×120`）中直接启动 `%LOCALAPPDATA%\agy\bin\agy.exe`，保留真实 `USERPROFILE` 和登录态，仅剔除父会话注入的 `ANTIGRAVITY_*` 环境变量。运行期间临时把 `~/.gemini/antigravity-cli/settings.json` 的 `statusLine.command` 改为：

```text
python -I D:/Documents/Code/Web/Exp/agyline/.trellis/research/agy-first-render/harness/sl_logger.py <raw-invdir> wrap -- pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:/Users/<user>/.gemini/antigravity-cli/statusline.ps1
```

共跑两轮：

- **`run1`（全新会话，45 次调用）**：纯启动静置 15 s → `/`、`model`、回车、`Esc` → 提交一条调用 `invoke_subagent(TypeName="research", Role="ProbeRoleLabel", Model="flash_lite", Prompt="Say PONG only and call no tools.")` 的请求，抓到子代理 `running` 与 `completed` 状态。
- **`run2`（`--conversation` 续接 `run1` 的会话，20 次调用）**：启动静置 8 s → 提交 `"Reply with the single word OK and do not call any tools."`，等待模型自然回复 `OK` 并回到 `idle` 后再静置 6 s，确认非中断结束后的字段状态。

两轮都在 `finally` 块中写回原始字节并做逐字节与哈希比对：

| 检查项 | 改动前 | `run1` 恢复后 | `run2` 恢复后 |
| --- | --- | --- | --- |
| 字节长度 | `697` | `697` | `697` |
| SHA256 | `81A072A65C7F88CBEF2BEF2E567DE7A059B48CD538CECF6D8663E997D6CD9262` | 相同 | 相同 |
| 逐字节相等（`SequenceEqual`） | — | `True` | `True` |

临时目录 `%TEMP%\agyline-real-venv` 及测试会话产生的 `%TEMP%\agy_*_5a85300c-*.dat` 已清理。

## 时间线与字段变化

### 1. 全新会话：从启动到第一条斜杠命令（`run1` 第 1–17 次）

时刻从 `agy.exe` 启动算起（秒）。“输出”是 `statusline.ps1` 的 stdout 字节数。

| # | 时刻 (s) | 触发原因（对照 `agy-cli.log`） | `session_id` | `agent_state` | 输出 | 与上一次相比的 payload 变化 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 0.338 | `NewManager` 初次 `notify` | `""` | `authenticating` | 0 B | 初始 13 个顶层键：`model` 仅有 `id`/`display_name`（无 `effort`），`context_window_size: 0`，`terminal_width: 80` |
| 2 | 0.634 | keyring OAuth 完成、`doRefreshVCS`、终端尺寸就绪 | `""` | `idle` | 0 B | 新增 `email`、`vcs: {"type":"git"}`；`terminal_width: 120` |
| 3 | 3.087 | `fetchAvailableModels` 返回 | `""` | `idle` | 0 B | `model.effort: "high"`，`context_window_size: 1048576`，`remaining_percentage: 100` |
| 4 | 3.364 | 首次 `doRefreshQuota` 返回 | `""` | `idle` | 0 B | 新增 `quota`（4 个桶：`3p-5h`、`3p-weekly`、`gemini-5h`、`gemini-weekly`） |
| 5 | 4.126 | `loadCodeAssist` / AICode 路由完成 | `""` | `idle` | 0 B | `quota.*.reset_in_seconds` 随时间减 1 |
| 6 | 4.385 | 登录后实验开关刷新，`SetCycleMode called: auto` | `""` | `idle` | 0 B | 新增 `cycle_mode: "auto"` |
| 7 | 4.687 | `loadCodeAssist` 填入订阅档位 | `""` | `idle` | 0 B | 新增 `plan_tier: "<plan-tier>"`（非空订阅档位显示名，已脱敏） |
| 8–10 | 4.986–5.585 | 二次配额/设置刷新收尾 | `""` | `idle` | 0 B | 仅 `quota` 的 `reset_time`/`reset_in_seconds` 微调；5.585–15.078 s 之间无调用 |
| 11–12 | 15.112–15.413 | 键入 `/` 打开斜杠菜单 | `""` | `idle` | 0 B | 仅 `reset_in_seconds` 递减 |
| 13–14 | 20.178–20.468 | `/model` 回车打开模型选择器 | `""` | `idle` | 0 B | 同上 |
| 15 | 24.279 | 按 `Esc` 关闭选择器（会话正在创建） | `""` | `idle` | 0 B | 同上 |
| 16 | 24.578 | `Created conversation 5a85300c-…`（+0.3 s 节流补跑） | UUID | `idle` | **275 B** | `session_id` 和 `conversation_id` 变为 UUID，`transcript_path` 插入该 UUID 目录；状态栏在 25.126 s 出现在屏幕上 |
| 17 | 28.242 | 输入框键入首字符 | UUID | `idle` | 275 B | 仅 `reset_in_seconds` 递减 |

### 2. 提交 prompt 与子代理生命周期（`run1` 第 18–45 次，`run2` 第 1–20 次）

| 阶段 | 代表调用 | 关键字段实测值 |
| --- | --- | --- |
| 提交后开始流式 | `run1` 第 19 次 (30.582 s) | `agent_state: "working"`，`current_usage: null`，`exceeds_200k_tokens: null` |
| 主代理第一步生成结束 | `run1` 第 26 次 (53.673 s) | `total_input_tokens: 20890`，`total_output_tokens: 767`，`used_percentage: 1.9922256469726562`，`remaining_percentage: 98.00777435302734`，`current_usage: {"input_tokens": 14812, "output_tokens": 767, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}`，`exceeds_200k_tokens: false` |
| 子代理运行中 | `run1` 第 29 次 (55.362 s) | `subagents: [{"name": "research", "role": "Say PONG only and call no tools.", "model": "Gemini 3.5 Flash Lite", "status": "running"}]` |
| 子代理完成 | `run1` 第 37 次 (61.245 s) | `subagents[0].status` 变为 `"completed"`，其余三项不变；`cost` 仍不存在 |
| 续接同一会话启动 | `run2` 第 1 → 2 → 6 次 | 第 1 次（0.403 s）`session_id` 仍是 `""`（输出 0 B）；第 2 次（0.703 s）恢复 UUID 并带出 `conversation_title: "Invoke Subagent For Research Task"`（输出 268 B）；第 6 次（4.394 s）带出 `subagents: [{"name": "research", "role": "ProbeRoleLabel", "model": "Gemini 3.5 Flash Lite", "status": "completed"}]` |
| 对话自然结束回到空闲 | `run2` 第 18–20 次 (67.3–67.9 s) | 第 18 次更新 `total_input_tokens: 22800`、`total_output_tokens: 1419`、`used_percentage: 2.17437744140625`、`current_usage: {"input_tokens": 16151, "output_tokens": 44, ...}`；第 19 次 `agent_state: "idle"`；第 20 次 `quota.gemini-5h.remaining_fraction` 从 `0.943195` 更新为 `0.94253737`；`cost` 仍不存在，`vcs` 仍为 `{"type": "git"}` |

## 脱敏后的代表性 payload 原文

### 启动第 1 次（`run1` `inv-0001.stdin.json`）

```json
{"cwd":"D:\\Documents\\Code\\Web\\Exp\\agyline","session_id":"","conversation_id":"","transcript_path":"C:\\Users\\<user>\\.gemini\\antigravity\\brain\\.system_generated\\logs\\transcript.jsonl","model":{"id":"Gemini 4 Carbon (High)","display_name":"Gemini 4 Carbon (High)"},"workspace":{"current_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline","project_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline"},"version":"1.5.9","context_window":{"total_input_tokens":0,"total_output_tokens":0,"context_window_size":0,"used_percentage":0,"remaining_percentage":0,"current_usage":null},"exceeds_200k_tokens":null,"product":"antigravity","agent_state":"authenticating","sandbox":{"enabled":false},"terminal_width":80}
```

### 启动稳定态（第 10 次，`run1` `inv-0010.stdin.json`）

```json
{"cwd":"D:\\Documents\\Code\\Web\\Exp\\agyline","session_id":"","conversation_id":"","transcript_path":"C:\\Users\\<user>\\.gemini\\antigravity\\brain\\.system_generated\\logs\\transcript.jsonl","model":{"id":"Gemini 4 Carbon (High)","display_name":"Gemini 4 Carbon (High)","effort":"high"},"workspace":{"current_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline","project_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline"},"version":"1.5.9","context_window":{"total_input_tokens":0,"total_output_tokens":0,"context_window_size":1048576,"used_percentage":0,"remaining_percentage":100,"current_usage":null},"exceeds_200k_tokens":null,"product":"antigravity","quota":{"3p-5h":{"remaining_fraction":1,"reset_time":"2026-11-14T14:30:05Z","reset_in_seconds":18000},"3p-weekly":{"remaining_fraction":0.49016213,"reset_time":"2026-11-20T04:08:18Z","reset_in_seconds":499093},"gemini-5h":{"remaining_fraction":0.96379286,"reset_time":"2026-11-14T13:51:38Z","reset_in_seconds":15693},"gemini-weekly":{"remaining_fraction":0.8028002,"reset_time":"2026-11-20T22:51:38Z","reset_in_seconds":566493}},"agent_state":"idle","vcs":{"type":"git"},"cycle_mode":"auto","sandbox":{"enabled":false},"plan_tier":"<plan-tier>","email":"<email>","terminal_width":120}
```

### 子代理完成时（`run1` `inv-0037.stdin.json`）

```json
{"cwd":"D:\\Documents\\Code\\Web\\Exp\\agyline","session_id":"5a85300c-f69f-412d-b57c-40b3ae243133","conversation_id":"5a85300c-f69f-412d-b57c-40b3ae243133","transcript_path":"C:\\Users\\<user>\\.gemini\\antigravity\\brain\\5a85300c-f69f-412d-b57c-40b3ae243133\\.system_generated\\logs\\transcript.jsonl","model":{"id":"Gemini 4 Carbon (High)","display_name":"Gemini 4 Carbon (High)","effort":"high"},"workspace":{"current_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline","project_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline"},"version":"1.5.9","context_window":{"total_input_tokens":20890,"total_output_tokens":767,"context_window_size":1048576,"used_percentage":1.9922256469726562,"remaining_percentage":98.00777435302734,"current_usage":{"input_tokens":14812,"output_tokens":767,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"exceeds_200k_tokens":false,"product":"antigravity","quota":{"3p-5h":{"remaining_fraction":1,"reset_time":"2026-11-14T14:30:05Z","reset_in_seconds":17944},"3p-weekly":{"remaining_fraction":0.49016213,"reset_time":"2026-11-20T04:08:18Z","reset_in_seconds":499037},"gemini-5h":{"remaining_fraction":0.96379286,"reset_time":"2026-11-14T13:51:38Z","reset_in_seconds":15637},"gemini-weekly":{"remaining_fraction":0.8028002,"reset_time":"2026-11-20T22:51:38Z","reset_in_seconds":566437}},"agent_state":"working","vcs":{"type":"git"},"cycle_mode":"auto","sandbox":{"enabled":false},"subagents":[{"name":"research","role":"Say PONG only and call no tools.","model":"Gemini 3.5 Flash Lite","status":"completed"}],"plan_tier":"<plan-tier>","email":"<email>","terminal_width":120}
```

### 续接会话且一轮自然结束时（`run2` `inv-0020.stdin.json`）

```json
{"cwd":"D:\\Documents\\Code\\Web\\Exp\\agyline","session_id":"5a85300c-f69f-412d-b57c-40b3ae243133","conversation_id":"5a85300c-f69f-412d-b57c-40b3ae243133","conversation_title":"Invoke Subagent For Research Task","transcript_path":"C:\\Users\\<user>\\.gemini\\antigravity\\brain\\5a85300c-f69f-412d-b57c-40b3ae243133\\.system_generated\\logs\\transcript.jsonl","model":{"id":"Gemini 4 Carbon (High)","display_name":"Gemini 4 Carbon (High)","effort":"high"},"workspace":{"current_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline","project_dir":"D:\\Documents\\Code\\Web\\Exp\\agyline"},"version":"1.5.9","context_window":{"total_input_tokens":22800,"total_output_tokens":1419,"context_window_size":1048576,"used_percentage":2.17437744140625,"remaining_percentage":97.82562255859375,"current_usage":{"input_tokens":16151,"output_tokens":44,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"exceeds_200k_tokens":false,"product":"antigravity","quota":{"3p-5h":{"remaining_fraction":1,"reset_time":"2026-11-14T14:39:07Z","reset_in_seconds":18000},"3p-weekly":{"remaining_fraction":0.49016213,"reset_time":"2026-11-20T04:08:18Z","reset_in_seconds":498551},"gemini-5h":{"remaining_fraction":0.94253737,"reset_time":"2026-11-14T13:51:38Z","reset_in_seconds":15151},"gemini-weekly":{"remaining_fraction":0.8024871,"reset_time":"2026-11-20T22:51:38Z","reset_in_seconds":565951}},"agent_state":"idle","vcs":{"type":"git"},"cycle_mode":"auto","sandbox":{"enabled":false},"subagents":[{"name":"research","role":"ProbeRoleLabel","model":"Gemini 3.5 Flash Lite","status":"completed"}],"plan_tier":"<plan-tier>","email":"<email>","terminal_width":120}
```

## 对后续决策的影响

- **[让状态栏启动即显示，选哪种修法](https://github.com/bahayonghang/agyline/issues/10)** 与 **[两份参考脚本不一致处以谁为准](https://github.com/bahayonghang/agyline/issues/7)**：真实登录会话的根因与隔离环境完全一致；即使用 `--conversation` 续接会话，第 1 次调用（0.403 s）的 `session_id` 仍然是 `""`，到第 2 次（0.703 s）才填入 UUID。
- **[参考脚本没读的 payload 字段，哪些做成 item](https://github.com/bahayonghang/agyline/issues/16)**：
  - `subagents[].role` 在当次会话里装的是完整 `Prompt` 原文（可能很长、含换行），恢复会话时才是简短 `Role`；若做成 item 或拼进子代理段，不能假定它是短标签。
  - `transcript_path` 在 Windows 1.5.9 CLI 下指向不存在的 `.gemini\antigravity\brain\...`（实际在 `.gemini\antigravity-cli\brain\...`），不能直接按原路径打开文件。
  - `quota` 同时包含 `gemini-*` 和 `3p-*` 桶；参考脚本对所有桶无差别取最小 `remaining_fraction`，于是在使用 Gemini 模型（剩余 96% / 80%）时状态栏显示的是 `3p-weekly` 的 `49% quota`。
  - Google 订阅账号会话里 `cost` 全程不出现。
- **[参考脚本里 1.5.9 上走不到的读法，agyline 留不留](https://github.com/bahayonghang/agyline/issues/17)**：真实脏 git 仓库里 `vcs` 实测只有 `{"type":"git"}`。
