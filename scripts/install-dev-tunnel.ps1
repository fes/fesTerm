[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('\A[0-9]+\.[0-9]+\.[0-9]+\z')]
    [string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if ([Security.Principal.WindowsIdentity]::GetCurrent().IsSystem) {
    throw 'Run setup as your normal user, not SYSTEM.'
}
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Use an unelevated PowerShell window.'
}
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
switch ($architecture) {
    'X64' { $arch = 'x86_64' }
    'Arm64' {
        $arch = 'aarch64'
        Write-Host 'The official Dev Tunnels CLI uses Windows x64 emulation; fesTerm helpers are native ARM64.'
    }
    default { throw "Unsupported Windows architecture: $architecture" }
}
$curl = (Get-Command curl.exe -CommandType Application -ErrorAction Stop).Source
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Get-Download([string]$Url, [string]$Path, [long]$Limit) {
    & $curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' `
        --connect-timeout 20 --max-time 180 --max-filesize $Limit $Url -o $Path
    if ($LASTEXITCODE -ne 0) { throw 'Authenticated HTTPS download failed.' }
}

function New-PrivateDirectory([string]$Path) {
    if (Test-Path -LiteralPath $Path) { throw "Refusing an existing staging directory: $Path" }
    $directory = New-Item -ItemType Directory -Path $Path
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $rule = New-Object Security.AccessControl.FileSystemAccessRule(
        $sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $directory.FullName -AclObject $acl
    return $directory.FullName
}

function Assert-PrivateDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'Installation directory is substituted or not a directory.'
    }
    $acl = Get-Acl -LiteralPath $Path
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
    if ($acl.GetOwner([Security.Principal.SecurityIdentifier]) -ne $sid -or -not $acl.AreAccessRulesProtected) {
        throw 'Installation directory is not protected for this user.'
    }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference -ne $sid) {
            throw 'Installation directory grants another account access.'
        }
    }
}

