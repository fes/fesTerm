[CmdletBinding()]
param(
    [Parameter(Mandatory)][string[]] $CargoArguments,
    [string] $OutputDirectory = (Join-Path (Split-Path $PSScriptRoot -Parent) 'target\windows-test-diagnostics'),
    [ValidateRange(1, 14400)][int] $TimeoutSeconds = 1800
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:OS -ne 'Windows_NT') { throw 'This test runner requires Windows.' }
$runner = @(
    (Get-Command python -ErrorAction Stop).Source,
    (Join-Path $PSScriptRoot 'windows_test_runner.py'),
    '--output-root', [IO.Path]::GetFullPath($OutputDirectory),
    '--timeout-seconds', "$TimeoutSeconds", '--'
)
# A TOML array (JSON string escaping is compatible) retains paths with spaces;
# Cargo executes the runner in each package's original working directory.
$configuration = 'target.x86_64-pc-windows-msvc.runner=' + (ConvertTo-Json -InputObject $runner -Compress)
& cargo --config $configuration @CargoArguments
exit $LASTEXITCODE
