"""Native window identity regression without a GUI renderer or desktop input."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest


@unittest.skipUnless(sys.platform == "win32", "Win32 window enumeration")
class ApplicationWindowTests(unittest.TestCase):
    def test_helper_hidden_owned_and_ambiguous_windows(self):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for native probe tests")
        environment = os.environ.copy()
        environment["FESTERM_WINDOW_HELPER"] = str(
            Path(__file__).resolve().parents[1] / "windows-application-window.ps1"
        )
        result = subprocess.run(
            [shell, "-NoProfile", "-NonInteractive", "-Command", SCRIPT],
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("application-window regression passed", result.stdout)


SCRIPT = r"""
$ErrorActionPreference = 'Stop'
. $env:FESTERM_WINDOW_HELPER
Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class WindowFixture {
    [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    private static extern IntPtr CreateWindowEx(int extendedStyle, string className,
        string name, uint style, int x, int y, int width, int height,
        IntPtr owner, IntPtr menu, IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll", EntryPoint="SetWindowLongW")]
    private static extern int SetWindowLong(IntPtr window, int index, int value);
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr window);
    public static IntPtr Create(int extendedStyle, bool visible, IntPtr owner) {
        var window = CreateWindowEx(extendedStyle, "STATIC", "", 0x80000000,
            -30000, -30000, 20, 20, owner, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (window == IntPtr.Zero) throw new Win32Exception();
        SetVisible(window, visible);
        return window;
    }
    public static void SetVisible(IntPtr window, bool visible) {
        // Mark visible without showing, activating, or moving the user's pointer.
        SetWindowLong(window, -16, unchecked((int)(visible ? 0x90000000u : 0x80000000u)));
    }
    public static void Destroy(IntPtr window) {
        if (IsWindow(window) && !DestroyWindow(window)) throw new Win32Exception();
    }
}
'@
function Assert([bool]$condition, [string]$message) {
    if (-not $condition) { throw $message }
}
$windows = [Collections.Generic.List[IntPtr]]::new()
try {
    # Winit's helper is WS_VISIBLE despite not being the application window.
    $helper = [WindowFixture]::Create(0x080800a0, $true, [IntPtr]::Zero)
    $windows.Add($helper)
    Assert ([FesTermApplicationWindow]::Find($PID) -eq [IntPtr]::Zero) 'Selected the event helper'
    foreach ($style in @(0x80, 0x08000000)) {
        $excluded = [WindowFixture]::Create($style, $true, [IntPtr]::Zero)
        $windows.Add($excluded)
        Assert ([FesTermApplicationWindow]::Find($PID) -eq [IntPtr]::Zero) 'Selected a tool or non-activating window'
    }
    $root = [WindowFixture]::Create(0, $false, [IntPtr]::Zero)
    $windows.Add($root)
    Assert ([FesTermApplicationWindow]::Find($PID) -eq [IntPtr]::Zero) 'Selected a hidden window'
    [WindowFixture]::SetVisible($root, $true)
    Assert ([FesTermApplicationWindow]::Find($PID) -eq $root) 'Did not select the application window'
    Assert (-not [FesTermApplicationWindow]::Matches($root, 2147483647)) 'Accepted a different process'
    [FesTermApplicationWindow]::RequireResponsive($root, $PID)
    $owned = [WindowFixture]::Create(0, $true, $root)
    $windows.Add($owned)
    Assert ([FesTermApplicationWindow]::Find($PID) -eq $root) 'Selected an owned popup'
    $other = [WindowFixture]::Create(0, $true, [IntPtr]::Zero)
    $windows.Add($other)
    $rejected = $false
    try { [void][FesTermApplicationWindow]::Find($PID) }
    catch { $rejected = $_.Exception.Message -match 'Expected one application window' }
    Assert $rejected 'Silently selected one of multiple application windows'
    [WindowFixture]::Destroy($other)
    $rejected = $false
    try { [FesTermApplicationWindow]::Close($helper, $PID) }
    catch { $rejected = $_.Exception.Message -match 'identity changed' }
    Assert $rejected 'Sent WM_CLOSE to the helper'
    [WindowFixture]::Destroy($root)
    Assert (-not [FesTermApplicationWindow]::Matches($root, $PID)) 'Accepted a stale HWND'
    Assert ([FesTermApplicationWindow]::Find($PID) -eq [IntPtr]::Zero) 'Fell back to the helper after close'
    'application-window regression passed'
} finally {
    for ($i = $windows.Count - 1; $i -ge 0; $i--) { [WindowFixture]::Destroy($windows[$i]) }
}
"""


if __name__ == "__main__":
    unittest.main()
