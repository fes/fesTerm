$ErrorActionPreference = "Stop"

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$sandboxRoot = Join-Path $repositoryRoot "target/festerm-dev"
$stateRoot = Join-Path $sandboxRoot "state"

New-Item -ItemType Directory -Force -Path $stateRoot | Out-Null
$env:FESTERM_CONFIG_PATH = Join-Path $sandboxRoot "config.toml"
$env:LOCALAPPDATA = $stateRoot

Push-Location $repositoryRoot
try {
    cargo build -p festerm-sessiond
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    cargo run -p festerm -- @args
    exit $LASTEXITCODE
}
finally {
    Pop-Location
}
