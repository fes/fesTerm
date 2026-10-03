[CmdletBinding()]
param(
    [string] $ResultPath = $(if ($env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH) {
        $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH
    } else {
        'optional-validation-result.txt'
    })
)

function Invoke-NativeCommand {
    param([scriptblock] $Command)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Command
    } finally {
        $ErrorActionPreference = $previous
    }
}

function Invoke-VisualStudioCommand {
    param(
        [string] $Command,
        [string] $VcVarsAllPath,
        [string] $Architecture,
        [string] $LlvmBinPath
    )

    $batchPath = Join-Path ([System.IO.Path]::GetTempPath()) (
        "festerm-validation-{0}.cmd" -f [System.Guid]::NewGuid().ToString('N')
    )
    try {
        @(
            '@echo off'
            "call `"$VcVarsAllPath`" $Architecture >nul"
            'if errorlevel 1 exit /b %errorlevel%'
            "set `"PATH=$LlvmBinPath;%PATH%`""
            'set "CC=clang"'
            $Command
        ) | Set-Content -LiteralPath $batchPath -Encoding Ascii
        Invoke-NativeCommand { & $batchPath }
    } finally {
        Remove-Item -LiteralPath $batchPath -ErrorAction Ignore
    }
}

if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to run optional validation.'
}

$p5ResultPath = if ($env:FESTERM_P5_REFERENCE_RESULT_PATH) {
    $env:FESTERM_P5_REFERENCE_RESULT_PATH
} else {
    'p5-reference-result.txt'
}
$p6ResultPath = if ($env:FESTERM_P6_RENDER_RESULT_PATH) {
    $env:FESTERM_P6_RENDER_RESULT_PATH
} else {
    'p6-render-result.txt'
}
$opensshResultPath = if ($env:FESTERM_OPENSSH_INTEROP_RESULT_PATH) {
    $env:FESTERM_OPENSSH_INTEROP_RESULT_PATH
} else {
    'openssh-interop-result.txt'
}
$nativeResultPath = 'native-smoke-window-result.txt'
$emojiResultPath = 'native-emoji-smoke-result.txt'
$status = 'pass'
Set-Content -Path $ResultPath -Value 'status=running' -NoNewline

if ($env:OS -eq 'Windows_NT') {
    try {
        & "$PSScriptRoot\stage-conpty.ps1" -RunSmoke
        if ($LASTEXITCODE -ne 0) {
            throw "Windows ConPTY staging failed with exit code $LASTEXITCODE."
        }
        Add-Content -Path $ResultPath -Value "`nsuite=conpty status=pass"
    } catch {
        Add-Content -Path $ResultPath -Value "`nsuite=conpty status=fail"
        $status = 'fail'
    }
} else {
    cargo build --workspace
    if ($LASTEXITCODE -ne 0) { throw 'Workspace build failed.' }
}

if ($env:OS -eq 'Windows_NT') {
    $vcvarsallPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat'
    $llvmBinPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\bin'
    if ((Test-Path -LiteralPath $vcvarsallPath) -and
        (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
        $architecture = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
        $env:PATH = "$llvmBinPath;$env:PATH"
        $env:CC = 'clang'
        Invoke-VisualStudioCommand `
            -Command 'cargo build -p festerm-pty-test-child && cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture' `
            -VcVarsAllPath $vcvarsallPath `
            -Architecture $architecture `
            -LlvmBinPath $llvmBinPath
    } else {
        Invoke-NativeCommand {
            cargo build -p festerm-pty-test-child
            if ($LASTEXITCODE -eq 0) {
                cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture
            }
        }
    }
} else {
    cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture
}
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=sessiond-native status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=sessiond-native status=fail"
    $status = 'fail'
}

