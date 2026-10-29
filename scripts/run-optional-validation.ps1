[CmdletBinding()]
param(
    [switch] $MarkdownScrollingOnly,
    [string] $ResultPath = $(if ($env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH) {
        $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH
    } else {
        "optional-validation-{0}.txt" -f [Guid]::NewGuid().ToString('N')
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

function Write-OptionalResultLocation {
    param([string] $Path)
    Write-Host ("Optional validation result: {0}" -f [IO.Path]::GetFileName($Path))
}

function Invoke-MarkdownScrollingValidation {
    if ($env:OS -ne 'Windows_NT') { throw 'Native Markdown scrolling requires Windows.' }
    foreach ($name in @('FESTERM_MARKDOWN_SCROLL_EXE', 'FESTERM_MARKDOWN_SCROLL_RECEIPT',
        'FESTERM_MARKDOWN_SCROLL_SOURCE_ROOT', 'FESTERM_MARKDOWN_SCROLL_OUT')) {
        if (-not [Environment]::GetEnvironmentVariable($name)) { throw "Set $name for native Markdown scrolling." }
    }
    & "$PSScriptRoot\check-windows-markdown-scrolling.ps1" `
        -Executable $env:FESTERM_MARKDOWN_SCROLL_EXE -Receipt $env:FESTERM_MARKDOWN_SCROLL_RECEIPT `
        -SourceRoot $env:FESTERM_MARKDOWN_SCROLL_SOURCE_ROOT -OutputDirectory $env:FESTERM_MARKDOWN_SCROLL_OUT `
        -AllowGlobalPointer:($env:FESTERM_MARKDOWN_SCROLL_ALLOW_POINTER -eq '1')
}

if ($MarkdownScrollingOnly) {
    $resultFullPath = [IO.Path]::GetFullPath($ResultPath)
    if (Test-Path -LiteralPath $resultFullPath) {
        throw 'ResultPath already exists; optional validation evidence is never overwritten.'
    }
    Write-OptionalResultLocation $resultFullPath
    $resultStream = [IO.File]::Open(
        $resultFullPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
    $resultWriter = [IO.StreamWriter]::new(
        $resultStream, [Text.UTF8Encoding]::new($false), 1024, $true)
    $resultWriter.AutoFlush = $true
    try {
        $resultWriter.WriteLine('status=running')
        Invoke-MarkdownScrollingValidation
        $resultWriter.WriteLine('suite=windows-markdown-scrolling status=pass')
        $resultWriter.WriteLine('status=pass')
    } catch {
        $resultWriter.WriteLine('suite=windows-markdown-scrolling status=fail')
        $resultWriter.WriteLine('status=fail')
        throw
    } finally {
        $resultWriter.Dispose()
        $resultStream.Dispose()
    }
    return
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
$resultFullPath = [IO.Path]::GetFullPath($ResultPath)
if (Test-Path -LiteralPath $resultFullPath) {
    throw 'ResultPath already exists; optional validation evidence is never overwritten.'
}
Write-OptionalResultLocation $resultFullPath
$resultStream = [IO.File]::Open(
    $resultFullPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$resultWriter = [IO.StreamWriter]::new(
    $resultStream, [Text.UTF8Encoding]::new($false), 1024, $true)
$resultWriter.AutoFlush = $true
function Write-OptionalResult {
    param([string] $Text)
    $resultWriter.Write($Text)
}
Write-OptionalResult 'status=running'
try {

if ($env:OS -eq 'Windows_NT') {
    try {
        & "$PSScriptRoot\stage-conpty.ps1" -RunSmoke
        if ($LASTEXITCODE -ne 0) {
            throw "Windows ConPTY staging failed with exit code $LASTEXITCODE."
        }
        Write-OptionalResult "`nsuite=conpty status=pass"
    } catch {
        Write-OptionalResult "`nsuite=conpty status=fail"
        $status = 'fail'
    }
} else {
    cargo build --workspace
    if ($LASTEXITCODE -ne 0) { throw 'Workspace build failed.' }
}

Invoke-NativeCommand {
    cargo bench --locked -p festerm-ui-egui --bench render_cache_allocations -- --check
}
if ($LASTEXITCODE -eq 0) {
    Write-OptionalResult "`nsuite=presentation-cache-allocations status=pass"
} else {
    Write-OptionalResult "`nsuite=presentation-cache-allocations status=fail"
    $status = 'fail'
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
    Write-OptionalResult "`nsuite=sessiond-native status=pass"
} else {
    Write-OptionalResult "`nsuite=sessiond-native status=fail"
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
    Write-OptionalResult "`nsuite=keyboard-routing-synthesized status=pass"
} else {
    Write-OptionalResult "`nsuite=keyboard-routing-synthesized status=fail"
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
    Write-OptionalResult "`nsuite=running-session-churn status=pass"
} else {
    Write-OptionalResult "`nsuite=running-session-churn status=fail"
    $status = 'fail'
}

& "$PSScriptRoot\run-p5-reference.ps1" -ResultPath $p5ResultPath
if ($LASTEXITCODE -eq 0) {
    Write-OptionalResult "`nsuite=p5 status=pass"
} else {
    Write-OptionalResult "`nsuite=p5 status=fail"
    $status = 'fail'
}

& "$PSScriptRoot\run-p6-render-validation.ps1" -ResultPath $p6ResultPath
if ($LASTEXITCODE -eq 0) {
    Write-OptionalResult "`nsuite=p6-renderer status=pass"
} else {
    Write-OptionalResult "`nsuite=p6-renderer status=fail"
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
    Write-OptionalResult "`nsuite=openssh-interop status=skipped reason=docker-unavailable"
} elseif ($opensshExitCode -eq 0) {
    Write-OptionalResult "`nsuite=openssh-interop status=pass"
} else {
    Write-OptionalResult "`nsuite=openssh-interop status=fail"
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
    Write-OptionalResult "`nsuite=p4-native-window status=pass"
} else {
    Write-OptionalResult "`nsuite=p4-native-window status=fail"
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
    Write-OptionalResult "`nsuite=native-emoji status=pass"
} else {
    Write-OptionalResult "`nsuite=native-emoji status=fail"
    $status = 'fail'
}

if ($env:OS -eq 'Windows_NT') {
    & "$PSScriptRoot\run-windows-os-input-smoke.ps1"
    if ($LASTEXITCODE -eq 0) {
        Write-OptionalResult "`nsuite=p5-windows-os-input status=pass"
    } else {
        Write-OptionalResult "`nsuite=p5-windows-os-input status=fail"
        $status = 'fail'
    }
    try {
        & "$PSScriptRoot\check-windows-idle-rendering.ps1" -IncludeSustainedOutput
        Write-OptionalResult "`nsuite=windows-idle-rendering status=pass"
    } catch {
        Write-Warning $_
        Write-OptionalResult "`nsuite=windows-idle-rendering status=fail"
        $status = 'fail'
    }
    if ($env:FESTERM_RUN_MARKDOWN_SCROLLING -eq '1') {
        try {
            Invoke-MarkdownScrollingValidation
            Write-OptionalResult "`nsuite=windows-markdown-scrolling status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=windows-markdown-scrolling status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=windows-markdown-scrolling status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_DIRECT2D_PROBE -eq '1') {
        try {
            $direct2dResult = if ($env:FESTERM_DIRECT2D_PROBE_DIR) {
                $env:FESTERM_DIRECT2D_PROBE_DIR
            } else {
                Join-Path $PSScriptRoot '..\target\direct2d-probe'
            }
            & "$PSScriptRoot\..\validation\direct2d\run.ps1" -ResultDirectory $direct2dResult
            Write-OptionalResult "`nsuite=direct2d-replay status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=direct2d-replay status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=direct2d-replay status=skipped reason=experimental-opt-in"
    }
    if ($env:FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS -eq '1') {
        try {
            & "$PSScriptRoot\run-windows-sampler-diagnostics.ps1"
            if ($LASTEXITCODE -ne 0) { throw 'Windows sampler diagnostics failed.' }
            Write-OptionalResult "`nsuite=windows-sampler-diagnostics status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=windows-sampler-diagnostics status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=windows-sampler-diagnostics status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_WARP_PANEL_PROBE -eq '1') {
        try {
            Invoke-NativeCommand {
                cargo test -p festerm replay_large_warp_panels -- --ignored --nocapture
            }
            if ($LASTEXITCODE -ne 0) { throw 'WARP panel replay failed.' }
            Write-OptionalResult "`nsuite=warp-panel-replay status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=warp-panel-replay status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=warp-panel-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_WARP_UI_PROBE -eq '1') {
        try {
            Invoke-NativeCommand {
                cargo test --release -p festerm --bin festerm replay_warp_ui_surfaces -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'WARP UI surface replay failed.' }
            $sceneSet = if ($env:FESTERM_WARP_UI_SCENES) { $env:FESTERM_WARP_UI_SCENES } else { 'all' }
            Write-OptionalResult "`nsuite=warp-ui-replay status=pass scene_set=$sceneSet"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=warp-ui-replay status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=warp-ui-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_TUI_RENDER_PROBE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The Direct2D TUI replay requires Windows x64.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm replay_terminal_tui_workloads -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Terminal TUI rendering replay failed.' }
            Write-OptionalResult "`nsuite=terminal-tui-replay status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=terminal-tui-replay status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=terminal-tui-replay status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_TUI_CPU_PROFILE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The terminal CPU profile requires Windows x64.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm profile_terminal_residual_cpu -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Terminal CPU profile failed.' }
            Write-OptionalResult "`nsuite=terminal-cpu-profile status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=terminal-cpu-profile status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=terminal-cpu-profile status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_SESSION_AGING -eq '1') {
        try {
            if (-not $env:FESTERM_AGING_SUITE_OUT) {
                throw 'Set FESTERM_AGING_SUITE_OUT to a fresh absolute evidence directory.'
            }
            & "$PSScriptRoot\check-windows-session-aging.ps1" -OutputDirectory $env:FESTERM_AGING_SUITE_OUT `
                -ThreadOwnership:($env:FESTERM_AGING_THREAD_OWNERSHIP -eq '1')
            Write-OptionalResult "`nsuite=six-session-aging status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=six-session-aging status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=six-session-aging status=skipped reason=explicit-opt-in"
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
            Write-OptionalResult "`nsuite=retained-prefix-comparison status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=retained-prefix-comparison status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=retained-prefix-comparison status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_DOCUMENT_RETENTION_PROFILE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The document retention profile requires Windows x64.' }
            if (-not $env:FESTERM_DOCUMENT_RETENTION_PROFILE_OUT) { throw 'Set FESTERM_DOCUMENT_RETENTION_PROFILE_OUT to a new directory.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm --bin festerm profile_document_retention_changing_frames -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Document retention profile failed.' }
            Write-OptionalResult "`nsuite=document-retention-profile status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=document-retention-profile status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=document-retention-profile status=skipped reason=explicit-opt-in"
    }
    if ($env:FESTERM_RUN_FONT_ATLAS_PROFILE -eq '1') {
        try {
            if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'The font atlas profile requires Windows x64.' }
            if (-not $env:FESTERM_FONT_ATLAS_PROFILE_OUT) { throw 'Set FESTERM_FONT_ATLAS_PROFILE_OUT to a new directory.' }
            Invoke-NativeCommand {
                cargo test --release -p festerm profile_native_font_atlas_capture -- --ignored --nocapture --test-threads=1
            }
            if ($LASTEXITCODE -ne 0) { throw 'Native font atlas profile failed.' }
            Write-OptionalResult "`nsuite=native-font-atlas-profile status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=native-font-atlas-profile status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=native-font-atlas-profile status=skipped reason=explicit-opt-in"
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
            Write-OptionalResult "`nsuite=terminal-tui-native status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=terminal-tui-native status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=terminal-tui-native status=skipped reason=explicit-opt-in"
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
            Write-OptionalResult "`nsuite=copy-native-qualification status=pass"
        } catch {
            Write-Warning $_
            Write-OptionalResult "`nsuite=copy-native-qualification status=fail"
            $status = 'fail'
        }
    } else {
        Write-OptionalResult "`nsuite=copy-native-qualification status=skipped reason=explicit-opt-in"
    }
}

if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    Write-OptionalResult "`nsuite=small-preview-navigation status=skipped reason=windows-x64-required"
} elseif ($env:FESTERM_RUN_PREVIEW_NAVIGATION_PROFILE -eq '1') {
    try {
        if (-not $env:FESTERM_PREVIEW_NAVIGATION_BUILD_RECEIPT -or
            -not $env:FESTERM_PREVIEW_NAVIGATION_TEST_EXE) {
            throw 'Set FESTERM_PREVIEW_NAVIGATION_BUILD_RECEIPT and FESTERM_PREVIEW_NAVIGATION_TEST_EXE to source-bound release artifacts.'
        }
        $navigationLogPrefix = "$ResultPath.preview-navigation-$([Guid]::NewGuid().ToString('N'))"
        function Invoke-PreviewNavigationExecutable {
            param([string[]] $Arguments, [string] $Stage)

            $stdout = "$navigationLogPrefix.$Stage.log"
            $stderr = "$navigationLogPrefix.$Stage.stderr"
            $process = Start-Process -FilePath $env:FESTERM_PREVIEW_NAVIGATION_TEST_EXE `
                -ArgumentList $Arguments -RedirectStandardOutput $stdout `
                -RedirectStandardError $stderr -PassThru -ErrorAction Stop
            if (-not $process.WaitForExit(240000)) {
                Stop-Process -Id $process.Id
                throw 'Navigation executable exceeded its four-minute process budget.'
            }
            [void]$process.WaitForExit()
            if ($process.ExitCode -ne 0) {
                throw "Navigation executable failed; see $stderr."
            }
            Get-Content -LiteralPath $stdout -ErrorAction Stop
        }
        $navigationTests = Invoke-PreviewNavigationExecutable -Arguments @('--list', '--ignored') -Stage 'list'
        if ($navigationTests -notcontains 'direct2d::preview_navigation::profile_small_preview_navigation: test') {
            throw 'The supplied executable does not contain the exact ignored navigation test.'
        }
        $navigationOutput = Invoke-PreviewNavigationExecutable -Arguments @(
            'direct2d::preview_navigation::profile_small_preview_navigation',
            '--exact', '--ignored', '--nocapture', '--test-threads=1'
        ) -Stage 'run'
        $completion = @($navigationOutput | Where-Object { $_.StartsWith('{') } |
            ForEach-Object { $_ | ConvertFrom-Json -ErrorAction Stop } |
            Where-Object { $_.kind -eq 'small-preview-navigation-complete' })
        if ($completion.Count -ne 1 -or $completion[0].status -ne 'pass' -or
            $completion[0].measured_frames -ne 1408 -or $completion[0].completed_draws -ne 1760) {
            throw 'Navigation executable did not produce the complete bounded campaign.'
        }
        $navigationOutput
        Write-OptionalResult "`nsuite=small-preview-navigation status=pass"
    } catch {
        Write-Warning $_
        Write-OptionalResult "`nsuite=small-preview-navigation status=fail"
        $status = 'fail'
    }
} else {
    Write-OptionalResult "`nsuite=small-preview-navigation status=skipped reason=explicit-opt-in"
}

if ($env:FESTERM_RUN_SURFACE_PROFILE -eq '1') {
    try {
        Invoke-NativeCommand {
            cargo test --release -p festerm --bin festerm profile_interactive_surfaces -- --ignored --nocapture --test-threads=1
        }
        if ($LASTEXITCODE -ne 0) { throw 'Interactive surface profile failed.' }
        $profileScenes = if ([string]::IsNullOrEmpty($env:FESTERM_SURFACE_PROFILE_SCENES)) { 'all' } else { $env:FESTERM_SURFACE_PROFILE_SCENES }
        Write-OptionalResult "`nsuite=interactive-surface-profile status=pass scene_set=$profileScenes"
    } catch {
        Write-Warning $_
        Write-OptionalResult "`nsuite=interactive-surface-profile status=fail"
        $status = 'fail'
    }
} else {
    Write-OptionalResult "`nsuite=interactive-surface-profile status=skipped reason=explicit-opt-in"
}

if ($IsMacOS) {
    Invoke-NativeCommand { python3 "$PSScriptRoot/smoke-ios-simulator.py" --run --build }
    if ($LASTEXITCODE -eq 0) {
        Write-OptionalResult "`nsuite=ios-simulator status=pass"
    } else {
        Write-OptionalResult "`nsuite=ios-simulator status=fail"
        $status = 'fail'
    }
} else {
    Write-OptionalResult "`nsuite=ios-simulator status=skipped reason=macos-required"
}

if ($env:FESTERM_RUN_ROW_CACHE_PROFILE -eq '1') {
    try {
        Invoke-NativeCommand {
            cargo test --release -p festerm-ui-egui --lib profile_retained_grid_row_stages -- --ignored --nocapture --test-threads=1
        }
        if ($LASTEXITCODE -ne 0) { throw 'Terminal row-cache profile failed.' }
        Write-OptionalResult "`nsuite=terminal-row-cache-profile status=pass"
    } catch {
        Write-Warning $_
        Write-OptionalResult "`nsuite=terminal-row-cache-profile status=fail"
        $status = 'fail'
    }
} else {
    Write-OptionalResult "`nsuite=terminal-row-cache-profile status=skipped reason=explicit-opt-in"
}

Write-OptionalResult "`nstatus=$status"
} finally {
    $resultWriter.Dispose()
    $resultStream.Dispose()
}
if ($status -eq 'fail') { exit 1 }
