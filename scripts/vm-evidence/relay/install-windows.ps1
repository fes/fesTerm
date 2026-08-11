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

$scriptPath = Join-Path $PSScriptRoot 'windows.ps1'
$jobsPath = Join-Path $Spool 'jobs'
$logsPath = Join-Path $Spool 'logs'
$resultsPath = Join-Path $Spool 'results'
New-Item -ItemType Directory -Force -Path $jobsPath, $logsPath, $resultsPath | Out-Null

# The controller owns the spool root, while each interactive relay execution
# must retain access to the log and result files it creates.
& icacls.exe $logsPath /grant '*S-1-3-0:(OI)(CI)F' | Out-Null
& icacls.exe $resultsPath /grant '*S-1-3-0:(OI)(CI)F' | Out-Null

Unregister-ScheduledTask -TaskName 'fesTerm VM Evidence Relay' -Confirm:$false -ErrorAction SilentlyContinue
Write-Output 'Installed the Windows relay. The host controller invokes it in the current console session.'
