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
New-Item -ItemType Directory -Force -Path (Join-Path $Spool 'jobs'), (Join-Path $Spool 'logs'), (Join-Path $Spool 'results') | Out-Null

$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -ExecutionPolicy Bypass -File `"$scriptPath`" -Spool `"$Spool`" -Repository `"$Repository`" -RepositoryUrl `"$RepositoryUrl`""
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited
Register-ScheduledTask -TaskName 'fesTerm VM Evidence Relay' -Action $action -Trigger $trigger -Principal $principal -Description 'Runs allowlisted fesTerm evidence jobs in the interactive desktop session.' -Force | Out-Null

Write-Output 'Installed the interactive Scheduled Task. It runs at the next graphical logon.'
