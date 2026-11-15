set windows-shell := ["pwsh.exe", "-NoLogo", "-NoProfile", "-Command"]

default:
    @just --list

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

bench-check:
    cargo bench --no-run

ci: fmt-check clippy test bench-check

install *args:
    cargo install --path . --locked
    agyline install {{args}}

uninstall *args:
    agyline uninstall {{args}}
    cargo uninstall agyline

[windows]
[script("pwsh.exe", "-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File")]
sandbox-install-test:
    $ErrorActionPreference = 'Stop'
    $sandbox = Join-Path ([System.IO.Path]::GetTempPath()) ("agyline_sandbox_" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $sandbox -Force | Out-Null
    try {
        $cargoRoot   = Join-Path $sandbox 'cargo-root'
        $fakeHome    = Join-Path $sandbox 'home'
        $agylineHome = Join-Path $sandbox 'agyline-home'
        $cliDir      = Join-Path $fakeHome '.gemini\antigravity-cli'
        $settings    = Join-Path $cliDir 'settings.json'
        $legacyPs1   = Join-Path $cliDir 'statusline.ps1'

        New-Item -ItemType Directory -Path $cliDir -Force | Out-Null
        [System.IO.File]::WriteAllText($legacyPs1, "# legacy statusline`n")
        $seedJson = "{`n  `"colorScheme`": `"dark`",`n  `"model`": `"Gemini 4 Carbon (High)`",`n  `"statusLine`": {`n    `"type`": `"command`",`n    `"command`": `"pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ~/.gemini/antigravity-cli/statusline.ps1`",`n    `"enabled`": true`n  },`n  `"verbosity`": `"high`"`n}`n"
        [System.IO.File]::WriteAllText($settings, $seedJson)

        cargo install --path . --locked --root $cargoRoot
        if ($LASTEXITCODE -ne 0) { throw "cargo install --root failed ($LASTEXITCODE)" }

        $origUserProfile  = $env:USERPROFILE
        $origHome         = $env:HOME
        $binDir           = Join-Path $cargoRoot 'bin'
        $agylineBin       = Join-Path $binDir 'agyline.exe'
        $env:USERPROFILE  = $fakeHome
        $env:HOME         = $fakeHome
        $env:AGYLINE_HOME = $agylineHome
        $env:PATH         = "$binDir;$env:PATH"

        & $agylineBin install --dry-run | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "dry-run failed ($LASTEXITCODE)" }
        if ([System.IO.File]::ReadAllText($settings) -ne $seedJson) { throw "dry-run mutated settings.json" }

        $null = "" | & $agylineBin install 2>$null
        if ($LASTEXITCODE -ne 1) { throw "non-TTY install without -y must exit 1, got $LASTEXITCODE" }
        if ([System.IO.File]::ReadAllText($settings) -ne $seedJson) { throw "refused install mutated settings.json" }

        & $agylineBin install -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "install -y failed ($LASTEXITCODE)" }
        if (-not (Test-Path -LiteralPath (Join-Path $agylineHome 'state\previous_statusline.json'))) {
            throw "missing previous_statusline.json snapshot"
        }
        if (-not (Test-Path -LiteralPath $legacyPs1)) {
            throw "legacy statusline.ps1 was deleted"
        }

        # Idempotent repeat install exits 0 even without -y
        $null = "" | & $agylineBin install
        if ($LASTEXITCODE -ne 0) { throw "idempotent install failed ($LASTEXITCODE)" }

        # Render smoke test on piped JSON
        $rendered = "{}" | & $agylineBin
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrEmpty($rendered)) {
            throw "live pipe render failed after sandbox install"
        }

        # Uninstall restores exact seeded settings.json
        & $agylineBin uninstall -y | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "uninstall -y failed ($LASTEXITCODE)" }
        if ([System.IO.File]::ReadAllText($settings) -ne $seedJson) {
            throw "uninstall did not restore exact settings.json"
        }

        # Re-install then uninstall --reset --purge
        & $agylineBin install -y | Out-Null
        & $agylineBin uninstall -y --reset --purge | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "uninstall --reset --purge failed ($LASTEXITCODE)" }
        if (Test-Path -LiteralPath $agylineHome) { throw "--purge did not remove AGYLINE_HOME" }

        $env:USERPROFILE = $origUserProfile
        $env:HOME        = $origHome
        Remove-Item Env:\AGYLINE_HOME -ErrorAction SilentlyContinue
        cargo uninstall --root $cargoRoot agyline
        if ($LASTEXITCODE -ne 0) { throw "cargo uninstall --root failed ($LASTEXITCODE)" }
        Write-Host "[OK] Sandbox install/migrate/idempotent/uninstall-restore/purge lifecycle verified."
    } finally {
        if (Test-Path -LiteralPath $sandbox) {
            Remove-Item -LiteralPath $sandbox -Recurse -Force -ErrorAction SilentlyContinue
        }
    }

[unix]
[script("sh", "-eu")]
sandbox-install-test:
    sandbox="$(mktemp -d "${TMPDIR:-/tmp}/agyline_sandbox.XXXXXX")"
    trap 'rm -rf "$sandbox"' EXIT
    cargo_root="$sandbox/cargo-root"
    fake_home="$sandbox/home"
    agyline_home="$sandbox/agyline-home"
    cli_dir="$fake_home/.gemini/antigravity-cli"
    settings="$cli_dir/settings.json"
    legacy_py="$cli_dir/statusline.py"
    mkdir -p "$cli_dir"
    printf '# legacy statusline\n' > "$legacy_py"
    cat > "$settings" <<'EOF'
    {
      "colorScheme": "dark",
      "model": "Gemini 4 Carbon (High)",
      "statusLine": {
        "type": "command",
        "command": "python3 ~/.gemini/antigravity-cli/statusline.py",
        "enabled": true
      },
      "verbosity": "high"
    }
    EOF
    cp "$settings" "$sandbox/seed.json"
    cargo install --path . --locked --root "$cargo_root"
    orig_home="${HOME:-}"
    export HOME="$fake_home"
    export AGYLINE_HOME="$agyline_home"
    export PATH="$cargo_root/bin:$PATH"
    "$cargo_root/bin/agyline" install --dry-run >/dev/null
    cmp -s "$settings" "$sandbox/seed.json"
    if printf '' | "$cargo_root/bin/agyline" install >/dev/null 2>&1; then
      echo "non-TTY install without -y should have exited 1" >&2
      exit 1
    fi
    "$cargo_root/bin/agyline" install -y >/dev/null
    test -f "$agyline_home/state/previous_statusline.json"
    test -f "$legacy_py"
    printf '' | "$cargo_root/bin/agyline" install >/dev/null
    printf '{}' | "$cargo_root/bin/agyline" >/dev/null
    "$cargo_root/bin/agyline" uninstall -y >/dev/null
    cmp -s "$settings" "$sandbox/seed.json"
    "$cargo_root/bin/agyline" install -y >/dev/null
    "$cargo_root/bin/agyline" uninstall -y --reset --purge >/dev/null
    test ! -e "$agyline_home"
    export HOME="$orig_home"
    unset AGYLINE_HOME
    cargo uninstall --root "$cargo_root" agyline
    echo "[OK] Sandbox install/migrate/idempotent/uninstall-restore/purge lifecycle verified."