$local = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
$root = Join-Path $local 'fesTerm-tunnels'
if (-not (Test-Path -LiteralPath $root)) { New-PrivateDirectory $root | Out-Null }
Assert-PrivateDirectory $root
$releases = Join-Path $root 'releases'
if (-not (Test-Path -LiteralPath $releases)) { New-PrivateDirectory $releases | Out-Null }
Assert-PrivateDirectory $releases
$install = New-PrivateDirectory (Join-Path $root ("staging-" + [Guid]::NewGuid().ToString('N')))
$archivePath = Join-Path $install 'bundle.zip'
$checksumPath = Join-Path $install 'bundle.sha256'
$asset = "fesTerm-tunnel-$Version-windows-$arch.zip"
$base = "https://github.com/fes/fesTerm/releases/download/v$Version"
try {
    Get-Download "$base/$asset.sha256" $checksumPath 1024
    $checksum = [IO.File]::ReadAllText($checksumPath)
    if ($checksum -cnotmatch ('\A([a-f0-9]{64})  ' + [regex]::Escape($asset) + '\n\z')) {
        throw 'Checksum does not name this exact release.'
    }
    $expected = $Matches[1]
    Get-Download "$base/$asset" $archivePath 536870912
    if ((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $expected) {
        throw 'Helper archive SHA-256 mismatch; nothing executed.'
    }
    $allowed = @('LICENSE', 'THIRD_PARTY_NOTICES.txt', 'VERSION',
        'festerm-sessiond.exe', 'festerm-tunnel-bridge.exe', 'festerm-tunnel-setup.exe')
    $seen = New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        if ($zip.Entries.Count -ne $allowed.Count) { throw 'Wrong helper archive entry count.' }
        foreach ($entry in $zip.Entries) {
            if ($allowed -cnotcontains $entry.FullName -or -not $seen.Add($entry.FullName) -or
                $entry.Length -gt 134217728 -or (($entry.ExternalAttributes -shr 16) -band 0xF000) -ne 0x8000) {
                throw 'Helper archive contains an unexpected, duplicated, oversized or non-regular entry.'
            }
        }
    } finally { $zip.Dispose() }
    $helpers = New-PrivateDirectory (Join-Path $install 'helpers')
    [IO.Compression.ZipFile]::ExtractToDirectory($archivePath, $helpers)
    if ([IO.File]::ReadAllText((Join-Path $helpers 'VERSION')) -cne "$Version`n") {
        throw 'Bundle version mismatch.'
    }
    foreach ($name in @('festerm-sessiond.exe', 'festerm-tunnel-bridge.exe', 'festerm-tunnel-setup.exe')) {
        if ((Get-AuthenticodeSignature -LiteralPath (Join-Path $helpers $name)).Status -ne 'Valid') {
            throw "Helper is not Authenticode-valid: $name"
        }
    }
    $cli = Join-Path $helpers 'devtunnel.exe'
    $cached = Join-Path $releases "$Version-windows-$arch"
    if (Test-Path -LiteralPath $cached) {
        Assert-PrivateDirectory $cached
        $cachedCli = Join-Path $cached 'devtunnel.exe'
        if (-not (Test-Path -LiteralPath $cachedCli -PathType Leaf) -or
            ((Get-Item -LiteralPath $cachedCli).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Cached official CLI is missing or substituted.'
        }
        Copy-Item -LiteralPath $cachedCli -Destination $cli
    } else {
        Get-Download 'https://aka.ms/TunnelsCliDownload/win-x64' $cli 134217728
    }
    if ((Get-FileHash -LiteralPath $cli -Algorithm SHA256).Hash.ToLowerInvariant() -cne
        'f11c797e90397a0a8e9bee711d500473632be27839deeb4fb3d0f5dbc6ffd2dc') {
        throw 'Official Dev Tunnels CLI hash changed; update reviewed pins before continuing.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $cli
    if ($signature.Status -ne 'Valid' -or
        $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -ne 'Microsoft Corporation') {
        throw 'Official Dev Tunnels CLI does not have a valid Microsoft Authenticode signature.'
    }
    & $cli --version
    if ($LASTEXITCODE -ne 0) { throw 'Official Dev Tunnels CLI runtime prerequisites are unavailable.' }
    if (Test-Path -LiteralPath $cached) {
        Assert-PrivateDirectory $cached
        foreach ($member in @($allowed) + @('devtunnel.exe')) {
            $existing = Join-Path $cached $member
            if (-not (Test-Path -LiteralPath $existing -PathType Leaf) -or
                ((Get-Item -LiteralPath $existing).Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                (Get-FileHash -LiteralPath $existing).Hash -cne (Get-FileHash -LiteralPath (Join-Path $helpers $member)).Hash) {
                throw 'Cached release differs from verified release; refusing to overwrite live helpers.'
            }
        }
        foreach ($member in @($allowed) + @('devtunnel.exe')) {
            Remove-Item -LiteralPath (Join-Path $helpers $member)
        }
        [IO.Directory]::Delete($helpers)
    } else {
        [IO.Directory]::Move($helpers, $cached)
    }
    $cli = Join-Path $cached 'devtunnel.exe'
    Write-Host "Installed private helpers: $cached"
    & (Join-Path $cached 'festerm-tunnel-setup.exe') --managed --devtunnel $cli --state-dir (Join-Path $root 'state')
    if ($LASTEXITCODE -ne 0) { throw 'Experimental tunnel setup refused/stopped.' }
} finally {
    foreach ($temporaryFile in @($archivePath, $checksumPath)) {
        if (Test-Path -LiteralPath $temporaryFile) {
            Remove-Item -LiteralPath $temporaryFile -Force
        }
        if ((Get-ChildItem -LiteralPath $install -Force | Measure-Object).Count -eq 0) {
            [IO.Directory]::Delete($install)
        }
    }
}
