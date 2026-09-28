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
    'FESTERM_PSRP_INTEROP_CA_PEM_PATH',
    'FESTERM_PSRP_INTEROP_PS7_CONFIG',
    'FESTERM_PSRP_INTEROP_PS7_REQUIRED'
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

function Invoke-BoundedProcess([string]$FilePath, [string[]]$ArgumentList, [int]$TimeoutMs, [string]$WorkDir) {
    # Run a child process under a hard deadline with sanitized, capped output so
    # the harness never depends on the workflow timeout as its only bound and
    # never leaks unbounded or secret-bearing child output.
    $stdoutPath = Join-Path $WorkDir ("proc-out-" + [Guid]::NewGuid().ToString('N') + '.log')
    $stderrPath = Join-Path $WorkDir ("proc-err-" + [Guid]::NewGuid().ToString('N') + '.log')
    $process = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -PassThru -NoNewWindow `
        -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    # Retain the native handle before waiting: Windows PowerShell's process
    # wrapper can otherwise lose ExitCode after a fast child has exited.
    $process.Handle | Out-Null
    $exitCode = $null
    try {
        if (-not $process.WaitForExit($TimeoutMs)) {
            if (-not $process.HasExited) {
                Stop-Process -Id $process.Id -Force -ErrorAction Stop
            }
            $process.WaitForExit(5000) | Out-Null
            throw 'bounded-process-timeout'
        }
        $exitCode = $process.ExitCode
        if ($null -eq $exitCode) { throw 'bounded-process-exit-code-unavailable' }
        [Console]::Error.WriteLine("psrp-interop child-exit-code=$exitCode")
    } finally {
        foreach ($streamPath in @($stdoutPath, $stderrPath)) {
            if (Test-Path -LiteralPath $streamPath) {
                $reader = [System.IO.StreamReader]::new($streamPath)
                try {
                    $buffer = [char[]]::new(4096)
                    $count = $reader.ReadBlock($buffer, 0, $buffer.Length)
                    $content = [string]::new($buffer, 0, $count)
                } finally {
                    $reader.Dispose()
                }
                if ($content) {
                    if ($password) { $content = $content.Replace($password, '[redacted]') }
                    $content = [regex]::Replace($content, '[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]', ' ')
                    $content = $content.Substring(0, [Math]::Min(1024, $content.Length))
                    [Console]::Error.WriteLine("psrp-interop child-output $content")
                }
                Remove-Item -LiteralPath $streamPath -ErrorAction Ignore
            }
        }
        $process.Dispose()
    }
    return $exitCode
}

function Wait-EndpointReady([string]$ConfigurationName, [string]$WorkDir) {
    if ($ConfigurationName -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,255}$') { throw 'invalid-readiness-configuration' }
    # WSMan operation timeouts do not bound Invoke-Command as a whole. A child
    # deadline bounds connect, execution and cleanup together, before Rust cases.
    $probe = {
        $ErrorActionPreference = 'Stop'
        $name = '__CONFIGURATION__'
        $secure = ConvertTo-SecureString $env:FESTERM_PSRP_INTEROP_PASSWORD -AsPlainText -Force
        $credential = [System.Management.Automation.PSCredential]::new(
            "$($env:COMPUTERNAME)\$($env:FESTERM_PSRP_INTEROP_USER)", $secure)
        $options = New-PSSessionOption -OpenTimeout 15000 -OperationTimeout 15000 -CancelTimeout 5000
        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        while ($clock.ElapsedMilliseconds -lt 110000) {
            $session = $null
            $ready = $false
            try {
                $session = New-PSSession -ComputerName 127.0.0.1 -Port 5986 -UseSSL `
                    -Authentication Negotiate -Credential $credential -ConfigurationName $name `
                    -SessionOption $options
                $edition = Invoke-Command -Session $session -ScriptBlock { $PSVersionTable.PSEdition }
                $ready = -not [string]::IsNullOrWhiteSpace([string]$edition)
            } catch {
                [Console]::Error.WriteLine("readiness attempt failed: $($_.Exception.GetType().Name)")
            } finally {
                if ($session) { Remove-PSSession -Session $session -ErrorAction Stop }
            }
            if ($ready) {
                [Console]::Error.WriteLine("readiness ready configuration=$name edition=$edition")
                exit 0
            }
            Start-Sleep -Milliseconds 2000
        }
        exit 1
    }.ToString().Replace('__CONFIGURATION__', $ConfigurationName)
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
    $exit = Invoke-BoundedProcess -FilePath "$PSHOME\powershell.exe" `
        -ArgumentList @('-NoProfile', '-NonInteractive', '-EncodedCommand', $encoded) `
        -TimeoutMs 120000 -WorkDir $WorkDir
    if ($exit -ne 0) { throw "endpoint-readiness-failed:$ConfigurationName" }
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
$ps7ConfigName = $null
$ps7Registered = $false
$ps7PluginInstalledByUs = $false
$ps7InstalledEndpoints = @()
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
# After snapshotting, clear any ambient PowerShell 7 targeting so setup cannot
# point the required PS7 test at a preexisting endpoint we do not own; only the
# owned provisioning below may set these, and the snapshot is restored on exit.
Remove-Item Env:\FESTERM_PSRP_INTEROP_PS7_CONFIG -ErrorAction Ignore
Remove-Item Env:\FESTERM_PSRP_INTEROP_PS7_REQUIRED -ErrorAction Ignore
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
    # A uniquely named, owned PowerShell 7 session configuration. Using a unique
    # name (rather than the well-known PowerShell.7) proves an arbitrary
    # configured Core 7 endpoint while guaranteeing we never clobber or depend on
    # a preexisting configuration; a name collision below is treated as fatal.
    $ps7ConfigName = "FestermInterop7$($nonce.Substring(0, 12))"
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

    # Register PowerShell 7 as a second explicitly selectable endpoint on the
    # same loopback HTTPS listener. This registers only an owned, uniquely named
    # session configuration (no new listeners, no firewall or profile changes)
    # and is cleaned up by Unregister below. pwsh is expected on GitHub-hosted
    # Windows runners, so its absence or a name collision is a hard failure: the
    # PS7 endpoint is required, never silently skipped to a green result.
    $failureReason = 'provision-powershell7-failed'
    $pwshCommand = Get-Command pwsh -ErrorAction SilentlyContinue
    if (-not $pwshCommand) { throw 'powershell7-required-but-pwsh-missing' }
    $existingPs7 = Get-PSSessionConfiguration -Name $ps7ConfigName -ErrorAction SilentlyContinue
    if ($existingPs7) { throw 'powershell7-owned-config-name-collision' }

    # PowerShell 7 does not register its WinRM plugin by default, so a custom
    # Register-PSSessionConfiguration fails with "pwrshplugin.dll is missing"
    # (observed on hosted runners) unless the install-owned plugin has been
    # placed at %windir%\System32\PowerShell\<version>\pwrshplugin.dll. The
    # canonical, network-free way to do that is the install-owned script
    # Install-PowerShellRemoting.ps1 shipped in pwsh's $PSHOME: it copies the
    # plugin DLL, writes RemotePowerShellConfig.txt, and registers the plugin
    # registry key. It does NOT create listeners, firewall rules, or run
    # Set-WSManQuickConfig, so it is strictly narrower than Enable-PSRemoting and
    # introduces no global network exposure. We run it only if the plugin is not
    # already present (never clobbering a preexisting install) and record every
    # PowerShell.7* endpoint it creates so we can unregister them on cleanup.
    $ps7BasePluginEndpoint = Get-PSSessionConfiguration -Name 'PowerShell.7' -ErrorAction SilentlyContinue
    if (-not $ps7BasePluginEndpoint) {
        $pwshHome = Split-Path -Parent $pwshCommand.Source
        $installScript = Join-Path $pwshHome 'Install-PowerShellRemoting.ps1'
        if (-not (Test-Path -LiteralPath $installScript)) { throw 'powershell7-install-script-missing' }
        $failureReason = 'install-powershell7-plugin-failed'
        $preexistingPs7Endpoints = @(
            Get-PSSessionConfiguration -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -like 'PowerShell.7*' } |
                ForEach-Object { $_.Name }
        )
        # Mark before running so any endpoints it creates are still cleaned up.
        $ps7PluginInstalledByUs = $true
        $installArgs = @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', "`"$installScript`"")
        $installExit = Invoke-BoundedProcess -FilePath $pwshCommand.Source -ArgumentList $installArgs -TimeoutMs 180000 -WorkDir $workDir
        if ($installExit -ne 0) { throw 'install-powershell7-plugin-command-failed' }
        $ps7BasePluginEndpoint = Get-PSSessionConfiguration -Name 'PowerShell.7' -ErrorAction SilentlyContinue
        if (-not $ps7BasePluginEndpoint) { throw 'install-powershell7-plugin-verification-failed' }
        $ps7InstalledEndpoints = @(
            Get-PSSessionConfiguration -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -like 'PowerShell.7*' -and $preexistingPs7Endpoints -notcontains $_.Name } |
                ForEach-Object { $_.Name }
        )
    }

    $failureReason = 'register-powershell7-failed'
    # Mark before registering so a partial registration is still cleaned.
    $ps7Registered = $true
    # Register from pwsh so the endpoint hosts PowerShell 7 itself, via a bounded
    # child process with a hard deadline and sanitized, capped output.
    $registerScript = "`$ErrorActionPreference = 'Stop'; Register-PSSessionConfiguration -Name '$ps7ConfigName' -Force -NoServiceRestart | Out-Null"
    $registerArgs = @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', $registerScript)
    $registerExit = Invoke-BoundedProcess -FilePath $pwshCommand.Source -ArgumentList $registerArgs -TimeoutMs 120000 -WorkDir $workDir
    if ($registerExit -ne 0) { throw 'register-powershell7-command-failed' }
    $registeredPs7 = Get-PSSessionConfiguration -Name $ps7ConfigName -ErrorAction SilentlyContinue
    if (-not $registeredPs7) { throw 'register-powershell7-verification-failed' }
    $ps7Sddl = Add-EndpointExecuteAce -Sddl $registeredPs7.SecurityDescriptorSddl -Sid $sid
    Set-PSSessionConfiguration -Name $ps7ConfigName -SecurityDescriptorSddl $ps7Sddl -Force -NoServiceRestart | Out-Null
    Restart-Service WinRM -Force
    $env:FESTERM_PSRP_INTEROP_PS7_CONFIG = $ps7ConfigName
    $env:FESTERM_PSRP_INTEROP_PS7_REQUIRED = '1'

    $env:FESTERM_PSRP_INTEROP_HOST = '127.0.0.1'
    $env:FESTERM_PSRP_INTEROP_PORT = '5986'
    $env:FESTERM_PSRP_INTEROP_USER = $userName
    $env:FESTERM_PSRP_INTEROP_DOMAIN = $env:COMPUTERNAME
    $env:FESTERM_PSRP_INTEROP_PASSWORD = $password
    Remove-Item Env:\FESTERM_PSRP_INTEROP_CA_PEM -ErrorAction Ignore
    $env:FESTERM_PSRP_INTEROP_CA_PEM_PATH = $caPemPath

    # Gate the native client cases on an explicit, bounded, fully authenticated
    # readiness check for each endpoint that the Rust suite will target: the
    # default Windows PowerShell configuration and the owned Core 7 config. This
    # warms cold WinRM plugins after the service restart so the first client
    # connect does not fail spuriously, while keeping TLS validation intact and
    # never retrying or masking the client test cases themselves.
    $failureReason = 'endpoint-readiness-failed'
    Wait-EndpointReady -ConfigurationName 'Microsoft.PowerShell' -WorkDir $workDir
    Wait-EndpointReady -ConfigurationName $ps7ConfigName -WorkDir $workDir

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
    if ($ps7Registered) {
        Add-CleanupError $cleanupErrors 'unregister-powershell7' {
            if (Get-PSSessionConfiguration -Name $ps7ConfigName -ErrorAction SilentlyContinue) {
                Unregister-PSSessionConfiguration -Name $ps7ConfigName -Force -NoServiceRestart -ErrorAction Stop
            }
        }
    }
    if ($ps7PluginInstalledByUs) {
        $ps7InstalledEndpoints = @(
            Get-PSSessionConfiguration -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -like 'PowerShell.7*' -and $preexistingPs7Endpoints -notcontains $_.Name } |
                ForEach-Object { $_.Name }
        )
        # Unregister every PowerShell.7* endpoint that the install-owned plugin
        # script created (only those not already present beforehand), so the
        # narrowly-scoped plugin install leaves no owned endpoints behind. The
        # copied plugin DLL under %windir%\System32\PowerShell resides on the
        # ephemeral, isolated hosted runner only and is discarded with the VM.
        foreach ($installedEndpoint in $ps7InstalledEndpoints) {
            $endpointName = $installedEndpoint
            Add-CleanupError $cleanupErrors "unregister-$endpointName" {
                if (Get-PSSessionConfiguration -Name $endpointName -ErrorAction SilentlyContinue) {
                    Unregister-PSSessionConfiguration -Name $endpointName -Force -NoServiceRestart -ErrorAction Stop
                }
            }.GetNewClosure()
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
