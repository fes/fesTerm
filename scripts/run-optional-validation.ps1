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
        $command = "call `"$vcvarsallPath`" $architecture >nul && set `"PATH=$llvmBinPath;%PATH%`" && set CC=clang && cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture"
        Invoke-NativeCommand { cmd.exe /d /c $command }
    } else {
        Invoke-NativeCommand {
            cargo test -p festerm-sessiond --test native_daemon -- --ignored --nocapture
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
& '.\target\debug\festerm.exe'
$nativePassed = $LASTEXITCODE -eq 0 -and
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
    & "$PSScriptRoot\run-windows-os-input-smoke.ps1"
    if ($LASTEXITCODE -eq 0) {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=pass"
    } else {
        Add-Content -Path $ResultPath -Value "`nsuite=p5-windows-os-input status=fail"
        $status = 'fail'
    }
}

Add-Content -Path $ResultPath -Value "`nstatus=$status"
if ($status -eq 'fail') { exit 1 }
