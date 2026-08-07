[CmdletBinding()]
param(
    [string] $ResultPath = 'os-input-smoke-result.txt'
)

if ($env:OS -ne 'Windows_NT') {
    throw 'Windows OS-input smoke is supported only on Windows.'
}

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class FesTermOsInputNative {
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool MoveWindow(
        IntPtr hWnd, int x, int y, int width, int height, bool repaint);

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(
        uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
}
'@

$mouseLeftDown = 0x0002
$mouseLeftUp = 0x0004
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if (-not [System.IO.Path]::IsPathRooted($ResultPath)) {
    $ResultPath = Join-Path $repositoryRoot $ResultPath
}
$nativeResultPath = [System.IO.Path]::GetFullPath($ResultPath)

cargo build --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Remove-Item $nativeResultPath -ErrorAction Ignore
$env:FESTERM_NATIVE_OS_INPUT_SMOKE = '1'
$env:FESTERM_NATIVE_SMOKE_RESULT_PATH = $nativeResultPath
$process = Start-Process -FilePath '.\target\debug\festerm.exe' -WorkingDirectory (Get-Location) -PassThru
Remove-Item Env:FESTERM_NATIVE_OS_INPUT_SMOKE -ErrorAction Ignore
Remove-Item Env:FESTERM_NATIVE_SMOKE_RESULT_PATH -ErrorAction Ignore

try {
    $deadline = (Get-Date).AddSeconds(10)
    do {
        $process.Refresh()
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    if ($process.MainWindowHandle -eq [IntPtr]::Zero) {
        throw 'fesTerm did not create a native window.'
    }

    [void] [FesTermOsInputNative]::ShowWindow($process.MainWindowHandle, 5)
    [void] [FesTermOsInputNative]::MoveWindow($process.MainWindowHandle, 100, 100, 860, 540, $true)
    [void] [FesTermOsInputNative]::SetForegroundWindow($process.MainWindowHandle)
    Start-Sleep -Milliseconds 500
    [void] [FesTermOsInputNative]::SetCursorPos(530, 370)
    [FesTermOsInputNative]::mouse_event($mouseLeftDown, 0, 0, 0, [UIntPtr]::Zero)
    [FesTermOsInputNative]::mouse_event($mouseLeftUp, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 100

    $shell = New-Object -ComObject WScript.Shell
    $shell.SendKeys('{TAB}{UP}os-input-ok{ENTER}')

    $deadline = (Get-Date).AddSeconds(20)
    do {
        if (Test-Path $nativeResultPath) {
            $status = Get-Content $nativeResultPath -TotalCount 1
            if ($status -ne 'status=running') { break }
        }
        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)
    if (-not (Test-Path $nativeResultPath)) {
        throw 'OS-input smoke did not write a result.'
    }
    Get-Content $nativeResultPath
    if ((Get-Content $nativeResultPath -TotalCount 1) -ne 'status=pass') {
        throw 'OS-input smoke failed.'
    }
} finally {
    $process.Refresh()
    if (-not $process.HasExited) {
        Stop-Process -Id $process.Id
    }
}
