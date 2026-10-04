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
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class FesTermOsInputNative {
    [StructLayout(LayoutKind.Sequential)]
    private struct Point { public int X, Y; }
    [DllImport("user32.dll", SetLastError=true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")]
    private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
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

    public static int[] FindOwnedClientClickPoint(int left, int top, int width, int height,
        Func<int, int, bool> isOwnedHit) {
        if (width < 80 || height < 200)
            throw new ArgumentException("The owned terminal client is too small for safe input.");
        if (isOwnedHit == null) throw new ArgumentNullException("isOwnedHit");
        int checkedPoints = 0;
        foreach (int row in new[] { 25, 50, 75 }) {
            foreach (int column in new[] { 10, 50, 90 }) {
                checkedPoints++;
                int x = checked(left + (int)((long)width * column / 100));
                int y = checked(top + (int)((long)height * row / 100));
                if (isOwnedHit(x, y)) return new[] { x, y, checkedPoints };
            }
        }
        throw new InvalidOperationException("No visible owned terminal client point was available.");
    }

    public static bool MatchesOwnedHit(IntPtr window, int processId, IntPtr root, uint owner) {
        return window != IntPtr.Zero && processId > 0 && owner == processId && root == window;
    }

    private static bool IsOwnedHit(IntPtr window, int processId, int x, int y) {
        var hit = WindowFromPoint(new Point { X = x, Y = y });
        uint owner;
        return GetWindowThreadProcessId(hit, out owner) != 0 &&
            MatchesOwnedHit(window, processId, GetAncestor(hit, 2), owner);
    }

    public static void ClickOwnedClient(IntPtr window, int processId, int left, int top,
        int width, int height) {
        uint owner;
        if (processId <= 0 || GetWindowThreadProcessId(window, out owner) == 0 || owner != processId)
            throw new ArgumentException("The client click target is not the owned application window.");
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero) throw new Win32Exception();
        try {
            var point = FindOwnedClientClickPoint(left, top, width, height,
                (x, y) => IsOwnedHit(window, processId, x, y));
            if (!SetCursorPos(point[0], point[1]))
                throw new InvalidOperationException("Owned client cursor positioning failed.");
            if (!IsOwnedHit(window, processId, point[0], point[1]))
                throw new InvalidOperationException("The owned client click target changed.");
            Console.WriteLine("owned-focus-click candidates_checked=" + point[2] +
                " physical_x=" + point[0] + " physical_y=" + point[1]);
            mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
            mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
        } finally {
            if (SetThreadDpiAwarenessContext(previous) == IntPtr.Zero) throw new Win32Exception();
        }
    }
}
'@

$repositoryRoot = Split-Path -Parent $PSScriptRoot
. "$PSScriptRoot\windows-application-window.ps1"
[FesTermApplicationWindow]::RequireInteractiveDesktop()
if (-not [System.IO.Path]::IsPathRooted($ResultPath)) {
    $ResultPath = Join-Path $repositoryRoot $ResultPath
}
$nativeResultPath = [System.IO.Path]::GetFullPath($ResultPath)

function Focus-SmokeWindowWithOwnedClick {
    [FesTermApplicationWindow]::RequireInteractiveDesktop()
    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    if ([FesTermOsInputNative]::IsIconic($window)) { throw 'The owned input window is minimized.' }
    $geometry = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
    if ($geometry[2] * 96 / $geometry[4] -lt 80 -or
        $geometry[3] * 96 / $geometry[4] -lt 200) {
        throw 'The owned terminal client is too small for safe input at its current DPI.'
    }
    [FesTermOsInputNative]::ClickOwnedClient($window, $process.Id,
        $geometry[0], $geometry[1], $geometry[2], $geometry[3])
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window -and
           $clock.ElapsedMilliseconds -lt 2000) { Start-Sleep -Milliseconds 50 }
    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
        throw 'The owned client click did not establish foreground focus.'
    }
}

function Send-SmokeKeys([string] $Keys) {
    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
        throw 'The OS-input application window lost foreground activation.'
    }
    $shell.SendKeys($Keys)
}

