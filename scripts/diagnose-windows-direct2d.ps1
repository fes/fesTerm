param(
    [Parameter(Mandatory = $true)]
    [string] $ResultDirectory
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
New-Item -ItemType Directory -Path $ResultDirectory -Force | Out-Null
$started = Get-Date
$messages = @(& cargo test --locked -p festerm-windows-direct2d --lib --no-run --message-format=json --color never)
if ($LASTEXITCODE -ne 0) { throw 'Native test build failed.' }
$executables = @($messages | ForEach-Object {
    $message = $_ | ConvertFrom-Json
    if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.executable) {
        $message.executable
    }
})
if ($executables.Count -ne 1) { throw "Expected one native test binary; found $($executables.Count)." }
$binary = $executables[0]
$debugger = "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\cdb.exe"
@{
    binary = $binary
    os = [System.Environment]::OSVersion.VersionString
    debugger_available = Test-Path -LiteralPath $debugger
} | ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/environment.json"

for ($iteration = 1; $iteration -le 40; $iteration++) {
    $output = "$ResultDirectory/iteration-$iteration.log"
    Write-Host "Native diagnostic iteration $iteration; stop at the first failure."
    & $binary --nocapture 2>&1 | Tee-Object -FilePath $output
    $exitCode = $LASTEXITCODE
    @{ iteration = $iteration; exit_code = $exitCode } |
        ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/iteration-$iteration.json"
    if ($exitCode -ne 0) {
        # Query only crash records for this synthetic test executable.
        $events = @(Get-WinEvent -FilterHashtable @{
            LogName = 'Application'
            StartTime = $started
            Id = 1000, 1001
        } -ErrorAction Continue | Where-Object {
            $_.Message -like "*$(Split-Path -Leaf $binary)*"
        })
        $events | Select-Object TimeCreated, Id, Message |
            ConvertTo-Json -Depth 4 | Set-Content -LiteralPath "$ResultDirectory/crash-events.json"
        if (Test-Path -LiteralPath $debugger) {
            & $debugger -g -G -o -logo "$ResultDirectory/debugger.log" `
                -c 'sxe -c ".echo NATIVE_ACCESS_VIOLATION; .exr -1; .ecxr; kv; ~* k 20; lm; q" av; g' `
                $binary --nocapture
            $debuggerExit = $LASTEXITCODE
            @{ debugger_exit_code = $debuggerExit } |
                ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/debugger-exit.json"
        } else {
            Write-Warning 'CDB is unavailable; crash events and stage logs are preserved.'
        }
        throw "Native test iteration $iteration failed with exit $exitCode; no green rerun replaces it."
    }
}
