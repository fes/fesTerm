param(
    [Parameter(Mandatory = $true)]
    [string] $ResultDirectory,
    [ValidateSet('prefix', 'lifecycle')]
    [string] $Mode = 'prefix'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
New-Item -ItemType Directory -Path $ResultDirectory -Force | Out-Null
$started = Get-Date
$messages = @(& cargo test --locked -p festerm-windows-direct2d -p festerm-ui-egui --lib --no-run --message-format=json --color never --config 'profile.test.package.festerm-windows-direct2d.debug=2')
if ($LASTEXITCODE -ne 0) { throw 'Native test build failed.' }
$executables = @($messages | ForEach-Object {
    $message = $_ | ConvertFrom-Json
    if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.executable) {
        [PSCustomObject] @{ name = $message.target.name; path = $message.executable }
    }
})
if ($executables.Count -ne 2) { throw "Expected two test binaries; found $($executables.Count)." }
$binary = ($executables | Where-Object { $_.name -eq 'festerm_windows_direct2d' }).path
$prefix = ($executables | Where-Object { $_.name -eq 'festerm_ui_egui' }).path
$debugger = "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\cdb.exe"
@{
    binary = $binary
    os = [System.Environment]::OSVersion.VersionString
    debugger_available = Test-Path -LiteralPath $debugger
} | ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/environment.json"
if (-not (Test-Path -LiteralPath $debugger)) { throw 'The SDK debugger is required for this diagnostic phase.' }
$env:_NT_SYMBOL_PATH = "srv*$env:RUNNER_TEMP/festerm-symbols*https://msdl.microsoft.com/download/symbols;$(Split-Path -Parent $binary)"
Remove-Item Env:FESTERM_TRACE_NATIVE_TESTS

$iterations = if ($Mode -eq 'lifecycle') { 1 } else { 40 }
for ($iteration = 1; $iteration -le $iterations; $iteration++) {
    $output = "$ResultDirectory/iteration-$iteration.log"
    Write-Host "UI prefix plus debugger-native iteration $iteration; stop at the first failure."
    $testArguments = @('--nocapture')
    if ($Mode -eq 'prefix') {
        Push-Location (Join-Path $env:GITHUB_WORKSPACE 'crates/festerm-ui-egui')
        try {
            & $prefix --nocapture 2>&1 | Tee-Object -FilePath "$ResultDirectory/ui-prefix-$iteration.log"
            $prefixExit = $LASTEXITCODE
        } finally {
            Pop-Location
        }
        if ($prefixExit -ne 0) { throw "The UI prefix itself failed in iteration $iteration with exit $prefixExit." }
    } else {
        $testArguments += @('--ignored', '--exact', 'renderer::tests::dx12_lifecycle_without_direct2d')
    }
    & $debugger -g -G -o -logo $output `
        -c 'sxd -c2 ".echo NATIVE_ACCESS_VIOLATION; .exr -1; .ecxr; kv; ~* k 20; lm; q" av; g' `
        $binary @testArguments
    $exitCode = $LASTEXITCODE
    @{ iteration = $iteration; exit_code = $exitCode } |
        ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/iteration-$iteration.json"
    $crashed = Select-String -LiteralPath $output -Pattern '^NATIVE_ACCESS_VIOLATION' -Quiet
    $failed = Select-String -LiteralPath $output -Pattern 'test result: FAILED' -Quiet
    if ($exitCode -ne 0 -or $crashed -or $failed) {
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
        throw "Native test iteration $iteration failed with exit $exitCode; no green rerun replaces it."
    }
}