if ($env:OS -eq 'Windows_NT' -and
    (Test-Path -LiteralPath $vcvarsallPath) -and
    (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
    Invoke-VisualStudioCommand `
        -Command 'python scripts/check_keyboard_routing.py' `
        -VcVarsAllPath $vcvarsallPath `
        -Architecture $architecture `
        -LlvmBinPath $llvmBinPath
} else {
    Invoke-NativeCommand { python scripts/check_keyboard_routing.py }
}
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=keyboard-routing-synthesized status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=keyboard-routing-synthesized status=fail"
    $status = 'fail'
}

if ($env:OS -eq 'Windows_NT' -and
    (Test-Path -LiteralPath $vcvarsallPath) -and
    (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
    Invoke-VisualStudioCommand `
        -Command 'python scripts/check_running_sessions.py' `
        -VcVarsAllPath $vcvarsallPath `
        -Architecture $architecture `
        -LlvmBinPath $llvmBinPath
} else {
    Invoke-NativeCommand { python scripts/check_running_sessions.py }
}
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=running-session-churn status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=running-session-churn status=fail"
    $status = 'fail'
}

& "$PSScriptRoot\run-p5-reference.ps1" -ResultPath $p5ResultPath
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=p5 status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=p5 status=fail"
    $status = 'fail'
}

& "$PSScriptRoot\run-p6-render-validation.ps1" -ResultPath $p6ResultPath
if ($LASTEXITCODE -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=p6-renderer status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=p6-renderer status=fail"
    $status = 'fail'
}

Remove-Item $opensshResultPath -ErrorAction Ignore
$env:FESTERM_OPENSSH_INTEROP_RESULT_PATH = $opensshResultPath
& "$PSScriptRoot\run-openssh-interop.ps1"
$opensshExitCode = $LASTEXITCODE
Remove-Item Env:FESTERM_OPENSSH_INTEROP_RESULT_PATH -ErrorAction Ignore
if ($opensshExitCode -eq 0 -and
    (Test-Path $opensshResultPath) -and
    ((Get-Content $opensshResultPath -TotalCount 1) -eq 'status=skipped reason=docker-unavailable')) {
    Add-Content -Path $ResultPath -Value "`nsuite=openssh-interop status=skipped reason=docker-unavailable"
} elseif ($opensshExitCode -eq 0) {
    Add-Content -Path $ResultPath -Value "`nsuite=openssh-interop status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=openssh-interop status=fail"
    $status = 'fail'
}

