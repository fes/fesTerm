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

$root = Split-Path -Parent $PSScriptRoot
if (-not [IO.Path]::IsPathRooted($Executable)) { $Executable = Join-Path $root $Executable }
if (-not [IO.Path]::IsPathRooted($ResultPath)) { $ResultPath = Join-Path $root $ResultPath }
$Executable = (Resolve-Path -LiteralPath $Executable).Path
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
}
'@

function Get-GuiFrameNumber([string] $Stdout, [string] $Stderr) {
    $frames = @(Select-String -LiteralPath $Stdout, $Stderr -Pattern 'gui_frame_number=(\d+)')
    if ($frames.Count -eq 0) { throw 'No GUI frame counters were logged by the candidate.' }
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
            $arguments = if ($sustained) {
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
        try {
            $deadline = [DateTime]::UtcNow.AddSeconds(20)
            do {
                Start-Sleep -Milliseconds 100
                $process.Refresh()
            } while (-not $process.HasExited -and $process.MainWindowHandle -eq [IntPtr]::Zero `
                -and [DateTime]::UtcNow -lt $deadline)
            if ($process.HasExited -or $process.MainWindowHandle -eq [IntPtr]::Zero) {
                throw "$scenario did not create a window; inspect $stdout"
            }
            Start-Sleep -Seconds 3
            $process.Refresh()
            [void] [FesTermIdleRenderingNative]::ShowWindow($process.MainWindowHandle, 3)
            Start-Sleep -Seconds 15
            $process.Refresh()
            if ($process.HasExited) { throw "$scenario exited during warmup." }
            if (-not [FesTermIdleRenderingNative]::IsZoomed($process.MainWindowHandle)) {
                throw "$scenario did not remain maximized."
            }
            if ($RequireSoftwareRenderer -and
                -not (Select-String -LiteralPath $stdout, $stderr -Pattern 'device_type=Cpu' -List)) {
                throw "$scenario did not select a software renderer."
            }
            if ($scenario -ne 'launcher' -and
                -not (Get-CimInstance Win32_Process -Filter "ParentProcessId=$($process.Id) AND Name='pwsh.exe'")) {
                throw 'The PowerShell fixture did not start.'
            }

            $firstFrame = if ($sustained) { Get-GuiFrameNumber $stdout $stderr } else { 0 }
            $before = $process.TotalProcessorTime.TotalSeconds
            $clock = [Diagnostics.Stopwatch]::StartNew()
            Start-Sleep -Seconds $SampleSeconds
            $process.Refresh()
            if ($process.HasExited) { throw "$scenario exited during measurement." }
            $elapsed = $clock.Elapsed.TotalSeconds
            $cpu = 100 * ($process.TotalProcessorTime.TotalSeconds - $before) /
                $elapsed / [Environment]::ProcessorCount
            if (-not [FesTermIdleRenderingNative]::IsZoomed($process.MainWindowHandle)) {
                throw "$scenario did not remain maximized during measurement."
            }
            $framesPerSecond = if ($sustained) {
                ((Get-GuiFrameNumber $stdout $stderr) - $firstFrame) / $elapsed
            } else { $null }
            $budget = if ($sustained) { $MaximumOutputCpuPercent } else { $MaximumCpuPercent }
            $passed = $cpu -le $budget -and
                (-not $sustained -or $framesPerSecond -ge $MinimumOutputFramesPerSecond)
            $results.Add([pscustomobject]@{
                scenario = $scenario
                cpu_percent = [Math]::Round($cpu, 3)
                elapsed_seconds = $elapsed
                maximum_cpu_percent = $budget
                gui_frames_per_second = $framesPerSecond
                minimum_gui_frames_per_second = if ($sustained) { $MinimumOutputFramesPerSecond } else { $null }
                status = if ($passed) { 'pass' } else { 'fail' }
            })
        } finally {
            $process.Refresh()
            if (-not $process.HasExited) {
                [void] $process.CloseMainWindow()
                if (-not $process.WaitForExit(10000)) { Stop-Process -Id $process.Id -Force }
            }
            $process.Dispose()
        }
    }
    if (@($results | Where-Object status -eq 'fail').Count -eq 0) { $status = 'pass' }
} finally {
    $env:FESTERM_CONFIG_PATH = $previousConfig
    $env:RUST_LOG = $previousLog
    [pscustomobject]@{
        status = $status
        logical_processors = [Environment]::ProcessorCount
        scenarios = $results.ToArray()
    } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ResultPath -Encoding utf8
    Remove-Item -LiteralPath $isolation -Recurse -Force
}
$results | Format-Table -AutoSize
if ($status -ne 'pass') { throw "Rendering CPU or GUI frame-rate budget failed; see $ResultPath" }
