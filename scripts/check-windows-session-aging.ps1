[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $OutputDirectory,
    [ValidateSet('debug', 'release')][string] $Profile = 'release',
    [ValidateRange(1, 2000)][int] $Cycles = 120,
    [ValidateRange(1, 1000)][int] $Frames = 100,
    [ValidateRange(1, 300)][int] $IdleSeconds = 10,
    [ValidateRange(60, 14400)][int] $TimeoutSeconds = 1800
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to run the owned aging probe.'
}
if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'The aging probe requires Windows x64 and a DX12 CPU adapter.'
}
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$head = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source HEAD.' }
$tree = (& git rev-parse 'HEAD^{tree}').Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify source tree.' }
if (& git status --porcelain) { throw 'Run from a clean committed checkout.' }
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw 'Use a new output directory.' }
New-Item -ItemType Directory -Path $output | Out-Null

$arguments = @('test', '--locked', '-p', 'festerm', '--bin', 'festerm',
    'profile_six_session_aging', '--no-run', '--message-format=json')
if ($Profile -eq 'release') { $arguments += '--release' }
$buildLog = Join-Path $output 'build.jsonl'
& cargo @arguments 1> $buildLog 2> (Join-Path $output 'build.stderr.log')
if ($LASTEXITCODE -ne 0) { throw 'Probe build failed; inspect build.stderr.log.' }
$artifacts = @(Get-Content -LiteralPath $buildLog | ForEach-Object {
    $record = $_ | ConvertFrom-Json
    if ($record.reason -eq 'compiler-artifact' -and $record.target.name -eq 'festerm' -and
        $record.profile.test -and $record.executable) { $record.executable }
})
if ($artifacts.Count -ne 1) { throw 'Expected exactly one compiler-reported test executable.' }
$executable = Join-Path $output 'festerm-aging-probe.exe'
Copy-Item -LiteralPath $artifacts[0] -Destination $executable
$binaryHash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
$testName = 'app::tests::session_aging::profile_six_session_aging'
$inventory = @(& $executable --list)
if ($LASTEXITCODE -ne 0 -or $inventory -notcontains "${testName}: test") {
    throw 'Archived executable lacks the exact aging test.'
}
$inventory | Set-Content -LiteralPath (Join-Path $output 'tests.txt')
$binding = @{
    schema = 1; source_head = $head; source_tree = $tree; profile = $Profile
    compiler_executable = $artifacts[0]; executable_sha256 = $binaryHash
    cycles = $Cycles; frames = $Frames; idle_seconds = $IdleSeconds
    resource_sample_interval_ms = 500; timeout_seconds = $TimeoutSeconds
    started_utc = [DateTime]::UtcNow.ToString('o')
    limitations = 'Owned offscreen fixture; background processes allowed. Resources are sampled current process values, not complete GPU allocation accounting or external CPU attribution.'
}
$binding | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'source.json')
$saved = @{}
$settings = @{
    FESTERM_AGING_OUT = (Join-Path $output 'probe')
    FESTERM_AGING_CYCLES = "$Cycles"; FESTERM_AGING_FRAMES = "$Frames"
    FESTERM_AGING_IDLE_SECONDS = "$IdleSeconds"; FESTERM_DIRECT2D_TIMINGS = '0'
}
$process = $null
$resources = $null
try {
    foreach ($key in $settings.Keys) {
        $saved[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
        [Environment]::SetEnvironmentVariable($key, $settings[$key], 'Process')
    }
    $resources = [System.IO.StreamWriter]::new((Join-Path $output 'resources.jsonl'), $false)
    $process = Start-Process -FilePath $executable -PassThru -NoNewWindow `
        -ArgumentList @('--exact', $testName, '--ignored', '--nocapture', '--test-threads=1') `
        -RedirectStandardOutput (Join-Path $output 'stdout.log') `
        -RedirectStandardError (Join-Path $output 'stderr.log')
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $phasePath = Join-Path $settings.FESTERM_AGING_OUT 'phase.json'
    while (-not $process.HasExited) {
        if ($clock.Elapsed.TotalSeconds -ge $TimeoutSeconds) { throw 'Aging probe exceeded its bounded deadline.' }
        $phase = 'starting'
        if (Test-Path -LiteralPath $phasePath) {
            $stream = [System.IO.File]::Open($phasePath, 'Open', 'Read',
                [System.IO.FileShare]::ReadWrite -bor [System.IO.FileShare]::Delete)
            $reader = [System.IO.StreamReader]::new($stream)
            try { $phase = ($reader.ReadToEnd() | ConvertFrom-Json).phase }
            finally { $reader.Dispose() }
        }
        $process.Refresh()
        if ($process.HasExited) { break }
        $threads = @($process.Threads | ForEach-Object {
            @{ id = $_.Id; cpu_ms = $_.TotalProcessorTime.TotalMilliseconds }
        })
        $record = @{
            unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            elapsed_seconds = $clock.Elapsed.TotalSeconds; pid = $process.Id; phase = $phase
            working_set_bytes = $process.WorkingSet64; private_bytes = $process.PrivateMemorySize64
            peak_working_set_bytes = $process.PeakWorkingSet64; handles = $process.HandleCount
            thread_count = $process.Threads.Count; process_cpu_ms = $process.TotalProcessorTime.TotalMilliseconds
            threads = $threads
        }
        $resources.WriteLine(($record | ConvertTo-Json -Depth 4 -Compress))
        $resources.Flush()
        Start-Sleep -Milliseconds 500
        $process.Refresh()
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "Aging probe failed with exit $($process.ExitCode); inspect logs." }
} finally {
    if ($process) {
        $process.Refresh()
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id
            $process.WaitForExit()
        }
    }
    if ($resources) { $resources.Dispose() }
    foreach ($key in $saved.Keys) {
        [Environment]::SetEnvironmentVariable($key, $saved[$key], 'Process')
    }
}
if ((& git rev-parse HEAD).Trim() -ne $head -or (& git status --porcelain)) {
    throw 'Source changed during the probe.'
}
if ((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash -ne $binaryHash) {
    throw 'Archived executable changed during the probe.'
}
& python (Join-Path $root 'validation\terminal-performance\check_session_aging.py') $output
if ($LASTEXITCODE -ne 0) { throw 'Aging evidence validation failed.' }
$binding.completed_utc = [DateTime]::UtcNow.ToString('o')
$binding.exit_code = 0
$binding | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'source.json')
Write-Output "Aging probe completed: $output"
