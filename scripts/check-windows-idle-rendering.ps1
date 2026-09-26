[CmdletBinding()]
param(
    [string] $Executable = 'target\debug\festerm.exe',
    [string] $ResultPath = 'target\windows-idle-rendering.json',
    [ValidateRange(5, 120)]
    [int] $SampleSeconds = 10,
    [ValidateRange(0.1, 100)]
    [double] $MaximumCpuPercent = 5,
    [switch] $RequireSoftwareRenderer,
    [switch] $IncludeSustainedOutput,
    [switch] $DenseOutput,
    [switch] $RequireDirect2D,
    [string] $DebuggerPath,
    [ValidateRange(0.1, 100)]
    [double] $MaximumOutputCpuPercent = 30,
    [ValidateRange(1, 120)]
    [double] $MinimumOutputFramesPerSecond = 5
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT' -or $env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Run on a Windows desktop with FESTERM_RUN_OPTIONAL_VALIDATION=1.'
}
if (($DenseOutput -or $RequireDirect2D) -and -not $IncludeSustainedOutput) {
    throw 'DenseOutput and RequireDirect2D require IncludeSustainedOutput.'
}
if ($RequireDirect2D -and $env:FESTERM_EXPERIMENTAL_DIRECT2D -ne '1') {
    throw 'RequireDirect2D requires FESTERM_EXPERIMENTAL_DIRECT2D=1.'
}

$root = Split-Path -Parent $PSScriptRoot
. "$PSScriptRoot\windows-application-window.ps1"
if (-not [IO.Path]::IsPathRooted($Executable)) { $Executable = Join-Path $root $Executable }
if (-not [IO.Path]::IsPathRooted($ResultPath)) { $ResultPath = Join-Path $root $ResultPath }
$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ($DebuggerPath) { $DebuggerPath = (Resolve-Path -LiteralPath $DebuggerPath).Path }
$ResultPath = [IO.Path]::GetFullPath($ResultPath)
New-Item -ItemType Directory -Force -Path (Split-Path $ResultPath) | Out-Null
$isolation = Join-Path ([IO.Path]::GetTempPath()) ("festerm-idle-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $isolation | Out-Null
$shellPath = (Get-Command pwsh -CommandType Application).Source | ConvertTo-Json -Compress
$previousConfig = $env:FESTERM_CONFIG_PATH
$previousLog = $env:RUST_LOG
$results = [Collections.Generic.List[object]]::new()
$status = 'fail'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FesTermIdleRenderingNative {
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr window, int command);
    [DllImport("user32.dll")]
    public static extern bool IsZoomed(IntPtr window);
    [StructLayout(LayoutKind.Sequential)]
    private struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr window);
    public static int[] ClientMetrics(IntPtr window) {
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero) throw new System.ComponentModel.Win32Exception();
        try {
            Rect rect;
            if (!GetClientRect(window, out rect)) throw new System.ComponentModel.Win32Exception();
            var dpi = GetDpiForWindow(window);
            if (dpi == 0) throw new InvalidOperationException("Window DPI unavailable.");
            return new[] { rect.Right - rect.Left, rect.Bottom - rect.Top, (int)dpi };
        } finally {
            if (SetThreadDpiAwarenessContext(previous) == IntPtr.Zero)
                throw new System.ComponentModel.Win32Exception();
        }
    }
}
'@

function Get-FrameNumber([string] $Stdout, [string] $Stderr, [string] $Counter) {
    $frames = @(Select-String -LiteralPath $Stdout, $Stderr -Pattern "$Counter=(\d+)")
    if ($frames.Count -eq 0) { throw "No $Counter counters were logged by the candidate." }
    return [long] $frames[-1].Matches[0].Groups[1].Value
}

