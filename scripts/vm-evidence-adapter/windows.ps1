[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $JobPath,

    [Parameter(Mandatory = $true)]
    [string] $SourceMapPath,

    [Parameter(Mandatory = $true)]
    [string] $ArtifactDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Test-FesTermJob {
    param([object] $Job)

    $Job.adapter_id -eq 'festerm' -and
        $Job.adapter_schema_version -eq 1 -and
        $Job.platform -eq 'windows' -and
        @('native-smoke', 'os-input-smoke', 'optional-validation') -contains $Job.mode -and
        @($Job.payload.PSObject.Properties).Count -eq 0
}

function Get-FesTermSource {
    param([array] $SourceMap)

    $matches = @($SourceMap | Where-Object { $_.id -eq 'festerm' })
    if ($matches.Count -ne 1 -or -not (Test-Path -LiteralPath $matches[0].path)) {
        throw 'Expected exactly one checked-out fesTerm source.'
    }
    $matches[0].path
}

function Require-PassStatus {
    param([string] $Path)

    if (-not (Test-Path -LiteralPath $Path) -or
        (Get-Content -LiteralPath $Path -Tail 1) -ne 'status=pass') {
        throw "Evidence runner did not write status=pass: $Path"
    }
}

function Invoke-NativeCommand {
    # Native tooling (cmd.exe/cargo/festerm.exe) routinely writes normal
    # progress output to stderr. With $ErrorActionPreference = 'Stop',
    # PowerShell promotes that stderr text into a script-terminating error.
    # Relax the preference for the duration of the call and rely on
    # $LASTEXITCODE (as callers already do) to detect real failures.
    param([scriptblock] $Command)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Command
    } finally {
        $ErrorActionPreference = $previous
    }
}

$job = Get-Content -Raw -LiteralPath $JobPath | ConvertFrom-Json
$sourceMap = @(Get-Content -Raw -LiteralPath $SourceMapPath | ConvertFrom-Json)
if (-not (Test-FesTermJob $job)) {
    throw 'fesTerm adapter rejected the job.'
}
$sourcePath = Get-FesTermSource $sourceMap
New-Item -ItemType Directory -Force -Path $ArtifactDirectory | Out-Null

$vcvarsallPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat'
$llvmBinPath = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\Llvm\bin'

Push-Location $sourcePath
try {
    switch ($job.mode) {
        'native-smoke' {
            if (-not (Test-Path -LiteralPath $vcvarsallPath) -or
                -not (Test-Path -LiteralPath (Join-Path $llvmBinPath 'clang.exe'))) {
                throw 'Windows ARM64 Build Tools and Clang are required for evidence builds.'
            }
            $buildCommand = "call `"$vcvarsallPath`" arm64 >nul && set `"PATH=$llvmBinPath;%PATH%`" && set CC=clang && cargo build --workspace"
            Invoke-NativeCommand { cmd.exe /d /c $buildCommand }
            if ($LASTEXITCODE -ne 0) {
                throw 'Workspace build failed.'
            }
            $nativePath = Join-Path $ArtifactDirectory 'native-smoke.txt'
            $env:FESTERM_NATIVE_WINDOW_SMOKE = '1'
            $env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativePath
            Invoke-NativeCommand { & (Join-Path $sourcePath 'target\debug\festerm.exe') }
            if ($LASTEXITCODE -ne 0) {
                throw 'Native-window validation failed.'
            }
            Require-PassStatus $nativePath
        }
        'os-input-smoke' {
            $resultPath = Join-Path $ArtifactDirectory 'os-input-smoke.txt'
            $hostInputDirectory = Join-Path $ArtifactDirectory 'host-input'
            Invoke-NativeCommand {
                & (Join-Path $sourcePath 'scripts\run-windows-os-input-smoke.ps1') `
                    -ResultPath $resultPath `
                    -HostInputStateDirectory $hostInputDirectory `
                    -HostInputRunId $job.run_id
            }
            if ($LASTEXITCODE -ne 0) {
                throw 'OS-input validation failed.'
            }
            Require-PassStatus $resultPath
        }
        'optional-validation' {
            $resultPath = Join-Path $ArtifactDirectory 'optional-validation.txt'
            $env:FESTERM_RUN_OPTIONAL_VALIDATION = '1'
            $env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH = $resultPath
            Invoke-NativeCommand {
                & (Join-Path $sourcePath 'scripts\run-optional-validation.ps1') `
                    -HostInputStateDirectory (Join-Path $ArtifactDirectory 'host-input') `
                    -HostInputRunId $job.run_id
            }
            if ($LASTEXITCODE -ne 0) {
                throw 'Optional validation failed.'
            }
            Require-PassStatus $resultPath
        }
    }
} finally {
    Pop-Location
    Remove-Item Env:FESTERM_NATIVE_WINDOW_SMOKE -ErrorAction Ignore
    Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore
    Remove-Item Env:FESTERM_RUN_OPTIONAL_VALIDATION -ErrorAction Ignore
    Remove-Item Env:FESTERM_OPTIONAL_VALIDATION_RESULT_PATH -ErrorAction Ignore
    Remove-Item Env:FESTERM_NATIVE_HOST_INPUT_STATE_DIRECTORY -ErrorAction Ignore
    Remove-Item Env:FESTERM_NATIVE_HOST_INPUT_RUN_ID -ErrorAction Ignore
}
