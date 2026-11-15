# `statusline.ps1` 与 `statusline.py` 输出字节分歧与对照样例集

对应 [statusline.ps1 与 statusline.py 在哪些输入上输出不同](https://github.com/bahayonghang/agyline/issues/6)。实测环境：Windows 11，`pwsh` 7.6.10（CLR 10.0.15），Python 3.14.x，Git for Windows，`ref/agy-patch/statusline.ps1`（690 行）与 `ref/agy-patch/statusline.py`（733 行）原样未改。

同目录产物：

- `run_diff_suite.py`：双进程隔离并发复现实验脚本（每次调用为 `ps1` 与 `py` 分配独立临时 `TEMP`/`TMP`/`USERPROFILE` 目录与 `probe_*` 专用会话 ID，并在退出时校验系统 `%TEMP%` 无残留）。
- `fixtures.json`：**65 组**机器可读对照样例（输入 payload、环境变量、前置状态文件/Git/SQLite 构造方式，以及两边完整退出码、字节长度、UTF-8 文本和十六进制字节流），其中 **31 组逐字节相等基线样例**、**34 组字节不同样例**（归并为 **21 条独立根因 `DIV-01`–`DIV-21`**）。

---

## 一、核心结论速览

1. **真实 `agy 1.5.9` 会话里高频触发的分歧有 4 类**：
   - **`DIV-04`（空串回退断路）**：启动期（及 `--conversation` 续接首帧）`session_id` 与 `conversation_id` 为 `""` 时，`ps1` 的 `??` 不把 `""` 当缺失，命中 `IsNullOrEmpty` 输出 **`0 B`**；`py` 的 `or` 回退到 `"default"` 正常输出完整两行（`B03`：`0 B` vs `210 B`）。同一机制还影响 `execution_mode: ""`、`model.effort: ""`、`cwd: ""` 对下游回退字段的遮蔽。
   - **`DIV-18`（会话库响应模型查找机制不同）**：`ps1` 从 `.db-wal` / `.db` 文件尾倒扫最多 **256 KB** 的原始字节并按厂商前缀过滤；`py` 通过 SQLite 查询 `SELECT substr(data, -600) FROM gen_metadata ORDER BY idx DESC LIMIT 5` 并用宽泛 ASCII 正则匹配。对本机 **49 个真实会话 `.db`**（其中 38 个含 `gen_metadata` 记录）只读实测：**13 个（占非空会话库的 34%）两边结果不同**——`steps` 表（工具返回、长输出）把最新 `gen_metadata` 页推到距文件尾 256 KB 之外（例如实测距 EOF 426,486 B），导致 `ps1` 返回 `""`、`py` 仍命中真实响应模型。此外在 WAL 检查点复用后残留尾部旧帧、跨表字节污染、尾部其他 field 19 字符串、`>600 B` 尾部填充、2 字符短模型名（如 `o3`）和两段非白名单模型名（如 `mistral-large`）6 种场景下两边也会分叉。
   - **`DIV-13`（非 UTF-8 控制台代码页下的 stdin 与 `git` stdout 解码）**：`ps1` 写 stdout 显式编码为 UTF-8，但读 stdin（`[Console]::In.ReadToEnd()`）与读 `git status`（`$gp.StandardOutput.ReadToEnd()`，未设 `$psi.StandardOutputEncoding`）依赖宿主控制台代码页。在简体中文 Windows 默认 **CP936 (GBK)** 下，含中文目录（如 `D:\中文目录\项目名`）的 UTF-8 JSON 会因末字节 `0x8D` 被当作 GBK 双字节前导吞掉闭合引号 `"`，导致 `ConvertFrom-Json` 失败退出输出 **`0 B`**（`D11`：`0 B` vs `145 B`）；在 **CP437** 下中文路径与中文 Git 分支名在 `ps1` 中变成乱码（`D12`：`feat/Σ╕¡µûçσêåµö»` vs `feat/中文分支`）。`py` 全程对原始字节按 UTF-8 解码，不受控制台代码页影响。
   - **`DIV-06`（`display_name` 中点 `·` 拆分失效）**：`ps1` 第 408 行正则写在单引号字符串 `'^(.*?)\s*`u{00b7}\s*(.+)$'` 内，`` `u{00b7} `` 不展开，导致：① 无 `model.effort` 时缺少灰色分隔符 SGR 序列（`C02`：`100 B` vs `114 B`），且 `medium` 等小写后缀不会被规整为 `Med`（`C04`）；② 同时带 `model.effort: "high"` 时可见文字重复渲染为 `Gemini 4 Carbon · High · High`（`C03`：`122 B` vs `114 B`）。
2. **工单列出的可疑项核查结论**：
   - **数字舍入（`.NET` 的 `F0`/`F1`/`F2`/`F3` 与 Python 格式化）——实测无分歧**：`statusline.ps1` 声明 `#Requires -Version 7.0`（运行于现代 .NET Core 3.0+ / .NET 10）。自 .NET Core 3.0 起 `Double.ToString("F...")` 已改为对精确 IEEE-754 binary64 值执行 **Round-Half-To-Even（四舍六入五成双）**，与 `[Math]::Round(double)`、Python `round()` 及 `f"{x:.0f/.1f/.2f/.3f}"` 数学等价。对 `10.5% -> 10%`、`11.5% -> 12%`、`0.25% -> 0.2%`、`1,250,000 -> 1.2M`、`$2.675 -> $2.67`、`$0.0625 -> $0.062`、`quota 0.445 -> 44%` 及 12,000 个稠密浮点样本实测**零分歧**（见 `E02`–`E04`）。*注：Rust 的 `format!("{:.0}", x)` 同样是 ties-to-even，但 `f64::round()` 是 ties-away-from-zero，后续实现需留意。* 数字相关的真正分歧在输入类型/边界保护（`DIV-14` `cost` 顶层数字串、`DIV-15` `quota > 1.0` 上限、`DIV-16` `"inf"` 异常崩溃）。
   - **GPT 名称与模型 slug 大小写处理——存在 3 处分歧**：`gpt-4o-32k` 等数字打头词两边一致（`GPT-4 O 32K`），但字母打头且词内含数字再接字母（如 `gpt-5-o4mini`）时，.NET `ToTitleCase` 输出 `GPT-5 O4mini`，Python `.title()` 输出 `GPT-5 O4Mini`（`DIV-07`）；连续连字符 `gpt-4--mini` 在 `ps1` 产生双空格 `GPT-4  Mini`（`DIV-08`）；大写 `MODELS/`、`-PREVIEW`、`-LATEST` 仅 `ps1` 不区分大小写剥离（`DIV-09`）。
   - **Temp 目录、Git 查找、状态文件读写——存在多处边界分歧**：Windows 上 `TEMP` 与 `TMP` 不同时 `ps1` 读 `TMP`、`py` 读 `TEMP`，macOS 上 `ps1` 读 `$TMPDIR`、`py` 硬编码 `/tmp`（`DIV-17`）；目录 Junction / 软链接下 `ps1` 纯词法上溯漏检 `.git`、`py` 先 `resolve()` 命中（`DIV-10`）；Linux/macOS 上 `ps1` 硬编码找 `git.exe` 必败，且 `git` 超时 1.5 s 时 `ps1` 因先同步 `ReadToEnd()` 会阻塞挂死；`agent_state: ""` 会令 `ps1` 误判热缓存并写坏状态文件首列（`DIV-05`）。

---

## 二、逐条分歧清单（`DIV-01` – `DIV-21`）

### 1. JSON 解析与字段回退链

| 编号 | 触发条件（对应样例） | `statusline.ps1` 输出与行为 | `statusline.py` 输出与行为 | 脚本根因（行号） |
| --- | --- | --- | --- | --- |
| **DIV-01** | 顶层合法 JSON 但非对象，如 `[1]`、`123`、`"abc"`、`true`（`A04`、`A05`；注：空数组 `[]` 在 PS 管道展开为 `$null`，两边均为 `0 B`，见 `A03`） | 正常渲染默认行（`82 B`：` Model \|  0% \|  cold`） | `0 B`（退出码 `0`） | `ps1:17-21` 只检查 `$null -eq $d`；`py:319` 检查 `if not isinstance(data, dict): return 0` |
| **DIV-02** | JSON 对象含仅大小写不同的重复键，如 `{"session_id":"s","x":1,"X":2}`（`A06`） | `0 B`（`ConvertFrom-Json` 抛异常退出） | 正常渲染（`82 B`） | `ps1:17` `ConvertFrom-Json` 构造 `PSCustomObject` 时属性名大小写不敏感冲突报错；`py:316` `json.loads` 大小写敏感 |
| **DIV-03** | JSON 字段名大小写变化，如 `{"THINKINGLEVEL":"high"}`（`A07`） | 识别并渲染 ` Model · High`（`104 B`） | 忽略，渲染 ` Model`（`82 B`） | PowerShell 对象属性访问 `$d.thinkingLevel` 大小写不敏感；Python `dict.get("thinkingLevel")` 大小写敏感 |
| **DIV-04** | **回退链上游字段为空字符串 `""`**：<br>① `session_id: ""`（无论 `conversation_id` 是否非空，或 `session_id` 缺失且 `conversation_id: ""`）（`B03`、`B04`）<br>② `model.effort: ""` + `thinkingLevel: "max"`（`C06`）<br>③ `cwd: ""` + `workspace.current_dir` 非空（`D02`）<br>④ `execution_mode: ""` + `cycle_mode: "plan"`（`G03`） | `??` 不把 `""` 视为 `$null`：<br>① `$sessionId` 为 `""`，第 179 行 `exit 0` 输出 **`0 B`**<br>② 丢掉 `Max`（`87 B`）<br>③ 丢掉目录段与 Git 段（`82 B`）<br>④ 丢掉第 2 行模式提示（`82 B`） | `or` 把 `""` 视为假值向后回退：<br>① 回退 `conversation_id` 或 `"default"` 正常输出（`210 B` / `82 B`）<br>② 渲染 `· Max`（`108 B`）<br>③ 渲染目录段与 Git 段（`148 B`）<br>④ 渲染第 2 行 `▸▸ plan mode on`（`147 B`） | `ps1:178,182,185,188` 使用空合并运算符 `??`（仅 `$null` 回退）；`py:326,331,335,339` 使用布尔 `or`（一切假值回退）。其中①正是 `agy 1.5.9` 启动阶段状态栏空白的直接原因 |
| **DIV-05** | `agent_state: ""`（`B06`） | 缓存段渲染蓝字热态 ` 5:00`（`82 B`），且写出的 `agy_telemetry_<sid>.dat` 首列为空（前导空格导致下次读取 `.Trim() -split '\s+'` 时全部 9 列左移错位） | 缓存段渲染暗灰 ` cold`（`82 B`），状态文件首列写 `idle` | `ps1:181` `$d.agent_state ?? 'idle'` 保留 `""`，第 313 行 `elseif ($state -ne 'idle')` 为真置 `$cacheTs = $nowSec`；`py:330` `or "idle"` 归一为 `"idle"` |

### 2. 模型显示名、Effort 与 Slug 美化

| 编号 | 触发条件（对应样例） | `statusline.ps1` 输出与行为 | `statusline.py` 输出与行为 | 脚本根因（行号） |
| --- | --- | --- | --- | --- |
| **DIV-06** | `model.display_name` 含中点 `·`，如 `"Gemini 4 Carbon · High"` 或 `"Gemini 4 Carbon · medium"`（`C02`、`C03`、`C04`） | ① 无 `effort` 时不拆分，整串着模型青色，无灰色分隔符（`100 B`）<br>② 同时有 `effort: "high"` 时可见文字重复：`Gemini 4 Carbon · High · High`（`122 B`）<br>③ 后缀 `medium` 原样保留小写（`102 B`） | ① 拆分为模型名 + 灰色 ` · ` + 青色 `High`（`114 B`）<br>② 同时有 `effort: "high"` 时只显示一次 `Gemini 4 Carbon · High`（`114 B`）<br>③ 后缀 `medium` 规整为 `Med`（`113 B`） | `ps1:408` 正则字面量使用单引号 `'^(.*?)\s*`u{00b7}\s*(.+)$'`，PowerShell 单引号内不解析 `` `u{00b7} `` 转义；`py:565` 直接判断 `" · " in base_model` |
| **DIV-07** | `gpt-*` 后缀含“字母+数字+字母”或撇号词，如 `model.id: "gpt-5-o4mini"`、`"gpt-4-v2a"`（`C08`） | `GPT-5 O4mini`（`89 B`） | `GPT-5 O4Mini`（`89 B`） | `ps1:91` `.NET InvariantCulture.TextInfo.ToTitleCase` 将字母开头的字母数字串视为同一词，数字后字母保持小写；`py:103` `str.title()` 在任何非大小写字符（含数字）后重新大写首字母 |
| **DIV-08** | `gpt-*` 含连续连字符或尾连字符，如 `"gpt-4--mini"`、`"gpt-4--"`（`C09`） | `GPT-4  Mini`（双空格，`88 B`）/ `GPT-4  `（尾空格） | `GPT-4 Mini`（单空格，`87 B`）/ `GPT-4` | `ps1:89` `($Matches[3] -replace '^-', '') -replace '-', ' '` 只剥离单个前导 `-` 且未 `Trim()`；`py:102` `.lstrip("-").replace("-", " ").strip()` |
| **DIV-09** | 模型 ID 含大写前缀或后缀，如 `"MODELS/gemini-2.5-pro-PREVIEW"`、 `"-LATEST"`（`C10`） | 剥离前后缀并美化为 `Gemini 2.5 Pro`（`91 B`） | 原样输出 `MODELS/gemini-2.5-pro-PREVIEW`（`106 B`） | `ps1:68-72` `-replace` 默认不区分大小写；`py:79-83` `startswith("models/")` 与 `re.sub` 默认区分大小写 |

### 3. 工作目录、Git / VCS 与控制台编码

| 编号 | 触发条件（对应样例） | `statusline.ps1` 输出与行为 | `statusline.py` 输出与行为 | 脚本根因（行号） |
| --- | --- | --- | --- | --- |
| **DIV-10** | `cwd` 是从非 Git 目录指向 Git 仓库内子目录的软链接 / Windows Junction（`D08`；反向从仓库内链向仓库外亦对称相反） | 不显示 Git 段：` Model \| 󰉋 linked_pkg \|  0% \|  cold`（`121 B`） | 显示 Git 段：`... \| 󰉋 linked_pkg \| 󰊢 main ✓ \| ...`（`151 B`） | `ps1:450` `[Path]::GetFullPath($cwd)` 只做词法路径规范化不解析重解析点，向上遍历落在 `outer_plain` 找不到 `.git`；`py:196` `cwd_path.resolve()` 先解析到真实仓库路径再向上找 `.git` |
| **DIV-11** | `vcs.branch`（或真实分支名）含增补平面字符（如 Emoji `🚀`），如 `"a"*22 + "🚀b"`（共 24 个码点、25 个 UTF-16 码元）（`D09`） | 判定超长并劈开 UTF-16 代理对，产生损坏替换符 `U+FFFD`：`aaaaaaaaaaaaaaaaaaaaaa… ✓`（`136 B`，含 `ef bf bd`） | 码点数 `24 <= 24` 不截断，完整显示：`aaaaaaaaaaaaaaaaaaaaaa🚀b ✓`（`135 B`） | `ps1:534` `.Length` 与 `.Substring(0, 23)` 按 UTF-16 code unit 计数并切片；`py:594` `len()` 与 `[:23]` 按 Unicode 码点计数切片 |
| **DIV-12** | 布尔字段传入非布尔真值，如 `vcs.dirty: 1`（或 `"true"`）、`sandbox.enabled: 1`、`sandbox.allow_network: 1`、`tool_confirmation_pending: 1`、`quota.*.disabled: 1`（`D10`） | 视为真：渲染脏标记 `main ●` 与第 2 行 ` sandbox`（`141 B`） | 视为假：渲染干净 `main ✓` 且无第 2 行（`112 B`） | `ps1:190-192,228,252` 使用 PowerShell 宽松相等 `-eq $true`（LHS 为整数 `1` 或字符串 `"true"` 时隐式转型为真）；`py:343,346-347,384,402` 使用 `is True` 严格布尔身份判断 |
| **DIV-13** | 宿主控制台代码页非 UTF-8（如中文 Windows 默认 **CP936** 或英文默认 **CP437**）且 stdin JSON 或 Git 分支名含非 ASCII UTF-8 字节（`D11`、`D12`） | CP936 下读中文路径因 GBK 吞引号致 JSON 解析报错输出 **`0 B`**（`D11`）；CP437 下中文路径与中文分支名乱码 `feat/Σ╕¡µûçσêåµö»`（`D12`，`181 B`） | 全程正常输出 UTF-8 中文路径与中文分支名（`D11` `145 B`，`D12` `168 B`） | `ps1:13` `[Console]::In.ReadToEnd()` 未设 `[Console]::InputEncoding = UTF8`，`ps1:482-492` `ProcessStartInfo` 未设 `StandardOutputEncoding = UTF8`；`py:312,247` 直接读字节流按 UTF-8 解码。（另：Linux/macOS 上 `ps1:480` 硬编码找 `'git.exe'` 永远找不到 `git`；`git` 卡死 >1.5 s 时 `ps1:492` 先调阻塞 `ReadToEnd()` 会挂死到 5 s 被强杀，而 `py:241` `timeout=1.5` 准时降级返回） |

### 4. 数字解析、配额上限与状态/数据库查找

| 编号 | 触发条件（对应样例） | `statusline.ps1` 输出与行为 | `statusline.py` 输出与行为 | 脚本根因（行号） |
| --- | --- | --- | --- | --- |
| **DIV-14** | `cost` 为顶层数字字符串，如 `"cost": "0.42"`（`E05`；注：`{"total_usd": "0.42"}` 两边均能解析） | 渲染花费段 `$0.420`（`105 B`） | 忽略花费段（`82 B`） | `ps1:216` 判断 `$d.cost -is [ValueType] -or $d.cost -is [string]`；`py:373` 只认 `isinstance(cost_raw, (int, float))` |
| **DIV-15** | `quota.*.remaining_fraction > 1.0`，如 `1.2`（`E06`） | 不显示配额段（`82 B`） | 直接乘 100 渲染 `󰁹 120% quota`（`114 B`） | `ps1:235` 有 `if ($minFrac -le 1.0)` 上限保护；`py:386-388` 只过滤 `v >= 0.0` 无上限 |
| **DIV-16** | 整数字段传入 `"inf"` / `"nan"`（或含下划线数字串如 `"1_000"`）（`E07`） | `"inf"` 无法被 `Double.TryParse` 解析，回退为 `0` 正常输出（`82 B`）；`"1_000"` 回退为 `0` | `"inf"` 在 `_to_int` 中触发未捕获 `OverflowError` 导致进程崩溃（退出码 `1`，输出 **`0 B`**）；`"1_000"` 按 PEP 515 解析为 `1000` | `py:45-46` `int(_to_float(val))` 未捕获 `OverflowError`/`ValueError`（`float("inf")` 成功但 `int(inf)` 抛异常） |
| **DIV-17** | 临时目录环境变量：Windows 下 `TEMP` 与 `TMP` 指向不同目录（`F02`），或 macOS 下 `$TMPDIR` 非 `/tmp` | Windows 下读 `TMP` 目录（未命中 `TEMP` 下的预置状态，输出冷态 `82 B`）；macOS 下读 `$TMPDIR` | Windows 下读 `TEMP` 目录（命中预置状态输出 `3:59 \|  48.2 tok/s`，`113 B`）；macOS 下硬编码读 `/tmp` | `ps1:25` `.NET [Path]::GetTempPath()` 在 Windows 优先读 `TMP -> TEMP`；`py:24` Windows 调 `tempfile.gettempdir()` 优先读 `TMPDIR -> TEMP -> TMP`，非 Windows 硬编码 `Path("/tmp")` |
| **DIV-18** | **会话数据库 `conversations/<sid>.db` 响应模型解析**（`F04`–`F09`）：<br>① `steps` 追加数据 >256 KB 使 `gen_metadata` 距 EOF >256 KB（`F04`，本机 13/38 真实非空库实测触发）<br>② WAL `RESTART` 检查点后头部新帧 + 尾部残留旧帧（`F05`）<br>③ `steps` 表含 `\x9a\x01<len><model>` 字节（`F06`）<br>④ `gen_metadata.data` 末尾在模型后跟其他 field 19 字符串如 `end_turn`（`F07`）<br>⑤ `gen_metadata.data` 模型标记后跟 >600 B 填充（`F08`）<br>⑥ 2–3 字节短模型名 `o3` 或两段非白名单名 `mistral-large`（`F09`） | ① 漏检保留 `Auto`<br>② 扫到尾部旧帧 `Opus 4.6`<br>③ 跨表误扫出 `Gemini 2.5 Pro`<br>④ 前缀过滤跳过 `end_turn` 命中 `Opus 4.6`<br>⑤ 256 KB 扫描命中 `Opus 4.6`<br>⑥ `$strLen -ge 4` 及厂商白名单拒收保留 `Auto` | ① SQLite B-tree 命中 `Opus 4.6`<br>② SQLite 按有效 WAL 帧命中最新 `Gemini 2.5 Pro`<br>③ 只查 `gen_metadata` 命中 `Opus 4.6`<br>④ 宽正则误把 `end_turn` 当模型显示 `end_turn`<br>⑤ `substr(data,-600)` 截断漏检保留 `Auto`<br>⑥ 命中 `o3` / `Mistral Large` | `ps1:114-175` 对 `-wal` 和 `.db` 末尾 256 KB 做无表感知原始字节倒扫，要求长度 `4..72`（导致第 151 行自写的 `o[1-9]` 对 2 字符 `o1`/`o3` 永远死代码）且匹配厂商前缀或三段连字符；`py:134-176` 走 SQLite 查询 `gen_metadata` 最新 5 行末尾 600 B，用宽泛 `^[a-zA-Z0-9][a-zA-Z0-9._:-]{1,80}$` 匹配。（注：两边字符集都未放行 `'/'`，故 `models/...` 在两边库扫描里都会被拒收） |

### 5. 第 2 行运行态与 `AGY_*` 环境变量

| 编号 | 触发条件（对应样例） | `statusline.ps1` 输出与行为 | `statusline.py` 输出与行为 | 脚本根因（行号） |
| --- | --- | --- | --- | --- |
| **DIV-19** | `subagents[].status` 含大写字母，如 `"RUNNING"`、`"Working"`（`G04`） | 计为运行中：青色 ` 1/1 agents running`（`122 B`） | 计为未运行：暗灰 ` 1 agents`（`112 B`） | `ps1:246` `-in @('running','working','thinking')` 大小写不敏感；`py:394` Python `in` 元组比较大小写敏感 |
| **DIV-20** | `AGY_THEME` 或 `COLORFGBG` 带首尾空白，如 `AGY_THEME="light "`（常见于 `cmd /c "set AGY_THEME=light && ..."`）（`H03`） | 落回深色主题（`\x1b[0;96m` 等，`82 B`） | `.strip()` 后切换浅色主题（`\x1b[0;36m` 等，`82 B`） | `ps1:350-353` 直接比较 `$env:AGY_THEME -eq 'light'` 未 `.Trim()`；`py:505,508` 显式 `.strip().lower()` |
| **DIV-21** | `AGY_CACHE_RELAY` 设为大写 `"OFF"` / `"Off"`（且存在 `agy_cache_<sid>.dat`）（`H09`） | 视为关闭保活：忽略 `agy_cache_<sid>.dat`，显示 ` 5:00`（`82 B`） | 视为非空 relay URL：读取状态文件覆盖倒计时并显示红色失败徽标 ` 3:00 ✗`（`93 B`），到阈值还会向 `OFF/_touch/<sid>` 发请求 | `ps1:325` `$relayUrl -eq 'off'` 不区分大小写；`py:473` `relay_url == "off"` 区分大小写 |

---

## 三、`AGENTS.md` 字段与环境变量覆盖索引

`fixtures.json` 中 65 组样例对 `AGENTS.md` 全部指定字段与 5 个环境变量的覆盖映射：

| 字段 / 环境变量 | 一致性基线样例（`equal=true`） | 分歧触发样例（`equal=false`） |
| --- | --- | --- |
| `session_id` | `B01`, `B05`, `C01`, `D04`, `E01` | `A06`, `B03`, `B04`, `F02` |
| `conversation_id` | `B01`, `B02` | `B03`, `B04` |
| `agent_state` | `B05`, `E01` | `B03`, `B06`, `F05`, `H09` |
| `execution_mode` | `G01` | `G03` |
| `cycle_mode` | `G02`, `H01`, `H02`, `H04`, `H06`, `H07` | `B03`, `G03` |
| `model.display_name` | `C01`, `F03` | `B03`, `C02`, `C03`, `C04`, `F04`–`F09` |
| `model.id` | `C01`, `C05`, `C07`, `C11` | `B03`, `C06`, `C08`, `C09`, `C10` |
| `model.effort` | `C01` | `B03`, `C03`, `C06` |
| `thinkingLevel` | `C05` | `A07`, `C06` |
| `cwd` | `D01`, `D03`, `D04`, `D05`, `D06`, `D07` | `D02`, `D08`, `D11`, `D12` |
| `workspace.current_dir` | `D01` | `D02` |
| `workspace.project_dir` | `D01` | —（回退机制同 `D02`） |
| `vcs.branch` | `D04`, `D05` | `D09`, `D10` |
| `vcs.dirty` | `D04`, `D05` | `D10` |
| `sandbox.enabled` | `G01`, `G02` | `D10` |
| `sandbox.allow_network` | `G01`, `G02` | —（布尔比较同 `D10`） |
| `context_window.current_usage.input_tokens` | `E01`, `E02`, `H08` | — |
| `context_window.current_usage.output_tokens` | `E01` | — |
| `context_window.current_usage.cache_read_input_tokens` | `E01`, `H08` | — |
| `context_window.total_output_tokens` | `F01`, `F03` | `F04`–`F09` |
| `context_window.context_window_size` | `E01`, `E02`, `E03` | — |
| `context_window.used_percentage` | `E03`, `E04` | — |
| `cost`（含 `cost.total_usd`） | `E03`, `E04` | `E05` |
| `quota.*.remaining_fraction`（含 `disabled`） | `E03`, `E04` | `B03`, `E06` |
| `subagents` | `G01`, `G02` | `G04` |
| `task_count` | `G01`, `G02`, `H04` | `E07` |
| `pending_input_count` | `G01` | — |
| `tool_confirmation_pending` | `G02`, `H05` | —（布尔比较同 `D10`） |
| `AGY_THEME`（含 `COLORFGBG`） | `H01`, `H02` | `H03` |
| `AGY_STATUSLINE_LINES` | `H04`, `H05` | — |
| `AGY_STATUSLINE_INDENT` | `H06`（另实测空串 `""` 两边一致） | — |
| `AGY_SHOW_DEFAULT_MODE` | `H07` | — |
| `AGY_CACHE_RELAY` | `H08` | `H09` |

---

## 四、对下游决策工单的输入要点

1. **[两份参考脚本不一致处以谁为准](https://github.com/bahayonghang/agyline/issues/7)**：
   - 21 处分歧可按性质分成三组裁决：
     - **明显属于 `ps1` 笔误/缺陷的（建议取 `py` 或修掉缺陷）**：`DIV-04`（空串阻塞回退链与启动 `0 B`）、`DIV-05`（`agent_state: ""` 写坏状态文件并误判热缓存）、`DIV-06`（单引号内 `` `u{00b7} `` 不展开）、`DIV-08`（`gpt-4--mini` 双空格）、`DIV-11`（UTF-16 截断劈裂 Emoji 代理对）、`DIV-13`（非 UTF-8 控制台代码页下 stdin 与 `git` 输出乱码/报错）、`DIV-20`（环境变量未 `trim`）。
     - **明显属于 `py` 缺陷或少保护的（建议取 `ps1` 或两边收紧）**：`DIV-01`（顶层非 JSON 对象时 `py` 静默忽略更合理，而 `DIV-16` `py` 遇 `"inf"` 抛异常退出码 1 应兜底）、`DIV-15`（`quota > 1.0` 上限或 clamp 策略）、`DIV-21`（`AGY_CACHE_RELAY=OFF` 不区分大小写）、`DIV-18④`（`py` 过宽的响应模型正则把 `end_turn` 当模型名）。
     - **两边各有取舍需明确拍板的**：`DIV-07`（GPT 后缀 `O4mini` vs `O4Mini`）、`DIV-09`（`models/` 与 `-preview` 是否不区分大小写）、`DIV-10`（`.git` 查找是否先 `canonicalize` 软链接）、`DIV-12`/`DIV-14`/`DIV-19`（布尔/数字串/状态枚举容忍度）、`DIV-17`（临时目录解析规则）、`DIV-18`（响应模型究竟走 SQLite 还是有界扫描，与 [git、会话数据库和 relay touch 怎样离开打印路径](https://github.com/bahayonghang/agyline/issues/9) 联动）。
2. **基线测试样例复用**：`fixtures.json` 中 31 组双脚本一致样例已覆盖全部默认段、阈值、颜色与 5 个 `AGY_*` 环境变量；待 [两份参考脚本不一致处以谁为准](https://github.com/bahayonghang/agyline/issues/7) 拍板 34 组分歧样例的期望输出后，可直接转为 Rust crate 的黄金回归测试集。
