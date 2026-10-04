[CmdletBinding()]
param(
    [string] $WindowsTerminal,
    [Parameter(Mandatory)][string] $ResultDirectory,
    [string] $FesTerm = 'target\release\festerm.exe',
    [ValidateSet('quiet','localized','streaming','full-redraw','changing-chrome')]
    [string[]] $Workloads = @('quiet','localized','streaming','full-redraw'),
    [switch] $RegisterBundledFont,
    [switch] $IncludeFullRepaintControl,
    [switch] $FesTermOnly,
    [switch] $QualifyCopyModes,
    [switch] $OverlayControl,
    [switch] $CaptureFinalFrame,
    [ValidateRange(10, 580)][int] $SampleSeconds = 10,
    [ValidateRange(200, 6000)][int] $ProducerFrames = 200,
    [string] $GuardStopFile
)

function Get-NativeCopyRequest {
    [pscustomobject]@{
        HostCopy = $true
        RetainedComposition = $true
    }
}

function Assert-NativeCandidateExecutable {
    param([string] $Requested, [string] $Expected)
    if (-not [IO.Path]::GetFullPath($Requested).Equals(
        [IO.Path]::GetFullPath($Expected), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'The automatic-mode driver only measures its freshly built checkout executable. Other binaries require their pinned historical driver.'
    }
}

function Assert-NativeGuardClear {
    param([AllowNull()][string] $StopFile)
    if ($StopFile -and (Test-Path -LiteralPath $StopFile)) {
        throw 'External CPU guard failed; stop requested without retry.'
    }
}

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ($QualifyCopyModes) {
    throw 'Native A/B/C switches have been removed. Use the automatic current mode; historical comparisons require their pinned driver and binary.'
}
if ($env:OS -ne 'Windows_NT' -or $env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'This native desktop probe requires Windows and FESTERM_RUN_OPTIONAL_VALIDATION=1.'
}
if (-not $FesTermOnly -and -not $RegisterBundledFont) {
    throw 'Explicit -RegisterBundledFont consent is required for this matched-font probe.'
}
if ($FesTermOnly -and $IncludeFullRepaintControl) {
    throw 'The Windows Terminal full-repaint control cannot run with FesTermOnly.'
}
if ($OverlayControl -and -not $FesTermOnly) {
    throw 'Overlay controls require FesTermOnly.'
}
if ($ProducerFrames * 0.1 -lt $SampleSeconds + 7 -or
    $ProducerFrames * 0.1 -gt $SampleSeconds + 35) {
    throw 'The producer must cover warmup/sampling and complete within the post-sample deadline.'
}
$copyRequest = Get-NativeCopyRequest
$hostCopy = $copyRequest.HostCopy
$retainedComposition = $copyRequest.RetainedComposition
$root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
if (-not [IO.Path]::IsPathRooted($FesTerm)) { $FesTerm = Join-Path $root $FesTerm }
Assert-NativeCandidateExecutable $FesTerm (Join-Path $root 'target\release\festerm.exe')
if ($env:CARGO_TARGET_DIR) {
    $cargoTarget = $env:CARGO_TARGET_DIR
    if (-not [IO.Path]::IsPathRooted($cargoTarget)) { $cargoTarget = Join-Path $root $cargoTarget }
    Assert-NativeCandidateExecutable $cargoTarget (Join-Path $root 'target')
}
$source = & git -C $root rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the source to build.' }
$dirty = @(& git -C $root status --porcelain)
if ($LASTEXITCODE -ne 0) { throw 'Cannot record source cleanliness.' }
if ($dirty.Count -gt 0) { throw 'Automatic native measurements require a committed clean checkout.' }
$oldTargetDirectory = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
    & "$root\scripts\stage-conpty.ps1" -Configuration Release
    if ($LASTEXITCODE -ne 0) { throw 'Current-source release build/staging failed.' }
} finally {
    $env:CARGO_TARGET_DIR = $oldTargetDirectory
}
$builtSource = & git -C $root rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $builtSource -ne $source) { throw 'Source changed during the release build.' }
$builtDirty = @(& git -C $root status --porcelain)
if ($LASTEXITCODE -ne 0 -or $builtDirty.Count -gt 0) { throw 'Checkout changed during the release build.' }
if ($GuardStopFile) {
    $GuardStopFile = [IO.Path]::GetFullPath($GuardStopFile)
    Assert-NativeGuardClear $GuardStopFile
}
. "$root\scripts\windows-application-window.ps1"
[FesTermApplicationWindow]::RequireInteractiveDesktop()
if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -ne 'X64' -or
    -not [Environment]::Is64BitProcess) {
    throw 'This performance comparison requires native Windows x64.'
}
$FesTerm = (Resolve-Path -LiteralPath $FesTerm).Path
if (-not $FesTermOnly) {
    if (-not $WindowsTerminal) { throw 'Supply a portable Windows Terminal path or use -FesTermOnly.' }
    $WindowsTerminal = (Resolve-Path -LiteralPath $WindowsTerminal).Path
    $wtDirectory = Split-Path -Parent $WindowsTerminal
    if (-not (Test-Path -LiteralPath "$wtDirectory\.portable" -PathType Leaf)) {
        throw 'Windows Terminal must be a separate portable distribution with its .portable marker.'
    }
}
$child = (Resolve-Path -LiteralPath "$root\target\release\festerm-pty-test-child.exe").Path
if (Test-Path -LiteralPath $ResultDirectory) { throw 'Use a new evidence directory; old runs are retained.' }
$ResultDirectory = [IO.Path]::GetFullPath($ResultDirectory)
New-Item -ItemType Directory -Path $ResultDirectory | Out-Null
if (-not $FesTermOnly) {
    if (Get-Process -Name WindowsTerminal -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $WindowsTerminal }) {
        throw 'The specified portable Windows Terminal already has a running process.'
    }
    $settingsDirectory = Join-Path $wtDirectory 'settings'
    if (Test-Path -LiteralPath "$settingsDirectory\settings.json") {
        throw 'Use a fresh portable Windows Terminal; this probe does not overwrite existing settings.'
    }
    New-Item -ItemType Directory -Path $settingsDirectory -Force | Out-Null
}

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class TuiComparisonNative {
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int Left,Top,Right,Bottom; }
    [StructLayout(LayoutKind.Sequential)] struct MonitorInfo { public uint Size; public Rect Monitor,Work; public uint Flags; }
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetWindowRect(IntPtr w, out Rect r);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetClientRect(IntPtr w, out Rect r);
    [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr w, uint flags);
    [DllImport("user32.dll", SetLastError=true)] static extern bool GetMonitorInfoW(IntPtr monitor, ref MonitorInfo info);
    [DllImport("user32.dll", SetLastError=true)] static extern bool RedrawWindow(IntPtr w, IntPtr rect, IntPtr region, uint flags);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr w);
    [DllImport("user32.dll", SetLastError=true)] static extern bool SetWindowPos(IntPtr w, IntPtr z, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll", SetLastError=true)] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("gdi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern int AddFontResourceExW(string file, uint flags, IntPtr reserved);
    [DllImport("gdi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool RemoveFontResourceExW(string file, uint flags, IntPtr reserved);
    public static void AddFont(string file) { if(AddFontResourceExW(file,0,IntPtr.Zero)==0) throw new Win32Exception(); }
    public static void RemoveFont(string file) { if(!RemoveFontResourceExW(file,0,IntPtr.Zero)) throw new Win32Exception(); }
    public static void Refresh(IntPtr w) {
        if(!RedrawWindow(w,IntPtr.Zero,IntPtr.Zero,0x101)) throw new Win32Exception();
    }
    public static int[] Metrics(IntPtr w) {
        var old=SetThreadDpiAwarenessContext(new IntPtr(-4)); if(old==IntPtr.Zero) throw new Win32Exception();
        try {
            Rect outer,client;
            if(!GetWindowRect(w,out outer) || !GetClientRect(w,out client)) throw new Win32Exception();
            uint dpi=GetDpiForWindow(w); if(dpi==0) throw new InvalidOperationException("Missing DPI");
            var info=new MonitorInfo { Size=(uint)Marshal.SizeOf(typeof(MonitorInfo)) };
            if(!GetMonitorInfoW(MonitorFromWindow(w,2),ref info)) throw new Win32Exception();
            return new[]{outer.Right-outer.Left,outer.Bottom-outer.Top,client.Right-client.Left,client.Bottom-client.Top,(int)dpi,outer.Left,outer.Top,info.Work.Left,info.Work.Top,info.Work.Right,info.Work.Bottom};
        } finally { if(SetThreadDpiAwarenessContext(old)==IntPtr.Zero) throw new Win32Exception(); }
    }
    public static void Resize(IntPtr w,int width,int height) {
        var old=SetThreadDpiAwarenessContext(new IntPtr(-4)); if(old==IntPtr.Zero) throw new Win32Exception();
        try {
            if(width<300 || height<300 || width>4096 || height>4096) throw new ArgumentOutOfRangeException();
            var info=new MonitorInfo { Size=(uint)Marshal.SizeOf(typeof(MonitorInfo)) };
            if(!GetMonitorInfoW(MonitorFromWindow(w,2),ref info)) throw new Win32Exception();
            int availableWidth=info.Work.Right-info.Work.Left,availableHeight=info.Work.Bottom-info.Work.Top;
            if(width>availableWidth || height>availableHeight) throw new InvalidOperationException("The matched grid does not fit the monitor work area.");
            if(!SetWindowPos(w,IntPtr.Zero,info.Work.Left+(availableWidth-width)/2,info.Work.Top+(availableHeight-height)/2,width,height,0x14)) throw new Win32Exception();
        } finally { if(SetThreadDpiAwarenessContext(old)==IntPtr.Zero) throw new Win32Exception(); }
    }
}
'@
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes

function Read-Geometry([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $stream = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    $reader = [IO.StreamReader]::new($stream)
    try { return $reader.ReadToEnd() | ConvertFrom-Json }
    finally { $reader.Dispose() }
}
function Frame-Count([string] $Directory, [string] $Counter = 'gui_frame_number') {
    $matches = @(Select-String -LiteralPath "$Directory\stdout.log","$Directory\stderr.log" -Pattern "$Counter=(\d+)")
    if ($matches.Count -eq 0) { throw "No $Counter diagnostics in $Directory" }
    return [long]$matches[-1].Matches[0].Groups[1].Value
}
function Terminal-Font([IntPtr] $Window) {
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($Window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty,$true)
    $candidates = $element.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition)
    $faces = @(
        foreach ($candidate in $candidates) {
            $pattern = $candidate.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            if ($pattern.DocumentRange.GetText(512).Contains('FESTERM-TUI-PROBE-READY')) {
                $pattern.DocumentRange.GetAttributeValue([System.Windows.Automation.TextPattern]::FontNameAttribute)
            }
        }
    )
    if (@($faces | Where-Object { $_ -eq 'JetBrains Mono NL' }).Count -ne 1) {
        throw "Expected one terminal TextPattern with JetBrains Mono NL; observed: $($faces -join ', ')"
    }
    return 'JetBrains Mono NL'
}

function Get-NativeWarmupGuard {
    param(
        [Parameter(Mandatory)][uint32] $InputBefore,
        [Parameter(Mandatory)][uint32] $InputObserved,
        [Parameter(Mandatory)][long] $ExpectedWindow,
        [Parameter(Mandatory)][long] $ObservedWindow,
        [Parameter(Mandatory)][int[]] $ExpectedMetrics,
        [Parameter(Mandatory)][int[]] $ObservedMetrics
    )
    [pscustomobject]@{
        SchemaVersion = 1
        Utc = [DateTime]::UtcNow.ToString('o')
        InputTickBefore = $InputBefore
        InputTickObserved = $InputObserved
        ExpectedWindow = $ExpectedWindow
        ObservedWindow = $ObservedWindow
        ExpectedMetrics = $ExpectedMetrics
        ObservedMetrics = $ObservedMetrics
        InputChanged = $InputObserved -ne $InputBefore
        ForegroundChanged = $ObservedWindow -ne $ExpectedWindow
        GeometryChanged = ($ExpectedMetrics -join ',') -ne ($ObservedMetrics -join ',')
    }
}

$oldConfig = $env:FESTERM_CONFIG_PATH
$oldLog = $env:RUST_LOG
$fonts = [Collections.Generic.List[string]]::new()
$results = [Collections.Generic.List[object]]::new()
$modes = @('current')
$runs = @(
    for ($sequence = 0; $sequence -lt $modes.Count; $sequence++) {
        foreach ($workload in $Workloads) {
            [pscustomobject]@{
                Host='festerm';Workload=$workload;FullRepaint=$false
                Mode=$modes[$sequence];Sequence=$sequence + 1
            }
            if (-not $FesTermOnly) {
                [pscustomobject]@{
                    Host='windows-terminal';Workload=$workload;FullRepaint=$false
                    Mode='current';Sequence=$sequence + 1
                }
            }
        }
    }
)
if ($IncludeFullRepaintControl) {
    $runs += [pscustomobject]@{
        Host='windows-terminal-full-repaint';Workload='localized';FullRepaint=$true
        Mode='current';Sequence=1
    }
}
$executableHash = (Get-FileHash -LiteralPath $FesTerm -Algorithm SHA256).Hash
$producerHash = (Get-FileHash -LiteralPath $child -Algorithm SHA256).Hash
[pscustomobject]@{
    SchemaVersion=1;SourceSha=$source;DirtyChanges=$dirty;Configuration='release'
    FesTermSha256=$executableHash;ProducerSha256=$producerHash
    DriverSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    DeclaredUtc=[DateTime]::UtcNow.ToString('o');Modes=$modes;Runs=$runs
    InitialHostCopySetting=$null;InitialRetainedCompositionSetting=$null
    CompositionPolicy='automatic-warp'
    SourceAttribution='driver-built-clean-checkout'
    ExternalGuardStopFile=$GuardStopFile
    OverlayControl=[bool]$OverlayControl;SampleSeconds=$SampleSeconds;ProducerFrames=$ProducerFrames
    LogicalProcessors=[Environment]::ProcessorCount
    CpuAffinity=[Diagnostics.Process]::GetCurrentProcess().ProcessorAffinity.ToInt64()
    OsVersion=[Environment]::OSVersion.Version.ToString()
    Architecture=[Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    CaptureBoundary='External desktop client capture after sampling; not displayed-frame cadence'
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath "$ResultDirectory\manifest.json"
try {
    foreach ($face in $(if ($FesTermOnly) { @() } else { @('Regular','Bold','Italic','BoldItalic') })) {
        $font = "$root\assets\fonts\jetbrains-mono\JetBrainsMonoNL-$face.ttf"
        [TuiComparisonNative]::AddFont($font)
        $fonts.Add($font)
    }
    foreach ($run in $runs) {
        Assert-NativeGuardClear $GuardStopFile
        [FesTermApplicationWindow]::RequireInteractiveDesktop()
        if ((Get-FileHash -LiteralPath $FesTerm -Algorithm SHA256).Hash -ne $executableHash -or
            (Get-FileHash -LiteralPath $child -Algorithm SHA256).Hash -ne $producerHash) {
            throw 'A measured executable changed; the series stops without retry.'
        }
        $name = "$($run.Host)-$($run.Workload)"
        $directory = Join-Path $ResultDirectory $name
        New-Item -ItemType Directory -Path $directory | Out-Null
        $start = "$directory\start"
        $report = "$directory\producer.json"
        $arguments = @('emit:FESTERM-TUI-PROBE-READY',"wait-for-file:$start","tui:$($run.Workload):${ProducerFrames}:100:$report",'spin')
        $isFesTerm = $run.Host -eq 'festerm'
        $executable = if ($isFesTerm) { $FesTerm } else { $WindowsTerminal }
        if ($isFesTerm) {
            $exeJson = ConvertTo-Json -InputObject $child -Compress
            $argsJson = ConvertTo-Json -InputObject $arguments -Compress
            $env:FESTERM_CONFIG_PATH = "$directory\config.toml"
            @"
schema_version = 1
workspace_enabled = true
[[profiles]]
kind = "local"
id = "tui-probe"
executable = $exeJson
arguments = $argsJson
[workspace]
focused_tab_id = "terminal"
[[workspace.tabs]]
kind = "local_session"
id = "terminal"
profile_id = "tui-probe"
[settings]
restore_workspace = true
show_resumable_sessions = false
automatic_update_checks = false
confirm_session_close = false
show_session_details = $(if ($run.Workload -eq 'changing-chrome') { 'true' } else { 'false' })
terminal_font = "jet-brains-mono"
terminal_ligatures = false
"@ | Set-Content -LiteralPath $env:FESTERM_CONFIG_PATH -Encoding utf8
            $env:RUST_LOG = 'festerm=info,festerm::rendering=debug,egui_wgpu::callback_copy=debug,egui_wgpu::retained_ui=debug,warn'
        } else {
            $commandline = (@($child) + $arguments | ForEach-Object { '"' + $_ + '"' }) -join ' '
            $settings = @{
                defaultProfile='{cd2e39d3-56bb-4439-9480-cdc08fe445f6}'
                initialCols=120; initialRows=40
                'experimental.rendering.software'=$true
                'experimental.rendering.forceFullRepaint'=$run.FullRepaint
                disabledProfileSources=@('Windows.Terminal.PowershellCore','Windows.Terminal.Wsl','Windows.Terminal.Azure')
                profiles=@{
                    defaults=@{font=@{face='JetBrains Mono NL';size=10.5};useAcrylic=$false;opacity=100;padding='8'}
                    list=@(@{guid='{cd2e39d3-56bb-4439-9480-cdc08fe445f6}';name='TUI comparison';commandline=$commandline})
                }
            }
            $settings | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath "$settingsDirectory\settings.json" -Encoding utf8
            $settings | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath "$directory\settings.json" -Encoding utf8
        }
        $process = Start-Process -FilePath $executable -WorkingDirectory $root -PassThru -NoNewWindow `
            -RedirectStandardOutput "$directory\stdout.log" -RedirectStandardError "$directory\stderr.log"
        $window = [IntPtr]::Zero
        $forcedResizeRepaints = 0
        $startedUtc = [DateTime]::UtcNow.ToString('o')
        try {
            $deadline = [DateTime]::UtcNow.AddSeconds(25)
            do {
                Start-Sleep -Milliseconds 100
                $process.Refresh()
                if ($process.HasExited) { throw "$name exited during startup; inspect its logs." }
                $window = [FesTermApplicationWindow]::Find($process.Id)
                $geometry = Read-Geometry "$start.geometry.json"
            } while (($window -eq [IntPtr]::Zero -or $null -eq $geometry) -and [DateTime]::UtcNow -lt $deadline)
            [FesTermApplicationWindow]::Activate($window,$process.Id)
            if ($null -eq $geometry) { throw "$name did not publish PTY geometry." }
            if ($process.Path -ne $executable) { throw "$name process identity did not match its isolated executable." }
            Start-Sleep -Seconds 2
            for ($attempt=0; $attempt -lt 12; $attempt++) {
                $geometry = Read-Geometry "$start.geometry.json"
                if ($geometry.columns -eq 120 -and $geometry.rows -eq 40) { break }
                $metrics = [TuiComparisonNative]::Metrics($window)
                $scale = $metrics[4]/96.0
                $width = $metrics[0]+[int][Math]::Round((120-$geometry.columns)*8.4*$scale)
                $height = $metrics[1]+[int][Math]::Round((40-$geometry.rows)*18.4*$scale)
                [pscustomobject]@{Attempt=$attempt;Geometry=$geometry;Metrics=$metrics;Requested=@($width,$height)} |
                    ConvertTo-Json -Compress -Depth 4 | Add-Content -LiteralPath "$directory\geometry.jsonl"
                [TuiComparisonNative]::Resize($window,$width,$height)
                Start-Sleep -Seconds 2
                $settled = Read-Geometry "$start.geometry.json"
                if ($settled.columns -ne 120 -or $settled.rows -ne 40) {
                    [TuiComparisonNative]::Refresh($window)
                    $forcedResizeRepaints++
                    Start-Sleep -Milliseconds 500
                }
            }
            $geometry = Read-Geometry "$start.geometry.json"
            if ($geometry.columns -ne 120 -or $geometry.rows -ne 40) { throw "$name could not establish a 120x40 PTY." }
            $fontFace = if ($isFesTerm) { 'JetBrains Mono NL (bundled, ligatures disabled)' } else { Terminal-Font $window }
            $producerProcess = Get-Process -Id $geometry.pid
            if ($producerProcess.Path -ne $child) { throw "$name did not run the controlled workload producer." }
            $metrics = [TuiComparisonNative]::Metrics($window)
            [TuiComparisonNative]::Resize($window,$metrics[0],$metrics[1])
            if ($OverlayControl) {
                [FesTermApplicationWindow]::RequireResponsive($window,$process.Id)
                if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) {
                    throw 'The overlay control lost foreground before its test-owned input.'
                }
                $shell = New-Object -ComObject WScript.Shell
                $shell.SendKeys('^+p')
                Start-Sleep -Seconds 1
                $element = [Windows.Automation.AutomationElement]::FromHandle($window)
                $palette = $element.FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::NameProperty, 'Command Palette'))
                if ($palette.Count -eq 0) { throw 'The controlled command palette did not open.' }
            }
            $inputBefore = [FesTermApplicationWindow]::LastInputTick()
            Start-Sleep -Seconds 5
            Assert-NativeGuardClear $GuardStopFile
            $metrics = [TuiComparisonNative]::Metrics($window)
            if ($metrics[5] -lt $metrics[7] -or $metrics[6] -lt $metrics[8] -or
                $metrics[5]+$metrics[0] -gt $metrics[9] -or $metrics[6]+$metrics[1] -gt $metrics[10]) {
                throw "$name did not settle fully inside the monitor work area."
            }
            New-Item -ItemType File -Path $start | Out-Null
            Start-Sleep -Seconds 5
            Assert-NativeGuardClear $GuardStopFile
            [FesTermApplicationWindow]::RequireInteractiveDesktop()
            [FesTermApplicationWindow]::RequireResponsive($window,$process.Id)
            $warmupGuard = Get-NativeWarmupGuard -InputBefore $inputBefore `
                -InputObserved ([FesTermApplicationWindow]::LastInputTick()) `
                -ExpectedWindow $window.ToInt64() `
                -ObservedWindow ([FesTermApplicationWindow]::GetForegroundWindow().ToInt64()) `
                -ExpectedMetrics $metrics -ObservedMetrics ([TuiComparisonNative]::Metrics($window))
            $warmupGuard | ConvertTo-Json -Depth 4 |
                Set-Content -LiteralPath "$directory\warmup-guard.json"
            if ($warmupGuard.InputChanged -or $warmupGuard.ForegroundChanged -or $warmupGuard.GeometryChanged) {
                throw "$name warmup input/window guard failed (input=$($warmupGuard.InputChanged), foreground=$($warmupGuard.ForegroundChanged), geometry=$($warmupGuard.GeometryChanged)); stopped before sampling without retry."
            }
            $beforeFrames = if ($isFesTerm) { Frame-Count $directory } else { $null }
            $nativeBefore = if ($isFesTerm) { Frame-Count $directory 'direct2d_frame_number' } else { $null }
            $copyBefore = if ($isFesTerm -and $hostCopy) { Frame-Count $directory 'final_callback_copy_frame' } else { $null }
            $retainedBefore = if ($isFesTerm -and $retainedComposition) { Frame-Count $directory 'retained_ui_reused_frames' } else { $null }
            $rebuiltBefore = if ($isFesTerm -and $retainedComposition) { Frame-Count $directory 'retained_ui_rebuilt_frames' } else { $null }
            $process.Refresh()
            $producerProcess.Refresh()
            $before = $process.TotalProcessorTime.TotalSeconds
            $producerBefore = $producerProcess.TotalProcessorTime.TotalSeconds
            $clock = [Diagnostics.Stopwatch]::StartNew()
            $sampleStarted = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $intervals = @()
            $lastTime = 0.0
            $lastCpu = $before
            $valid = $true
            $inputChanged = $false
            $foregroundChanged = $false
            $geometryChanged = $false
            do {
                Start-Sleep -Milliseconds 1000
                Assert-NativeGuardClear $GuardStopFile
                $process.Refresh()
                if ($process.HasExited) { throw "$name exited during measurement." }
                [FesTermApplicationWindow]::RequireInteractiveDesktop()
                $elapsed = $clock.Elapsed.TotalSeconds
                $cpu = $process.TotalProcessorTime.TotalSeconds
                $currentMetrics = [TuiComparisonNative]::Metrics($window)
                $inputChanged = $inputChanged -or [FesTermApplicationWindow]::LastInputTick() -ne $inputBefore
                $foreground = [FesTermApplicationWindow]::GetForegroundWindow()
                $foregroundChanged = $foregroundChanged -or $foreground -ne $window
                $geometryChanged = $geometryChanged -or ($metrics -join ',') -ne ($currentMetrics -join ',')
                $valid = -not ($inputChanged -or $foregroundChanged -or $geometryChanged)
                $intervals += [pscustomobject]@{
                    Seconds=$elapsed;CpuPercent=100*($cpu-$lastCpu)/($elapsed-$lastTime)/[Environment]::ProcessorCount
                    DesktopActive=$true;InputChanged=$inputChanged;Foreground=$foreground.ToInt64();Metrics=$currentMetrics
                    InputTick=[FesTermApplicationWindow]::LastInputTick()
                    WorkingSetBytes=$process.WorkingSet64;PrivateBytes=$process.PrivateMemorySize64
                    PeakWorkingSetBytes=$process.PeakWorkingSet64;PeakPagedBytes=$process.PeakPagedMemorySize64
                    Handles=$process.HandleCount;Threads=$process.Threads.Count
                }
                $intervals[-1] | ConvertTo-Json -Compress -Depth 5 |
                    Add-Content -LiteralPath "$directory\intervals.jsonl"
                $lastCpu=$cpu; $lastTime=$elapsed
                if (-not $valid) { break }
            } while ($elapsed -lt $SampleSeconds)
            $afterFrames = if ($isFesTerm) { Frame-Count $directory } else { $null }
            $nativeAfter = if ($isFesTerm) { Frame-Count $directory 'direct2d_frame_number' } else { $null }
            $copyAfter = if ($isFesTerm -and $hostCopy) { Frame-Count $directory 'final_callback_copy_frame' } else { $null }
            $retainedAfter = if ($isFesTerm -and $retainedComposition) { Frame-Count $directory 'retained_ui_reused_frames' } else { $null }
            $rebuiltAfter = if ($isFesTerm -and $retainedComposition) { Frame-Count $directory 'retained_ui_rebuilt_frames' } else { $null }
            $producerProcess.Refresh()
            Assert-NativeGuardClear $GuardStopFile
            $producerCpu = $producerProcess.TotalProcessorTime.TotalSeconds-$producerBefore
            [FesTermApplicationWindow]::RequireInteractiveDesktop()
            [FesTermApplicationWindow]::RequireResponsive($window,$process.Id)
            $inputChanged = $inputChanged -or [FesTermApplicationWindow]::LastInputTick() -ne $inputBefore
            $foregroundChanged = $foregroundChanged -or [FesTermApplicationWindow]::GetForegroundWindow() -ne $window
            $geometryChanged = $geometryChanged -or
                ($metrics -join ',') -ne ([TuiComparisonNative]::Metrics($window) -join ',')
            $valid = -not ($inputChanged -or $foregroundChanged -or $geometryChanged)
            $deadline = [DateTime]::UtcNow.AddSeconds(30)
            while (-not (Test-Path -LiteralPath $report) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
            if (-not (Test-Path -LiteralPath $report)) { throw "$name producer did not complete." }
            [FesTermApplicationWindow]::RequireInteractiveDesktop()
            $producer = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
            if ($producer.frames -ne $ProducerFrames -or $producer.geometry.columns -ne 120 -or $producer.geometry.rows -ne 40 -or
                $producer.pid -ne $producerProcess.Id -or $producer.workload -ne $run.Workload -or
                $producer.interval_ms -ne 100 -or $producer.completed_ms.Count -ne $ProducerFrames -or $producer.bytes -le 0) {
                throw "$name producer count/geometry did not match."
            }
            for ($index=0; $index -lt $ProducerFrames; $index++) {
                if ($producer.completed_ms[$index] -lt ($index+1)*100 -or
                    ($index -gt 0 -and $producer.completed_ms[$index] -lt $producer.completed_ms[$index-1])) {
                    throw "$name producer timestamps were invalid."
                }
            }
            $peer = @($results | Where-Object Workload -EQ $run.Workload)
            if ($peer.Count -gt 0 -and $peer[0].Producer.bytes -ne $producer.bytes) { throw "$name workload byte counts differed." }
            if ($peer.Count -gt 0 -and $peer[0].Metrics[4] -ne $metrics[4]) { throw "$name DPI differed from its comparison peer." }
            $sameApplicationPeers = @($peer | Where-Object Host -EQ $run.Host)
            if ($sameApplicationPeers.Count -gt 0 -and
                ($sameApplicationPeers[0].Metrics[2..4] -join ',') -ne ($metrics[2..4] -join ',')) {
                throw "$name physical client size differed from its comparison peer."
            }
            if ($isFesTerm) {
                if (-not (Select-String -LiteralPath "$directory\stdout.log","$directory\stderr.log" -Pattern 'device_type=Cpu' -Quiet)) {
                    throw "$name did not select a CPU renderer."
                }
                if (Select-String -LiteralPath "$directory\stdout.log","$directory\stderr.log" -Pattern 'unsupported Direct2D frame|disabling experimental Direct2D|Direct2D initialization failed|Direct2D state poisoned' -Quiet) {
                    throw "$name fell back from Direct2D."
                }
                if ($run.Workload -ne 'quiet' -and $nativeAfter -le $nativeBefore) { throw "$name built no native terminal frames." }
                if ($hostCopy -and -not $OverlayControl -and $run.Workload -ne 'quiet' -and $copyAfter -le $copyBefore) {
                    throw "$name did not exercise final-target host copies."
                }
                if ($retainedComposition -and -not $OverlayControl -and
                    $run.Workload -notin @('quiet','changing-chrome') -and $retainedAfter -le $retainedBefore) {
                    throw "$name reused no window prefixes during the measured interval."
                }
                if ($retainedComposition -and -not $OverlayControl -and
                    $run.Workload -eq 'changing-chrome' -and $rebuiltAfter -le $rebuiltBefore) {
                    throw "$name did not rebuild its changing chrome."
                }
                if ($OverlayControl -and $hostCopy -and $copyAfter -ne $copyBefore) {
                    throw "$name overlay unexpectedly allowed a final terminal copy."
                }
                if ($OverlayControl -and $retainedComposition -and
                    ($retainedAfter -ne $retainedBefore -or $rebuiltAfter -ne $rebuiltBefore)) {
                    throw "$name overlay unexpectedly retained an ineligible prefix."
                }
            }
            $result = [pscustomobject]@{
                Host=$run.Host;Workload=$run.Workload;DesktopActive=$true;Status=$(if($valid){'valid'}else{'invalid-input-or-window'})
                Mode=$run.Mode;Sequence=$run.Sequence;SourceSha=$source;StartedUtc=$startedUtc
                OverlayControl=[bool]$OverlayControl
                ProcessId=$process.Id;Window=$window.ToInt64();Metrics=$metrics
                CpuPercent=100*($cpu-$before)/$elapsed/[Environment]::ProcessorCount
                ProducerCpuPercent=100*$producerCpu/$elapsed/[Environment]::ProcessorCount
                Font=$fontFace;Executable=$executable
                ExecutableSha256=(Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
                ProducerSha256=(Get-FileHash -LiteralPath $child -Algorithm SHA256).Hash
                SampleSeconds=$elapsed
                SampleStartedUnixMs=$sampleStarted
                LogicalProcessors=[Environment]::ProcessorCount
                WorkingSetBytes=$process.WorkingSet64
                PrivateBytes=$process.PrivateMemorySize64
                ForcedResizeRepaints=$forcedResizeRepaints
                GuiFramesPerSecond=$(if($isFesTerm){($afterFrames-$beforeFrames)/$elapsed}else{$null})
                Direct2DFramesPerSecond=$(if($isFesTerm){($nativeAfter-$nativeBefore)/$elapsed}else{$null})
                HostCopyRequested=($isFesTerm -and $hostCopy)
                HostCopySetting=$null
                HostCopyFramesPerSecond=$(if($isFesTerm -and $hostCopy){($copyAfter-$copyBefore)/$elapsed}else{$null})
                RetainedCompositionRequested=($isFesTerm -and $retainedComposition)
                RetainedCompositionSetting=$null
                RetainedUiFramesPerSecond=$(if($isFesTerm -and $retainedComposition){($retainedAfter-$retainedBefore)/$elapsed}else{$null})
                RetainedUiRebuildsPerSecond=$(if($isFesTerm -and $retainedComposition){($rebuiltAfter-$rebuiltBefore)/$elapsed}else{$null})
                Producer=$producer;Intervals=$intervals
                InputChanged=$inputChanged;ForegroundChanged=$foregroundChanged;GeometryChanged=$geometryChanged
            }
            $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath "$directory\result.json"
            $results.Add($result)
            $results | ConvertTo-Json -Depth 9 | Set-Content -LiteralPath "$ResultDirectory\results.json"
            $result | Select-Object Host,Workload,Status,CpuPercent,GuiFramesPerSecond | Format-Table
            if (-not $valid) { throw "$name native observation was invalid; no retry is performed." }
            if ($CaptureFinalFrame) {
                Save-FesTermWindowCapture -Window $window -ProcessId $process.Id -Path "$directory\desktop-final.png" |
                    ConvertTo-Json -Depth 5 | Set-Content -LiteralPath "$directory\desktop-final.json"
            }
        } catch {
            [pscustomobject]@{
                Status=$(if ($_.ToString() -match 'External CPU guard failed') { 'invalid-external-cpu' }
                    elseif ($_.ToString() -match 'guard failed|observation was invalid') { 'invalid-input-or-window' }
                    else { 'failed' })
                Error=$_.ToString();StartedUtc=$startedUtc;FinishedUtc=[DateTime]::UtcNow.ToString('o')
            } | ConvertTo-Json |
                Set-Content -LiteralPath "$directory\failure.json"
            throw
        } finally {
            $process.Refresh()
            if ($window -ne [IntPtr]::Zero -and [FesTermApplicationWindow]::Matches($window,$process.Id)) {
                $cleanup = Close-FesTermOwnedApplication -Process $process -Window $window -ConfirmQuit:$isFesTerm
            } elseif (-not $process.HasExited) {
                Stop-Process -Id $process.Id
                if (-not $process.WaitForExit(5000)) { throw "$name did not terminate during owned-process cleanup." }
                $cleanup = [pscustomobject]@{ProcessId=$process.Id;Forced=$true;NormalExit=$false}
            } else {
                $cleanup = [pscustomobject]@{
                    ProcessId=$process.Id;Forced=$false;NormalExit=($process.ExitCode -eq 0);ExitCode=$process.ExitCode
                }
            }
            $cleanup | Add-Member -NotePropertyName FinishedUtc -NotePropertyValue ([DateTime]::UtcNow.ToString('o'))
            $cleanup | ConvertTo-Json -Depth 6 |
                Set-Content -LiteralPath "$directory\cleanup.json"
            if (-not $cleanup.NormalExit) { throw "$name required forced or incomplete cleanup; the series stops." }
        }
    }
} finally {
    $env:FESTERM_CONFIG_PATH=$oldConfig
    $env:RUST_LOG=$oldLog
    $cleanupErrors = @()
    foreach ($font in $fonts) {
        try { [TuiComparisonNative]::RemoveFont($font) }
        catch { $cleanupErrors += $_.ToString() }
    }
    if (-not $FesTermOnly -and (Test-Path -LiteralPath "$settingsDirectory\settings.json")) {
        Remove-Item -LiteralPath "$settingsDirectory\settings.json"
    }
    if ($cleanupErrors.Count -gt 0) { throw ($cleanupErrors -join "`n") }
}
