# agy 1.5.9 怎样运行 `statusLine.command`（Windows）

对应 issue：[bahayonghang/agyline#2](https://github.com/bahayonghang/agyline/issues/2)。调查日期 2026-11-14。

## 对象

| 项 | 值 |
| --- | --- |
| 文件 | `%LOCALAPPDATA%\agy\bin\agy.exe` |
| 版本 | `agy.exe --version` 输出 `1.5.9` |
| 大小 | 231726744 字节 |
| SHA256 | `8803C289CAC04D6E53F2211592D9981C2D5760BC0D9B4A7F222EF6ECC7ABB38A` |
| 格式 | PE32+，windows/amd64，镜像基址 `0x140000000` |
| 工具链 | `go version -m` 输出 `go1.28-20261102-RC00 cl/1012564305 +298f53e483 X:boringcrypto,simd,nogolistexportnewformat` |

全程只读 `agy.exe`，没有启动交互会话，没有改动 `~/.gemini/` 和 `ref/`。

## 证据标记

- 【读】直接读自二进制或官方文档，后面给符号、地址、字符串或 URL。地址是 `agy.exe` 1.5.9 的虚拟地址。
- 【跑】运行某个命令观察到，后面给命令。
- 【推】推断，后面写依据。
- 【读·Go 文档】读自本机 `go doc` 输出的 Go 标准库文档。

三个来源：

1. 二进制。用本目录的 `gopclntab_pe.py` 解析 PE 里的 Go pclntab，反汇编并标注源码行号、内联函数名、调用目标和字符串。用法见文末。
2. 官方文档 <https://antigravity.google/docs/cli/statusline>（2026-11-14 抓取）。文档里的示例 `version` 是 `1.4.5`，可能落后于 1.5.9，但它写的数值与二进制一致。
3. 官方 changelog <https://raw.githubusercontent.com/google-antigravity/antigravity-cli/refs/heads/main/CHANGELOG.md>，与二进制内嵌的 changelog 文本一致。

`exec_replica.go` 是按二进制里读到的调用形状写的 Go 小程序，用本机 `go1.27.4` 编译运行。它不是 agy，只说明 Go 标准库在 Windows 上对这种调用的行为，结论都标【跑·复制品】。

## 结论

| # | 问题 | 结论 | 把握 |
| --- | --- | --- | --- |
| 1 | 触发 | 只由 `(*Manager).notify` 触发。启动时跑一次。没有自己的空闲定时器。两次启动之间至少隔 300 ms，中间的触发合并成一次，用最新的数据。 | 已确认；空闲期是否完全没有运行未穷尽，见下 |
| 2 | 超时与失败 | 单次 5 秒。超时算失败，非零退出算失败，空输出不算失败。 | 已确认 |
| 3 | 自动禁用 | 连续 30 次失败后停用，只在内存里，不写 `settings.json`。`/statusline enable`、`/statusline delete`、重启 agy 都会换一个新的 runner，计数从 0 开始。一次成功的运行也把计数清零。 | 已确认；“agy 保存别的设置也会重建 runner”是推断 |
| 4 | 上一次没结束 | 新一次运行先取消上一次的 context，再启动自己的进程。上一次的结果被丢弃。在 Windows 上被杀的只是 `cmd.exe`，它启动的子进程会继续跑完。 | 取消逻辑已确认；Windows 上子进程存活是复制品观察 |
| 5 | 高度与宽度 | 没有找到数值上限。高度等于输出行数加 `padding`，从消息区的行数里扣。超宽的行由渲染器截断，不折行。 | 高度已确认；宽度是静态阅读，未在真实终端里看过 |
| 6 | 配置键 | `type`、`command`、`padding`、`enabled`、`stack_with_default`，共五个。 | 已确认 |
| 7 | 启动方式 | `cmd /c <command>`，继承 agy 的工作目录和环境变量，stdin 是写完 JSON 就关闭的管道，stdout 和 stderr 各自读进内存缓冲。 | 已确认 |

## 与“已知”清单的出入

- `store.NewStatusLineRunner` 和 `maxStatusLineHeight` 在二进制里只是名字，没有独立的函数体。前者内联进了 `(*Manager).initStatusLine`（`0x14382ba94..0x14382bab7`）。后者只出现在类型的方法名表里，pclntab 没有它的函数，也没有任何函数的内联树提到它，所以读不出一个高度常量。【读】`gopclntab_pe.py list 'maxStatusLineHeight'` 无结果，`inl '.' --callee 'maxStatusLineHeight'` 无结果。
- `types.(*StatusLineConfig).CommandRendersTelemetry`（`0x143415ee0`）是 `xor eax, eax; ret`，恒为 false。`(*StatusLineData).SetTelemetry`（`0x143415f00`）是空函数。【读】
- `context-window-patch.md` 说上游“空闲时从不重绘”。Windows 1.5.9 基本如此，有一个例外：`vcsRefresh` 循环每 1 分钟查一次 VCS 状态，状态变了就调 `notify`，状态栏命令会跟着跑一次。【读】见第 1 项。
- 同一份文件说流式输出期间 `lastRun` 每 300 ms 更新一次。这与 300 ms 节流一致。【读】
- 清单里的字符串、符号和源文件名都在。`Run.func1` 是节流定时器的回调，`Run.gowrap2` 是 `go r.run(data)` 的包装。
- 副产品，不在清单里：`AGENTS.md` 列的 stdin 字段有 `execution_mode` 和 `thinkingLevel`，1.5.9 的 `types.StatusLineData` 没有这两个 JSON 键，对应的是 `cycle_mode` 和 `model.effort`。参考脚本本来就写成 `execution_mode ?? cycle_mode`、`model.effort ?? thinkingLevel`。【读】结构体定义见文末。

## 重建的逻辑

下面是按反汇编重建的 Go 伪代码，不是原始源码。字段名来自类型信息（`gopclntab_pe.py type 0x14b605f40`），行号是 pclntab 里的 `statusline_runner.go` 行号。

```go
type StatusLineRunner struct {        // 152 字节
    mu                  sync.Mutex         // +0x00
    command             string             // +0x08
    padding             int                // +0x18
    cancel              context.CancelFunc // +0x20
    lastRun             time.Time          // +0x28
    timer               *time.Timer        // +0x40
    output              []string           // +0x48
    notify              func()             // +0x60
    stopped             bool               // +0x68
    consecutiveFailures int                // +0x70
    errorHint           string             // +0x78
    logFilePath         string             // +0x88
}

func (r *StatusLineRunner) Run(data *StatusLineData) {   // :66  0x14382c380
    r.mu.Lock(); defer r.mu.Unlock()
    if r.stopped { return }                              // :69
    elapsed := time.Since(r.lastRun)                     // :73
    if elapsed < 300*time.Millisecond {                  // :74  cmp rax, 0x11e1a300
        if r.timer != nil { r.timer.Stop() }             // :76
        r.timer = time.AfterFunc(300*time.Millisecond-elapsed, func() {   // :79  Run.func1
            r.mu.Lock(); r.lastRun = time.Now(); r.mu.Unlock()
            r.run(data)
        })
        return
    }
    r.lastRun = time.Now()                               // :89
    go r.run(data)                                       // :90  Run.gowrap2
}

func (r *StatusLineRunner) run(data *StatusLineData) {   // :131  0x14382c980
    r.mu.Lock()
    if r.stopped { r.mu.Unlock(); return }               // :133
    if r.cancel != nil { r.cancel() }                    // :138  取消上一次
    ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)  // :141  0x12a05f200 ns
    r.cancel = cancel
    r.lastRun = time.Now()                               // :143
    command := r.command
    r.mu.Unlock()
    defer cancel()                                       // :147

    payload, err := json.Marshal(data)                   // :150
    if err != nil { r.recordFailure(fmt.Sprintf("marshal error: %v", err)); return }

    command = types.ExpandHome(command)                  // :157  expandTilde 内联
    cmd := exec.CommandContext(ctx, "cmd", "/c", command) // :173
    cmd.Stdin = bytes.NewReader(payload)                 // :174
    var stdout, stderr bytes.Buffer                      // :175
    cmd.Stdout = &stdout                                 // :176
    cmd.Stderr = &stderr                                 // :177
    cmd.Env = os.Environ()                               // :179
    if err := cmd.Run(); err != nil {                    // :181
        if ctx.Err() == context.Canceled { return }      // :183  被新一次运行或 Stop 取消，不计失败
        r.recordFailure(fmt.Sprintf("command failed: %v (stderr: %s)", err, strings.TrimSpace(stderr.String())))  // :186
        return
    }
    r.mu.Lock(); r.consecutiveFailures = 0; r.errorHint = ""; r.mu.Unlock()   // :191-194
    lines := strings.Split(strings.TrimRight(stdout.String(), "\n"), "\n")    // :198
    if len(lines) == 1 && lines[0] == "" { lines = nil }                      // :199
    r.mu.Lock(); r.output = lines; r.mu.Unlock()         // :203-205
    r.notify()                                           // :206
}

func (r *StatusLineRunner) recordFailure(msg string) {   // :211  0x14382d0c0
    r.mu.Lock()
    r.consecutiveFailures++
    n := r.consecutiveFailures
    cleaned := cleanAndTruncateError(msg, 5)   // 按 "\n" 拆开，逐行 TrimSpace，去空行，最多留 5 行
    r.output = nil                                       // :217
    var hint []string
    if n >= 30 {                                         // :219  cmp r8, 0x1e
        r.stopped = true                                 // :221
        hint = []string{
            fmt.Sprintf("⚠ Statusline disabled after %d consecutive failures.", n),
            "Run /statusline delete to reset, or /statusline enable to retry.",
        }
    } else {
        hint = append([]string{"⚠ Statusline error:"}, cleaned...)
    }
    if r.logFilePath != "" { hint = append(hint, fmt.Sprintf("Full logs at: %s", r.logFilePath)) }
    if n >= 30 { hint = append(hint, fmt.Sprintf("Last error: %s", firstLineOf(cleaned, msg))) }
    r.errorHint = strings.Join(hint, "\n")
    r.mu.Unlock()
    if n >= 30 {
        log.Errorf("statusline: auto-disabled after %d consecutive failures. Last error: %s", n, msg)
    } else if n == 1 || n%10 == 0 {
        log.Infof("statusline: %s (failure %d/%d)", msg, n, 30)
    }
    r.notify()
}
```

## 证据

### 1. 触发、启动、空闲、节流

- 【读】`(*StatusLineRunner).Run` 的直接调用者只有 `(*Manager).triggerStatusLineUpdate`（`0x14382bd4c`）和 `(*Manager).triggerTitleUpdate`（`0x143847520`，窗口标题功能，用的是另一个 runner 实例）。`triggerStatusLineUpdate` 的直接调用者只有 `(*Manager).notify`（`0x1437f8e20`）。命令：`gopclntab_pe.py xref '...'`。
- 【读】`(*Manager).notify`（`manager.go:1184`，`0x1437f8de0`）依次做四件事：`publishSnapshot()`、调 `m.onChange`、`triggerStatusLineUpdate()`、`triggerTitleUpdate()`。
- 【读】`notify` 被 138 个函数直接调用，共 150 个调用点（其中一个是方法值包装 `notify-fm`）。调用者包括 `SendUserMessage`、`startStreaming` 的几个闭包、`(*textDrip).start.func1`（流式文本）、`applyRunStatus`、`applyCostSummary`、`doRefreshQuota`、`doRefreshVCS`、`SetCycleMode`、`SetSelectedModel`、`SetPromptHasInput`、`SetTerminalInfo`、`SetVimMode`、`UpdateSubagentSteps`、`(*TaskManager).UpdateShellCommandOutput`、`ToggleItemExpanded`、`SetActivePanel` 等。也就是说触发面是“store 的任何状态变化”，不只是 agent 状态变化。
- 【读】`triggerStatusLineUpdate`（`statusline_manager.go:69`）：runner 为 nil 就返回；否则 `BuildStatusLineData(m.Store())`，再 `runner.Run(data)`。`Manager+0x420` 的布尔为真时还会打一行 `Debug StatusLineData: %s`。
- 【读】启动：`store.NewManager` 在 `manager.go:713` 调 `initStatusLine`（`0x1437f4ce0`），在 `manager.go:727` 调 `notify`（`0x1437f4e40`）。此时 `lastRun` 是零值，`time.Since` 远大于 300 ms，所以立刻 `go r.run(data)`。官方文档说第一个对话开始前 `conversation_id` 是空串。
- 【读】headless 不跑：`initStatusLine` 在 `statusline_manager.go:23` 读 `Manager+0x48` 的 `atomic.Bool`，为真就返回，不建 runner。【推】这个标志是 `-p` 打印模式，依据是 changelog 1.4.7：“headless `-p` runs ... no longer start your custom status line and window title commands”。
- 【读】运行结束后的回调不会再触发运行。runner 的 `notify` 字段是 `initStatusLine.func1`（`0x14385f1e0`），它只调 `m.onChange(m.Store())` 让界面重画，不调 `(*Manager).notify`。
- 【读】节流与合并：见上面 `Run` 的重建。300 ms 从上一次运行的开始时刻算起（`lastRun` 在 `Run`、定时器回调、`run` 三处都会写）。节流期内每来一次触发，就停掉旧定时器、用新的 `data` 建一个新定时器，所以到点时只跑一次，用的是最后一次的数据。官方文档：“runs your script at most once every 300 ms”。
- 【读】空闲期的两个周期来源：
  - `vcsRefresh`：`NewManager` 在 `manager.go:720` 调 `async.(*Group).StartPeriodicLoop("vcsRefresh", 1m, m.doRefreshVCS)`（`0x1437f4d80`，间隔常量 `0xdf8475800` ns）。这是全二进制里 `StartPeriodicLoop` 唯一的调用点。`doRefreshVCS`（`statusline_manager.go:129`）用 `equalVCS` 比较新旧结果，相同就返回，不同才更新 store 并 `notify`（`0x14382c316`）。
  - `userInputLoop` 的 500 ms ticker（`input_loop.go:265`，`0x1437e99e7`）：ticker 分支（`:652`，select 下标 9）只调闭包 `userInputLoop.func5`。`func5` 开头在标志位置位、或者没有待提交的输入批次时直接返回（`0x14385a686` 跳到 `0x14385abcd`）。没有读完 `func5` 的全部 3936 字节。
- 未确定：150 个调用点没有逐个分析，`interpolationLoop`、`SetStatusFlash.func1` 这类带定时器的路径在完全空闲时是否还会 `notify` 没有穷尽。要确认“空闲时到底跑不跑”，需要动态观察。

### 2. 超时与失败

- 【读】超时 5 秒：`0x14382ca2d: movabs rcx, 0x12a05f200`，随后 `call context.WithTimeout`。官方文档：“stops any run that takes longer than 5 seconds”。
- 【读】失败判定只有两处 `recordFailure` 调用（`0x14382cb88`、`0x14382d054`）：`json.Marshal` 出错，和 `cmd.Run()` 返回非 nil。
- 【读】`cmd.Run()` 出错后先比较 `ctx.Err()` 与 `.data:0x14d7df6a0`。这个全局是 `context.Canceled`：`context.WithCancel.func1`（`0x1400d7492`）用的是同一个地址，它指向的字符串是 `context canceled`；`context.DeadlineExceeded` 在 `0x14d7df6b0`（`context.WithDeadlineCause.func2`）。相等就直接返回，不计失败。
  - 超时：`ctx.Err()` 是 `DeadlineExceeded`，不等于 `Canceled`，计失败。
  - 非零退出：`cmd.Run()` 返回 `*exec.ExitError`，计失败，stdout 被丢弃，`output` 置 nil。
  - 被取消（新一次运行顶替，或 `Stop`）：不计失败，也不改 `output`。
- 【读】空输出不算失败：退出码 0 时先把 `consecutiveFailures` 清零、`errorHint` 清空，再处理 stdout。只有一个空串时 `lines = nil`，状态栏那一块不显示任何内容。
- 【读】失败信息的格式是 `command failed: %v (stderr: %s)`，stderr 先 `strings.TrimSpace`。失败后界面上有一条标题为 `Statusline Error` 的错误提示，正文是 `⚠ Statusline error:` 加最多 5 行错误文本（`initStatusLine.func1`，`0x14385f305`）。同一串连续失败只提示一次，由 `Manager+0x490` 的布尔控制，成功一次后复位。
- 【跑·复制品】Windows 上超时后 `cmd.Run()` 并不在 5 秒时返回。`go run exec_replica.go timeout 1s "pwsh -NoLogo -NoProfile -Command Start-Sleep -Milliseconds 3000; Set-Content <tmp>\marker.txt done; Write-Output late-output"`：`ctx.Err` 是 `context deadline exceeded`，`err` 是 `exit status 1`，但 `Run returned after: 3.311s`，marker 文件被写出，stdout 里有 `late-output`。原因见第 4 项。
- 【推】所以在 agy 里，超时那一次的失败要等命令自己退出才记上；命令永远不退出的话，这一次既不记失败也不更新输出。依据：`run` 没有设 `WaitDelay`（见第 4 项引的 Go 文档），加上复制品的观察。

### 3. 自动禁用与恢复

- 【读】阈值 30：`0x14382d188: cmp r8, 0x1e`，达到后 `mov byte ptr [rdx + 0x68], 1`（`stopped = true`）。日志格式串 `statusline: %s (failure %d/%d)` 的第三个参数取自 `.rdata:0x144e4bf50`，值是 `0x1e`。官方文档：“After 30 failed runs in a row, it stops running the script for the rest of the session”。
- 【读】停用后 `Run` 和 `run` 开头都检查 `stopped` 并返回，不再启动进程。
- 【读】停用只在内存里。`recordFailure` 里的调用只有 `cleanAndTruncateError`、`fmt.Sprintf`、`strings.Join`、日志和 `r.notify()`，没有写设置的调用。
- 【读】提示文本：`⚠ Statusline disabled after %d consecutive failures.`、`Run /statusline delete to reset, or /statusline enable to retry.`、`Last error: %s`，有日志路径时加 `Full logs at: %s`。
- 【读】恢复途径是换一个新的 runner。`initStatusLine`（`statusline_manager.go:14`）先 `Stop()` 旧 runner 并把 `m.statusLineRunner` 置 nil，再按当前配置内联 `NewStatusLineRunner` 建新的，计数从 0 开始。它的调用者只有两个：
  - `NewManager`，也就是重启 agy。
  - `(*Manager).applySettingsSideEffects`（`0x1437bfb0a`），其后紧跟 `InitTitle` 和 `notify`。它的调用者是 `WriteCliSettings`、`SetSessionSetting`、`ClearSessionSetting`、`ResetSettingsToDefaults`。
- 【读】`/statusline` 除 `help` 以外的子命令都走 `(*Manager).SetStatusLineConfig`（`0x1437c3240`）→ `WriteCliSettings`。`/statusline help` 的原文：

  ```
  Usage:
    /statusline                 Toggle statusline on/off.
    /statusline <command>       Configure a custom shell command to render the statusline.
    /statusline delete          Delete the custom command and revert to the built-in default.
    /statusline on | enable     Enable the statusline.
    /statusline off | disable   Disable the statusline.
    /statusline help            Show this help message.
  ```

  `Execute`（`0x1438ff740`）先 `TrimSpace` 再 `ToLower`，比较的字面量是 `help`、`-h`、`--help`、`delete`、`reset`、`enable`、`on`、`disable`、`off`。其余文本当作命令，写成 `type: "command"`。
- 【推】agy 自己保存任何一项设置（例如 `/config` 里改一项）都会重建 runner，顺带把失败计数清零。依据：`WriteCliSettings` 有 16 个调用者（`SetDefaultModel`、`SetToolPermission`、`UpdateSettings`、`SettingsModel.saveFieldCmd.func1` 等），它内部在 `0x1437bf5d4` 调 `applySettingsSideEffects`。这个调用前面的条件分支没有读，所以不能排除“设置没变就跳过”。
- 【推】agy 运行期间从外部改 `settings.json` 不会生效，要等下次启动。依据：`ReadCliSettings` 只被 `initCliSettings` 调用，后者只被 `NewManager` 调用。
- 【读】计数清零的两种情况：一次退出码为 0 的运行（`statusline_runner.go:192`）；runner 被重建。
- 【读】失败日志：第 1 次和每逢 10 的倍数次打 `Infof`，达到 30 打 `Errorf`。
- 【跑】`rg -i statusline ~/.gemini/antigravity-cli/log`：本机现有日志里没有 `statusline: ... (failure n/30)` 这样的行，所以日志这一条没有实际样本可对照。

### 4. 上一次还没结束

- 【读】`run` 在 `statusline_runner.go:138` 先调上一次留下的 `r.cancel`，再建自己的 context（见重建）。两次运行各在自己的 goroutine 里，不排队，也不跳过新的。
- 【读】被取消的那一次，`cmd.Run()` 返回错误后因为 `ctx.Err() == context.Canceled` 直接返回，结果丢弃，不计失败。
- 【读】`run` 没有设置 `cmd.Cancel`、`cmd.WaitDelay`、`cmd.SysProcAttr`：`exec.Cmd` 里被写的只有 `+0x28` Env、`+0x50` Stdin、`+0x60` Stdout、`+0x70` Stderr。
- 【读·Go 文档】`go doc os/exec.CommandContext`：context 结束时默认调 `os.Process.Kill`，`WaitDelay` 不设。`go doc os/exec.Cmd.WaitDelay`：“If WaitDelay is zero (the default), I/O pipes will be read until EOF, which might not occur until orphaned subprocesses of the command have also closed their descriptors for the pipes.”
- 【跑·复制品】Windows 上被杀的只是 `cmd.exe`。`go run exec_replica.go cancel 300ms "pwsh -NoLogo -NoProfile -Command Start-Sleep -Milliseconds 2000; Set-Content <tmp>\marker.txt done; Write-Output late-output"`：`ctx.Err` 是 `context canceled`，`err` 是 `exit status 1`，`Run returned after: 2.296s`，marker 文件被写出。`pwsh` 继承了 stdout 管道的写端，`cmd.Run()` 要等它退出、管道读到 EOF 才返回。
- 【推】对 agy 的含义：
  - 一个启动加运行超过 300 ms 的命令，在触发连续不断的时候（例如流式输出期间）每一次都会被下一次顶掉，结果全部丢弃，状态栏要等触发停下来才更新。依据是 300 ms 节流加上“新的取消旧的”。这一条值得用动态观察验证。
  - 被顶掉的旧进程不会被杀，会与新进程并存到自己退出。慢命令在流式输出期间会叠出多个并发进程。
  - 如果旧进程在被取消之前已经正常退出，它的输出照常写入 `output`。

### 5. 高度与宽度

- 【读】`StatusLineModel.RenderCustom`（`statusline.go:44`，`0x143afa660`）：runner 为 nil 或 `Output()` 为空就返回空串；否则先放 `Padding()` 个空串，再接上输出的各行，用 `"\n"` 连接。没有截行，没有量宽度。
- 【读】`StatusLineModel.Height`（`:187`）是 `Render(...)` 结果里 `"\n"` 的个数加 1，空串为 0。
- 【读】`(*RootModel).handleStatusLineLayout`（`root.go:877`，`0x143ae25e0`）把 `Height(...)` 原样写进 `Layout.StatusLineHeight`，再 `SetLayout`，中间没有比较或取最小值的指令。
- 【读】`Layout.FrameOverhead`（`layout.go:82`）把 `StatusLineHeight` 与其它几项高度相加；`Layout.MessageListAvailableLines`（`layout.go:95`）用终端高度减去它们，结果不大于 0 时取 1。也就是状态栏越高，消息区越矮，最少剩 1 行。
- 【读】`StatusLineModel.MaxHeight`（`:199`，`0x143afb080`）：有自定义状态栏时返回 `Height()+1`（高度为 0 时返回 1），否则返回 1。它没有直接调用者，只有指针接收者的包装函数。
- 【读】`(*RootModel).View`（`root.go:593-599`，`0x143ae0c76`）把 `Render` 的结果用 `"\n"` 接在主内容后面，不做处理。
- 【读】`Render`（`statusline.go:225`）里 `stack_with_default` 为真时先放内置状态栏、再放自定义输出。changelog 1.5.8：状态消息（例如复制提示、设置保存错误）会短暂替换自定义状态栏，消失后恢复，不改变布局。
- 【读】宽度：`agy` 自己不折行也不截断，交给 Bubble Tea v2。`(*cursedRenderer).flush`（`cursed_renderer.go:308`）内联 `uv.NewStyledString`，结构体先清零再只写 `Text`，整个函数没有对 `Wrap` 字段（`[rsp+0x250]`）的写入。`(*StyledString).Draw`（`styled.go:61`，`0x1433ca847`）把 `!s.Wrap` 作为截断标志传给 `uv.printString`，`Tail` 为空。所以超出终端宽度的部分被丢弃，没有省略号。
- 未确定：没有在真实终端里看过超宽行和超高输出的实际效果（例如输出行数超过终端高度时哪一部分被裁掉）。
- 【读】stdin 里有 `terminal_width`（官方文档：“Live width of the interactive terminal”），命令可以据此自己截断。

### 6. `statusLine` 的键

【读】`types.StatusLineConfig`，56 字节，`gopclntab_pe.py type 0x14b422818`：

| 偏移 | 字段 | 类型 | JSON 标签 |
| --- | --- | --- | --- |
| `+0x00` | `Type` | `string` | `json:"type"` |
| `+0x10` | `Command` | `string` | `json:"command"` |
| `+0x20` | `Padding` | `int` | `json:"padding,omitempty"` |
| `+0x28` | `Enabled` | `*bool` | `json:"enabled,omitempty"` |
| `+0x30` | `StackWithDefault` | `bool` | `json:"stack_with_default,omitempty"` |
| `+0x31` | `statusLineConfigInternal` | 空结构体 | 无 |

含义，官方文档与二进制互相印证：

- `command`：要运行的命令。【读】为空串时 `initStatusLine` 不建 runner（`statusline_manager.go:27`），`HasCustomStatusLine` 返回 false，显示内置状态栏。
- `type`：文档说默认 `"command"`，是唯一支持的值。【读】`initStatusLine` 和 `HasCustomStatusLine` 都不读这个字段，1.5.9 实际上不检查它。
- `padding`：整数，默认 0，自定义输出上方的空行数。【读】`RenderCustom` 的循环。
- `enabled`：默认 true。【读】指针非 nil 且指向 false 时不建 runner（`statusline_manager.go:31`），显示内置状态栏，`command` 留在文件里。
- `stack_with_default`：默认 false。为 true 时自定义输出显示在内置状态栏下面，否则替换它。【读】`Render` 在 `statusline.go:234` 读 `+0x30`。changelog 1.0.6 加入。
- 文档另有一句：`statusLine` 要写成对象，写成 `true` 或 `false` 会被读成没有 `command` 的对象，不跑任何脚本。
- 【读】设置文件里还有一个 `title` 键（“Custom terminal title script”，见 <https://antigravity.google/docs/cli/reference>），用同一个 `StatusLineRunner` 类型、同一份 JSON，由 `InitTitle` 和 `triggerTitleUpdate` 驱动，输出各行用空格连接后设为窗口标题。

### 7. 启动方式

- 【读】经过 shell，Windows 上是 `cmd /c`：`0x14382cbff` 取字符串 `cmd`（长度 3），`0x14382cbd3` 取 `/c`（长度 2），`0x14382cc20: call os/exec.CommandContext`，参数切片是 `{"/c", command}`。这个函数里没有 `sh` 分支。官方文档：“the CLI runs your command with `sh -c` (`cmd /c` on Windows)”。
- 【读】`~` 展开：`types.ExpandHome`（`home.go:12`，`0x14340bae0`）只在整个命令等于 `~`、或以 `~/` 开头（`cmp word ptr [rax], 0x2f7e`）时，把开头的 `~` 换成 `os.UserHomeDir()`。`~\` 开头不展开，命令中间的 `~` 也不展开。
- 【读】工作目录：没有写 `cmd.Dir`（`+0x40`），继承 agy 进程的当前目录。
- 【读】环境变量：`cmd.Env = os.Environ()`（`statusline_runner.go:179`，`syscall.Environ` 的返回值原样存入），没有追加任何变量。命令看到的就是 agy 进程的环境。
- 【读】stdin：`bytes.NewReader(payload)`。`os/exec` 对非 `*os.File` 的 `Stdin` 会建管道，由一个 goroutine 把内容写完后关闭写端，所以命令读完 JSON 就读到 EOF。payload 是 `encoding/json` 对 `*types.StatusLineData` 的一次 `Marshal`，一个 JSON 对象。
- 【读】stdout 和 stderr：各是一个 `bytes.Buffer`，同样经管道读到 EOF。stdout 只在退出码为 0 时使用：`strings.TrimRight(s, "\n")` 后按 `"\n"` 拆行。stderr 只在失败时用于错误文本，成功时丢弃。
- 【推】输出用 `\r\n` 换行时，每行末尾会留一个 `\r`，因为只按 `\n` 处理。`StyledString.Draw` 会把内容里的 `\r\n` 换成 `\n`，但最后一行的 `\r` 后面没有 `\n`，会留下。实际显示效果没有观察。命令只输出 `\n` 可以避开这一点。
- 【读】没有设 `SysProcAttr`，所以没有 `HideWindow`、没有自定义 `CmdLine`、没有新的进程组或作业对象。
- 【跑·复制品】命令字符串里不能有双引号。Go 把整个 `command` 当作一个参数按 `CommandLineToArgvW` 的规则转义，内部的 `"` 变成 `\"`，而 `cmd.exe` 不认这种转义。`go run exec_replica.go quote ...` 的结果：

  | `command` | 结果 |
  | --- | --- |
  | `<tmp>\hello.cmd "one two" three` | 运行了，但参数是 `\"one two\" three`，带着反斜杠 |
  | `"<tmp>\dir with space\hello.cmd" one` | 失败，`'\"<tmp>\dir with space\hello.cmd\"' is not recognized as an internal or external command`，退出码 1 |
  | `<tmp>\dir with space\hello.cmd one` | 失败，`'<tmp>\dir' is not recognized ...` |
  | `C:/.../hello.cmd one`（正斜杠） | 正常 |
  | `%TEMP%\agy-research\hello.cmd one` | 正常，`cmd` 展开了环境变量 |
  | `exit /b 3` | `err` 是 `exit status 3` |

  所以在 Windows 上，`statusLine.command` 里的可执行文件路径不能含空格，也不能靠加引号解决。
- 【读·Go 文档】`go doc os/exec.Command`：“Notable exceptions are msiexec.exe and cmd.exe (and thus, all batch files), which have a different unquoting algorithm. In these or other similar cases, you can do the quoting yourself and provide the full command line in SysProcAttr.CmdLine”。agy 没有用 `SysProcAttr.CmdLine`。

## 没能确定的

- 空闲期是否完全没有运行。读到的周期来源只有 `vcsRefresh`（1 分钟，且状态变了才触发）；150 个 `notify` 调用点没有逐个排查。
- `WriteCliSettings` 是否在设置没有变化时跳过 `applySettingsSideEffects`。
- 输出行数超过终端高度时的实际显示，以及超宽行在各终端里的实际显示。这两条只读了代码路径。
- `Render` 和 `MaxHeight` 开头检查的 `TerminalInfo+0x2b` 布尔是什么。它为真时状态栏整体不渲染。
- 慢命令在流式输出期间“结果全部被丢弃”的推断，没有在真实会话里验证。
- 复制品用的是 `go1.27.4`，agy 用的是 `go1.28-20261102-RC00`。`os/exec` 在这两个版本之间的行为差异没有核对。

以上几条都需要启动交互会话才能验证，本次调查按约束没有做。

## 复现

```powershell
$exe = "$env:LOCALAPPDATA\agy\bin\agy.exe"
uv venv $env:TEMP\agy-research\venv --python 3.12
uv pip install --python $env:TEMP\agy-research\venv\Scripts\python.exe capstone   # 用的是 5.0.9
$py = "$env:TEMP\agy-research\venv\Scripts\python.exe"
$t  = ".trellis\research\agy-statusline-runner\gopclntab_pe.py"

& $py $t $exe info                                   # 节、pclntab、moduledata
& $py $t $exe list 'jetski/cli' --file 'statusline'  # 按源文件列函数
& $py $t $exe dis  'store\.\(\*StatusLineRunner\)'   # 反汇编，带行号、内联名、字符串、时长
& $py $t $exe xref 'store\.\(\*Manager\)\.notify$'   # 直接调用者
& $py $t $exe type 0x14b422818                       # types.StatusLineConfig
& $py $t $exe type 0x14b605f40                       # store.StatusLineRunner
& $py $t $exe type 0x14a742838 --depth 3             # *types.StatusLineData
& $py $t $exe inl  '.' --callee 'maxStatusLineHeight'

$env:GOCACHE = "$env:TEMP\agy-research\gocache"
go run .trellis\research\agy-statusline-runner\exec_replica.go quote 'exit /b 3'
```

`gopclntab_pe.py` 的几个要点：

- `go tool objdump` 对这个文件报 `no runtime.pclntab symbol found`，因为它是外部链接的，没有 COFF 符号表。脚本直接在文件里找 pclntab 魔数 `f1 ff ff ff 00 00 01 08`。
- `moduledata` 靠“首个字段指向 pclntab 头、第二个字段指向函数名表”定位，在 `0x14b96f100`。
- 这个工具链的 `moduledata.gofunc` 在偏移 344，不是 Go 1.25 及以前的 320；`types` 在 296。脚本先试 320 再试 344。换 agy 版本后如果内联名或类型名解不出来，先查这两个偏移。
- `xref` 是对 Go 代码段做 `E8`/`E9` 相对跳转的字节扫描，只能找到直接调用，找不到经接口或闭包的调用。

## 附：1.5.9 的 stdin 结构

【读】`types.StatusLineData`（336 字节，`0x14b70ff70`）的 JSON 键，按结构体顺序：

`cwd`、`session_id`、`conversation_id`、`conversation_title`、`transcript_path`、`model`（`id`、`display_name`、`effort`）、`workspace`（`current_dir`、`project_dir`）、`version`、`context_window`（`total_input_tokens`、`total_output_tokens`、`context_window_size`、`used_percentage`、`remaining_percentage`、`current_usage`：`input_tokens`、`output_tokens`、`cache_creation_input_tokens`、`cache_read_input_tokens`）、`exceeds_200k_tokens`、`agent`（`name`）、`product`、`quota`（桶名到 `remaining_fraction`、`remaining_amount`、`reset_time`、`reset_in_seconds`、`disabled`）、`agent_state`、`vcs`（`type`、`branch`、`client`、`dirty`）、`cycle_mode`、`sandbox`（`enabled`、`allow_network`）、`subagents`（`name`、`role`、`model`、`status`）、`artifact_count`、`plan_tier`、`email`、`pending_input_count`、`tool_confirmation_pending`、`task_count`、`terminal_width`、`battle`（`status`、`focused_arm`）、`vim`（`mode`）、`cost`（`total_usd`、`subagent_usd`、`estimated`）。

官方文档的字段表没有列 `vcs.branch`、`vcs.client`、`vcs.dirty`、`sandbox.allow_network`、`subagents[].model`、`battle`。这份结构里没有 `execution_mode` 和 `thinkingLevel`。