try {
    $env:RUST_LOG = 'festerm=info,festerm::rendering=debug,warn'
    $scenarios = @('launcher', 'background-output')
    if ($IncludeSustainedOutput) { $scenarios += 'sustained-output' }
    foreach ($scenario in $scenarios) {
        $sustained = $scenario -eq 'sustained-output'
        $configuration = "schema_version = 1`n"
        if ($scenario -ne 'launcher') {
            $arguments = if ($sustained -and $DenseOutput) {
                @('-NoLogo', '-NoProfile', '-File',
                    (Join-Path $root 'validation\direct2d\dense-output.ps1'))
            } elseif ($sustained) {
                @('-NoLogo', '-NoProfile', '-Command',
                  '$end = [DateTime]::UtcNow.AddSeconds(180); $i = 0; while ([DateTime]::UtcNow -lt $end) { [Console]::Write(([char]13 + "Rendering frame {0:D5}" -f $i)); $i++; Start-Sleep -Milliseconds 100 }')
            } else {
                @('-NoLogo', '-NoProfile', '-NoExit', '-Command', "Write-Output 'idle validation ready'")
            }
            $arguments = ConvertTo-Json -InputObject $arguments -Compress
            $focusedTab = if ($sustained) { 'shell' } else { 'launcher' }
            $configuration += @"
workspace_enabled = true
[[profiles]]
kind = "local"
id = "idle-shell"
executable = $shellPath
arguments = $arguments
[workspace]
focused_tab_id = "$focusedTab"
[[workspace.tabs]]
kind = "launcher"
id = "launcher"
[[workspace.tabs]]
kind = "local_session"
id = "shell"
profile_id = "idle-shell"

"@
        }
        $discovery = if ($scenario -eq 'launcher') { 'true' } else { 'false' }
        $restore = if ($scenario -ne 'launcher') { 'true' } else { 'false' }
        $configuration += @"
[settings]
confirm_session_close = false
restore_workspace = $restore
automatic_update_checks = false
show_resumable_sessions = $discovery
pulse_new_output_dot = true
"@
        $env:FESTERM_CONFIG_PATH = Join-Path $isolation "$scenario.toml"
        Set-Content -LiteralPath $env:FESTERM_CONFIG_PATH -Value $configuration -Encoding utf8
        $stdout = "$ResultPath.$scenario.stdout.log"
        $stderr = "$ResultPath.$scenario.stderr.log"
        $process = Start-Process -FilePath $Executable -WorkingDirectory $root -PassThru -NoNewWindow `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        $window = [IntPtr]::Zero
        try {
            $deadline = [DateTime]::UtcNow.AddSeconds(20)
            do {
                Start-Sleep -Milliseconds 100
                $process.Refresh()
                $window = [FesTermApplicationWindow]::Find($process.Id)
            } while (-not $process.HasExited -and $window -eq [IntPtr]::Zero `
                -and [DateTime]::UtcNow -lt $deadline)
            if ($process.HasExited -or $window -eq [IntPtr]::Zero) {
                throw "$scenario did not create an application window; inspect $stdout"
            }
            $warmupInput = [FesTermApplicationWindow]::LastInputTick()
            Start-Sleep -Seconds 3
            $process.Refresh()
            [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
            [void] [FesTermIdleRenderingNative]::ShowWindow($window, 3)
            [FesTermApplicationWindow]::Activate($window, $process.Id)
            Start-Sleep -Seconds 15
            $warmupInputChanged = [FesTermApplicationWindow]::LastInputTick() -ne $warmupInput
            $process.Refresh()
            if ($process.HasExited) { throw "$scenario exited during warmup." }
            [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
            if (-not [FesTermIdleRenderingNative]::IsZoomed($window) -or
                [FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
                throw "$scenario did not remain maximized and foreground."
            }
            $windowMetrics = [FesTermIdleRenderingNative]::ClientMetrics($window)
            if ($RequireSoftwareRenderer -and
                -not (Select-String -LiteralPath $stdout, $stderr -Pattern 'device_type=Cpu' -List)) {
                throw "$scenario did not select a software renderer."
            }
            if ($scenario -ne 'launcher' -and
                -not (Get-CimInstance Win32_Process -Filter "ParentProcessId=$($process.Id) AND Name='pwsh.exe'")) {
                throw 'The PowerShell fixture did not start.'
            }
            $firstFrame = Get-FrameNumber $stdout $stderr 'gui_frame_number'
            $firstNativeFrame = if ($sustained -and $RequireDirect2D) {
                Get-FrameNumber $stdout $stderr 'direct2d_frame_number'
            } else { 0 }
            $before = $process.TotalProcessorTime.TotalSeconds
            $inputBefore = [FesTermApplicationWindow]::LastInputTick()
            $sampleStarted = [DateTime]::UtcNow
            $clock = [Diagnostics.Stopwatch]::StartNew()
            $intervals = [Collections.Generic.List[object]]::new()
            $previousCpu = $before
            $previousElapsed = 0.0
            $previousFrame = $firstFrame
            do {
                $remaining = $SampleSeconds - $clock.Elapsed.TotalSeconds
                Start-Sleep -Milliseconds ([int][Math]::Max(1, [Math]::Min(1000, $remaining * 1000)))
                $process.Refresh()
                if ($process.HasExited) { throw "$scenario exited during measurement." }
                $elapsed = $clock.Elapsed.TotalSeconds
                $currentCpu = $process.TotalProcessorTime.TotalSeconds
                $currentFrame = Get-FrameNumber $stdout $stderr 'gui_frame_number'
                $intervals.Add([pscustomobject]@{
                    elapsed_seconds = $elapsed
                    cpu_percent = [Math]::Round(100 * ($currentCpu - $previousCpu) /
                        ($elapsed - $previousElapsed) / [Environment]::ProcessorCount, 3)
                    gui_frames = $currentFrame - $previousFrame
                })
                $previousCpu = $currentCpu
                $previousElapsed = $elapsed
                $previousFrame = $currentFrame
            } while ($elapsed -lt $SampleSeconds)
            $cpu = 100 * ($currentCpu - $before) /
                $elapsed / [Environment]::ProcessorCount
            $inputChanged = [FesTermApplicationWindow]::LastInputTick() -ne $inputBefore
            [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
            if (-not [FesTermIdleRenderingNative]::IsZoomed($window) -or
                [FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
                throw "$scenario did not remain maximized and foreground during measurement."
            }
            if (($windowMetrics -join ',') -ne
                ([FesTermIdleRenderingNative]::ClientMetrics($window) -join ',')) {
                throw "$scenario changed physical size or DPI during measurement."
            }
            $framesPerSecond = ($currentFrame - $firstFrame) / $elapsed
            $nativeFramesPerSecond = if ($sustained -and $RequireDirect2D) {
                ((Get-FrameNumber $stdout $stderr 'direct2d_frame_number') - $firstNativeFrame) / $elapsed
            } else { $null }
            if ($sustained -and $RequireDirect2D -and $nativeFramesPerSecond -le 0) {
                throw 'The foreground fixture did not build Direct2D frames during measurement.'
            }
            if ($RequireDirect2D -and
                (Select-String -LiteralPath $stdout, $stderr -Pattern 'disabling experimental Direct2D|Direct2D initialization failed|Direct2D state poisoned' -List)) {
                throw 'The Direct2D fixture fell back; inspect its logs.'
            }
            $budget = if ($sustained) { $MaximumOutputCpuPercent } else { $MaximumCpuPercent }
            $passed = -not ($inputChanged -or $warmupInputChanged) -and $cpu -le $budget -and
                (-not $sustained -or $framesPerSecond -ge $MinimumOutputFramesPerSecond)
            $results.Add([pscustomobject]@{
                scenario = $scenario
                process_id = $process.Id
                window_handle = $window.ToInt64()
                window_class = [FesTermApplicationWindow]::ClassName($window)
                foreground = $true
                input_during_sample = $inputChanged
                input_during_warmup = $warmupInputChanged
                cpu_percent = [Math]::Round($cpu, 3)
                elapsed_seconds = $elapsed
                sample_started_utc = $sampleStarted.ToString('o')
                client_width_pixels = $windowMetrics[0]
                client_height_pixels = $windowMetrics[1]
                window_dpi = $windowMetrics[2]
                maximum_cpu_percent = $budget
                gui_frames_per_second = $framesPerSecond
                intervals = $intervals.ToArray()
                direct2d_frames_per_second = $nativeFramesPerSecond
                working_set_bytes = $process.WorkingSet64
                private_bytes = $process.PrivateMemorySize64
                minimum_gui_frames_per_second = if ($sustained) { $MinimumOutputFramesPerSecond } else { $null }
                status = if ($inputChanged -or $warmupInputChanged) { 'invalid-input' } elseif ($passed) { 'pass' } else { 'fail' }
            })
            if ($inputChanged -or $warmupInputChanged) {
                Write-Warning "$scenario received desktop input during warmup or measurement; its sample is invalid."
            }
            if (-not $passed -and $DebuggerPath) {
                & $DebuggerPath -pv -p $process.Id -c '!runaway 7;~* k 30;lm;q' `
                    *> "$ResultPath.$scenario.stacks.log"
                if ($LASTEXITCODE -ne 0) {
                    throw "Failure stack capture failed; inspect $ResultPath.$scenario.stacks.log"
                }
            }
        } finally {
            $process.Refresh()
            if (-not $process.HasExited) {
                if ($window -ne [IntPtr]::Zero -and
                    [FesTermApplicationWindow]::Matches($window, $process.Id)) {
                    [FesTermApplicationWindow]::Close($window, $process.Id)
                }
                if (-not $process.WaitForExit(10000)) {
                    Write-Warning "$scenario did not exit after WM_CLOSE; terminating the isolated process."
                    Stop-Process -Id $process.Id -Force
                }
            }
            $process.Dispose()
        }
    }
    if (@($results | Where-Object status -ne 'pass').Count -eq 0) { $status = 'pass' }
} finally {
    $env:FESTERM_CONFIG_PATH = $previousConfig
    $env:RUST_LOG = $previousLog
    [pscustomobject]@{
        status = $status
        logical_processors = [Environment]::ProcessorCount
        output_workload = if ($DenseOutput) { '80x24-dense' } else { 'sparse-line' }
        scenarios = $results.ToArray()
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $ResultPath -Encoding utf8
    Remove-Item -LiteralPath $isolation -Recurse -Force
}
$results | Format-Table scenario, cpu_percent, gui_frames_per_second, direct2d_frames_per_second, status -AutoSize
if ($status -ne 'pass') { throw "Rendering measurement invalid or CPU/GUI frame-rate budget failed; see $ResultPath" }
