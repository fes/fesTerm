[CmdletBinding()]
param(
    [string] $ResultPath = 'os-input-smoke-result.txt',
    [ValidateSet('Debug','Release')][string] $Configuration = 'Debug',
    [switch] $SkipBuild,
    [string] $CaptureDirectory,
    [switch] $ExerciseWindowLifecycle
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

    [DllImport("user32.dll")]
    public static extern bool IsIconic(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool IsZoomed(IntPtr hWnd);
}
'@

$mouseLeftDown = 0x0002
$mouseLeftUp = 0x0004
$repositoryRoot = Split-Path -Parent $PSScriptRoot
. "$PSScriptRoot\windows-application-window.ps1"
[FesTermApplicationWindow]::RequireInteractiveDesktop()
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

$executable = Join-Path $repositoryRoot "target\$($Configuration.ToLowerInvariant())\festerm.exe"
if (-not $SkipBuild) {
    $buildArguments = @('build','--workspace')
    if ($Configuration -eq 'Release') { $buildArguments += '--release' }
    cargo @buildArguments
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "The selected $Configuration application has not been built: $executable"
}
if (Test-Path -LiteralPath $nativeResultPath) { throw 'Use a fresh OS-input result path; attempts are not overwritten.' }
if ($CaptureDirectory) {
    if (Test-Path -LiteralPath $CaptureDirectory) { throw 'Use a fresh native capture directory.' }
    $CaptureDirectory = [IO.Path]::GetFullPath($CaptureDirectory)
    New-Item -ItemType Directory -Path $CaptureDirectory | Out-Null
}

$isolation = "$nativeResultPath.isolation"
if (Test-Path -LiteralPath $isolation) { throw 'OS-input isolation directory already exists.' }
New-Item -ItemType Directory -Path $isolation | Out-Null
$previousConfigPath = $env:FESTERM_CONFIG_PATH
$env:FESTERM_CONFIG_PATH = Join-Path $isolation 'config.toml'
$env:FESTERM_NATIVE_OS_INPUT_SMOKE = '1'
$env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativeResultPath
try {
    $process = Start-Process -FilePath $executable -WorkingDirectory $repositoryRoot -PassThru `
        -RedirectStandardOutput "$nativeResultPath.stdout.log" -RedirectStandardError "$nativeResultPath.stderr.log"
} catch {
    Remove-Item -LiteralPath $isolation -Recurse -Force
    throw
} finally {
    $env:FESTERM_CONFIG_PATH = $previousConfigPath
    Remove-Item Env:FESTERM_NATIVE_OS_INPUT_SMOKE -ErrorAction Ignore
    Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
}

$window = [IntPtr]::Zero
$descendants = @()
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
    $descendants = @(Get-FesTermOwnedProcessTree -Process $process)
    $descendants | ConvertTo-Json -Depth 5 | Set-Content "$nativeResultPath.child-tree.json"

    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    [void] [FesTermOsInputNative]::ShowWindow($window, 5)
    [void] [FesTermOsInputNative]::MoveWindow($window, 100, 100, 860, 540, $true)
    [FesTermApplicationWindow]::Activate($window, $process.Id)
    Start-Sleep -Milliseconds 500
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
        throw 'The OS-input application window lost foreground activation.'
    }
    $lifecycle = [Collections.Generic.List[object]]::new()
    $initialGeometry = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
    if ($CaptureDirectory) {
        Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\initial.png" |
            ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\initial.json"
    }
    if ($ExerciseWindowLifecycle) {
        [void][FesTermOsInputNative]::ShowWindow($window, 3)
        Start-Sleep -Milliseconds 500
        if (-not [FesTermOsInputNative]::IsZoomed($window)) { throw 'Owned maximize did not take effect.' }
        $lifecycle.Add([pscustomobject]@{State='maximized';Geometry=[FesTermApplicationWindow]::ClientGeometry($window, $process.Id)})
        if ($CaptureDirectory) {
            Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\maximized.png" |
                ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\maximized.json"
        }
        [void][FesTermOsInputNative]::ShowWindow($window, 6)
        Start-Sleep -Milliseconds 250
        if (-not [FesTermOsInputNative]::IsIconic($window)) { throw 'Owned minimize did not take effect.' }
        $lifecycle.Add([pscustomobject]@{State='minimized';IsIconic=$true})
        [void][FesTermOsInputNative]::ShowWindow($window, 9)
        [FesTermApplicationWindow]::Activate($window, $process.Id)
        Start-Sleep -Milliseconds 500
        $restoredGeometry = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
        if ([FesTermOsInputNative]::IsIconic($window) -or [FesTermOsInputNative]::IsZoomed($window) -or
            ($initialGeometry -join ',') -ne ($restoredGeometry -join ',')) {
            throw 'Owned restore did not recover its exact original client geometry.'
        }
        $lifecycle.Add([pscustomobject]@{State='restored';Geometry=$restoredGeometry})
        if ($CaptureDirectory) {
            Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\restored.png" |
                ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\restored.json"
        }
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
        if ($CaptureDirectory) {
            Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\palette.png" |
                ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\palette.json"
        }
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
    if (-not $process.WaitForExit(4000) -or $process.ExitCode -ne 0) {
        throw 'A passing OS-input result did not produce normal application exit.'
    }
    if ($CaptureDirectory) {
        [pscustomobject]@{
            SourceSha=(& git -C $repositoryRoot rev-parse HEAD)
            DirtyChanges=@(& git -C $repositoryRoot status --porcelain)
            Configuration=$Configuration;SkipBuild=[bool]$SkipBuild
            ExecutableSha256=(Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
            HostCopy=$env:FESTERM_EXPERIMENTAL_HOST_COPY
            Retention=$env:FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION
            Lifecycle=$lifecycle;Status='pass'
            Boundary='Independent native OS input and external desktop captures, not a CPU sample'
        } | ConvertTo-Json -Depth 6 | Set-Content "$CaptureDirectory\manifest.json"
    }
} catch {
    [pscustomobject]@{
        SourceSha=(& git -C $repositoryRoot rev-parse HEAD)
        Configuration=$Configuration;SkipBuild=[bool]$SkipBuild
        ExecutableSha256=(Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
        ProcessId=$process.Id;Status='fail';Error=$_.ToString()
        HostCopy=$env:FESTERM_EXPERIMENTAL_HOST_COPY
        Retention=$env:FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION
    } | ConvertTo-Json -Depth 5 | Set-Content "$nativeResultPath.failure.json"
    throw
} finally {
    $process.Refresh()
    if ($window -ne [IntPtr]::Zero) {
        Close-FesTermOwnedApplication -Process $process -Window $window -ConfirmQuit -KnownDescendants $descendants |
            ConvertTo-Json -Depth 6 | Set-Content "$nativeResultPath.cleanup.json"
    } else {
        $forced = -not $process.HasExited
        if ($forced) {
            Stop-Process -Id $process.Id
            if (-not $process.WaitForExit(5000)) { throw 'Owned OS-input process cleanup did not finish.' }
        }
        [pscustomobject]@{ProcessId=$process.Id;Forced=$forced;ExitCode=$process.ExitCode} |
            ConvertTo-Json | Set-Content "$nativeResultPath.cleanup.json"
    }
    if (-not $CaptureDirectory) { Remove-Item -LiteralPath $isolation -Recurse -Force }
}
