[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$profileDirectory = $Configuration.ToLowerInvariant()
$releaseDirectory = Join-Path $repositoryRoot "target\$profileDirectory"
$source = Join-Path $releaseDirectory 'festerm-sessiond.exe'

if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
    throw "Windows session daemon is missing: $source"
}

Push-Location $repositoryRoot
try {
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

$package = @($metadata.packages | Where-Object { $_.name -eq 'festerm-sessiond' })
if ($package.Count -ne 1) {
    throw "Expected one festerm-sessiond workspace package, found $($package.Count)"
}

$destination = Join-Path $releaseDirectory "festerm-sessiond-$($package[0].version).exe"
Copy-Item -LiteralPath $source -Destination $destination -Force
Write-Host "Staged Windows session daemon: $destination"
# The staged name is derived here only; callers consume this path rather than
# rebuilding it from a version they happen to have.
Write-Output $destination
