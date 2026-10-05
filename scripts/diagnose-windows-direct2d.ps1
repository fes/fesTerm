param(
    [Parameter(Mandatory = $true)]
    [string] $ResultDirectory,
    [ValidateSet('prefix', 'ui', 'lifecycle')]
    [string] $Mode = 'prefix'
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
New-Item -ItemType Directory -Path $ResultDirectory -Force | Out-Null
$started = Get-Date
$messages = @(& cargo test --locked --workspace --no-run --message-format=json --color never --config 'profile.test.package.festerm-windows-direct2d.debug=2' --config 'profile.test.package.festerm-ui-egui.debug=2')
if ($LASTEXITCODE -ne 0) { throw 'Native test build failed.' }
$executables = @($messages | ForEach-Object {
    $message = $_ | ConvertFrom-Json
    if ($message.reason -eq 'compiler-artifact' -and $message.profile.test -and $message.executable -and
        $message.target.name -in @('festerm_windows_direct2d', 'festerm_ui_egui') -and
        $message.target.kind -contains 'lib') {
        [PSCustomObject] @{ name = $message.target.name; path = $message.executable }
    }
})
if ($executables.Count -ne 2) { throw "Expected two test binaries; found $($executables.Count)." }
$binary = ($executables | Where-Object { $_.name -eq 'festerm_windows_direct2d' }).path
$prefix = ($executables | Where-Object { $_.name -eq 'festerm_ui_egui' }).path
if ($Mode -eq 'ui') { $binary = $prefix }
$debugger = "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\cdb.exe"
@{
    binary = $binary
    mode = $Mode
    os = [System.Environment]::OSVersion.VersionString
    debugger_available = Test-Path -LiteralPath $debugger
} | ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/environment.json"
if (-not (Test-Path -LiteralPath $debugger)) { throw 'The SDK debugger is required for this diagnostic phase.' }
$env:_NT_SYMBOL_PATH = "srv*$env:RUNNER_TEMP/festerm-symbols*https://msdl.microsoft.com/download/symbols;$(Split-Path -Parent $binary)"
Remove-Item Env:FESTERM_TRACE_NATIVE_TESTS

# Calibrate nonzero-exit capture separately from GPU work.
$debugCommands = 'sxd -c2 ".echo NATIVE_ACCESS_VIOLATION; .exr -1; .ecxr; kv; ~* k 20; lm; q" av; bu ntdll!RtlExitUserProcess ".if (@ecx != 0) { .echo TEST_NONZERO_EXIT; r rcx; kv; ~* k 20; lm; q } .else { gc }"; bu ntdll!NtTerminateProcess ".if (@edx != 0) { .echo TEST_NONZERO_TERMINATION; r rcx; r rdx; kv; ~* k 20; lm; q } .else { gc }"; g'
$compiler = "$env:WINDIR\Microsoft.NET\Framework64\v4.0.30319\csc.exe"
if (-not (Test-Path -LiteralPath $compiler)) { throw 'The installed Framework C# compiler is required for exit-capture calibration.' }
$exitProbe = Join-Path $env:RUNNER_TEMP 'festerm-exit-probe.exe'
$probeSource = (Resolve-Path -LiteralPath (Join-Path $env:GITHUB_WORKSPACE 'scripts/diagnose-windows-exit-probe.cs')).Path
& $compiler /nologo /platform:x64 "/out:$exitProbe" $probeSource
if ($LASTEXITCODE -ne 0) { throw 'Exit-capture calibration build failed.' }
& $debugger -g -G -o -logo "$ResultDirectory/exit-probe.log" -c $debugCommands $exitProbe 2>&1 |
    Tee-Object -FilePath "$ResultDirectory/exit-probe-console.log"
if (-not (Select-String -LiteralPath "$ResultDirectory/exit-probe-console.log" -Pattern '^TEST_NONZERO_(EXIT|TERMINATION)' -Quiet) -or
    -not (Select-String -LiteralPath "$ResultDirectory/exit-probe-console.log" -Pattern '(rcx|rdx)=000000000000087d' -Quiet)) {
    throw 'Debugger did not capture the controlled exit 2173; no test replay will run with an unverified tracer.'
}

$iterations = if ($Mode -eq 'lifecycle') { 1 } else { 40 }
for ($iteration = 1; $iteration -le $iterations; $iteration++) {
    $output = "$ResultDirectory/iteration-$iteration.log"
    Write-Host "Diagnostic mode $Mode iteration $iteration; stop at the first failure."
    $testArguments = @('--nocapture')
    if ($Mode -eq 'prefix') {
        Push-Location (Join-Path $env:GITHUB_WORKSPACE 'crates/festerm-ui-egui')
        try {
            & $prefix --nocapture 2>&1 | Tee-Object -FilePath "$ResultDirectory/ui-prefix-$iteration.log"
            $prefixExit = $LASTEXITCODE
        } finally {
            Pop-Location
        }
        @{ iteration = $iteration; exit_code = $prefixExit } |
            ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/ui-prefix-$iteration.json"
        if ($prefixExit -ne 0) { throw "The UI prefix itself failed in iteration $iteration with exit $prefixExit." }
    } elseif ($Mode -eq 'lifecycle') {
        $testArguments += @('--ignored', '--exact', 'renderer::tests::dx12_lifecycle_without_direct2d')
    }
    $console = "$ResultDirectory/test-console-$iteration.log"
    $testDirectory = if ($Mode -eq 'ui') { 'crates/festerm-ui-egui' } else { 'crates/festerm-windows-direct2d' }
    Push-Location (Join-Path $env:GITHUB_WORKSPACE $testDirectory)
    try {
        & $debugger -g -G -o -logo $output -c $debugCommands `
            $binary @testArguments 2>&1 | Tee-Object -FilePath $console
        $exitCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    @{ iteration = $iteration; exit_code = $exitCode } |
        ConvertTo-Json | Set-Content -LiteralPath "$ResultDirectory/iteration-$iteration.json"
    $crashed = Select-String -LiteralPath $output -Pattern '^(NATIVE_ACCESS_VIOLATION|TEST_NONZERO_EXIT|TEST_NONZERO_TERMINATION)' -Quiet
    $failed = Select-String -LiteralPath $console -Pattern 'test result: FAILED' -Quiet
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
    if (-not (Select-String -LiteralPath $console -Pattern 'test result: ok\. [1-9][0-9]* passed' -Quiet)) {
        throw "Iteration $iteration did not report successful nonzero test execution."
    }
}
