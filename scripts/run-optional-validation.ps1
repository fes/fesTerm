[CmdletBinding()]
param(
    [string] $ResultPath = $(if ($env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH) {
        $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH
    } else {
        'optional-validation-result.txt'
    }),
    [string] $HostInputStateDirectory = $env:FESTERM_NATIVE_HOST_INPUT_STATE_DIRECTORY,
    [string] $HostInputRunId = $env:FESTERM_NATIVE_HOST_INPUT_RUN_ID
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
$status = 'pass'
Set-Content -Path $ResultPath -Value 'status=running' -NoNewline

if ($env:OS -eq 'Windows_NT') {
    $vcvarsallPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat'
    $llvmBinPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\bin'
    $architecture = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
    try {
        if ((Test-Path -LiteralPath $vcvarsallPath) -and
            (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
            $stageConptyPath = Join-Path $PSScriptRoot 'stage-conpty.ps1'
            Invoke-VisualStudioCommand `
                -Command "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$stageConptyPath`" -RunSmoke" `
                -VcVarsAllPath $vcvarsallPath `
                -Architecture $architecture `
                -LlvmBinPath $llvmBinPath
        } else {
            & "$PSScriptRoot\stage-conpty.ps1" -RunSmoke
        }
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
    if ((Test-Path -LiteralPath $vcvarsallPath) -and
        (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
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

if ($env:OS -eq 'Windows_NT') {
    $osInputResultPath = Join-Path ([System.IO.Path]::GetTempPath()) (
        "festerm-os-input-result-{0}.txt" -f [System.Guid]::NewGuid().ToString('N')
    )
    Invoke-NativeCommand {
        & "$PSScriptRoot\run-windows-os-input-smoke.ps1" `
            -ResultPath $osInputResultPath `
            -HostInputStateDirectory $HostInputStateDirectory `
            -HostInputRunId $HostInputRunId
    }
    $osInputPassed = (Test-Path -LiteralPath $osInputResultPath) -and
        ((Get-Content -LiteralPath $osInputResultPath -TotalCount 1) -eq 'status=pass')
    Remove-Item -LiteralPath $osInputResultPath -Force -ErrorAction Ignore
    if ($osInputPassed) {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=pass"
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=fail"
        $status = 'fail'
    }
}

Add-Content -Path $ResultPath -Value "`nstatus=$status"
if ($status -eq 'fail') { exit 1 }