Remove-Item $nativeResultPath -ErrorAction Ignore
$env:FESTERM_NATIVE_WINDOW_SMOKE = '1'
$env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativeResultPath
$nativeBuildExitCode = 0
if ($env:OS -eq 'Windows_NT' -and
    (Test-Path -LiteralPath $vcvarsallPath) -and
    (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
    Invoke-VisualStudioCommand `
        -Command 'cargo build --workspace' `
        -VcVarsAllPath $vcvarsallPath `
        -Architecture $architecture `
        -LlvmBinPath $llvmBinPath
    $nativeBuildExitCode = $LASTEXITCODE
} else {
    Invoke-NativeCommand { cargo build --workspace }
    $nativeBuildExitCode = $LASTEXITCODE
}
if ($nativeBuildExitCode -eq 0) {
    & '.\target\debug\festerm.exe'
}
$nativePassed = $LASTEXITCODE -eq 0 -and
    $nativeBuildExitCode -eq 0 -and
    (Test-Path $nativeResultPath) -and
    ((Get-Content $nativeResultPath -TotalCount 1) -eq 'status=pass')
Remove-Item Env:FESTERM_NATIVE_WINDOW_SMOKE -ErrorAction Ignore
Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
Remove-Item $nativeResultPath -ErrorAction Ignore

if ($nativePassed) {
    Add-Content -Path $ResultPath -Value "`nsuite=p4-native-window status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=p4-native-window status=fail"
    $status = 'fail'
}

Remove-Item $emojiResultPath -ErrorAction Ignore
$env:FESTERM_NATIVE_EMOJI_SMOKE = '1'
$env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $emojiResultPath
& '.\target\debug\festerm.exe'
$emojiPassed = $LASTEXITCODE -eq 0 -and
    (Test-Path $emojiResultPath) -and
    ((Get-Content $emojiResultPath -TotalCount 1) -eq 'status=pass')
Remove-Item Env:FESTERM_NATIVE_EMOJI_SMOKE -ErrorAction Ignore
Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
Remove-Item $emojiResultPath -ErrorAction Ignore
if ($emojiPassed) {
    Add-Content -Path $ResultPath -Value "`nsuite=native-emoji status=pass"
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=native-emoji status=fail"
    $status = 'fail'
}

if ($env:OS -eq 'Windows_NT') {
    & "$PSScriptRoot\run-windows-os-input-smoke.ps1"
    if ($LASTEXITCODE -eq 0) {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=pass"
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=fail"
        $status = 'fail'
    }
    try {
        $renderingOptions = @{}
        if ($env:FESTERM_EXPERIMENTAL_DIRECT2D -eq '1') {
            $renderingOptions.DenseOutput = $true
            $renderingOptions.RequireDirect2D = $true
        }
        & "$PSScriptRoot\check-windows-idle-rendering.ps1" -IncludeSustainedOutput @renderingOptions
        Add-Content -Path $ResultPath -Value "`nsuite=windows-idle-rendering status=pass"
    } catch {
        Write-Warning $_
        Add-Content -Path $ResultPath -Value "`nsuite=windows-idle-rendering status=fail"
        $status = 'fail'
    }
    if ($env:FESTERM_RUN_DIRECT2D_PROBE -eq '1') {
        try {
            $direct2dResult = if ($env:FESTERM_DIRECT2D_PROBE_DIR) {
                $env:FESTERM_DIRECT2D_PROBE_DIR
            } else {
                Join-Path $PSScriptRoot '..\target\direct2d-probe'
            }
            & "$PSScriptRoot\..\validation\direct2d\run.ps1" -ResultDirectory $direct2dResult
            Add-Content -Path $ResultPath -Value "`nsuite=direct2d-replay status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=direct2d-replay status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=direct2d-replay status=skipped reason=experimental-opt-in"
    }
    if ($env:FESTERM_RUN_WARP_PANEL_PROBE -eq '1') {
        try {
            Invoke-NativeCommand {
                cargo test -p festerm replay_large_warp_panels -- --ignored --nocapture
            }
            if ($LASTEXITCODE -ne 0) { throw 'WARP panel replay failed.' }
            Add-Content -Path $ResultPath -Value "`nsuite=warp-panel-replay status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=warp-panel-replay status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=warp-panel-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_WARP_UI_PROBE -eq '1') {
        try {
            Invoke-NativeCommand {
                cargo test --release -p festerm --bin festerm replay_warp_ui_surfaces -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'WARP UI surface replay failed.' }
            Add-Content -Path $ResultPath -Value "`nsuite=warp-ui-replay status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=warp-ui-replay status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=warp-ui-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_TUI_RENDER_PROBE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The Direct2D TUI replay requires Windows x64.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm replay_terminal_tui_workloads -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Terminal TUI rendering replay failed.' }
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-replay status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-replay status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_TUI_CPU_PROFILE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The terminal CPU profile requires Windows x64.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm profile_terminal_residual_cpu -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Terminal CPU profile failed.' }
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-cpu-profile status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-cpu-profile status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=terminal-cpu-profile status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_RETAINED_COMPARISON -eq '1') {
        try {
            if (-not $env:FESTERM_RETAINED_PROBE_EXE -or -not $env:FESTERM_RETAINED_COMPARE_OUT -or
                -not $env:FESTERM_RETAINED_SOURCE_LABEL) {
                throw 'Set FESTERM_RETAINED_PROBE_EXE, FESTERM_RETAINED_COMPARE_OUT, and FESTERM_RETAINED_SOURCE_LABEL.'
            }
            Invoke-NativeCommand {
                python "$PSScriptRoot\..\validation\terminal-performance\compare_retained.py" run `
                    --probe $env:FESTERM_RETAINED_PROBE_EXE `
                    --directory $env:FESTERM_RETAINED_COMPARE_OUT `
                    --source-label $env:FESTERM_RETAINED_SOURCE_LABEL
            }
            if ($LASTEXITCODE -ne 0) { throw 'Balanced retained-prefix comparison failed.' }
            Add-Content -Path $ResultPath -Value "`nsuite=retained-prefix-comparison status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=retained-prefix-comparison status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=retained-prefix-comparison status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_TUI_NATIVE_COMPARISON -eq '1') {
        try {
            if (-not $env:FESTERM_WINDOWS_TERMINAL_PORTABLE -or -not $env:FESTERM_TUI_NATIVE_OUT) {
                throw 'Set FESTERM_WINDOWS_TERMINAL_PORTABLE and FESTERM_TUI_NATIVE_OUT.'
            }
            & "$PSScriptRoot\stage-conpty.ps1" -Configuration Release
            if ($LASTEXITCODE -ne 0) { throw 'Release ConPTY staging failed.' }
            & "$PSScriptRoot\..\validation\terminal-performance\compare-windows.ps1" `
                -WindowsTerminal $env:FESTERM_WINDOWS_TERMINAL_PORTABLE `
                -ResultDirectory $env:FESTERM_TUI_NATIVE_OUT `
                -RegisterBundledFont:($env:FESTERM_REGISTER_TUI_FONT -eq '1')
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-native status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-native status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=terminal-tui-native status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_COPY_QUALIFICATION -eq '1') {
        try {
            if (-not $env:FESTERM_COPY_QUALIFICATION_OUT) {
                throw 'Set FESTERM_COPY_QUALIFICATION_OUT to a fresh native evidence directory.'
            }
            & "$PSScriptRoot\stage-conpty.ps1" -Configuration Release
            if ($LASTEXITCODE -ne 0) { throw 'Release qualification staging failed.' }
            & "$PSScriptRoot\..\validation\terminal-performance\compare-windows.ps1" `
                -FesTermOnly -QualifyCopyModes -CaptureFinalFrame `
                -ResultDirectory $env:FESTERM_COPY_QUALIFICATION_OUT
            Invoke-NativeCommand {
                python "$PSScriptRoot\..\validation\terminal-performance\check_windows.py" `
                    $env:FESTERM_COPY_QUALIFICATION_OUT
            }
            if ($LASTEXITCODE -ne 0) { throw 'Saved native qualification evidence failed validation.' }
            Add-Content -Path $ResultPath -Value "`nsuite=copy-native-qualification status=pass"
        } catch {
            Write-Warning $_
            Add-Content -Path $ResultPath -Value "`nsuite=copy-native-qualification status=fail"
            $status = 'fail'
        }
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=copy-native-qualification status=skipped reason=explicit-opt-in"
    }
}

if ($env:FESTERM_RUN_SURFACE_PROFILE -eq '1') {
    try {
        Invoke-NativeCommand {
            cargo test --release -p festerm --bin festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1
        }
        if ($LASTEXITCODE -ne 0) { throw 'Interactive surface profile failed.' }
        Add-Content -Path $ResultPath -Value "`nsuite=interactive-surface-profile status=pass"
    } catch {
        Write-Warning $_
        Add-Content -Path $ResultPath -Value "`nsuite=interactive-surface-profile status=fail"
        $status = 'fail'
    }
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=interactive-surface-profile status=skipped reason=explicit-opt-in"
}

if ($IsMacOS) {
    Invoke-NativeCommand { python3 "$PSScriptRoot/smoke-ios-simulator.py" --run --build }
    if ($LASTEXITCODE -eq 0) {
        Add-Content -Path $ResultPath -Value "`nsuite=ios-simulator status=pass"
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=ios-simulator status=fail"
        $status = 'fail'
    }
} else {
    Add-Content -Path $ResultPath -Value "`nsuite=ios-simulator status=skipped reason=macos-required"
}

Add-Content -Path $ResultPath -Value "`nstatus=$status"
if ($status -eq 'fail') { exit 1 }
