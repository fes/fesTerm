<#
.SYNOPSIS
Opt-in, owned-window WARP Markdown scroll profiling.
.DESCRIPTION
Requires PowerShell 7, an interactive Windows desktop, a staged release
festerm.exe and an external JSON receipt with exactly these identity fields:
schema_version: 1, executable_sha256: 64 hexadecimal digits,
source_revision: full git HEAD commit ID, source_tree: full HEAD^{tree} ID.
The receipt is build provenance supplied by the builder, not a signature.
The source must be completely clean, including untracked files. Stage the
receipt and output outside SourceRoot (or in an already ignored directory).
No build, PTY, native smoke or keyboard input is used. AllowGlobalPointer
explicitly permits activating the owned window and moving/restoring the real
cursor; wheel messages still target only that HWND. Without this option,
Windows may clear synthetic hover and the actual-offset guard will fail.
Cases contain one 20/300-line Rust fence, one 30-body-row table, or 20 prose
paragraphs. Five five-wheel bursts alternate down/up/down/up/down. Each drain
requires at least 500ms and 200ms without observed motion, bounded to 3s;
all drains are included in the measured CPU/wall interval.
The helper, executable and receipt are copied into a newly created
user/SYSTEM-only non-reparse runtime directory beneath LocalApplicationData
and held under write/delete-denying read handles throughout every case. Each
generated fixture, log, result and the recipe are likewise sealed for the
complete run. Completed case directories are published to OutputDirectory
only after profiling. Recipe, fixture and receipt SHA256 values are checked at
launch and after exit; source and executable are revalidated after exit as
well. Results contain no artifact paths. Frame timing percentiles cover only
the measured root-frame range and are host-return durations, not GPU completion
or physical display latency.
Example:
 $env:FESTERM_RUN_OPTIONAL_VALIDATION='1'
 .\scripts\check-windows-markdown-scrolling.ps1 -Executable C:\stage\festerm.exe `
   -Receipt C:\stage\receipt.json -SourceRoot C:\src\fesTerm `
   -OutputDirectory C:\evidence\scroll-001 -Case all
