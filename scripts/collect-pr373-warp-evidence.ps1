[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $SourceDirectory,
    [Parameter(Mandatory)][string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$expectedHead = '6e8fbff56de69717ed9c89d888178397b9a04caf'
$testName = 'direct2d::document_retention::profile_document_retention_changing_frames'
$testArguments = @($testName, '--exact', '--ignored', '--nocapture', '--test-threads=1')
if ($env:OS -ne 'Windows_NT' -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw 'Evidence requires Windows x64, not an ARM64 emulation control.'
}
if (Test-Path $OutputDirectory) { throw 'Refusing to overwrite earlier evidence.' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $output | Out-Null
Set-Location $SourceDirectory

function Git-Receipt([string[]] $Arguments) {
    $result = & git @Arguments
    if ($LASTEXITCODE -ne 0) { throw "git receipt failed: $Arguments" }
    return ($result -join "`n").Trim()
}

$head = Git-Receipt @('rev-parse', 'HEAD')
$tree = Git-Receipt @('rev-parse', 'HEAD^{tree}')
$status = Git-Receipt @('status', '--porcelain=v1', '--untracked-files=all')
if ($head -ne $expectedHead -or $status -ne '') { throw 'Source is not the frozen clean PR head.' }
$diff = Git-Receipt @('--no-pager', 'diff', '--binary', '--full-index', '--no-ext-diff', '--no-textconv', 'HEAD', '--')
if ($diff -ne '') { throw 'Tracked source differs from the frozen head.' }
$env:CARGO_TARGET_DIR = Join-Path $env:RUNNER_TEMP 'pr373-release-target'
$env:FESTERM_CONFIG_PATH = Join-Path $env:RUNNER_TEMP 'pr373-owned-config.toml'
$env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
$rustc = & rustc -Vv
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify Rust toolchain.' }
$provenance = [ordered]@{
    source_revision = $head
    source_tree = $tree
    source_status_before = $status
    tracked_diff = $diff
    controller_revision = $env:GITHUB_SHA
    run_id = $env:GITHUB_RUN_ID
    run_attempt = $env:GITHUB_RUN_ATTEMPT
    runner_image = $env:ImageOS
    runner_image_version = $env:ImageVersion
    os_version = [Environment]::OSVersion.Version.ToString()
    architecture = $env:PROCESSOR_ARCHITECTURE
    logical_processors = [Environment]::ProcessorCount
    rustc = $rustc
    build_invocation = 'cargo test --locked --release -p festerm --bin festerm --no-run --message-format=json'
    test_arguments = $testArguments
    optional_validation = '1'
    fixture_viewport_pixels = @(1280, 800)
    pixels_per_point = 2
    maximum_on_off_ratio = 1.10
    limitations = 'Hosted Windows x64 DX12 CPU WARP, same-executable prepared-document all-miss control. Not quiet dedicated hardware, native presentation, active input, clipboard or physical latency. Three predeclared independent ABBA controls; no retries or discarded runs. Background CPU samples are contextual observations, not a calibrated exclusion gate.'
}
$provenance | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $output 'provenance.json')

& cargo test --locked --release -p festerm --bin festerm --no-run --message-format=json `
    1> (Join-Path $output 'build.jsonl') 2> (Join-Path $output 'build.stderr.log')
if ($LASTEXITCODE -ne 0) { throw "Release build failed; inspect retained build logs ($LASTEXITCODE)." }
$executables = @(
    Get-Content (Join-Path $output 'build.jsonl') | ForEach-Object {
        $message = $_ | ConvertFrom-Json
        if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and
            $message.target.name -eq 'festerm' -and $message.executable) {
            $message.executable
        }
    }
)
if ($executables.Count -ne 1) { throw 'Expected exactly one actual festerm libtest executable.' }
$executable = $executables[0]
$binaryHash = (Get-FileHash -Algorithm SHA256 $executable).Hash
$provenance.test_executable = $executable
$provenance.test_executable_sha256 = $binaryHash
$provenance.source_status_after_build = Git-Receipt @('status', '--porcelain=v1', '--untracked-files=all')
if ($provenance.source_status_after_build -ne '') { throw 'Build modified source checkout.' }
$provenance | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $output 'provenance.json')

$results = @()
for ($attempt = 1; $attempt -le 3; $attempt++) {
    $attemptRoot = Join-Path $output "control-$attempt"
    New-Item -ItemType Directory -Path $attemptRoot | Out-Null
    $background = @()
    for ($sample = 0; $sample -lt 5; $sample++) {
        $cpu = Get-CimInstance Win32_PerfFormattedData_PerfOS_Processor -Filter "Name='_Total'"
        $background += [ordered]@{
            utc = [DateTime]::UtcNow.ToString('o')
            total_machine_cpu_percent = $cpu.PercentProcessorTime
        }
        Start-Sleep -Seconds 1
    }
    $background | ConvertTo-Json | Set-Content (Join-Path $attemptRoot 'background-before.json')
    $env:FESTERM_DOCUMENT_RETENTION_PROFILE_OUT = Join-Path $attemptRoot 'measurement'
    [ordered]@{
        executable = $executable
        sha256 = $binaryHash
        arguments = $provenance.test_arguments
        working_directory = (Get-Location).Path
        environment = @{
            FESTERM_RUN_OPTIONAL_VALIDATION = $env:FESTERM_RUN_OPTIONAL_VALIDATION
            FESTERM_DOCUMENT_RETENTION_PROFILE_OUT = $env:FESTERM_DOCUMENT_RETENTION_PROFILE_OUT
            FESTERM_CONFIG_PATH = $env:FESTERM_CONFIG_PATH
        }
        start_utc = [DateTime]::UtcNow.ToString('o')
    } | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $attemptRoot 'invocation.json')
    $process = Start-Process -FilePath $executable -ArgumentList $testArguments `
        -RedirectStandardOutput (Join-Path $attemptRoot 'test.stdout.log') `
        -RedirectStandardError (Join-Path $attemptRoot 'test.stderr.log') `
        -PassThru -Wait
    $exitCode = $process.ExitCode
    $result = [ordered]@{
        attempt = $attempt
        exit_code = $exitCode
        finish_utc = [DateTime]::UtcNow.ToString('o')
        profile_path = "control-$attempt/measurement/profile.json"
        complete = $false
        scenes = @()
    }
    $profilePath = Join-Path $env:FESTERM_DOCUMENT_RETENTION_PROFILE_OUT 'profile.json'
    if (Test-Path $profilePath) {
        $profile = Get-Content -Raw $profilePath | ConvertFrom-Json
        if ($profile.records.Count -eq 12 -and $profile.adapter -match 'Cpu' -and
            $profile.adapter -match 'Dx12') {
            $valid = $true
            foreach ($scene in @('fixture.txt', 'fixture.md', 'split.md')) {
                $records = @($profile.records | Where-Object scene -eq $scene | Sort-Object order)
                if ($records.Count -ne 4) { $valid = $false; continue }
                for ($index = 0; $index -lt 4; $index++) {
                    $record = $records[$index]
                    $enabled = $index -in @(1, 2)
                    if ($record.order -ne $index -or $record.enabled -ne $enabled -or
                        $record.frames -ne 32 -or $record.hits -ne 0 -or
                        $record.rebuilds -ne $(if ($enabled) { 32 } else { 0 })) {
                        $valid = $false
                    }
                }
                $cpuOff = $records[0].cpu_ms_per_frame + $records[3].cpu_ms_per_frame
                $timeOff = $records[0].completed_ms_per_frame + $records[3].completed_ms_per_frame
                if ($cpuOff -le 0 -or $timeOff -le 0) { $valid = $false; continue }
                $cpuRatio = ($records[1].cpu_ms_per_frame + $records[2].cpu_ms_per_frame) / $cpuOff
                $renderRatio = ($records[1].completed_ms_per_frame + $records[2].completed_ms_per_frame) / $timeOff
                $result.scenes += @{ scene = $scene; cpu_ratio = $cpuRatio; completed_render_ratio = $renderRatio }
                if ($cpuRatio -gt 1.10 -or $renderRatio -gt 1.10) { $valid = $false }
            }
            $testLog = Get-Content -Raw (Join-Path $attemptRoot 'test.stdout.log')
            $result.complete = $valid -and $exitCode -eq 0 -and
                $testLog -match 'test result: ok\. 1 passed; 0 failed; 0 ignored'
        }
    }
    $result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $attemptRoot 'result.json')
    $results += $result
    $results | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $output 'summary.json')
    Write-Host ($result | ConvertTo-Json -Depth 6 -Compress)
}
$provenance.source_status_after_measurement = Git-Receipt @('status', '--porcelain=v1', '--untracked-files=all')
$provenance.test_executable_sha256_after = (Get-FileHash -Algorithm SHA256 $executable).Hash
$provenance | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $output 'provenance.json')
if ($provenance.source_status_after_measurement -ne '' -or
    $provenance.test_executable_sha256_after -ne $binaryHash) {
    throw 'Source or test executable changed during measurement.'
}
if (@($results | Where-Object { -not $_.complete }).Count -ne 0) {
    throw 'One or more predeclared controls failed; all receipts are retained. No retry or exclusion.'
}
