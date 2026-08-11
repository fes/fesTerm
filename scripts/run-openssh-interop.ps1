[CmdletBinding()]
param(
    [string] $ResultPath = $(if ($env:FESTERM_OPENSSH_INTEROP_RESULT_PATH) {
        $env:FESTERM_OPENSSH_INTEROP_RESULT_PATH
    } else {
        'openssh-interop-result.txt'
    })
)

$containerName = $null
$imageTag = $null
$password = $null

function Write-Result([string] $result) {
    Set-Content -Path $ResultPath -Value $result -NoNewline
}

function Write-Diagnostics {
    Write-Error 'openssh-interop diagnostic=container-log-begin'
    if ($containerName) {
        $logs = (& docker logs --tail 50 $containerName 2>&1 | Out-String)
        if ($password) {
            $logs = $logs -replace [regex]::Escape($password), '[REDACTED]'
        }
        Write-Error $logs.TrimEnd()
    }
    Write-Error 'openssh-interop diagnostic=container-log-end'
}

function Fail([string] $reason) {
    Write-Diagnostics
    Write-Result "status=fail reason=$reason"
    exit 1
}

try {
    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        Write-Result 'status=skipped reason=docker-unavailable'
        exit 0
    }
    & docker info *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Result 'status=skipped reason=docker-unavailable'
        exit 0
    }

    Write-Result 'status=running'
    $nonce = [guid]::NewGuid().ToString('N').Substring(0, 12)
    $containerName = "festerm-openssh-interop-$nonce"
    $imageTag = "festerm-openssh-interop:$nonce"
    $bytes = [byte[]]::new(24)
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
    $password = [Convert]::ToHexString($bytes).ToLowerInvariant()
    $fixturePath = Join-Path $PSScriptRoot '..\tests\openssh'

    & docker build --quiet --tag $imageTag $fixturePath *> $null
    if ($LASTEXITCODE -ne 0) { Fail 'docker-build-failed' }

    $port = $null
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
        $candidatePort = [System.Security.Cryptography.RandomNumberGenerator]::GetInt32(49152, 65536)
        $previousPassword = $env:FESTERM_OPENSSH_PASSWORD
        $env:FESTERM_OPENSSH_PASSWORD = $password
        & docker run --detach --name $containerName --env FESTERM_OPENSSH_PASSWORD `
            -p "127.0.0.1:$candidatePort`:22" $imageTag *> $null
        $runExitCode = $LASTEXITCODE
        if ($null -eq $previousPassword) {
            Remove-Item Env:FESTERM_OPENSSH_PASSWORD -ErrorAction Ignore
        } else {
            $env:FESTERM_OPENSSH_PASSWORD = $previousPassword
        }
        if ($runExitCode -eq 0) {
            $port = $candidatePort
            break
        }
        & docker rm --force $containerName *> $null
    }
    if (-not $port) { Fail 'container-start-failed' }

    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    $ready = $false
    while ([DateTime]::UtcNow -lt $deadline) {
        $health = (& docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' `
            $containerName 2>$null)
        if ($health -eq 'healthy') {
            $mapping = (& docker port $containerName 22/tcp)
            if ($mapping -match ":$port\s*$") {
                $ready = $true
                break
            }
        }
        if ($health -eq 'unhealthy') { Fail 'container-readiness-failed' }
        Start-Sleep -Seconds 1
    }
    if (-not $ready) { Fail 'container-readiness-timed-out' }

    $previousValues = @{
        FESTERM_OPENSSH_HOST = $env:FESTERM_OPENSSH_HOST
        FESTERM_OPENSSH_PORT = $env:FESTERM_OPENSSH_PORT
        FESTERM_OPENSSH_USER = $env:FESTERM_OPENSSH_USER
        FESTERM_OPENSSH_PASSWORD = $env:FESTERM_OPENSSH_PASSWORD
        FESTERM_OPENSSH_CONTAINER_NAME = $env:FESTERM_OPENSSH_CONTAINER_NAME
    }
    $env:FESTERM_OPENSSH_HOST = '127.0.0.1'
    $env:FESTERM_OPENSSH_PORT = $port
    $env:FESTERM_OPENSSH_USER = 'festerm'
    $env:FESTERM_OPENSSH_PASSWORD = $password
    $env:FESTERM_OPENSSH_CONTAINER_NAME = $containerName
    & cargo test -p festerm-ssh --test openssh_interop -- --ignored --test-threads=1
    $testExitCode = $LASTEXITCODE
    foreach ($name in $previousValues.Keys) {
        if ($null -eq $previousValues[$name]) {
            Remove-Item "Env:$name" -ErrorAction Ignore
        } else {
            Set-Item "Env:$name" $previousValues[$name]
        }
    }
    if ($testExitCode -ne 0) { Fail 'cargo-test-failed' }

    Write-Result 'status=pass'
} finally {
    if ($containerName) {
        & docker rm --force $containerName *> $null
    }
    if ($imageTag) {
        & docker image rm --force $imageTag *> $null
    }
}
