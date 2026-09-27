#requires -Version 5.1
[CmdletBinding()]
param(
    [switch]$ProvisionIsolatedRunner,
    [string]$ResultPath = "psrp-interop-result.txt"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$interopEnvNames = @(
    'FESTERM_PSRP_INTEROP_HOST',
    'FESTERM_PSRP_INTEROP_PORT',
    'FESTERM_PSRP_INTEROP_USER',
    'FESTERM_PSRP_INTEROP_DOMAIN',
    'FESTERM_PSRP_INTEROP_PASSWORD',
    'FESTERM_PSRP_INTEROP_CA_PEM',
    'FESTERM_PSRP_INTEROP_CA_PEM_PATH'
)

function Write-Result([string]$Line) {
    Set-Content -LiteralPath $ResultPath -Value $Line -Encoding ASCII
}

function Write-SanitizedDiagnostic([string]$Category, $ErrorRecord) {
    $exceptionType = if ($ErrorRecord -and $ErrorRecord.Exception) { $ErrorRecord.Exception.GetType().FullName } else { 'unknown' }
    $line = if ($ErrorRecord -and $ErrorRecord.InvocationInfo) { $ErrorRecord.InvocationInfo.ScriptLineNumber } else { 0 }
    $hresult = if ($ErrorRecord -and $ErrorRecord.Exception) { $ErrorRecord.Exception.HResult } else { 0 }
    [Console]::Error.WriteLine("psrp-interop diagnostic category=$Category exception_type=$exceptionType line=$line hresult=$hresult")
    if ($ErrorRecord -and $ErrorRecord.Exception) {
        $message = $ErrorRecord.Exception.Message
        if ($password) { $message = $message.Replace($password, '[redacted]') }
        $message = [regex]::Replace($message, '[\x00-\x1f\x7f]', ' ')
        [Console]::Error.WriteLine($message.Substring(0, [Math]::Min(1024, $message.Length)))
    }
}

function Add-CleanupError([System.Collections.Generic.List[string]]$Errors, [string]$Step, [scriptblock]$Action) {
    try {
        & $Action
    } catch {
        $Errors.Add($Step)
        Write-SanitizedDiagnostic "cleanup-$Step" $_
    }
}

function Assert-GitHubHostedWindowsRunner {
    if ($env:OS -ne 'Windows_NT') {
        throw 'windows-hosted-runner-required'
    }
    if ($env:GITHUB_ACTIONS -ne 'true' -or $env:RUNNER_ENVIRONMENT -ne 'github-hosted') {
        throw 'github-hosted-runner-required'
    }
    if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
        throw 'global-opt-in-required'
    }
    if (-not $ProvisionIsolatedRunner) {
        throw 'isolated-runner-provisioning-not-requested'
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'elevation-required'
    }
}

function New-RandomPassword {
    $bytes = [byte[]]::new(24)
    $rng = [Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $rng.GetBytes($bytes)
    } finally {
        $rng.Dispose()
    }
    $base = [Convert]::ToBase64String($bytes).TrimEnd('=')
    return "Fes1!$base"
}

function Export-CertificatePem([System.Security.Cryptography.X509Certificates.X509Certificate2]$Certificate, [string]$Path) {
    $der = $Certificate.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert)
    $base64 = [Convert]::ToBase64String($der)
    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add('-----BEGIN CERTIFICATE-----')
    for ($i = 0; $i -lt $base64.Length; $i += 64) {
        $lines.Add($base64.Substring($i, [Math]::Min(64, $base64.Length - $i)))
    }
    $lines.Add('-----END CERTIFICATE-----')
    Set-Content -LiteralPath $Path -Value $lines -Encoding ASCII
}

function Add-EndpointExecuteAce([string]$Sddl, [Security.Principal.SecurityIdentifier]$Sid) {
    $descriptor = [Security.AccessControl.CommonSecurityDescriptor]::new($false, $false, $Sddl)
    $descriptor.DiscretionaryAcl.AddAccess(
        [Security.AccessControl.AccessControlType]::Allow,
        $Sid,
        0x20000000,
        [Security.AccessControl.InheritanceFlags]::None,
        [Security.AccessControl.PropagationFlags]::None
    )
    return $descriptor.GetSddlForm([Security.AccessControl.AccessControlSections]::All)
}

