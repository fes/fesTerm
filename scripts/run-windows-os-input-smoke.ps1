[CmdletBinding()]
param(
    [string] $ResultPath = 'os-input-smoke-result.txt'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') {
    throw 'Windows OS-input smoke is supported only on Windows.'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class FesTermOsInputNative {
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool MoveWindow(
        IntPtr hWnd, int x, int y, int width, int height, bool repaint);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(
        uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
}
'@

$mouseLeftDown = 0x0002
$mouseLeftUp = 0x0004
$repositoryRoot = Split-Path -Parent $PSScriptRoot
. "$PSScriptRoot\windows-application-window.ps1"
if (-not [System.IO.Path]::IsPathRooted($ResultPath)) {
    $ResultPath = Join-Path $repositoryRoot $ResultPath
}
$nativeResultPath = [System.IO.Path]::GetFullPath($ResultPath)

function Send-SmokeKeys([string] $Keys) {
    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
        throw 'The OS-input application window lost foreground activation.'
    }
    $shell.SendKeys($Keys)
}

cargo build --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Remove-Item $nativeResultPath -ErrorAction Ignore
$isolation = "$nativeResultPath.isolation"
if (Test-Path -LiteralPath $isolation) { throw 'OS-input isolation directory already exists.' }
New-Item -ItemType Directory -Path $isolation | Out-Null
$previousConfigPath = $env:FESTERM_CONFIG_PATH
$env:FESTERM_CONFIG_PATH = Join-Path $isolation 'config.toml'
$env:FESTERM_NATIVE_OS_INPUT_SMOKE = '1'
$env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativeResultPath
try {
    $process = Start-Process -FilePath '.\target\debug\festerm.exe' -WorkingDirectory (Get-Location) -PassThru `
        -RedirectStandardOutput "$nativeResultPath.stdout.log" -RedirectStandardError "$nativeResultPath.stderr.log"
} catch {
    Remove-Item -LiteralPath $isolation -Recurse -Force
    throw
} finally {
    $env:FESTERM_CONFIG_PATH = $previousConfigPath
    Remove-Item Env:FESTERM_NATIVE_OS_INPUT_SMOKE -ErrorAction Ignore
    Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
}

try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        $process.Refresh()
        $window = [FesTermApplicationWindow]::Find($process.Id)
        if ($window -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    if ($window -eq [IntPtr]::Zero) {
        throw 'fesTerm did not create a native application window.'
    }

    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    [void] [FesTermOsInputNative]::ShowWindow($window, 5)
    [void] [FesTermOsInputNative]::MoveWindow($window, 100, 100, 860, 540, $true)
    [FesTermApplicationWindow]::Activate($window, $process.Id)
    Start-Sleep -Milliseconds 500
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
        throw 'The OS-input application window lost foreground activation.'
    }
    [void] [FesTermOsInputNative]::SetCursorPos(530, 370)
    [FesTermOsInputNative]::mouse_event($mouseLeftDown, 0, 0, 0, [UIntPtr]::Zero)
    [FesTermOsInputNative]::mouse_event($mouseLeftUp, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 100

    $shell = New-Object -ComObject WScript.Shell
    if ($env:FESTERM_NATIVE_KEYBOARD_ROUTING_SMOKE -eq '1') {
        Send-SmokeKeys '^+c'
        Send-SmokeKeys '^+p'
        Start-Sleep -Seconds 1
        Send-SmokeKeys '{ESC}'
        Start-Sleep -Seconds 1
        Send-SmokeKeys '^+p'
        Start-Sleep -Seconds 1
        Send-SmokeKeys 'paste{ENTER}'
        Start-Sleep -Seconds 1
        Send-SmokeKeys '^b^+b'
    }
    Send-SmokeKeys '{TAB}{UP}os-input-ok{ENTER}'

    $deadline = (Get-Date).AddSeconds(20)
    do {
        if (Test-Path $nativeResultPath) {
            $status = Get-Content $nativeResultPath -TotalCount 1
            if ($status -ne 'status=running') { break }
        }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    if (-not (Test-Path $nativeResultPath)) {
        throw 'OS-input smoke did not write a result.'
    }
    Get-Content $nativeResultPath
    if ((Get-Content $nativeResultPath -TotalCount 1) -ne 'status=pass') {
        throw 'OS-input smoke failed.'
    }
} finally {
    $process.Refresh()
    if (-not $process.HasExited) {
        Stop-Process -Id $process.Id
    }
    Remove-Item -LiteralPath $isolation -Recurse -Force
}
