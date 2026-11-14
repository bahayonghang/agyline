# agy 1.5.9 状态栏 payload 有哪些字段

对应 issue：[bahayonghang/agyline#4](https://github.com/bahayonghang/agyline/issues/4)。调查日期 2026-11-14。这份笔记只做静态分析：读 `agy.exe` 的类型元数据和机器码、内嵌 changelog、官方文档。没有启动 agy 会话，真实 payload 的抓取由另一项调查负责，凡是需要它确认的地方都在“没能确定的”一节列出。

## 结论

- payload 是 `types.StatusLineData` 经 `encoding/json` 的 v1 语义序列化出来的一个 JSON 对象。它和 12 个嵌套结构体一共有 **65 个 JSON 路径**（顶层 28 个，嵌套 37 个）。65 个全部从二进制的类型元数据读出，字段归属没有靠猜。
- 这 13 个结构体都没有 `MarshalJSON`。所有 tag 只用到 `omitempty`，没有一个用 `omitzero`。
- `session_id` 和 `conversation_id` 的 tag 是 `json:"session_id"` 和 `json:"conversation_id"`，**不能省略**，两者永远是同一个值（`Conversation.ID`）。官方文档说第一个会话开始前这个值是空串，所以脚本要处理 `""`，不用处理缺失。
- 参考脚本读的字段里，`execution_mode` 和 `thinkingLevel` 在 1.5.9 的 payload 里不存在，`cost` 不再可能是数字。对应的现有字段是 `cycle_mode`、`model.effort`、`cost.total_usd`。
- `vcs.branch`、`vcs.dirty`、`vcs.client`、`sandbox.allow_network` 在结构体里有定义，但 1.5.9 这个构建里没有任何代码给它们赋值，所以永远不会出现在 JSON 里。
- 官方文档 <https://antigravity.google/docs/cli/statusline/> 有一张字段表，和二进制对得上。二进制比文档多出 `battle`、`subagents[].model`，以及上面那四个从不赋值的字段。

## 对象

| 项 | 值 |
| --- | --- |
| 文件 | `%LOCALAPPDATA%\agy\bin\agy.exe` |
| 版本 | `agy.exe --version` 输出 `1.5.9` |
| 大小 | 231726744 字节 |
| SHA256 | `8803C289CAC04D6E53F2211592D9981C2D5760BC0D9B4A7F222EF6ECC7ABB38A`（没有官方哈希可对照） |
| 格式 | PE32+，windows/amd64，镜像基址 `0x140000000` |
| 工具链 | `go version` 读出 `go1.28-20261102-RC00 cl/1012564305 +298f53e483 X:boringcrypto,simd,nogolistexportnewformat` |
| 类型所在包 | `google3/third_party/jetski/cli/types/types` |

全程只读 `agy.exe`，只运行过 `--version` 和 `--help`。没有改动 `~/.gemini/` 和 `ref/`。

## 证据标记

- 【元】读自二进制的类型元数据（`abi.Type` 和结构体字段表）：字段名、Go 类型、偏移、struct tag 和所属结构体都是直接读出来的。复现命令见“复现”一节，完整输出在 [statusline-types.txt](statusline-types.txt)。
- 【汇】读自机器码：反汇编 `store.BuildStatusLineData` 等函数，对照类型元数据里的字段偏移读出“谁给这个字段赋值、什么条件下赋值”。这是读汇编得到的，不是源码，可信度低于【元】。括号里的地址是 1.5.9 的虚拟地址。
- 【文】读自官方文档或内嵌 changelog，带引文。
- 【推】推断，写明依据。
- 【标】只见到 tag 字符串、没有确认归属。**本文没有这一类的行**：一开始用 `rg -a` 搜到的 tag 字符串在二进制里是去重存放的（同名同 tag 的字段共用一条记录），后来全部用类型元数据确认了归属。

下面字段表里，“JSON 路径、Go 类型、可省略”三列全部是【元】，不再逐行标注。“何时有值”和“含义”两列逐行标注。

## payload 是怎么生成的

| 事实 | 依据 |
| --- | --- |
| `store.(*Manager).triggerStatusLineUpdate` 调用 `store.BuildStatusLineData(*store.Store)`，把返回的 `*types.StatusLineData` 交给 `store.(*StatusLineRunner).Run` | 【汇】`0x14382bc3a`、`0x14382bd4c` |
| `store.(*Manager).triggerTitleUpdate` 走的是同一个 `BuildStatusLineData` 和同一种 runner，所以 `title` 脚本收到的 JSON 结构相同 | 【汇】`0x143847507`、`0x143847520`；全二进制对 `BuildStatusLineData` 只有这两处调用（`find_xrefs.py 0x143829980`）；【文】设置文档把 `statusLine` 和 `title` 写在一起：“It receives the session state as JSON on `stdin`.” |
| `(*StatusLineRunner).run` 用 `encoding/json/v2.Marshal(data, opt)` 序列化，`opt` 是一个 `*jsonopts.Struct`，`Flags.Presence` 和 `Flags.Values` 都是 `0x5ffc0b80626` | 【汇】`0x14382cb31`；选项常量在 `0x14d862060`（`read_tables_1_5_9.py`） |
| 这个选项就是 `encoding/json` 的 v1 默认选项，也就是源码里写的是 `json.Marshal(data)`，被内联后表现为带 v1 选项调用 v2 | 【推】`Values` 非零且和 `Presence` 相等，21 个置位的分布是 5 个编码标志、4 个 v2 标志、12 个 v1 兼容标志，符合 `jsonopts.DefaultOptionsV1`；没有逐位对照这个开发版工具链的标志定义 |
| `types.StatusLineData` 和 `*types.StatusLineData` 的方法表里只有 `SetTelemetry`，没有 `MarshalJSON`。`SetTelemetry` 的函数体只有一条 `ret` | 【元】方法表；【汇】`0x143415f00` |
| 内嵌字段 `statusLineDataInternal` 是大小为 0、没有字段的未导出结构体，不产生任何 JSON 键 | 【元】 |
| `Manager.Debug` 为真时，同一份 JSON 会以 `Debug StatusLineData: %s` 写进日志 | 【汇】`0x14382bc47` 到 `0x14382bd33`；`Manager` 的 `+0x420` 是 `Debug bool`【元】 |

由 v1 语义得到的规则，适用于整张表：

- `omitempty` 省略的是：空串、`0`、`false`、nil 指针、空 map、空 slice。所以 `task_count` 为 0、`tool_confirmation_pending` 为 false 时，键不存在。
- 没有 `omitempty` 的指针字段为 nil 时输出 `null`。`model`、`exceeds_200k_tokens`、`context_window.current_usage` 会出现 `null`。
- 键的顺序是结构体字段顺序，也就是下面表格的行序。`quota` 的键按字典序排列。
- 字符串里的 `<`、`>`、`&` 会被写成 `\u003c` 这样的转义。标准 JSON 解析器能读回来。

以上四条是【推】：依据是上面的 v1 选项和 `encoding/json` 的公开行为，需要真实 payload 确认。

## 字段表

### 顶层：`types.StatusLineData`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `cwd` | `string` | 否 | 总是有。【汇】`0x143829b8d` | `Store.SessionConfig.CurrentWorkspaceDir`【汇】。【文】“Directory the CLI was started in.” |
| `session_id` | `string` | 否 | 总是有，可能是 `""`。【汇】`0x143829bdb` | 和 `conversation_id` 取自同一个 `Conversation.ID`【汇】。【文】“Same value as `conversation_id`, kept for compatibility.” |
| `conversation_id` | `string` | 否 | 总是有，可能是 `""`。【汇】`0x143829bbb` | `Conversation.ID`【汇】。【文】“ID of the active conversation (empty before the first one starts).” |
| `conversation_title` | `string` | `omitempty` | `Conversation.Title` 非空时。【汇】`0x143829c47` | 当前会话标题。【文】changelog 1.1.27：“Added a `conversation_title` field to the JSON payload passed to custom status line and window title scripts” |
| `transcript_path` | `string` | `omitempty` | `local.GeminiDir()` 没报错时。【汇】`0x143829a03` 到 `0x143829b13` | `<GeminiDir>/<ProductName，空则 antigravity>/brain/<conversation id>/.system_generated/logs/transcript.jsonl`，六段拼接【汇】。【文】“Path where the conversation transcript is written (the file may not exist yet).” |
| `model` | `*types.StatusLineModelInfo` | 否，nil 时是 `null` | `ModelConfig.SelectedModelID` 非空，或者设置里的 `model` 非空；两者都没有时是 `null`。【汇】`0x143829cff`、`0x143829d72` | 见下表。【文】“`null` until a model is selected.” |
| `workspace` | `*types.StatusLineWorkspace` | 否 | 总是分配。【汇】`0x143829e12` | 见下表 |
| `version` | `string` | 否 | 总是有。【汇】`0x143829bfb` | `VersionInfo.Version`【汇】。【文】“CLI version string.” |
| `context_window` | `*types.StatusLineContext` | 否 | 总是分配。【汇】`0x143829f36` | 见下表 |
| `exceeds_200k_tokens` | `*bool` | 否，nil 时是 `null` | 最近一次生成的元数据是 `ChatModel` 且带 `ChatStartMetadata.ContextWindowMetadata` 时有值，否则是 `null`。【汇】`0x143829f72` 到 `0x14382a032` | `EstimatedTokensUsed > 200000`【汇】`0x143829ffd`。【文】“`null` before the first model response” |
| `agent` | `*types.StatusLineAgent` | `omitempty` | `Conversation.AgentName` 非空时。【汇】`0x14382a4dc` | 【文】“Contains the `name` of the active agent (omitted for the default agent).” |
| `product` | `string` | `omitempty` | `VersionInfo.ProductName` 非空时。【汇】`0x143829cb6` | 【文】“Application name (`antigravity`).” |
| `quota` | `map[string]types.StatusLineQuotaBucket` | `omitempty` | `AccountState.QuotaSummary` 非 nil 且至少有一个桶时。【汇】`0x14382a84d` | 键是 `backend.QuotaBucket.BucketID`【汇】`0x14382abd6`。桶见下表 |
| `agent_state` | `string` | `omitempty` | 实际上总是有：代码对每个 `UIPhase` 都赋了非空值。【汇】`0x14382a3f3` 到 `0x14382a4d5` | 七个取值，见“枚举取值” |
| `vcs` | `*types.StatusLineVCS` | `omitempty` | 工作目录或它的某个上级目录里有 `.jj`、`.git` 或 `.hg` 时。【汇】`0x14382a3c0`、`store.detectVCSOnDisk` | 见下表。【文】“omitted outside a repository” |
| `cycle_mode` | `string` | `omitempty` | `SessionConfig.CycleMode` 非空时，默认模式下为空。【汇】`0x14382a6f2` | 【文】“Mode selected with Shift + Tab: `accept-edits`, `auto`, or `plan` (omitted in the default mode).” |
| `sandbox` | `*types.StatusLineSandbox` | `omitempty` | 设置对象存在时总是分配，也就是正常运行时总是有。【汇】`0x14382a323` | 见下表 |
| `subagents` | `[]types.StatusLineSubagent` | `omitempty` | `Conversation.SubagentStates` 里至少有一个 `Workflow` 为空的条目时。【汇】`0x14382a629`、`0x14382acd6` | 见下表。`Workflow` 非空的条目被跳过 |
| `artifact_count` | `int` | `omitempty` | 大于 0 时 | `len(TurnState.ArtifactSnapshotURIs)`【汇】`0x14382a6cf`。【文】“Number of artifacts produced in the conversation (omitted when 0).” |
| `plan_tier` | `string` | `omitempty` | `Auth.Status.PlanTier` 非空时。【汇】`0x14382a5dc` | 【文】“Subscription tier of the authenticated user (optional).” |
| `email` | `string` | `omitempty` | `Auth.Status.Email` 非空时。【汇】`0x14382a58d` | 【文】“Email address of the signed-in account.” 这是个人信息，状态栏程序不应把它写进日志或缓存 |
| `pending_input_count` | `int` | `omitempty` | 大于 0 时 | `len(Conversation.PendingInputs)`【汇】`0x14382a6dd`。【文】“Number of queued user messages (omitted when 0).” |
| `tool_confirmation_pending` | `bool` | `omitempty` | 为 true 时 | `PromptState.PendingToolConfirmation != nil`【汇】`0x14382a724`。【文】“True when a tool confirmation dialog is showing (omitted otherwise).” |
| `task_count` | `int` | `omitempty` | 大于 0 时 | `DerivedState.BackgroundTasksCount`【汇】`0x14382a734`。【文】changelog 1.3.1：“the status line's task count ... timers and recurring jobs the agent schedules, and other tools running in the background, now appear there too” |
| `terminal_width` | `int` | `omitempty` | 大于 0 时 | `TerminalInfo.Width`【汇】`0x14382a87f`。【文】“Live width of the interactive terminal.” |
| `battle` | `*types.StatusLineBattle` | `omitempty` | `TurnState.BattleSession` 非 nil 时。【汇】`0x14382a742` | 见下表。官方文档没有这个字段 |
| `vim` | `*types.StatusLineVim` | `omitempty` | 设置里 `editorMode` 等于 `vim` 时。【汇】`0x14382a88d` 到 `0x14382a8b4` | 见下表。【文】“Present only when Vim editor mode is enabled.” |
| `cost` | `*types.StatusLineCost` | `omitempty` | `Conversation.CostSummary.Total` 非 nil 时。【汇】`0x14382a21b` 到 `0x14382a243` | 见下表。【文】“omitted until a cost is reported” |

### `model`：`types.StatusLineModelInfo`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `model.id` | `string` | 否 | `model` 非 null 时总是有 | `ModelConfig.SelectedModelID`；没有它时是设置里的 `model`。【汇】`0x143829d37`、`0x143829daf` |
| `model.display_name` | `string` | 否 | 同上 | 在 `ModelConfig.Models` 里找 `Label` 等于 `SelectedModelID` 的模型，取它的 `Label`；找不到就取 `id`。两条路径的结果都和 `id` 相同。【汇】`0x14382b000` 到 `0x14382b14c`。【文】官方示例里 `id` 和 `display_name` 都是 `"Gemini 4 Argon (Medium)"` |
| `model.effort` | `string` | `omitempty` | 所选模型所属的基础模型有不止一个 effort 变体时。【汇】`store.ModelEffortDisplay`，`0x14382b160` | effort 后缀。【文】“`effort` (`low` through `max`, only for models with several effort levels)” |

### `workspace`：`types.StatusLineWorkspace`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `workspace.current_dir` | `string` | 否 | 总是有 | `CurrentWorkspaceDir`，和 `cwd` 同源。【汇】`0x143829e30` |
| `workspace.project_dir` | `string` | 否 | 总是有 | 同一个 `CurrentWorkspaceDir`。1.5.9 里它和 `current_dir` 总是相等。【汇】`0x143829e56` |

### `context_window`：`types.StatusLineContext`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `context_window.total_input_tokens` | `int` | 否 | 总是有，可为 0 | 有 `ContextWindowMetadata` 时是 `EstimatedTokensUsed`；没有时是最后一个带 `ModelUsage` 的步骤的 `InputTokens`。它不是累加值。【汇】`0x143829f1d`、`0x143829fe6` |
| `context_window.total_output_tokens` | `int` | 否 | 总是有，可为 0 | 所有步骤的 `Metadata.ModelUsage.OutputTokens` 之和。【汇】`0x143829f15` |
| `context_window.context_window_size` | `int` | 否 | 总是有，未知时是 0 | 在 `ModelConfig.Models` 里按 `Label` 找到当前模型，取 `MaxTokens`（大于 0 才用）。【汇】`0x14382af00` 到 `0x14382afee`。【文】“`context_window_size` (`0` if unknown)” |
| `context_window.used_percentage` | `*float64` | 否 | 总是非 nil（两个百分比一起分配），窗口大小为 0 时是 0 | `used / context_window_size * 100`。`used` 在有 `ContextWindowMetadata` 时是 `EstimatedTokensUsed`，否则是 `total_input_tokens + total_output_tokens`。代码里没有取整和截断。【汇】`0x143829f79`、`0x14382a04e`、`0x14382a184` 到 `0x14382a1b0` |
| `context_window.remaining_percentage` | `*float64` | 否 | 同上 | `100 - used_percentage`。【汇】`0x14382a1ac` |
| `context_window.current_usage` | `*types.StatusLineCurrentUsage` | 否，nil 时是 `null` | 最近一次生成的 `ChatModelMetadata.Usage` 非 nil 时。【汇】`0x14382a051` | 【文】“`current_usage` (`null` before the first model response)” |
| `context_window.current_usage.input_tokens` | `int` | 否 | `current_usage` 非 null 时 | `ModelUsageStats.InputTokens`。【汇】`0x14382a063` |
| `context_window.current_usage.output_tokens` | `int` | 否 | 同上 | `ModelUsageStats.OutputTokens`。【汇】`0x14382a06c` |
| `context_window.current_usage.cache_creation_input_tokens` | `int` | 否 | 同上 | 1.5.9 里没有代码给它赋值，总是 0（`ModelUsageStats.CacheWriteTokens` 没有被读）。【汇】`0x14382a083` 到 `0x14382a0aa` 只写了另外三个字段 |
| `context_window.current_usage.cache_read_input_tokens` | `int` | 否 | 同上 | `ModelUsageStats.CacheReadTokens`。【汇】`0x14382a075` |

### `agent`：`types.StatusLineAgent`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `agent.name` | `string` | 否 | `agent` 存在时 | `Conversation.AgentName`。【汇】`0x14382a4fb` |

### `quota.<bucket>`：`types.StatusLineQuotaBucket`

`<bucket>` 是服务端返回的 `BucketID`。官方示例里的键是 `gemini-weekly`【文】。

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `quota.<bucket>.remaining_fraction` | `*float32` | `omitempty` | `QuotaBucket.HasRemainingFraction` 为真时。【汇】`0x14382aa73` | 剩余比例。nil 才省略，所以 `0` 会照常输出 |
| `quota.<bucket>.remaining_amount` | `*int64` | `omitempty` | `HasRemainingFraction` 为假时。和 `remaining_fraction` 二选一。【汇】`0x14382aaa2` | 剩余数量 |
| `quota.<bucket>.reset_time` | `*time.Time` | `omitempty` | `QuotaBucket.ResetTime` 不是零值时。【汇】`0x14382aac1` | 重置时刻，`time.Time` 的 JSON 形式是 RFC 3339 字符串。【文】示例 `"2026-07-06T07:50:32Z"` |
| `quota.<bucket>.reset_in_seconds` | `*int64` | `omitempty` | 和 `reset_time` 同时出现。【汇】`0x14382ab45` 到 `0x14382abce` | `time.Until(ResetTime)` 的秒数，截断取整；已经过了重置时刻时是 `0` |
| `quota.<bucket>.disabled` | `bool` | `omitempty` | 为 true 时 | `QuotaBucket.Disabled`。【汇】`0x14382aa64` |

### `vcs`：`types.StatusLineVCS`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `vcs.type` | `string` | 否 | `vcs` 存在时 | `jj`、`git`、`hg` 之一。从工作目录逐级向上，每一级按 `.jj`、`.git`、`.hg` 的顺序 `os.Stat`。【汇】`store.detectVCSOnDisk`，`0x14382b200` |
| `vcs.branch` | `string` | `omitempty` | **1.5.9 里从不赋值** | 结构体里有，`detectVCSOnDisk` 的三个分配点都只写 `Type`。【汇】`0x14382b35c`、`0x14382b386`、`0x14382b3b0` |
| `vcs.client` | `string` | `omitempty` | **1.5.9 里从不赋值** | 同上 |
| `vcs.dirty` | `bool` | `omitempty` | **1.5.9 里从不赋值** | 同上 |

`Store.vcsInfo` 只由 `(*Manager).doRefreshVCS` 写入，它调用的函数变量（`0x14d7dc720`）静态初始化为 `store.init.func10`，后者只调用 `detectVCSOnDisk`。全二进制对这个变量只有一处读、没有写，对 `types.StatusLineVCS` 的分配也只有 `detectVCSOnDisk` 里那三处（`find_xrefs.py 0x14d7dc720 0x14b25bde0`）。【推】字段留在结构体里，是给会替换这个函数变量的其他构建用的。

### `sandbox`：`types.StatusLineSandbox`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `sandbox.enabled` | `bool` | 否 | `sandbox` 存在时总是有 | 命令行覆盖值 `Overrides.EnableTerminalSandbox`（有的话）否则设置里的 `enableTerminalSandbox`；为真时再调一个“沙箱是否可用”的函数变量，结果才是最终值。【汇】`0x14382a32f` 到 `0x14382a37e`。【文】changelog 1.2.1：“Fixed the status line reporting the terminal sandbox as disabled when the session was launched with the `--sandbox` command-line flag.” |
| `sandbox.allow_network` | `bool` | `omitempty` | **1.5.9 里从不赋值** | `types.StatusLineSandbox` 全二进制只在 `BuildStatusLineData` 分配一次，只写了 `Enabled`。【汇】`0x14382a368` 到 `0x14382a37e`，`find_xrefs.py 0x14b008640` |

### `subagents[]`：`types.StatusLineSubagent`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `subagents[].name` | `string` | 否 | 每个条目都有 | `SubagentState.Name`。【汇】`0x14382ad83` |
| `subagents[].role` | `string` | `omitempty` | 非空时 | `SubagentState.Prompt` 非空就取它，否则取 `SubagentState.Role`。【汇】`0x14382ad4a` 到 `0x14382ad61`。取 `Prompt` 这一点和字段名不相称，需要真实 payload 确认 |
| `subagents[].model` | `string` | `omitempty` | 非空时 | `SubagentState.Model`。【汇】`0x14382adab`。【文】changelog 1.5.3：“a `model` field on each `subagents` entry in the status line JSON” |
| `subagents[].status` | `string` | 否 | 每个条目都有 | `SubagentState.Status` 等于 `running` 时是 `running`，其他情况一律是 `completed`。【汇】`0x14382ace2` 到 `0x14382ad43`。【文】“`status` (`running` or `completed`)” |

### `battle`：`types.StatusLineBattle`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `battle.status` | `string` | 否 | `battle` 存在时 | 由 `BattleSession.Status` 映射：0 是 `initializing`，1 是 `working`，3 是 `merging`，其余是 `idle`。【汇】`0x14382a752` 到 `0x14382a79d` |
| `battle.focused_arm` | `int` | 否 | 同上 | `BattleSession.FocusedArm`。【汇】`0x14382a801` |

### `vim`：`types.StatusLineVim`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `vim.mode` | `string` | 否 | `vim` 存在时 | `ViewState.VimMode`，为空时是 `NORMAL`。【汇】`0x14382a8ba` 到 `0x14382a915` |

### `cost`：`types.StatusLineCost`

| JSON 路径 | Go 类型 | 可省略 | 何时有值 | 含义 |
| --- | --- | --- | --- | --- |
| `cost.total_usd` | `float64` | 否 | `cost` 存在时，可为 0 | `CostSummary.Total.CumulativeTotal.EstimatedCostUsd`，由 `float32` 转成 `float64`，所以小数位会带转换误差。【汇】`0x14382a249` 到 `0x14382a2bd`。【文】changelog 1.1.21：“exposing the unrounded estimated cost of the current session” |
| `cost.subagent_usd` | `float64` | 否 | 同上 | `CostSummary.Total.Subagent.EstimatedCostUsd`。【汇】`0x14382a25d` 到 `0x14382a2cb`。【文】“`total_usd` (including subagents), `subagent_usd`” |
| `cost.estimated` | `bool` | 否 | 同上 | `CostSummary.Total.CumulativeTotal.Pricing.IsEstimated`。【汇】`0x14382a278` 到 `0x14382a2d5` |

## 枚举取值

| 字段 | 取值 | 依据 |
| --- | --- | --- |
| `agent_state` | `idle`、`working`、`tool_use`、`reviewing`、`authenticating`、`initializing`、`error` | 【汇】`BuildStatusLineData` 里按 `store.UIPhase` 的跳转表，下面是完整映射；【文】文档列的是同样七个 |
| `cycle_mode` | `accept-edits`、`auto`、`plan`；默认模式是空串，键被省略 | 【汇】`(*Store).UserVisibleModes` 用的切片常量是这三个值（`read_tables_1_5_9.py`）；【文】同上引文；`--help`：“--mode  Set the agent execution mode for this session (accept-edits, auto, plan)” |
| `cycle_mode`（另一个内部值） | `bestn` | 【汇】`types.AgentMode.Valid`（`0x143410be0`）接受空串、`auto`、`plan`、`accept-edits` 和五个字节的 `bestn`；内置状态栏把它显示成 `best-of-n`（`StatusLineModel.renderModeLabel`，`0x143afe071`）。它不在切换列表里，什么条件下会进入 `CycleMode` 没有确定 |
| `model.effort` | `low`、`medium`、`high`、`xhigh`、`max` | 【汇】`backend.IsValidEffort` 用的切片常量；`--help`：“--effort ... (low\|medium\|high\|xhigh\|max)” |
| `subagents[].status` | `running`、`completed` | 【汇】【文】见上表 |
| `vcs.type` | `jj`、`git`、`hg` | 【汇】【文】“Version control `type`: `git`, `jj`, or `hg`” |
| `vim.mode` | `NORMAL`、`INSERT`、`VISUAL`、`VISUAL LINE` | 【汇】`editing.EditArea.VimMode`（`0x1438ea8e0`）只返回这四个串；【文】“`mode` is `NORMAL`, `INSERT`, `VISUAL`, or `VISUAL LINE`” |
| `battle.status` | `initializing`、`working`、`merging`、`idle` | 【汇】见上表 |
| `quota` 的键 | 不能静态枚举 | 键来自服务端返回的配额摘要（`backend.mapQuotaSummary`）。【文】示例里有 `gemini-weekly` |

`store.UIPhase` 到 `agent_state` 的映射【汇】（`read_tables_1_5_9.py` 的输出）：

| `UIPhase` | `agent_state` |
| --- | --- |
| `AuthStart`、`AuthWaiting`、`AuthFailure` | `authenticating` |
| `SettingsError` | `error` |
| `Onboarding`、`WorkspaceTrust` | `initializing` |
| `Idle`、`BattleIdle` | `idle` |
| `PanelHelp`、`PanelSettings`、`PanelModelSelector`、`PanelPermissions`、`PanelSkills`、`PanelConversationPicker`、`PanelHooks`、`PanelMcp`、`PanelTasks`、`PanelContext`、`PanelUsage`、`PanelCredits` | `idle` |
| `AgentWorking`、`BattleWorking` | `working` |
| `ToolConfirmation` | `tool_use` |
| `ReviewAvailable`、`ArtifactReview`、`EditingFeedback` | `reviewing` |

## 清单一：参考脚本没有读的字段

“参考脚本”指 `ref/agy-patch/statusline.ps1` 和 `statusline.py`。两个文件读的字段集合完全相同（分别用 `rg` 列出 `$d.` 和 `.get("` 后比对）。

26 个路径：

- 顶层：`conversation_title`、`transcript_path`、`version`、`exceeds_200k_tokens`、`agent.name`、`product`、`artifact_count`、`plan_tier`、`email`、`terminal_width`、`battle.status`、`battle.focused_arm`、`vim.mode`
- `context_window`：`total_input_tokens`、`remaining_percentage`、`current_usage.cache_creation_input_tokens`
- `quota.<bucket>`：`remaining_amount`、`reset_time`、`reset_in_seconds`
- `vcs`：`type`、`client`
- `subagents[]`：`name`、`role`、`model`
- `cost`：`subagent_usd`、`estimated`

`statusline.sh`（macOS 原版）比这两个脚本多读 `vcs.client`、`context_window.total_input_tokens`、`exceeds_200k_tokens`、`cost.subagent_usd`、`artifact_count`。

## 清单二：参考脚本读了、但 1.5.9 里不存在或改了形态的

| 脚本读的 | 1.5.9 的情况 | 现在该读什么 | 依据 |
| --- | --- | --- | --- |
| `execution_mode` | 不存在。整个二进制里没有任何结构体字段带 `json:"execution_mode"` 这个 tag | `cycle_mode`（脚本已经把它当作回退） | 【元】`extract_statusline_types.py --owners execution_mode` |
| `thinkingLevel` | 不在 payload 里。带这个 tag 的字段属于 `genai.ThinkingConfig`、`types.CustomModelConfig`、`genai.ReinforcementTuningHyperParameters`、`genai.CreateTuningJobConfig`，是模型请求和自定义模型配置用的 | `model.effort`（脚本已经先读它） | 【元】`--owners thinkingLevel` |
| `cost` 为数字 | 不可能。`Cost` 的类型是 `*types.StatusLineCost`，JSON 里只会是对象或者缺失 | `cost.total_usd`（脚本的另一个分支） | 【元】 |

## 清单三：字段存在，但脚本期待的值在 1.5.9 里不会出现

这一类不是“改名”，放在一起是因为效果相同：脚本里对应的分支在 1.5.9 上走不到。

| 脚本读的 | 1.5.9 的情况 | 依据 |
| --- | --- | --- |
| `vcs.branch`、`vcs.dirty` | 字段有定义，从不赋值，键永远不出现。脚本靠自己跑 `git status` 得到分支和脏标记 | 【汇】见 `vcs` 表 |
| `sandbox.allow_network` | 字段有定义，从不赋值，键永远不出现。脚本的 `sandbox+net` 标签不会显示 | 【汇】见 `sandbox` 表 |
| `subagents[].status` 等于 `working` 或 `thinking` | 只会是 `running` 或 `completed` | 【汇】【文】 |
| 模式串里含 `yolo` | `cycle_mode` 没有这个值。payload 里也没有任何字段表示权限模式或 `--dangerously-skip-permissions` | 【汇】`AgentMode.Valid`；【元】字段表里没有这样的字段 |
| 模式串里含 `best-of-n` | 内部值是 `bestn`，`best-of-n` 只是内置状态栏的显示文字，脚本的子串匹配对不上 | 【汇】 |
| 模式等于 `default` 或 `request-review` | 默认模式是空串，键被省略。两个串都不是 `AgentMode` 的合法值；`request-review` 在内嵌 changelog 里是权限模式的取值（“`always-proceed`, `request-review`, `strict`”） | 【汇】`AgentMode.Valid`；【文】 |
| `agent_state` 缺失时当作 `idle` | 这个键实际上总是存在 | 【汇】 |

脚本读的其余字段在 1.5.9 里都在，路径没有变：`session_id`、`conversation_id`、`agent_state`、`cycle_mode`、`model.display_name`、`model.id`、`model.effort`、`cwd`、`workspace.current_dir`、`workspace.project_dir`、`sandbox.enabled`、`context_window.current_usage.input_tokens`、`.output_tokens`、`.cache_read_input_tokens`、`context_window.total_output_tokens`、`context_window.context_window_size`、`context_window.used_percentage`、`cost.total_usd`、`quota.<bucket>.remaining_fraction`、`quota.<bucket>.disabled`、`subagents`、`task_count`、`pending_input_count`、`tool_confirmation_pending`。

## 和官方文档的差异

官方文档页 <https://antigravity.google/docs/cli/statusline/> 的字段表列了 28 个顶层字段里的 27 个，取值说明和二进制一致。差异：

| 项 | 文档 | 二进制 1.5.9 |
| --- | --- | --- |
| `battle` | 没有 | 有，`status` 和 `focused_arm` |
| `subagents[]` 的字段 | “each with `name`, `role`, and `status`” | 还有 `model`（changelog 1.5.3） |
| `vcs` 的字段 | 只提 `type` | 结构体还有 `branch`、`client`、`dirty`，但从不赋值，所以文档的说法符合实际输出 |
| `sandbox` 的字段 | 只提 `enabled` | 结构体还有 `allow_network`，从不赋值 |
| 示例里的键顺序 | `transcript_path` 在 `conversation_title` 前面 | 结构体顺序是 `conversation_title` 在前 |
| 示例里的 `used_percentage` | `14.24`，等于 `(total_input_tokens + total_output_tokens) / context_window_size` | 1.5.9 的代码在有 `ContextWindowMetadata` 时用 `EstimatedTokensUsed` 做分子，不加输出 token，也不取整。文档示例的 `version` 是 `1.4.5` |
| 内嵌 changelog | | 1.5.9 的二进制里，changelog 的第一个版本标题是 `## 1.5.8`，没有 1.5.9 的条目 |

文档还给了运行方式的数字：“The CLI runs your script at most once every 300 ms and stops any run that takes longer than 5 seconds.”二进制里对应的常量是 `0x11e1a300` 纳秒（`(*StatusLineRunner).Run`，`0x14382c40d`）和 `0x12a05f200` 纳秒（`run`，`0x14382ca2d`），Windows 上的命令是 `cmd /c <command>`（`0x14382cbd3` 到 `0x14382cc20`）。运行方式的细节属于 [issue #2](https://github.com/bahayonghang/agyline/issues/2) 的范围，这里只记录读到的三处。

`/statusline help` 的文本只列子命令，没有描述 payload。

## 没能确定的

| 问题 | 为什么没定 | 怎样确定 |
| --- | --- | --- |
| `product` 的实际值，以及 `transcript_path` 的第一段目录名 | `VersionInfo` 是运行时通过后端接口取的（`(*Manager).initVersionInfo`，`0x14384bae6` 是接口调用），静态读不到值。文档说是 `antigravity`。本机符合 `brain/<id>/.system_generated/logs/transcript.jsonl` 这个布局的文件都在 `~/.gemini/antigravity-cli/` 下面，`~/.gemini/antigravity/brain` 下没有本机 CLI 会话的转录文件 | 看真实 payload 的 `product` 和 `transcript_path` |
| `model.id` 到底是标签还是模型 ID | 字段名叫 `SelectedModelID`，但代码拿它和 `AgentModel.Label` 比较，官方示例里也是带括号的显示名。静态分析只能说“`id` 和 `display_name` 相等” | 看真实 payload |
| `subagents[].role` 是否真的是 `Prompt` 优先 | 偏移读出来是 `SubagentState.Prompt`（`+0xd0`），语义上有疑问 | 起一个子代理后看 payload |
| `used_percentage` 输出几位小数 | 代码里没有取整，文档示例是两位小数 | 看真实 payload |
| JSON 的四条 v1 规则（省略、`null`、键序、HTML 转义） | 选项常量的判读是按位数分布推断的，没有对照这个开发版 Go 的标志定义 | 看真实 payload 里 `task_count` 为 0 时键是否缺失、`model` 是否出现过 `null` |
| `bestn` 模式什么时候出现 | 只确认它是合法的 `AgentMode`，没有追它的写入点 | 需要读 `SetCycleMode` 的调用方，或者进入 best-of-n 流程后看 payload |
| `Manager.Debug` 怎样打开 | 没有追它的写入点。`~/.gemini/antigravity-cli/log` 下 284 个日志文件里没有一行 `Debug StatusLineData` | 需要读 `Manager` 的构造代码 |
| `quota` 的键有哪些 | 来自服务端 | 看真实 payload |
| 其他平台和其他版本 | 只读了 windows/amd64 的 1.5.9。`vcs` 的探测函数是个可替换的函数变量，别的构建可能不同 | 对别的二进制重跑脚本 |
| 这个 `agy.exe` 是不是未经修改的官方文件 | 没有官方哈希可比。文件修改时间是 2026-11-14 07:04 UTC | 和官方发布包比对哈希 |

## 复现

脚本都在本目录，只读打开 `agy.exe`。前两个只用标准库，后两个各需要一个第三方包，装在用完即删的 venv 里。

| 脚本 | 做什么 | 依赖 |
| --- | --- | --- |
| [gobin.py](gobin.py) | PE 节表、`abi.Type` 和结构体字段表、pclntab 函数表的只读解析 | 无 |
| [extract_statusline_types.py](extract_statusline_types.py) | 从 `types.StatusLineData` 出发递归打印所有结构体的字段、类型、tag 和方法；`--owners <json key>` 列出哪些结构体有这个 tag | 无 |
| [read_tables_1_5_9.py](read_tables_1_5_9.py) | 读 `UIPhase` 到 `agent_state` 的映射、effort 列表、模式列表、JSON 选项常量。地址只对 1.5.9 有效 | 无 |
| [disasm_go_func.py](disasm_go_func.py) | 按函数名正则反汇编，给调用目标标函数名，给 RIP 相对操作数标 Go 类型或字符串候选 | `capstone` |
| [find_xrefs.py](find_xrefs.py) | 在 `.text` 里找指向某个地址的 rel32 引用 | `numpy` |

```powershell
$env:PYTHONDONTWRITEBYTECODE = '1'
python -B extract_statusline_types.py --owners execution_mode thinkingLevel   # 输出即 statusline-types.txt
python -B read_tables_1_5_9.py

python -m venv $env:TEMP\agy-venv
& $env:TEMP\agy-venv\Scripts\python.exe -m pip install capstone numpy
& $env:TEMP\agy-venv\Scripts\python.exe -B disasm_go_func.py 'store\.BuildStatusLineData$'
& $env:TEMP\agy-venv\Scripts\python.exe -B find_xrefs.py 0x143829980 0x14b008640 0x14d7dc720
Remove-Item -Recurse -Force $env:TEMP\agy-venv
```

类型元数据是怎么定位的：结构体字段表的每一项是（名字指针、类型指针、偏移）三个字。先在文件里找到 `ToolConfirmationPending` 加 `json:"tool_confirmation_pending,omitempty"` 这条名字记录，再找指向它的指针，就落在某个字段表里；向前走到表头，找到指向表头的结构体类型；用这个类型的名字偏移和 `*types.StatusLineData` 这条名字记录的地址相减，得到名字偏移的基址 `0x14a312d20`。之后所有类型名都由这个基址解出，解出来的类型名、字段名都是可读的 Go 标识符，这本身就是对基址的校验。

读 `BuildStatusLineData` 时，参数是 `*store.Store`，各子结构的偏移（例如 `+0x168` 是 `TurnState.conversation`，`+0x148` 是 `AccountState.ModelConfig`，`+0x298` 是 `DerivedState.UI.Phase`）也是从类型元数据读的，不是猜的。

## 来源

- `agy.exe` 1.5.9 本身：类型元数据、pclntab、机器码、只读数据区的字符串和内嵌的 `# Antigravity CLI Changelog`（文件偏移 `0x4b82d3e` 起）。
- 官方文档 [Status line customization](https://antigravity.google/docs/cli/statusline/)，2026-11-14 读取。引文都出自“Available JSON fields”表和它前面的两段。
- 官方文档 [Settings](https://antigravity.google/docs/settings/) 的 “`statusLine` and `title`” 一节，2026-11-14 读取。
- `agy.exe --help` 的输出。
- `ref/agy-patch/statusline.ps1`、`statusline.py`、`statusline.sh`：脚本读了哪些字段。
- `ref/agy-patch/references/context-window-patch.md`：它记录的 macOS 补丁点说原版用 `estimatedTokensUsed` 做百分比的分子，和这里读到的一致。这是二手材料，只作旁证。
- 搜索时还见到两篇第三方文章（<https://agy-powerline.com/docs/payload/> 和 DEV Community 上的一篇），它们报告在少数 payload 里见过 `vcs.branch`、`vcs.dirty`、`sandbox.allow_network`。没有采信：不是一手来源，也没有写明版本和平台。