HelperOnly defines pure functions for deterministic tests; it never runs a
native probe or bypasses the native probe's opt-in / clean-source checks.
#>
[CmdletBinding()]
param(
    [string] $Executable,
    [string] $Receipt,
    [string] $SourceRoot,
    [string] $OutputDirectory,
    [ValidateSet('rust20', 'rust300', 'table30', 'prose', 'all')]
    [string] $Case = 'all',
    [switch] $HelperOnly,
    [switch] $AllowGlobalPointer
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using Microsoft.Win32.SafeHandles;
using System.Runtime.InteropServices;

public sealed class FesTermDirectoryLock : IDisposable {
    private const uint FILE_READ_ATTRIBUTES = 0x80;
    private const uint FILE_SHARE_READ = 0x1;
    private const uint FILE_SHARE_WRITE = 0x2;
    private const uint OPEN_EXISTING = 3;
    private const uint FILE_FLAG_BACKUP_SEMANTICS = 0x02000000;
    private const uint FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000;
    private const uint FILE_ATTRIBUTE_REPARSE_POINT = 0x400;
    private const int FileAttributeTagInfo = 9;

    [StructLayout(LayoutKind.Sequential)]
    private struct AttributeTagInfo {
        public uint FileAttributes;
        public uint ReparseTag;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFile(
        string name, uint access, uint share, IntPtr security, uint creation,
        uint flags, IntPtr template);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandleEx(
        SafeFileHandle file, int informationClass, out AttributeTagInfo information, uint size);

    private SafeFileHandle handle;

    private FesTermDirectoryLock(SafeFileHandle handle) {
        this.handle = handle;
    }

    public static FesTermDirectoryLock Open(string path) {
        var handle = CreateFile(path, FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE,
            IntPtr.Zero, OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, IntPtr.Zero);
        if (handle.IsInvalid) throw new Win32Exception();
        AttributeTagInfo information;
        if (!GetFileInformationByHandleEx(handle, FileAttributeTagInfo, out information,
                (uint)Marshal.SizeOf<AttributeTagInfo>())) {
            var error = new Win32Exception();
            handle.Dispose();
            throw error;
        }
        if ((information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0) {
            handle.Dispose();
            throw new InvalidOperationException("Profiling runtime directories must not be reparse points.");
        }
        return new FesTermDirectoryLock(handle);
    }

    public void Dispose() {
        if (handle != null) {
            handle.Dispose();
            handle = null;
        }
    }
}
'@

function Get-MarkdownScrollSamples {
    param([string] $Text)
    # Strip ANSI decoration before parsing invariant diagnostic fields only.
    $clean = $Text -replace '\x1b\[[0-9;]*m', ''
    $number = '[-+]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][-+]?\d+)?'
    foreach ($line in ($clean -split "`n")) {
        if ($line -notmatch 'festerm::markdown_scroll' -or
            $line -notmatch 'Markdown (?:viewer|preview pane) scroll state') { continue }
        $values = @()
        foreach ($field in @('offset_y', 'content_height', 'viewport_height')) {
            if ($line -notmatch "\bmarkdown_preview_$field=($number)(?=\s|$)") {
                throw "Malformed Markdown scroll diagnostic: $field."
            }
            $value = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
            if (-not [double]::IsFinite($value) -or $value -lt 0) {
                throw "Invalid Markdown scroll diagnostic: $field."
            }
            $values += $value
        }
        if ($values[2] -le 0 -or $values[0] -gt [Math]::Max(0, $values[1] - $values[2]) + 1) {
            throw 'Impossible Markdown scroll geometry.'
        }
        $rect = @()
        foreach ($edge in @('left', 'top', 'right', 'bottom')) {
            if ($line -match "\bmarkdown_preview_$edge=($number)(?=\s|$)") {
                $coordinate = [double]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture)
                if (-not [double]::IsFinite($coordinate)) { throw 'Invalid preview hit region.' }
                $rect += $coordinate
            }
        }
        if ($rect.Count -ne 0 -and $rect.Count -ne 4) { throw 'Incomplete preview hit region.' }
        [pscustomobject]@{ Offset = $values[0]; Content = $values[1]; Viewport = $values[2]; Rect = $rect }
    }
}

function Get-MarkdownPhysicalClientPoint {
    param($Sample, [int] $Dpi, [int] $Width, [int] $Height)
    if ($Dpi -le 0 -or $Width -le 0 -or $Height -le 0 -or $Sample.Rect.Count -ne 4) {
        throw 'A source-proven preview hit region and positive client geometry are required.'
    }
    $scale = $Dpi / 96.0
    $left = [Math]::Max(0, $Sample.Rect[0] * $scale)
    $top = [Math]::Max(0, $Sample.Rect[1] * $scale)
    $right = [Math]::Min($Width, $Sample.Rect[2] * $scale)
    $bottom = [Math]::Min($Height, $Sample.Rect[3] * $scale)
    if ($right - $left -lt 4 -or $bottom - $top -lt 4) { throw 'Preview is not visibly targetable.' }
    return @([int][Math]::Round(($left + $right) / 2), [int][Math]::Round(($top + $bottom) / 2))
}

function Assert-MarkdownScrollProgress {
    param([object[]] $Samples, [int] $PreviousCount, [double] $PreviousOffset,
        [ValidateSet(-1, 1)][int] $Direction = 1)
    if ($Samples.Count -le $PreviousCount) {
        throw 'No later scroll diagnostic; targeted PostMessage may have been ignored.'
    }
    $last = $PreviousOffset
    foreach ($sample in $Samples[$PreviousCount..($Samples.Count - 1)]) {
        if (($sample.Offset - $last) * $Direction -lt 0) {
            throw 'Scroll offset moved against the requested direction.'
        }
        $last = $sample.Offset
    }
    if (($last - $PreviousOffset) * $Direction -le 0.05) {
        throw 'No actual offset change; targeted PostMessage may have been ignored or the document reached its end.'
    }
    return $last
}

function Get-MarkdownScrollDrainState {
    param([object[]] $Samples, [int] $PreviousCount, [double] $PreviousOffset,
        [double] $LastChangeMs, [double] $ElapsedMs)
    $last = $PreviousOffset
    for ($i = $PreviousCount; $i -lt $Samples.Count; $i++) {
        if ($Samples[$i].Offset -ne $last) { $LastChangeMs = $ElapsedMs }
        $last = $Samples[$i].Offset
    }
    $settled = $ElapsedMs -ge 500 -and $ElapsedMs - $LastChangeMs -ge 200
    [pscustomobject]@{
        Count = $Samples.Count; Offset = $last; LastChangeMs = $LastChangeMs
        Settled = $settled; TimedOut = ($ElapsedMs -ge 3000 -and -not $settled)
    }
}

function Assert-MarkdownWarpAdapter {
    param([string] $Text)
    $clean = $Text -replace '\x1b\[[0-9;]*m', ''
    foreach ($line in ($clean -split "`n")) {
        if ($line -match 'wgpu selected a rendering adapter' -and
            $line -match '\badapter_name="?Microsoft Basic Render Driver"?(?=\s+\w+=|\s*$)' -and
            $line -match '\bbackend=Dx12(?=\s|$)' -and
            $line -match '\bdevice_type=Cpu(?=\s|$)') { return }
    }
    throw 'Missing actual Microsoft Basic Render Driver / Dx12 / Cpu adapter log proof.'
}

function Get-MarkdownFrameCounters {
    param([string] $Text)
    $root = @([regex]::Matches($Text, 'root_paint completed_frames=(\d+)'))
    $retained = @([regex]::Matches($Text,
        'retained_ui_reused_frames=(\d+) retained_ui_rebuilt_frames=(\d+)[^\r\n]*'))
    $optional = @{}
    foreach ($field in @('partial_reused_frames', 'cached_prefix_jobs', 'cached_signature_jobs')) {
        $optional[$field] = $null
        if ($retained.Count -and $retained[-1].Value -match "\bretained_ui_$field=(\d+)(?=\s|$)") {
            $optional[$field] = [long]$Matches[1]
        }
    }
    [pscustomobject]@{
        RootSamples = $root.Count
        RootCompleted = $(if ($root.Count) { [long]$root[-1].Groups[1].Value } else { $null })
        RetentionSamples = $retained.Count
        Reused = $(if ($retained.Count) { [long]$retained[-1].Groups[1].Value } else { $null })
        Rebuilt = $(if ($retained.Count) { [long]$retained[-1].Groups[2].Value } else { $null })
        PartialReused = $optional['partial_reused_frames']
        CachedPrefixJobs = $optional['cached_prefix_jobs']
        CachedSignatureJobs = $optional['cached_signature_jobs']
    }
}

function Get-MarkdownHostFrameTiming {
    param([string] $Text, [long] $BeforeFrame, [long] $AfterFrame)
    if ($BeforeFrame -lt 0 -or $AfterFrame -lt $BeforeFrame) { throw 'Invalid measured root-frame range.' }
    $fields = @('primitives', 'prepare_ms', 'acquire_ms', 'encode_ms', 'submit_ms', 'present_ms', 'total_ms')
    $values = @{}
    foreach ($field in $fields) { $values[$field] = [Collections.Generic.List[double]]::new() }
    foreach ($line in ($Text -split "`n")) {
        if ($line -notmatch 'egui_wgpu::frame_timing' -or
            $line -notmatch '\broot_paint completed_frames=(\d+)(?=\s|$)') { continue }
        $frame = [long]$Matches[1]
        if ($frame -le $BeforeFrame -or $frame -gt $AfterFrame) { continue }
        foreach ($field in $fields) {
            if ($line -notmatch "\b$field=(\S+)") { throw "Missing measured root-frame field: $field." }
            $token = $Matches[1]
            $value = 0.0
            if (-not [double]::TryParse($token, [Globalization.NumberStyles]::Float,
                [Globalization.CultureInfo]::InvariantCulture, [ref]$value) -or
                -not [double]::IsFinite($value) -or $value -lt 0 -or
                ($field -eq 'primitives' -and $token -notmatch '^\d+$')) {
                throw "Invalid measured root-frame field: $field."
            }
            $values[$field].Add($value)
        }
    }
    $summary = [ordered]@{
        samples = $values['primitives'].Count
        root_frame_exclusive_start = $BeforeFrame; root_frame_inclusive_end = $AfterFrame
        percentile_method = 'nearest rank'
        limitation = 'Host-return durations only; not completed GPU work, physical display latency, or per-message present acknowledgments. Raw primitive counts are the logged root_paint primitives, not inferred rendering work.'
    }
    foreach ($field in $fields) {
        $sorted = @($values[$field] | Sort-Object)
        $summary[$field] = if ($sorted.Count) {
            @{
                p50 = $sorted[[int][Math]::Ceiling($sorted.Count * 0.50) - 1]
                p95 = $sorted[[int][Math]::Ceiling($sorted.Count * 0.95) - 1]
            }
        } else { @{ p50 = $null; p95 = $null } }
    }
    return [pscustomobject]$summary
}

function Get-MarkdownArtifactIdentity {
    param([string] $RecipePath, [string] $FixturePath, [string] $ReceiptPath)
    [pscustomobject]@{
        recipe_sha256 = (Get-FileHash -LiteralPath $RecipePath -Algorithm SHA256).Hash.ToLowerInvariant()
        fixture_sha256 = (Get-FileHash -LiteralPath $FixturePath -Algorithm SHA256).Hash.ToLowerInvariant()
        receipt_sha256 = (Get-FileHash -LiteralPath $ReceiptPath -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Assert-MarkdownArtifactIdentity {
    param($Expected, $Actual)
    foreach ($field in @('recipe_sha256', 'fixture_sha256', 'receipt_sha256')) {
        if ($Expected.$field -notmatch '^[0-9a-fA-F]{64}$' -or $Actual.$field -ne $Expected.$field) {
            throw "Profiling artifact identity changed: $field."
        }
    }
}

function Invoke-MarkdownCleanup {
    param([Collections.IDictionary] $Result, [object[]] $Steps)
    $failures = @()
    foreach ($step in $Steps) {
        try { & $step.Action $step | Out-Null }
        catch {
            # Exception messages may contain personal paths; publish only stage/type.
            $failures += [pscustomobject]@{ stage = $step.Name; exception_type = $_.Exception.GetType().Name }
        }
    }
    $Result['cleanup_errors'] = $failures
    if ($failures.Count) { $Result['status'] = 'failed' }
}

function ConvertTo-MarkdownPackedPoint {
    param([int] $X, [int] $Y)
    if ($X -lt -32768 -or $X -gt 32767 -or $Y -lt -32768 -or $Y -gt 32767) {
        throw 'Wheel coordinates exceed signed Win32 message range.'
    }
    # Sign-extend the packed LPARAM on 64-bit hosts.
    $packed = ([long]($X -band 65535)) -bor (([long]($Y -band 65535)) -shl 16)
    if ($packed -ge 2147483648) { $packed -= 4294967296 }
    return [IntPtr]::new($packed)
}

function New-MarkdownScrollFixture {
    param([ValidateSet('rust20', 'rust300', 'table30', 'prose')][string] $Name)
    $parts = [Collections.Generic.List[string]]::new()
    $parts.Add("# Synthetic scrolling fixture`n")
    if ($Name.StartsWith('rust')) {
        $count = if ($Name -eq 'rust20') { 20 } else { 300 }
        $parts.Add('```rust')
        for ($i = 0; $i -lt $count; $i++) {
            $suffix = '{0:D3}' -f $i
            $parts.Add("const VALUE_${suffix}: usize = $i;")
        }
        $parts.Add('```')
    } elseif ($Name -eq 'table30') {
        $parts.Add("| Column | Value | Note |`n| --- | --- | --- |")
        for ($row = 0; $row -lt 30; $row++) {
            $parts.Add("| Row $row | 12345 | Synthetic bounded table content |")
        }
    } else {
        for ($i = 0; $i -lt 20; $i++) {
            $parts.Add(('Synthetic paragraph for scrolling with **bold**, *emphasis*, and `inline code`. ' * 3) + "`n")
        }
    }
    return $parts -join "`n"
}

function Assert-MarkdownSourceReceipt {
    param([string] $Binary, [string] $ReceiptPath, [string] $Root, $ExpectedIdentity = $null)
    $identity = Get-Content -LiteralPath $ReceiptPath -Raw | ConvertFrom-Json
    if ($identity.schema_version -ne 1 -or
        $identity.executable_sha256 -notmatch '^[0-9a-fA-F]{64}$' -or
        $identity.source_revision -notmatch '^[0-9a-fA-F]{40,64}$' -or
        $identity.source_tree -notmatch '^[0-9a-fA-F]{40,64}$') { throw 'Invalid build receipt schema.' }
    $revision = & git -C $Root rev-parse --verify HEAD
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve source HEAD.' }
    $tree = & git -C $Root rev-parse --verify 'HEAD^{tree}'
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve source tree.' }
    $status = @(& git -C $Root status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $status.Count -ne 0) {
        throw 'Native profiling requires a clean frozen source commit, including untracked files.'
    }
    $hash = (Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($hash -ne $identity.executable_sha256 -or $revision -ne $identity.source_revision -or
        $tree -ne $identity.source_tree) { throw 'Build receipt does not match executable and clean git source.' }
    if ($null -ne $ExpectedIdentity -and
        ($hash -ne $ExpectedIdentity.executable_sha256 -or $revision -ne $ExpectedIdentity.source_revision -or
            $tree -ne $ExpectedIdentity.source_tree)) { throw 'Executable or frozen source identity changed during profiling.' }
    return [pscustomobject]@{ executable_sha256 = $hash; source_revision = $revision; source_tree = $tree }
}

function Open-MarkdownImmutableArtifact {
    param([string] $Path)
    Assert-MarkdownNonReparsePath $Path
    return [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
}

function Assert-MarkdownNonReparsePath {
    param([string] $Path)
    $item = Get-Item -LiteralPath $Path -Force
    while ($null -ne $item) {
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Profiling artifacts and their parent directories must not be reparse points.'
        }
        $item = $item.Parent
    }
}

function Copy-MarkdownImmutableArtifact {
    param([string] $Source, [string] $Destination)
    $sourceLock = Open-MarkdownImmutableArtifact $Source
    try {
        $destinationLock = [IO.File]::Open(
            $Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
        try {
            $sourceLock.CopyTo($destinationLock)
            $destinationLock.Flush($true)
            $destinationLock.Position = 0
            return $destinationLock
        } catch {
            $destinationLock.Dispose()
            throw
        }
    } finally {
        $sourceLock.Dispose()
    }
}

function Protect-MarkdownPrivateDirectory {
    param([string] $Path)
    $owner = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $system = [Security.Principal.SecurityIdentifier]::new(
        [Security.Principal.WellKnownSidType]::LocalSystemSid, $null)
    $acl = [Security.AccessControl.DirectorySecurity]::new()
    $acl.SetOwner($owner)
    $acl.SetAccessRuleProtection($true, $false)
    $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor
        [Security.AccessControl.InheritanceFlags]::ObjectInherit
    foreach ($sid in @($owner, $system)) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            $sid, [Security.AccessControl.FileSystemRights]::FullControl, $inheritance,
            [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow))
    }
    Set-Acl -LiteralPath $Path -AclObject $acl
}

function Assert-MarkdownCommittedHelper {
    param([string] $Path, [string] $Root)
    $expected = & git -C $Root rev-parse --verify 'HEAD:scripts/windows-application-window.ps1'
    if ($LASTEXITCODE -ne 0 -or $expected -notmatch '^[0-9a-fA-F]{40,64}$') {
        throw 'Cannot resolve the committed native profiling helper.'
    }
    $actual = & git -C $Root hash-object `
        '--path=scripts/windows-application-window.ps1' -- $Path
    if ($LASTEXITCODE -ne 0 -or $actual -ne $expected) {
        throw 'Staged native profiling helper does not match the frozen source commit.'
    }
}

if ($HelperOnly) { return }
if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to opt in to native Markdown scroll profiling.'
}
if (-not $IsWindows -or $PSVersionTable.PSVersion.Major -lt 7) { throw 'Windows and PowerShell 7 are required.' }
foreach ($required in @($Executable, $Receipt, $SourceRoot, $OutputDirectory)) {
    if ([string]::IsNullOrWhiteSpace($required)) { throw 'Executable, Receipt, SourceRoot and OutputDirectory are required.' }
}
$Executable = (Resolve-Path -LiteralPath $Executable).Path
$Receipt = (Resolve-Path -LiteralPath $Receipt).Path
$SourceRoot = (Resolve-Path -LiteralPath $SourceRoot).Path
if ([IO.Path]::GetFileName($Executable) -ne 'festerm.exe') { throw 'Supply staged release festerm.exe.' }
$recipePath = $PSCommandPath
$sourceHelper = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot 'windows-application-window.ps1')).Path
$sourceExecutable = $Executable
$sourceReceipt = $Receipt
$publicationDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $publicationDirectory) { throw 'OutputDirectory already exists; evidence is never overwritten.' }
[void](New-Item -ItemType Directory -Path $publicationDirectory)
Protect-MarkdownPrivateDirectory $publicationDirectory
$localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
if ([string]::IsNullOrWhiteSpace($localAppData)) { throw 'Cannot resolve trusted local application data.' }
Assert-MarkdownNonReparsePath $localAppData
$trustedBase = Join-Path $localAppData 'fesTerm-validation'
[void](New-Item -ItemType Directory -Path $trustedBase -Force)
Protect-MarkdownPrivateDirectory $trustedBase
$OutputDirectory = Join-Path $trustedBase ([Guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $OutputDirectory)
Protect-MarkdownPrivateDirectory $OutputDirectory
$artifactDirectory = Join-Path $OutputDirectory '.native-profile-artifacts'
$immutableArtifactLocks = @()
$stagedArtifacts = @()
$evidenceLocks = [Collections.Generic.List[IDisposable]]::new()
$directoryLocks = [Collections.Generic.List[IDisposable]]::new()
$cases = @()
$publicationFailure = $null
try {
    $directoryLocks.Add([FesTermDirectoryLock]::Open($publicationDirectory))
    $directoryLocks.Add([FesTermDirectoryLock]::Open($localAppData))
    $directoryLocks.Add([FesTermDirectoryLock]::Open($trustedBase))
    $directoryLocks.Add([FesTermDirectoryLock]::Open($OutputDirectory))
    [void](New-Item -ItemType Directory -Path $artifactDirectory)
    Protect-MarkdownPrivateDirectory $artifactDirectory
    $directoryLocks.Add([FesTermDirectoryLock]::Open($artifactDirectory))
    $Executable = Join-Path $artifactDirectory 'festerm.exe'
    $Receipt = Join-Path $artifactDirectory 'receipt.json'
    $helperPath = Join-Path $artifactDirectory 'windows-application-window.ps1'
    $stagedArtifacts = @($Executable, $Receipt, $helperPath)
    $immutableArtifactLocks += Copy-MarkdownImmutableArtifact $sourceExecutable $Executable
    $immutableArtifactLocks += Copy-MarkdownImmutableArtifact $sourceReceipt $Receipt
    $immutableArtifactLocks += Copy-MarkdownImmutableArtifact $sourceHelper $helperPath
    $immutableArtifactLocks += Open-MarkdownImmutableArtifact $recipePath
    $recipeSha256 = (Get-FileHash -LiteralPath $recipePath -Algorithm SHA256).Hash.ToLowerInvariant()
    $receiptSha256 = (Get-FileHash -LiteralPath $Receipt -Algorithm SHA256).Hash.ToLowerInvariant()
    $identity = Assert-MarkdownSourceReceipt $Executable $Receipt $SourceRoot
    Assert-MarkdownCommittedHelper $helperPath $SourceRoot
    . $helperPath
    [FesTermApplicationWindow]::RequireInteractiveDesktop()

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class FesTermMarkdownMessages {
    [StructLayout(LayoutKind.Sequential)]
    private struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    private struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool SetWindowPos(IntPtr window, IntPtr after, int x, int y,
        int width, int height, uint flags);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll", SetLastError=true)]
    private static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")]
    private static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")]
    private static extern IntPtr GetAncestor(IntPtr window, uint flags);
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr window);
    public static IntPtr Foreground() { return GetForegroundWindow(); }
    public static uint Owner(IntPtr window) {
        uint pid;
        GetWindowThreadProcessId(window, out pid);
        return pid;
    }
    public static int[] Cursor() {
        Point point;
        if (!GetCursorPos(out point)) throw new Win32Exception();
        return new[] { point.X, point.Y };
    }
    public static void RequireOwnedPoint(IntPtr window, int pid, int x, int y, bool cursor) {
        var root = GetAncestor(WindowFromPoint(new Point { X=x, Y=y }), 2);
        if (GetForegroundWindow() != window || root != window || Owner(window) != (uint)pid)
            throw new InvalidOperationException("Owned foreground window no longer owns the input point.");
        var actual = Cursor();
        if (cursor && (actual[0] != x || actual[1] != y))
            throw new InvalidOperationException("Real cursor moved; aborting rather than overriding user input.");
    }
    public static void MoveOwnedCursor(IntPtr window, int pid, int x, int y) {
        RequireOwnedPoint(window, pid, x, y, false);
        if (!SetCursorPos(x, y)) throw new Win32Exception();
        RequireOwnedPoint(window, pid, x, y, true);
    }
    public static bool RestoreCursor(IntPtr window, IntPtr previousWindow, uint previousPid,
        int x, int y, int targetX, int targetY) {
        var actual = Cursor();
        if (GetForegroundWindow() != window || actual[0] != targetX || actual[1] != targetY)
            return false; // Another desktop interaction takes precedence over restoration.
        if (!SetCursorPos(x, y)) throw new Win32Exception();
        if (previousWindow != IntPtr.Zero && Owner(previousWindow) == previousPid)
            SetForegroundWindow(previousWindow);
        return true;
    }
    public static void Resize(IntPtr window, int width, int height) {
        Rect outer, client;
        if (!GetWindowRect(window, out outer) || !GetClientRect(window, out client))
            throw new Win32Exception();
        // NOMOVE | NOZORDER | NOACTIVATE | NOOWNERZORDER: never change focus.
        if (!SetWindowPos(window, IntPtr.Zero, 0, 0,
            width + outer.Right - outer.Left - client.Right,
            height + outer.Bottom - outer.Top - client.Bottom, 0x216))
            throw new Win32Exception();
    }
    public static void Post(IntPtr window, uint message, IntPtr wparam, IntPtr lparam) {
        if (!PostMessage(window, message, wparam, lparam)) throw new Win32Exception();
    }
}
'@

function Send-MarkdownWindowMessage {
    param([IntPtr] $Window, [uint32] $Message, [IntPtr] $WParam, [IntPtr] $LParam)
    # PostMessage otherwise tags physical coordinates with PowerShell's virtualized DPI context.
    $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
    try { [FesTermMarkdownMessages]::Post($Window, $Message, $WParam, $LParam) }
    finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
}

function Read-MarkdownRawLog {
    param([string] $Path)
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    $reader = [IO.StreamReader]::new($stream)
    try {
        $text = $reader.ReadToEnd()
        # A concurrent write may end halfway through a diagnostic record.
        $end = $text.LastIndexOf("`n")
        if ($end -lt 0) { return '' }
        return $text.Substring(0, $end + 1)
    } finally { $reader.Dispose() }
}

function Assert-MarkdownOwnedInterval {
    $process.Refresh()
    if ($process.HasExited) { throw 'Owned process exited before profiling completed.' }
    if ($lifetime.Elapsed.TotalSeconds -gt 70) { throw 'Owned process profiling deadline exceeded.' }
    [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
    if ($null -ne $pinned -and ($pinned -join ',') -ne
        ([FesTermApplicationWindow]::ClientGeometry($window, $process.Id) -join ',')) {
        throw 'Actual physical client geometry or DPI changed during profiling.'
    }
    if ($pointerControlled) {
        $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
        try {
            [FesTermMarkdownMessages]::RequireOwnedPoint($window, $process.Id, $targetX, $targetY, $true)
        } finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
    }
}

function Restore-MarkdownPointer {
    if (-not $pointerControlled) { return }
    $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
    try {
        $result['pointer_restored'] = [FesTermMarkdownMessages]::RestoreCursor(
            $window, $originalForeground, $originalForegroundPid,
            $originalPointer[0], $originalPointer[1], $targetX, $targetY)
    } finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
    $script:pointerControlled = $false
}

$cases = if ($Case -eq 'all') { @('rust20', 'rust300', 'table30', 'prose') } else { @($Case) }
foreach ($name in $cases) {
    $folder = Join-Path $OutputDirectory $name
    [void](New-Item -ItemType Directory -Path $folder)
    $directoryLocks.Add([FesTermDirectoryLock]::Open($folder))
    $fixture = Join-Path $folder 'synthetic.md'
    $fixtureText = New-MarkdownScrollFixture $name
    $fixtureBytes = [Text.UTF8Encoding]::new($false).GetBytes($fixtureText)
    $fixtureLock = [IO.File]::Open(
        $fixture, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
    $fixtureLock.Write($fixtureBytes, 0, $fixtureBytes.Length)
    $fixtureLock.Flush($true)
    $fixtureLock.Position = 0
    $config = Join-Path $folder 'config.toml'
    $configText = @'
schema_version = 1
[settings]
confirm_session_close = false
restore_workspace = false
automatic_update_checks = false
show_resumable_sessions = false
'@
    $configBytes = [Text.UTF8Encoding]::new($false).GetBytes($configText)
    $configLock = [IO.File]::Open(
        $config, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
    $configLock.Write($configBytes, 0, $configBytes.Length)
    $configLock.Flush($true)
    $configLock.Position = 0
    $local = Join-Path $folder 'local-app-data'
    [void](New-Item -ItemType Directory -Path $local)
    $directoryLocks.Add([FesTermDirectoryLock]::Open($local))
    $info = [Diagnostics.ProcessStartInfo]::new($Executable)
    $info.UseShellExecute = $false
    $info.WorkingDirectory = $folder
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in @('--open', '--', $fixture)) { $info.ArgumentList.Add($argument) }
    foreach ($key in @($info.Environment.Keys)) {
        if ($key -like 'FESTERM_NATIVE_*' -or $key -like 'FESTERM_*SMOKE*' -or
            $key -eq 'FESTERM_PROTOCOL_TRACE') { [void]$info.Environment.Remove($key) }
    }
    $info.Environment['FESTERM_CONFIG_PATH'] = $config
    # Activation IPC is LOCALAPPDATA scoped, NOT FESTERM_CONFIG_PATH scoped.
    $info.Environment['LOCALAPPDATA'] = $local
    $info.Environment['WGPU_BACKEND'] = 'dx12'
    $info.Environment['WGPU_ADAPTER_NAME'] = 'Microsoft Basic Render Driver'
    $info.Environment['RUST_LOG_STYLE'] = 'never'
    $info.Environment['RUST_LOG'] = 'festerm=info,festerm::markdown_scroll=debug,egui_winit::pointer_input=debug,egui_wgpu::retained_ui=debug,egui_wgpu::frame_timing=debug'
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $started = $false
    $window = [IntPtr]::Zero
    $pinned = $null
    $streams = @()
    $copies = @()
    $launchArtifacts = $null
    $primaryFailure = $null
    $pointerControlled = $false
    $originalForeground = [FesTermMarkdownMessages]::Foreground()
    $originalForegroundPid = [FesTermMarkdownMessages]::Owner($originalForeground)
    $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
    try { $originalPointer = [FesTermMarkdownMessages]::Cursor() }
    finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
    $result = [ordered]@{
        schema_version = 1; case = $name; status = 'failed'; identity = $identity
        exit_code = $null; forced_termination = $false
        artifacts_revalidated = $false; source_identity_revalidated = $false
        real_pointer_control_opted_in = [bool]$AllowGlobalPointer
        limitation = 'Host-return timings only: not GPU completion, completed presents per message, or physical display latency. Targeted PostMessage may be ignored; offset proof is required. Receipt is builder provenance, not cryptographic source attestation.'
    }
    try {
        $launchArtifacts = Get-MarkdownArtifactIdentity $recipePath $fixture $Receipt
        $result['artifacts_at_launch'] = $launchArtifacts
        if ($launchArtifacts.recipe_sha256 -ne $recipeSha256 -or $launchArtifacts.receipt_sha256 -ne $receiptSha256) {
            throw 'Recipe or receipt changed before launching the owned application.'
        }
        [void](Assert-MarkdownSourceReceipt $Executable $Receipt $SourceRoot $identity)
        [void]$process.Start()
        $started = $true
        $lifetime = [Diagnostics.Stopwatch]::StartNew()
        foreach ($channel in @('stdout', 'stderr')) {
            $path = Join-Path $folder "$channel.log"
            $stream = [IO.FileStream]::new($path, [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write, [IO.FileShare]::Read, 1, $true)
            $streams += $stream
            $source = if ($channel -eq 'stdout') { $process.StandardOutput } else { $process.StandardError }
            $copies += $source.BaseStream.CopyToAsync($stream)
        }
        $logPath = Join-Path $folder 'stderr.log'
        do {
            $process.Refresh()
            if ($process.HasExited) { throw 'Owned process exited before opening its application window.' }
            $window = [FesTermApplicationWindow]::Find($process.Id)
            if ($lifetime.Elapsed.TotalSeconds -gt 25) { throw 'Owned application window startup timeout.' }
            if ($window -eq [IntPtr]::Zero) { Start-Sleep -Milliseconds 100 }
        } while ($window -eq [IntPtr]::Zero)
        [FesTermApplicationWindow]::RequireResponsive($window, $process.Id)
        $geometry = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
        $width = [int][Math]::Round(640 * $geometry[4] / 96)
        $height = [int][Math]::Round(400 * $geometry[4] / 96)
        $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
        try { [FesTermMarkdownMessages]::Resize($window, $width, $height) }
        finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
        Start-Sleep -Milliseconds 500
        $pinned = [FesTermApplicationWindow]::ClientGeometry($window, $process.Id)
        if ($pinned[2] -ne $width -or $pinned[3] -ne $height -or $pinned[4] -ne $geometry[4]) {
            throw 'Could not pin a 640x400 logical client at the actual window DPI.'
        }
        Assert-MarkdownOwnedInterval
        # Rendering/font/cache warmup is deliberately outside CPU measurement.
        Start-Sleep -Milliseconds 1500
        do {
            Assert-MarkdownOwnedInterval
            $text = Read-MarkdownRawLog $logPath
            $samples = @(Get-MarkdownScrollSamples $text)
            if ($samples.Count) { break }
            Start-Sleep -Milliseconds 100
        } while ($lifetime.Elapsed.TotalSeconds -lt 35)
        Assert-MarkdownWarpAdapter $text
        if (-not $samples.Count -or $samples[-1].Content -le $samples[-1].Viewport) {
            throw 'Missing baseline diagnostic or fixture does not actually scroll.'
        }
        $point = Get-MarkdownPhysicalClientPoint $samples[-1] $pinned[4] $pinned[2] $pinned[3]
        $clientX, $clientY = $point
        $targetX = $pinned[0] + $clientX
        $targetY = $pinned[1] + $clientY
        if ($AllowGlobalPointer) {
            [FesTermApplicationWindow]::Activate($window, $process.Id)
            $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
            try {
                # Set before movement so failure cleanup also attempts restoration.
                $pointerControlled = $true
                [FesTermMarkdownMessages]::MoveOwnedCursor($window, $process.Id, $targetX, $targetY)
            } finally { [FesTermApplicationWindow]::RestorePixelContext($previous) }
        }
        if (-not $AllowGlobalPointer) {
            Send-MarkdownWindowMessage $window 0x0200 ([IntPtr]::Zero) `
                (ConvertTo-MarkdownPackedPoint $clientX $clientY)
        }
        Start-Sleep -Milliseconds 500
        Assert-MarkdownOwnedInterval
        $text = Read-MarkdownRawLog $logPath
        $samples = @(Get-MarkdownScrollSamples $text)
        $before = Get-MarkdownFrameCounters $text
        $baselineOffset = $samples[-1].Offset
        $bursts = @()
        $process.Refresh()
        $cpuStart = $process.TotalProcessorTime.TotalSeconds
        $measured = [Diagnostics.Stopwatch]::StartNew()
        foreach ($direction in @(1, -1, 1, -1, 1)) {
            Assert-MarkdownOwnedInterval
            $samples = @(Get-MarkdownScrollSamples (Read-MarkdownRawLog $logPath))
            $count = $samples.Count
            $offset = $samples[-1].Offset
            $counterStart = Get-MarkdownFrameCounters (Read-MarkdownRawLog $logPath)
            for ($wheel = 0; $wheel -lt 5; $wheel++) {
                Assert-MarkdownOwnedInterval
                if (-not $AllowGlobalPointer) {
                    Send-MarkdownWindowMessage $window 0x0200 ([IntPtr]::Zero) `
                        (ConvertTo-MarkdownPackedPoint $clientX $clientY)
                }
                # Signed wheel delta in the high word; no modifier keys.
                Send-MarkdownWindowMessage $window 0x020A `
                    ([IntPtr]::new(-7864320 * $direction)) `
                    (ConvertTo-MarkdownPackedPoint ($pinned[0] + $clientX) ($pinned[1] + $clientY))
                Start-Sleep -Milliseconds 45
            }
            $drain = [Diagnostics.Stopwatch]::StartNew()
            $state = [pscustomobject]@{ Count = $count; Offset = $offset; LastChangeMs = 0 }
            do {
                Assert-MarkdownOwnedInterval
                Start-Sleep -Milliseconds 100
                $text = Read-MarkdownRawLog $logPath
                $later = @(Get-MarkdownScrollSamples $text)
                $state = Get-MarkdownScrollDrainState $later $state.Count $state.Offset `
                    $state.LastChangeMs $drain.Elapsed.TotalMilliseconds
            } while (-not $state.Settled -and -not $state.TimedOut)
            $finalOffset = $state.Offset
            $bursts += [pscustomobject]@{
                requested_wheel_messages = 5; cadence_ms = 45
                requested_direction = $(if ($direction -eq 1) { 'down' } else { 'up' })
                requested_wheel_delta_each = -120 * $direction
                offset_before = $offset; offset_after = $finalOffset
                actual_offset_delta = $finalOffset - $offset
                diagnostic_samples = $later.Count - $count
                drain_ms = $drain.Elapsed.TotalMilliseconds
                settled = $state.Settled; settle_timed_out = $state.TimedOut
                counters_before = $counterStart; counters_after = (Get-MarkdownFrameCounters $text)
                observation = 'Samples are frame diagnostics, not acknowledgments; messages and redraws may coalesce.'
            }
            $result['bursts'] = $bursts
            if ($state.TimedOut) { throw 'Scroll offset was still changing at the bounded settle deadline.' }
            [void](Assert-MarkdownScrollProgress $later $count $offset $direction)
        }
        Assert-MarkdownOwnedInterval
        $measured.Stop()
        $process.Refresh()
        $cpu = $process.TotalProcessorTime.TotalSeconds - $cpuStart
        $after = Get-MarkdownFrameCounters $text
        if ($finalOffset -le $baselineOffset -or $finalOffset -le 0 -or
            $null -eq $before.RootCompleted -or $null -eq $after.RootCompleted -or
            $after.RootCompleted -le $before.RootCompleted) {
            throw 'Missing final actual scroll/root-frame progress evidence.'
        }
        $result['adapter'] = @{ name = 'Microsoft Basic Render Driver'; backend = 'Dx12'; device_type = 'Cpu'; proof = 'owned process adapter diagnostic' }
        # Do not publish screen origin (raw logs remain local).
        $result['client'] = @{ logical_width = 640; logical_height = 400; physical_width = $pinned[2]; physical_height = $pinned[3]; dpi = $pinned[4] }
        $result['measurement'] = @{
            wall_seconds = $measured.Elapsed.TotalSeconds; process_cpu_seconds = $cpu
            cpu_percent_one_core = 100 * $cpu / $measured.Elapsed.TotalSeconds
            counters_before = $before; counters_after = $after
            host_frame_timing = (Get-MarkdownHostFrameTiming $text $before.RootCompleted $after.RootCompleted)
            retention_note = 'Missing fields are null, not zero. In the new renderer, Reused is exact-only; Rebuilt, Reused and PartialReused partition retention outcomes. Older renderer counters retain their original meaning.'
        }
        Restore-MarkdownPointer
        [FesTermApplicationWindow]::Close($window, $process.Id)
        if (-not $process.WaitForExit(6000) -or $process.ExitCode -ne 0) {
            throw 'Owned WM_CLOSE did not produce normal exit code 0 within six seconds.'
        }
        $result['status'] = 'passed'
    } catch {
        $primaryFailure = $_
        $result['status'] = 'failed'
        $result['failure'] = @{ stage = 'profile'; exception_type = $_.Exception.GetType().Name }
    } finally {
        $steps = @(
            @{ Name = 'pointer_restore'; Action = { Restore-MarkdownPointer } },
            @{ Name = 'owned_pid'; Action = {
                # No process-name matching, tree-wide killing, or other-instance cleanup.
                if ($started) {
                    $process.Refresh()
                    if (-not $process.HasExited) {
                        $result['forced_termination'] = $true
                        $result['status'] = 'failed'
                        Stop-Process -Id $process.Id
                        if (-not $process.WaitForExit(4000)) { throw 'Targeted owned PID cleanup timeout.' }
                    }
                }
            } },
            @{ Name = 'exit_code'; Action = {
                if ($started) {
                    $process.Refresh()
                    if ($process.HasExited) { $result['exit_code'] = $process.ExitCode }
                }
            } }
        )
        foreach ($copy in $copies) {
            $steps += @{ Name = 'raw_log_drain'; Resource = $copy; Action = {
                param($step)
                if (-not $step.Resource.Wait(2000)) { throw 'Owned raw-log drain timeout.' }
                [void]$step.Resource.GetAwaiter().GetResult()
            } }
        }
        foreach ($stream in $streams) {
            $steps += @{ Name = 'raw_log_retain'; Resource = $stream; Action = {
                param($step)
                $step.Resource.Flush($true)
                $evidenceLocks.Add($step.Resource)
            } }
        }
        $steps += @(
            @{ Name = 'artifact_revalidation'; Action = {
                if ($null -ne $launchArtifacts) {
                    $finalArtifacts = Get-MarkdownArtifactIdentity $recipePath $fixture $Receipt
                    $result['artifacts_after_exit'] = $finalArtifacts
                    Assert-MarkdownArtifactIdentity $launchArtifacts $finalArtifacts
                    $result['artifacts_revalidated'] = $true
                }
            } },
            @{ Name = 'source_revalidation'; Action = {
                $result['identity_after_exit'] = Assert-MarkdownSourceReceipt $Executable $Receipt $SourceRoot $identity
                $result['source_identity_revalidated'] = $true
            } },
            @{ Name = 'fixture_retain'; Action = { $evidenceLocks.Add($fixtureLock) } },
            @{ Name = 'config_retain'; Action = { $evidenceLocks.Add($configLock) } },
            @{ Name = 'process_dispose'; Action = { $process.Dispose() } }
        )
        Invoke-MarkdownCleanup $result $steps
        $resultPath = Join-Path $folder 'result.json'
        $resultBytes = [Text.UTF8Encoding]::new($false).GetBytes(
            (($result | ConvertTo-Json -Depth 10) + "`n"))
        $resultLock = [IO.File]::Open(
            $resultPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
        $resultLock.Write($resultBytes, 0, $resultBytes.Length)
        $resultLock.Flush($true)
        $resultLock.Position = 0
        $evidenceLocks.Add($resultLock)
    }
    if ($null -ne $primaryFailure) { throw $primaryFailure }
    if ($result['status'] -ne 'passed') { throw 'Profiling cleanup or final identity validation failed; inspect the owned result JSON.' }
}
} finally {
    foreach ($lock in $immutableArtifactLocks) {
        try { $lock.Dispose() } catch { Write-Warning 'Could not release a staged artifact lock.' }
    }
    foreach ($path in $stagedArtifacts) {
        try { Remove-Item -LiteralPath $path -Force } catch { Write-Warning 'Could not remove a staged artifact.' }
    }
    foreach ($name in $cases) {
        $sourceCase = Join-Path $OutputDirectory $name
        if (Test-Path -LiteralPath $sourceCase) {
            try {
                Copy-Item -LiteralPath $sourceCase -Destination (Join-Path $publicationDirectory $name) -Recurse
            } catch {
                $publicationFailure = $_
                Write-Warning "Could not publish the $name evidence directory."
            }
        }
    }
    foreach ($lock in $directoryLocks) {
        try { $lock.Dispose() } catch { Write-Warning 'Could not release a runtime directory lock.' }
    }
    foreach ($lock in $evidenceLocks) {
        try { $lock.Dispose() } catch { Write-Warning 'Could not release an evidence artifact lock.' }
    }
    try { Remove-Item -LiteralPath $artifactDirectory -Force } catch { Write-Warning 'Could not remove the staging directory.' }
    try { Remove-Item -LiteralPath $OutputDirectory -Recurse -Force } catch {
        Write-Warning 'Could not remove the private runtime evidence directory.'
    }
}
if ($null -ne $publicationFailure) {
    throw 'Could not publish native profiling evidence to OutputDirectory.'
}