function Save-InteropEnvironment {
    $saved = @{}
    foreach ($name in $interopEnvNames) {
        $path = "Env:\$name"
        if (Test-Path $path) {
            $saved[$name] = @{ Exists = $true; Value = (Get-Item $path).Value }
        } else {
            $saved[$name] = @{ Exists = $false; Value = $null }
        }
    }
    return $saved
}

function Restore-InteropEnvironment($Saved) {
    foreach ($name in $interopEnvNames) {
        $path = "Env:\$name"
        if ($Saved[$name].Exists) {
            Set-Item -Path $path -Value $Saved[$name].Value
        } else {
            Remove-Item -Path $path -ErrorAction Ignore
        }
    }
}

function Restore-WinRmStartupType([string]$StartMode) {
    switch ($StartMode) {
        'Auto' { Set-Service WinRM -StartupType Automatic }
        'Manual' { Set-Service WinRM -StartupType Manual }
        'Disabled' { Set-Service WinRM -StartupType Disabled }
        default { throw "unsupported-original-start-mode" }
    }
}

Assert-GitHubHostedWindowsRunner

$originalServiceWasRunning = ((Get-Service WinRM).Status -eq 'Running')
$originalStartMode = (Get-CimInstance Win32_Service -Filter "Name='WinRM'").StartMode
$startupTypeChanged = $false
$createdUser = $false
$caCert = $null
$leafCert = $null
$rootStoreAdded = $false
$listenerCreationAttempted = $false
$originalEndpointSddl = $null
$scriptFailure = $null
$failureReason = 'setup-failed'
$testSucceeded = $false
$workDir = $null
$caPemPath = $null
$userName = $null
$password = $null
$securePassword = $null
$cleanupErrors = [System.Collections.Generic.List[string]]::new()
$envSnapshot = Save-InteropEnvironment
$wsmanCaptured = $false
$originalIPv4Filter = $null
$originalIPv6Filter = $null
$originalAllowUnencrypted = $null
$originalBasic = $null
$originalCredSsp = $null
$originalNegotiate = $null

