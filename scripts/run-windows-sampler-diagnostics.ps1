[CmdletBinding()]
param(
    [string] $OutputDirectory = (Join-Path (Split-Path $PSScriptRoot -Parent) 'target\windows-sampler-diagnostics'),
    [ValidateRange(1, 32)][int] $Threads = 16,
    [ValidateRange(1, 128)][int] $Rounds = 32
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ($env:OS -ne 'Windows_NT' -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
    throw 'Sampler diagnostics require Windows x64.'
}
if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1' -or
    $env:FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 and FESTERM_RUN_WINDOWS_SAMPLER_DIAGNOSTICS=1.'
}

$names = @('FESTERM_SAMPLER_THREADS', 'FESTERM_SAMPLER_ROUNDS', 'FESTERM_SAMPLER_INSTANCE', 'FESTERM_SAMPLER_WORKLOAD')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$failure = 0
try {
    $env:FESTERM_SAMPLER_THREADS = "$Threads"
    $env:FESTERM_SAMPLER_ROUNDS = "$Rounds"
    foreach ($workload in @('sampler', 'mixed')) {
        foreach ($mode in @('independent', 'shared')) {
            $env:FESTERM_SAMPLER_INSTANCE = $mode
            $env:FESTERM_SAMPLER_WORKLOAD = $workload
            Write-Host "sampler-diagnostic workload=$workload mode=$mode threads=$Threads rounds=$Rounds"
            & "$PSScriptRoot\run-windows-tests.ps1" `
                -OutputDirectory (Join-Path $OutputDirectory "$workload-$mode") `
                -TimeoutSeconds 600 `
                -CargoArguments @(
                    'test', '--locked', '-p', 'festerm', '--test', 'windows_sampler_lifecycles',
                    '--', 'concurrent_sampler_device_lifecycles', '--ignored', '--exact',
                    '--nocapture', '--test-threads=1'
                )
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "Sampler diagnostic $workload/$mode failed with exit code $LASTEXITCODE."
                if ($failure -eq 0) { $failure = $LASTEXITCODE }
            }
        }
    }
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
exit $failure
