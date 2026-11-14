#Requires -Version 7.0
# Runs the observation scenarios one after another. Each run gets its own isolated home under
# <Root>\runs\<name>; agy neither reads nor writes the real ~/.gemini.
#
#   & .\run_scenarios.ps1 -Root $env:TEMP\agyline-research-fr                      # all scenarios
#   & .\run_scenarios.ps1 -Root $env:TEMP\agyline-research-fr -Only s1-ps1,s1-py
#
# <Root>\venv must hold pywinpty and pyte:
#   python -m venv <Root>\venv; <Root>\venv\Scripts\python -m pip install pywinpty pyte
param(
    [Parameter(Mandatory)] [string]$Root,
    [string[]]$Only = @()
)

$python = Join-Path $Root 'venv\Scripts\python.exe'
$observe = Join-Path $PSScriptRoot 'observe.py'

# A step is label|keys|seconds. One slash command: `/`, `model`, Enter, Esc.
$slash = @('slash|/|3', 'model|model|2', 'enter|\r|4', 'esc|\x1b|4', 'idle||6')
$launch = @('boot||15') + $slash

# The same input events, used before and after a conversation exists.
$events = @(
    'char-a|a|3', 'char-b|b|3', 'backspace-2|\x7f\x7f|3',
    'slash-open|/|3', 'slash-filter|st|3', 'slash-close|\x7f\x7f\x7f|3',
    'resize-100|@resize:40x100|3', 'resize-120|@resize:40x120|3',
    'question-open|?|3', 'question-close|\x1b|3',
    'shift-tab|\x1b[Z|3',
    'statusline-help-typed|/statusline help|2', 'statusline-help-enter|\r|4')

$continue = @('--home-template', (Join-Path $Root 'runs\s1-minimal\home'),
              '--workspace', (Join-Path $Root 'runs\s1-minimal\ws'), '--agy-arg=-c')

$scenarios = [ordered]@{
    # 1. Launch to first slash command, one run per status line command.
    's1-minimal'     = @{ kind = 'minimal';     steps = @('boot||60') + $slash }
    's1-py'          = @{ kind = 'py';          steps = $launch }
    's1-ps1'         = @{ kind = 'ps1';         steps = $launch }
    's1-py-direct'   = @{ kind = 'py-direct';   steps = $launch }
    's1-ps1-direct'  = @{ kind = 'ps1-direct';  steps = $launch }
    's1-none'        = @{ kind = 'none';        steps = $launch }
    # Control: statusline.ps1 with only its empty-session-id exit changed (a copy; ref/ is untouched).
    's1-ps1-patched' = @{ kind = 'ps1-patched'; steps = $launch }

    # 2. Which input events run the command: before a conversation exists, then after one does.
    's2-events'       = @{ kind = 'minimal'; steps = @('boot||10') + $events + @('idle||5') }
    's2-events-after' = @{ kind = 'minimal'; steps = @('boot||8', 'model-typed|/model|2', 'model-enter|\r|3', 'model-esc|\x1b|4') + $events +
                           @('model2-typed|/model|2', 'model2-enter|\r|3', 'model2-select|\r|4', 'idle||60') }

    # 3. What agy does with output that is empty, late, or a failure.
    's3-empty'  = @{ kind = 'minimal'; loggerArgs = @('--empty');      steps = $launch }
    's3-exit1'  = @{ kind = 'minimal'; loggerArgs = @('--exit', '1');  steps = @('boot||10') + (1..8 | ForEach-Object { "open$_|/|1.5"; "close$_|\x7f|1.5" }) + @('idle||4') }
    's3-sleep2' = @{ kind = 'minimal'; loggerArgs = @('--sleep', '2'); steps = @('boot||12', 'slash|/|6', 'close|\x7f|6') }
    's3-sleep8' = @{ kind = 'minimal'; loggerArgs = @('--sleep', '8'); steps = @('boot||20', 'slash|/|14', 'close|\x7f|14') }

    # 4. Relaunch with --continue on the home and workspace that s1-minimal left behind.
    's4-continue'     = @{ kind = 'minimal'; steps = @('boot||12', 'slash|/|3', 'close|\x7f|3'); extra = $continue }
    's4-continue-ps1' = @{ kind = 'ps1';     steps = @('boot||12', 'slash|/|3', 'close|\x7f|3'); extra = $continue }

    # 5. Five and a half minutes without input.
    's5-idle-long' = @{ kind = 'minimal'; steps = @('boot||330') }

    # 6. A fresh home, which opens with the onboarding screens.
    's6-onboarding' = @{ kind = 'minimal'; steps = @('boot||10'); extra = @('--onboarding') }

    # 7. The shell these runs were started from exports NO_COLOR=1; the same launch without it.
    's7-color-minimal'    = @{ kind = 'minimal';    steps = $launch; extra = @('--unset-env', 'NO_COLOR') }
    's7-color-ps1-direct' = @{ kind = 'ps1-direct'; steps = $launch; extra = @('--unset-env', 'NO_COLOR') }
}

foreach ($name in $scenarios.Keys) {
    if ($Only.Count -and $name -notin $Only) { continue }
    $s = $scenarios[$name]
    $argv = @($observe, '--root', $Root, '--name', $name, '--kind', $s.kind, '--git') + ($s.extra ?? @())
    foreach ($a in ($s.loggerArgs ?? @())) { $argv += "--logger-arg=$a" }
    foreach ($step in $s.steps) { $argv += @('--step', $step) }
    Write-Host "=== $name"
    & $python @argv
}