$executable = Join-Path $repositoryRoot "target\$($Configuration.ToLowerInvariant())\festerm.exe"
$runnerSource = & git -C $repositoryRoot rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify OS-input runner source.' }
$runnerDirty = @(& git -C $repositoryRoot status --porcelain)
if ($LASTEXITCODE -ne 0) { throw 'Cannot record OS-input source cleanliness.' }
if (-not $SkipBuild) {
    $buildArguments = @('build','--locked','--workspace',
        '--manifest-path',(Join-Path $repositoryRoot 'Cargo.toml'),
        '--target-dir',(Join-Path $repositoryRoot 'target'))
    if ($Configuration -eq 'Release') { $buildArguments += '--release' }
    cargo @buildArguments
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $builtSource = & git -C $repositoryRoot rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $builtSource -ne $runnerSource) {
        throw 'OS-input source changed during build.'
    }
    $builtDirty = @(& git -C $repositoryRoot status --porcelain)
    if ($LASTEXITCODE -ne 0 -or ($builtDirty -join "`n") -ne ($runnerDirty -join "`n")) {
        throw 'OS-input checkout changed during build.'
    }
}
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "The selected $Configuration application has not been built: $executable"
}
$executableHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
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
        $maximizedGeometry = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
        $lifecycle.Add([pscustomobject]@{State='maximized';Geometry=$maximizedGeometry})
        if ($CaptureDirectory) {
            Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\maximized.png" |
                ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\maximized.json"
        }
        [void][FesTermOsInputNative]::ShowWindow($window, 6)
        Start-Sleep -Milliseconds 250
        if (-not [FesTermOsInputNative]::IsIconic($window)) { throw 'Owned minimize did not take effect.' }
        $lifecycle.Add([pscustomobject]@{State='minimized';IsIconic=$true})
        [void][FesTermOsInputNative]::ShowWindow($window, 9)
        Start-Sleep -Milliseconds 500
        Focus-SmokeWindowWithOwnedClick
        if (-not [FesTermOsInputNative]::IsZoomed($window) -or
            ($maximizedGeometry -join ',') -ne
                ([FesTermApplicationWindow]::ClientGeometry($window, $process.Id) -join ',')) {
            throw 'Owned restore from minimized did not recover the prior maximized geometry.'
        }
        $lifecycle.Add([pscustomobject]@{State='restored-maximized';Geometry=$maximizedGeometry})
        if ($CaptureDirectory) {
            Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$CaptureDirectory\restored-maximized.png" |
                ConvertTo-Json -Depth 4 | Set-Content "$CaptureDirectory\restored-maximized.json"
        }
        [void][FesTermOsInputNative]::ShowWindow($window, 9)
        Start-Sleep -Milliseconds 500
        Focus-SmokeWindowWithOwnedClick
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
    Focus-SmokeWindowWithOwnedClick
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
            SourceSha=$(if ($SkipBuild) { $null } else { $runnerSource })
            RunnerSourceSha=$runnerSource
            DirtyChanges=$runnerDirty
            Configuration=$Configuration;SkipBuild=[bool]$SkipBuild
            ExecutableSha256=$executableHash
            SourceAttribution=$(if ($SkipBuild) { 'unverified-prebuilt-executable' } else { 'runner-build-with-recorded-dirty-state' })
            CompositionPolicy=$(if ($SkipBuild) { 'unverified-executable' } else { 'automatic-warp' })
            Lifecycle=$lifecycle;Status='pass'
            Boundary='Independent native OS input and external desktop captures, not a CPU sample'
        } | ConvertTo-Json -Depth 6 | Set-Content "$CaptureDirectory\manifest.json"
    }
} catch {
    [pscustomobject]@{
        SourceSha=$(if ($SkipBuild) { $null } else { $runnerSource })
        RunnerSourceSha=$runnerSource
        DirtyChanges=$runnerDirty
        Configuration=$Configuration;SkipBuild=[bool]$SkipBuild
        ExecutableSha256=$executableHash
        ProcessId=$process.Id;Status='fail';Error=$_.ToString()
        SourceAttribution=$(if ($SkipBuild) { 'unverified-prebuilt-executable' } else { 'runner-build-with-recorded-dirty-state' })
        CompositionPolicy=$(if ($SkipBuild) { 'unverified-executable' } else { 'automatic-warp' })
    } | ConvertTo-Json -Depth 5 | Set-Content "$nativeResultPath.failure.json"
    throw
} finally {
    $process.Refresh()
    if ($window -ne [IntPtr]::Zero) {
        Close-FesTermOwnedApplication -Process $process -Window $window -KnownDescendants $descendants |
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
