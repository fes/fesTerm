[CmdletBinding()]
param(
    [switch]$RunSmoke
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Sha512 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    return (Get-FileHash -LiteralPath $Path -Algorithm SHA512).Hash.ToUpperInvariant()
}

function Assert-Sha512 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [string]$Expected,

        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }

    $actual = Get-Sha512 -Path $Path
    if (-not [string]::Equals($actual, $Expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description SHA-512 mismatch: expected $Expected, got $actual"
    }
}

function Assert-ManifestSha512 {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value,

        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if ($Value -notmatch '^[A-Fa-f0-9]{128}$') {
        throw "$Description must be a 128-character SHA-512 hex digest"
    }
}

$repositoryRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$manifestPath = Join-Path $repositoryRoot 'third_party\conpty\manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
    throw "Pinned ConPTY manifest is missing: $manifestPath"
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$archiveSha512 = [string]$manifest.sha512
$dllSha512 = [string]$manifest.files.'win-x64/conpty.dll'.sha512
$hostSha512 = [string]$manifest.files.'x64/OpenConsole.exe'.sha512

Assert-ManifestSha512 -Value $archiveSha512 -Description 'Package archive hash'
Assert-ManifestSha512 -Value $dllSha512 -Description 'win-x64 conpty.dll hash'
Assert-ManifestSha512 -Value $hostSha512 -Description 'x64 OpenConsole.exe hash'

$sourceUri = $null
if (-not [Uri]::TryCreate([string]$manifest.source_url, [UriKind]::Absolute, [ref]$sourceUri) -or
    $sourceUri.Scheme -ne 'https') {
    throw 'ConPTY package source_url must be an absolute HTTPS URL'
}

$safePackage = [regex]::Replace([string]$manifest.package, '[^A-Za-z0-9._-]', '_')
$safeVersion = [regex]::Replace([string]$manifest.version, '[^A-Za-z0-9._-]', '_')
if ([string]::IsNullOrWhiteSpace($safePackage) -or [string]::IsNullOrWhiteSpace($safeVersion)) {
    throw 'ConPTY manifest package and version must be non-empty'
}

$localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
if ([string]::IsNullOrWhiteSpace($localAppData)) {
    throw 'LocalApplicationData is unavailable; cannot create a safe ConPTY package cache'
}

$cacheRoot = [IO.Path]::GetFullPath((Join-Path $localAppData 'fesTerm\conpty'))
$repositoryRootWithSeparator = $repositoryRoot.TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
if ($cacheRoot.StartsWith($repositoryRootWithSeparator, [StringComparison]::OrdinalIgnoreCase)) {
    throw "ConPTY cache must not be inside the repository: $cacheRoot"
}

$cacheKey = '{0}-{1}-{2}' -f $safePackage, $safeVersion, $archiveSha512.Substring(0, 16)
$cacheDirectory = Join-Path $cacheRoot $cacheKey
$archivePath = Join-Path $cacheDirectory 'package.nupkg'
$packageDirectory = Join-Path $cacheDirectory 'package'

New-Item -ItemType Directory -Force -Path $cacheDirectory | Out-Null
if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
    try {
        Assert-Sha512 -Path $archivePath -Expected $archiveSha512 -Description 'Cached ConPTY package archive'
        Write-Host "Using verified ConPTY package cache: $archivePath"
    }
    catch {
        Remove-Item -LiteralPath $archivePath -Force
    }
}

if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
    $downloadPath = Join-Path $cacheDirectory ("package.{0}.download" -f $PID)
    try {
        Write-Host "Downloading pinned ConPTY package $($manifest.package) $($manifest.version)"
        Invoke-WebRequest -UseBasicParsing -Uri $sourceUri.AbsoluteUri -OutFile $downloadPath
        Assert-Sha512 -Path $downloadPath -Expected $archiveSha512 -Description 'Downloaded ConPTY package archive'
        Move-Item -LiteralPath $downloadPath -Destination $archivePath -Force
    }
    finally {
        if (Test-Path -LiteralPath $downloadPath -PathType Leaf) {
            Remove-Item -LiteralPath $downloadPath -Force
        }
    }
}

Assert-Sha512 -Path $archivePath -Expected $archiveSha512 -Description 'ConPTY package archive'

$dllRelativePath = 'runtimes\win-x64\native\conpty.dll'
$hostRelativePath = 'build\native\runtimes\x64\OpenConsole.exe'
$dllSource = Join-Path $packageDirectory $dllRelativePath
$hostSource = Join-Path $packageDirectory $hostRelativePath

try {
    Assert-Sha512 -Path $dllSource -Expected $dllSha512 -Description 'Extracted win-x64 conpty.dll'
    Assert-Sha512 -Path $hostSource -Expected $hostSha512 -Description 'Extracted x64 OpenConsole.exe'
    Write-Host "Using verified extracted ConPTY package cache: $packageDirectory"
}
catch {
    if (Test-Path -LiteralPath $packageDirectory) {
        Remove-Item -LiteralPath $packageDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $packageDirectory | Out-Null
    Expand-Archive -LiteralPath $archivePath -DestinationPath $packageDirectory -Force
    Assert-Sha512 -Path $dllSource -Expected $dllSha512 -Description 'Extracted win-x64 conpty.dll'
    Assert-Sha512 -Path $hostSource -Expected $hostSha512 -Description 'Extracted x64 OpenConsole.exe'
}

Push-Location $repositoryRoot
try {
    & cargo build --workspace
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --workspace failed with exit code $LASTEXITCODE"
    }

    foreach ($binaryDirectory in @('target\debug', 'target\debug\deps')) {
        $runtimeDirectory = Join-Path $repositoryRoot ($binaryDirectory + '\runtime\conpty\win-x64')
        $hostDirectory = Join-Path $runtimeDirectory 'x64'
        New-Item -ItemType Directory -Force -Path $hostDirectory | Out-Null

        $stagedDll = Join-Path $runtimeDirectory 'conpty.dll'
        $stagedHost = Join-Path $hostDirectory 'OpenConsole.exe'
        Copy-Item -LiteralPath $dllSource -Destination $stagedDll -Force
        Copy-Item -LiteralPath $hostSource -Destination $stagedHost -Force
        Assert-Sha512 -Path $stagedDll -Expected $dllSha512 -Description "Staged $binaryDirectory conpty.dll"
        Assert-Sha512 -Path $stagedHost -Expected $hostSha512 -Description "Staged $binaryDirectory OpenConsole.exe"
    }

    Write-Host 'Staged verified x64 ConPTY runtime in target\debug and target\debug\deps.'

    if ($RunSmoke) {
        & cargo test -p festerm windows_conpty_smoke_flow_with_test_child_and_issue3_resizes -- --include-ignored --nocapture
        if ($LASTEXITCODE -ne 0) {
            throw "Pinned Windows ConPTY retention smoke failed with exit code $LASTEXITCODE"
        }
    }
}
finally {
    Pop-Location
}
