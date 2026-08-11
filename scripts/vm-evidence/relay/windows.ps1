[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Spool,

    [Parameter(Mandatory = $true)]
    [string] $Repository,

    [Parameter(Mandatory = $true)]
    [string] $RepositoryUrl
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-RelayResult {
    param(
        [string] $Path,
        [string] $Status,
        [string] $RunId,
        [string] $Sha,
        [string] $Mode,
        [string] $Message,
        [string] $ResolvedSha = $null,
        [string] $Phase = 'complete'
    )

    [ordered]@{
        status = $Status
        run_id = $RunId
        sha = $Sha
        mode = $Mode
        message = $Message
        completed_at = (Get-Date).ToUniversalTime().ToString('o')
        resolved_sha = if ([string]::IsNullOrEmpty($ResolvedSha)) { $null } else { $ResolvedSha }
        phase = $Phase
    } | ConvertTo-Json -Compress | Set-Content -Path "$Path.partial" -NoNewline
    Move-Item -LiteralPath "$Path.partial" -Destination $Path -Force
}

function Test-RelayJob {
    param([object] $Job)

    $Job.sha -is [string] -and
    $Job.sha -match '^[0-9a-f]{40}$|^[0-9a-f]{64}$' -and
    $Job.run_id -is [string] -and
    $Job.run_id -match '^[A-Za-z0-9._-]{1,128}$' -and
    @('readiness-probe', 'native-smoke', 'optional-validation') -contains $Job.mode
}

$jobsPath = Join-Path $Spool 'jobs'
$logsPath = Join-Path $Spool 'logs'
$resultsPath = Join-Path $Spool 'results'
New-Item -ItemType Directory -Force -Path $jobsPath, $logsPath, $resultsPath | Out-Null

Get-ChildItem -Path $jobsPath -Filter '*.json' -File |
    Where-Object {
        $_.Name -notlike 'processed-*' -and
        $_.Name -notlike 'rejected-*' -and
        $_.Name -notlike 'infrastructure-failed-*'
    } |
    Sort-Object Name | ForEach-Object {
    $jobPath = $_.FullName
    $job = Get-Content -Raw $jobPath | ConvertFrom-Json
    if (-not (Test-RelayJob $job)) {
        throw "Invalid relay job: $jobPath"
    }

    $resultPath = Join-Path $resultsPath "$($job.run_id).json"
    if (Test-Path $resultPath) {
        throw "Result already exists for run ID: $($job.run_id)"
    }
    $logPath = Join-Path $logsPath "$($job.run_id).log"
    $resolvedSha = $null
    Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'relay accepted job' $null 'queued'

    try {
        Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'checking graphical-session build prerequisites' $null 'preflight'
        foreach ($command in 'git', 'cargo', 'rustc') {
            if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
                throw "Missing required guest command: $command"
            }
        }
        if ($job.mode -eq 'readiness-probe') {
            Write-RelayResult $resultPath 'pass' $job.run_id $job.sha $job.mode 'graphical relay and build prerequisites are ready' $null 'complete'
            Move-Item -Path $jobPath -Destination (Join-Path $jobsPath "processed-$($_.Name)")
            return
        }
        Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'checking out requested revision' $null 'checkout'
        if (-not (Test-Path (Join-Path $Repository '.git'))) {
            git clone $RepositoryUrl $Repository
        }
        git -C $Repository fetch --no-tags $RepositoryUrl $job.sha
        git -C $Repository checkout --detach --force $job.sha
        $resolvedSha = (git -C $Repository rev-parse HEAD).Trim()
        if ($resolvedSha -ne $job.sha) {
            throw 'Resolved SHA differs from requested SHA.'
        }

        Push-Location $Repository
        if ($job.mode -eq 'native-smoke') {
            Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'building workspace' $resolvedSha 'build'
            cargo build --workspace *>> $logPath
            if ($LASTEXITCODE -ne 0) {
                throw 'Workspace build failed.'
            }
            $env:FESTERM_NATIVE_WINDOW_SMOKE = '1'
            Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'running native-window smoke' $resolvedSha 'app'
            $nativePath = Join-Path $resultsPath "$($job.run_id).native.txt"
            $env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativePath
            & (Join-Path $Repository 'target\debug\festerm.exe') *>> $logPath
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path $nativePath) -or
                (Get-Content $nativePath -TotalCount 1) -ne 'status=pass') {
                throw 'Native-window validation failed.'
            }
        } else {
            Write-RelayResult $resultPath 'running' $job.run_id $job.sha $job.mode 'running optional validation' $resolvedSha 'app'
            $env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
            $optionalPath = Join-Path $resultsPath "$($job.run_id).optional.txt"
            $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH = $optionalPath
            & (Join-Path $Repository 'scripts\run-optional-validation.ps1') *>> $logPath
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path $optionalPath) -or
                (Get-Content $optionalPath -Tail 1) -ne 'status=pass') {
                throw 'Optional validation failed.'
            }
        }
        Write-RelayResult $resultPath 'pass' $job.run_id $job.sha $job.mode 'repository-owned validation passed' $resolvedSha
    } catch {
        $_ | Out-String | Add-Content -Path $logPath
        Write-RelayResult $resultPath 'fail' $job.run_id $job.sha $job.mode "validation failed; inspect $logPath" $resolvedSha
    } finally {
        if ((Get-Location).Path -eq $Repository) {
            Pop-Location
        }
        Remove-Item Env:FESTERM_NATIVE_WINDOW_SMOKE -ErrorAction Ignore
        Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
        Remove-Item Env:FESTERM_RUN_OPTIONAL_VALIDATION -ErrorAction Ignore
        Remove-Item Env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH -ErrorAction Ignore
    }

    Move-Item -Path $jobPath -Destination (Join-Path $jobsPath "processed-$($_.Name)")
}
