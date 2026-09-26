[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $ResultDirectory,
    [ValidateRange(320, 4096)][int] $Width = 1920,
    [ValidateRange(200, 4096)][int] $Height = 1080,
    [ValidateRange(1, 2)][double] $PixelsPerPoint = 1,
    [ValidateRange(5, 30)][int] $SampleSeconds = 10,
    [string] $Python = 'python',
    [switch] $CaptureOnly,
    [switch] $SelfTestOnly
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'The Direct2D probe requires Windows.' }
if ($env:FESTERM_RUN_OPTIONAL_VALIDATION -ne '1') {
    throw 'Set FESTERM_RUN_OPTIONAL_VALIDATION=1 to run the Direct2D probe.'
}
$repo = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$output = [System.IO.Path]::GetFullPath($ResultDirectory)
New-Item -ItemType Directory -Path $output -Force | Out-Null
& $Python -c 'import PIL'
if ($LASTEXITCODE -ne 0) {
    throw "Install the probe dependencies: $Python -m pip install -r validation\direct2d\requirements.txt"
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Visual Studio Installer is required.' }
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or -not $installation) { throw 'Install Visual Studio C++ tools and Windows SDK.' }
$vcvars = Join-Path $installation 'VC\Auxiliary\Build\vcvars64.bat'
if (-not (Test-Path -LiteralPath $vcvars)) { throw 'The x64 C++ toolchain was not found.' }
$native = Join-Path $output 'direct2d-probe.exe'
$setup = "set `"PATH=$([IO.Path]::GetDirectoryName($vswhere));%PATH%`" && call `"$vcvars`" >nul"
$compile = "$setup && cl /nologo /std:c++20 /EHsc /W4 /WX /O2 /MD `"$PSScriptRoot\render_probe.cpp`" /Fo`"$output\direct2d-probe.obj`" /Fe`"$native`" /link d2d1.lib d3d11.lib dxgi.lib psapi.lib"
& $env:ComSpec /c $compile
if ($LASTEXITCODE -ne 0) { throw 'Direct2D probe compilation failed.' }
& $native --self-test
if ($LASTEXITCODE -ne 0) { throw 'Native probe checks failed.' }
& $Python -m unittest discover -s $PSScriptRoot -p 'test_*.py'
if ($LASTEXITCODE -ne 0) { throw 'Framebuffer comparison checks failed.' }
if ($SelfTestOnly) { return }

$names = @('FESTERM_DIRECT2D_PROBE_DIR', 'FESTERM_DIRECT2D_WIDTH', 'FESTERM_DIRECT2D_HEIGHT',
    'FESTERM_DIRECT2D_SCALE', 'FESTERM_DIRECT2D_SAMPLE_MS')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
Push-Location $repo
try {
    $build = "$setup && cargo test --release -p festerm direct2d_probe --no-run --locked -j 8 --message-format=json"
    & $env:ComSpec /c $build 1> "$output\build.jsonl" 2> "$output\build.stderr.log"
    if ($LASTEXITCODE -ne 0) {
        Get-Content -LiteralPath "$output\build.stderr.log" -Tail 50
        throw 'Rust probe compilation failed.'
    }
    $artifacts = Get-Content -LiteralPath "$output\build.jsonl" |
        Where-Object { $_.StartsWith('{') } |
        ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'festerm' -and $_.executable }
    if (@($artifacts).Count -ne 1) { throw 'Could not identify the Rust probe executable.' }
    $env:FESTERM_DIRECT2D_PROBE_DIR = $output
    $env:FESTERM_DIRECT2D_WIDTH = [string]$Width
    $env:FESTERM_DIRECT2D_HEIGHT = [string]$Height
    $env:FESTERM_DIRECT2D_SCALE = $PixelsPerPoint.ToString([Globalization.CultureInfo]::InvariantCulture)
    $milliseconds = if ($CaptureOnly) { 0 } else { $SampleSeconds * 1000 }
    $env:FESTERM_DIRECT2D_SAMPLE_MS = [string]$milliseconds
    # Release test binaries inherit the GUI subsystem, so explicitly wait and capture their output.
    $test = Start-Process -FilePath $artifacts.executable -WorkingDirectory $repo -ArgumentList @(
        'direct2d_probe::compare_direct2d_render_stage', '--ignored', '--exact', '--nocapture', '--test-threads=1'
    ) -RedirectStandardOutput "$output\wgpu.stdout.log" -RedirectStandardError "$output\wgpu.stderr.log" -PassThru -Wait
    if ($test.ExitCode -ne 0) {
        Get-Content -LiteralPath "$output\wgpu.stderr.log" -Tail 50
        Get-Content -LiteralPath "$output\wgpu.stdout.log" -Tail 20
        throw "wgpu probe failed with exit code $($test.ExitCode)."
    }
    foreach ($case in @('sparse', 'dense', 'scrolling', 'colored', 'unicode')) {
        & $native "$output\$case.scene" "$output\$case-d2d.json" $milliseconds
        if ($LASTEXITCODE -ne 0) { throw "Direct2D probe failed: $case" }
    }
    & $Python "$PSScriptRoot\compare.py" $output
    if ($LASTEXITCODE -ne 0) { throw 'Direct2D framebuffer comparison failed; see pixel-comparison.json.' }
} finally {
    Pop-Location
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
}