try {
    Write-Result 'status=running'

    if ($originalStartMode -eq 'Disabled') {
        Set-Service WinRM -StartupType Manual
        $startupTypeChanged = $true
    }
    if (-not $originalServiceWasRunning) {
        Start-Service WinRM
    }

    $failureReason = 'read-wsman-settings-failed'
    $originalIPv4Filter = (Get-Item WSMan:\localhost\Service\IPv4Filter).Value
    $originalIPv6Filter = (Get-Item WSMan:\localhost\Service\IPv6Filter).Value
    $originalAllowUnencrypted = (Get-Item WSMan:\localhost\Service\AllowUnencrypted).Value
    $originalBasic = (Get-Item WSMan:\localhost\Service\Auth\Basic).Value
    $originalCredSsp = (Get-Item WSMan:\localhost\Service\Auth\CredSSP).Value
    $originalNegotiate = (Get-Item WSMan:\localhost\Service\Auth\Negotiate).Value
    $wsmanCaptured = $true

    $existingLoopbackHttps = Get-ChildItem WSMan:\localhost\Listener | Where-Object {
        $_.Keys -contains 'Transport=HTTPS' -and $_.Keys -contains 'Address=IP:127.0.0.1'
    }
    if ($existingLoopbackHttps) { throw 'loopback-https-listener-already-exists' }

    $nonce = [Guid]::NewGuid().ToString('N')
    $workDir = Join-Path $env:RUNNER_TEMP "festerm-psrp-interop-$nonce"
    New-Item -ItemType Directory -Path $workDir -Force | Out-Null
    $caPemPath = Join-Path $workDir 'festerm-psrp-ca.pem'
    $userName = "fespsrp$($nonce.Substring(0, 12))"
    $password = New-RandomPassword
    Write-Host "::add-mask::$password"

    $failureReason = 'restrict-listeners-failed'
    Set-Item WSMan:\localhost\Service\AllowUnencrypted $false
    Set-Item WSMan:\localhost\Service\Auth\Basic $false
    Set-Item WSMan:\localhost\Service\Auth\CredSSP $false
    Set-Item WSMan:\localhost\Service\Auth\Negotiate $true
    Set-Item WSMan:\localhost\Service\IPv4Filter '127.0.0.1-127.0.0.1'
    Set-Item WSMan:\localhost\Service\IPv6Filter '::1-::1'

    $failureReason = 'create-account-failed'
    $securePassword = ConvertTo-SecureString $password -AsPlainText -Force
    New-LocalUser -Name $userName -Password $securePassword -PasswordNeverExpires -UserMayNotChangePassword -AccountNeverExpires | Out-Null
    $createdUser = $true
    Add-LocalGroupMember -Group 'Remote Management Users' -Member $userName
    $sid = (Get-LocalUser -Name $userName).Sid

    $failureReason = 'create-certificates-failed'
    $caCert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject "CN=fesTerm PSRP Interop CA $nonce" `
        -CertStoreLocation Cert:\LocalMachine\My `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeyExportPolicy NonExportable `
        -KeyUsage CertSign, CRLSign, DigitalSignature `
        -TextExtension @('2.5.29.19={critical}{text}ca=1&pathlength=0') `
        -NotAfter (Get-Date).AddHours(4)

    $leafCert = New-SelfSignedCertificate `
        -Type Custom `
        -Subject "CN=127.0.0.1" `
        -CertStoreLocation Cert:\LocalMachine\My `
        -Signer $caCert `
        -KeyAlgorithm RSA `
        -KeyLength 2048 `
        -KeyExportPolicy NonExportable `
        -KeyUsage DigitalSignature, KeyEncipherment `
        -TextExtension @('2.5.29.17={text}IPAddress=127.0.0.1&IPAddress=::1', '2.5.29.37={text}1.3.6.1.5.5.7.3.1') `
        -NotAfter $caCert.NotAfter

    $store = [System.Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
    $store.Open([System.Security.Cryptography.X509Certificates.OpenFlags]::ReadWrite)
    try { $store.Add($caCert); $rootStoreAdded = $true } finally { $store.Close() }
    Export-CertificatePem -Certificate $caCert -Path $caPemPath

    $failureReason = 'create-listener-failed'
    $listenerCreationAttempted = $true
    New-WSManInstance -ResourceURI 'winrm/config/Listener' `
        -SelectorSet @{ Address = 'IP:127.0.0.1'; Transport = 'HTTPS' } `
        -ValueSet @{ Hostname = '127.0.0.1'; CertificateThumbprint = $leafCert.Thumbprint; Enabled = $true; Port = 5986 } | Out-Null
    $verified = Get-ChildItem WSMan:\localhost\Listener | Where-Object {
        $_.Keys -contains 'Transport=HTTPS' -and $_.Keys -contains 'Address=IP:127.0.0.1'
    }
    if (-not $verified) { throw 'created-listener-verification-failed' }

    $failureReason = 'configure-endpoint-failed'
    $endpoint = Get-PSSessionConfiguration -Name Microsoft.PowerShell
    $originalEndpointSddl = $endpoint.SecurityDescriptorSddl
    $updatedSddl = Add-EndpointExecuteAce -Sddl $originalEndpointSddl -Sid $sid
    Set-PSSessionConfiguration -Name Microsoft.PowerShell -SecurityDescriptorSddl $updatedSddl -Force | Out-Null

    $env:FESTERM_PSRP_INTEROP_HOST = '127.0.0.1'
    $env:FESTERM_PSRP_INTEROP_PORT = '5986'
    $env:FESTERM_PSRP_INTEROP_USER = $userName
    $env:FESTERM_PSRP_INTEROP_DOMAIN = $env:COMPUTERNAME
    $env:FESTERM_PSRP_INTEROP_PASSWORD = $password
    Remove-Item Env:\FESTERM_PSRP_INTEROP_CA_PEM -ErrorAction Ignore
    $env:FESTERM_PSRP_INTEROP_CA_PEM_PATH = $caPemPath

    $failureReason = 'test-failed'
    cargo test -p festerm-powershell --test psrp_interop --locked -- --ignored --test-threads=1 --nocapture
    if ($LASTEXITCODE -ne 0) { throw 'cargo-test-failed' }
    $testSucceeded = $true
} catch {
    $scriptFailure = $_
    Write-SanitizedDiagnostic $failureReason $_
} finally {
    Add-CleanupError $cleanupErrors 'restore-environment' { Restore-InteropEnvironment $envSnapshot }
    if ($securePassword) { $securePassword.Dispose() }

    if ($createdUser) {
        Add-CleanupError $cleanupErrors 'remove-local-user' { Remove-LocalUser -Name $userName -ErrorAction Stop }
    }
    if ($originalEndpointSddl) {
        Add-CleanupError $cleanupErrors 'restore-endpoint-sddl' {
            Set-PSSessionConfiguration -Name Microsoft.PowerShell -SecurityDescriptorSddl $originalEndpointSddl -Force | Out-Null
        }
    }
    if ($listenerCreationAttempted) {
        Add-CleanupError $cleanupErrors 'remove-loopback-listener' {
            $ownedListener = Get-ChildItem WSMan:\localhost\Listener | Where-Object {
                $_.Keys -contains 'Transport=HTTPS' -and $_.Keys -contains 'Address=IP:127.0.0.1'
            }
            if ($ownedListener) {
                $thumbprint = (Get-Item -LiteralPath "$($ownedListener.PSPath)\CertificateThumbprint").Value
                if ($thumbprint -ne $leafCert.Thumbprint) { throw 'listener-identity-changed' }
                Remove-WSManInstance -ResourceURI 'winrm/config/Listener' `
                    -SelectorSet @{ Address = 'IP:127.0.0.1'; Transport = 'HTTPS' } | Out-Null
            }
        }
    }
    if ($wsmanCaptured) {
        Add-CleanupError $cleanupErrors 'restore-ipv4-filter' { Set-Item WSMan:\localhost\Service\IPv4Filter $originalIPv4Filter }
        Add-CleanupError $cleanupErrors 'restore-ipv6-filter' { Set-Item WSMan:\localhost\Service\IPv6Filter $originalIPv6Filter }
        Add-CleanupError $cleanupErrors 'restore-allow-unencrypted' { Set-Item WSMan:\localhost\Service\AllowUnencrypted $originalAllowUnencrypted }
        Add-CleanupError $cleanupErrors 'restore-basic-auth' { Set-Item WSMan:\localhost\Service\Auth\Basic $originalBasic }
        Add-CleanupError $cleanupErrors 'restore-credssp-auth' { Set-Item WSMan:\localhost\Service\Auth\CredSSP $originalCredSsp }
        Add-CleanupError $cleanupErrors 'restore-negotiate-auth' { Set-Item WSMan:\localhost\Service\Auth\Negotiate $originalNegotiate }
    }
    if ($rootStoreAdded -and $caCert) {
        Add-CleanupError $cleanupErrors 'remove-root-ca' { Remove-Item -LiteralPath "Cert:\LocalMachine\Root\$($caCert.Thumbprint)" -Force -ErrorAction Stop }
    }
    if ($leafCert) {
        Add-CleanupError $cleanupErrors 'remove-leaf-cert' { Remove-Item -LiteralPath "Cert:\LocalMachine\My\$($leafCert.Thumbprint)" -DeleteKey -Force -ErrorAction Stop }
    }
    if ($caCert) {
        Add-CleanupError $cleanupErrors 'remove-personal-ca' { Remove-Item -LiteralPath "Cert:\LocalMachine\My\$($caCert.Thumbprint)" -DeleteKey -Force -ErrorAction Stop }
    }
    if ($workDir) {
        Add-CleanupError $cleanupErrors 'remove-work-dir' { Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction Stop }
    }
    if (-not $originalServiceWasRunning) {
        Add-CleanupError $cleanupErrors 'restore-stopped-service' { Stop-Service WinRM -Force -ErrorAction Stop }
    }
    if ($startupTypeChanged) {
        Add-CleanupError $cleanupErrors 'restore-startup-type' { Restore-WinRmStartupType $originalStartMode }
    }

    if ($cleanupErrors.Count -gt 0) {
        Write-Result 'status=fail reason=cleanup-failed'
        exit 1
    }
    if ($scriptFailure) {
        Write-Result "status=fail reason=$failureReason"
        exit 1
    }
    if ($testSucceeded) {
        Write-Result 'status=pass'
    }
}
