"""Native window identity regression without a GUI renderer or desktop input."""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest


@unittest.skipUnless(sys.platform == "win32", "Win32 window enumeration")
class ApplicationWindowTests(unittest.TestCase):
    def run_probe(self, script):
        shell = shutil.which("pwsh")
        self.assertIsNotNone(shell, "PowerShell 7 is required for native probe tests")
        environment = os.environ.copy()
        environment["FESTERM_WINDOW_HELPER"] = str(
            Path(__file__).resolve().parents[1] / "windows-application-window.ps1"
        )
        result = subprocess.run(
            [shell, "-NoProfile", "-NonInteractive", "-Command", script],
            env=environment,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def test_helper_hidden_owned_and_ambiguous_windows(self):
        self.assertIn("application-window regression passed", self.run_probe(SCRIPT))

    def test_child_tree_rejects_children_of_a_previous_parent_pid(self):
        self.assertIn("owned-tree regression passed", self.run_probe(TREE_SCRIPT))

    @unittest.skipUnless(
        os.environ.get("FESTERM_RUN_OPTIONAL_VALIDATION") == "1",
        "Opt-in interactive desktop activation",
    )
    def test_separate_gui_thread_activation_without_input(self):
        self.assertIn("activation regression passed", self.run_probe(ACTIVATION_SCRIPT))


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
    $rejected = $false
    try { [FesTermApplicationWindow]::Activate($root, 2147483647) }
    catch { $rejected = $_.Exception.Message -match 'identity changed' }
    Assert $rejected 'Activated a foreign window'
    $rejected = $false
    try { [void][FesTermApplicationWindow]::ClientGeometry($root, 2147483647) }
    catch { $rejected = $_.Exception.Message -match 'identity changed' }
    Assert $rejected 'Read geometry from a foreign window'
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
    $rejected = $false
    try { [FesTermApplicationWindow]::Activate($root, $PID) }
    catch { $rejected = $_.Exception.Message -match 'identity changed' }
    Assert $rejected 'Activated a stale HWND'
    Assert ([FesTermApplicationWindow]::Find($PID) -eq [IntPtr]::Zero) 'Fell back to the helper after close'
    'application-window regression passed'
} finally {
    for ($i = $windows.Count - 1; $i -ge 0; $i--) { [WindowFixture]::Destroy($windows[$i]) }
}
"""

TREE_SCRIPT = r"""
$ErrorActionPreference = 'Stop'
. $env:FESTERM_WINDOW_HELPER
$processes = [Collections.Generic.List[Diagnostics.Process]]::new()
try {
    $exe = (Get-Process -Id $PID).Path
    $old = Start-Process $exe -ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 20"' -NoNewWindow -PassThru
    $processes.Add($old)
    Start-Sleep -Milliseconds 50
    $root = Start-Process $exe -ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 20"' -NoNewWindow -PassThru
    $processes.Add($root)
    Start-Sleep -Milliseconds 50
    $child = Start-Process $exe -ArgumentList '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 20"' -NoNewWindow -PassThru
    $processes.Add($child)
    function Get-CimInstance {
        param($ClassName,$Filter)
        if ($Filter -eq "ParentProcessId=$($root.Id)") {
            [pscustomobject]@{ProcessId=$old.Id;CreationDate=$old.StartTime}
            [pscustomobject]@{ProcessId=$child.Id;CreationDate=$child.StartTime}
        }
    }
    $tree = @(Get-FesTermOwnedProcessTree -Process $root)
    if ($tree.Count -ne 1 -or $tree[0].ProcessId -ne $child.Id -or
        $tree[0].ParentProcessId -ne $root.Id) { throw 'Accepted an old-parent child or lost a current child.' }
    $old.Refresh()
    if ($old.HasExited) { throw 'Touched the unrelated sentinel.' }
    'owned-tree regression passed'
} finally {
    foreach ($owned in $processes) {
        $owned.Refresh()
        if (-not $owned.HasExited) { Stop-Process -Id $owned.Id; [void]$owned.WaitForExit(5000) }
    }
}
"""

ACTIVATION_SCRIPT = r"""
$ErrorActionPreference = 'Stop'
. $env:FESTERM_WINDOW_HELPER
[FesTermApplicationWindow]::RequireInteractiveDesktop()
Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Threading;
public static class SeparateGuiFixture {
 [StructLayout(LayoutKind.Sequential)] struct Message { public IntPtr Window; public uint Id; public UIntPtr Wparam; public IntPtr Lparam; public uint Time; public int X,Y; }
 [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr CreateWindowEx(int ex,string cls,string title,uint style,int x,int y,int width,int height,IntPtr parent,IntPtr menu,IntPtr instance,IntPtr argument);
 [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr window);
 [DllImport("user32.dll",EntryPoint="SetWindowLongW")] static extern int SetWindowLong(IntPtr window,int index,int value);
 [DllImport("user32.dll",SetLastError=true)] static extern int GetMessage(out Message message,IntPtr window,uint first,uint last);
 [DllImport("user32.dll")] static extern bool TranslateMessage(ref Message message);
 [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref Message message);
 [DllImport("user32.dll",SetLastError=true)] static extern bool PostThreadMessage(uint thread,uint message,UIntPtr wparam,IntPtr lparam);
 [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
 static Thread worker; static uint threadId; static IntPtr window; static Exception error;
 static ManualResetEventSlim ready=new ManualResetEventSlim();
 public static IntPtr Start() {
  worker=new Thread(()=>{
   try {
    threadId=GetCurrentThreadId();
    window=CreateWindowEx(0,"STATIC","fesTerm owned activation regression",0x80000000,-30000,-30000,20,20,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero);
    if(window==IntPtr.Zero)throw new Win32Exception();
    SetWindowLong(window,-16,unchecked((int)0x90000000));
    ready.Set(); Message message; int status;
    while((status=GetMessage(out message,IntPtr.Zero,0,0))>0){TranslateMessage(ref message);DispatchMessage(ref message);}
    if(status<0)throw new Win32Exception();
   } catch(Exception e){error=e;ready.Set();}
   finally { if(window!=IntPtr.Zero)DestroyWindow(window); }
  });
  worker.IsBackground=true; worker.Start();
  if(!ready.Wait(5000))throw new TimeoutException("GUI fixture startup.");
  if(error!=null)throw error;
  return window;
 }
 public static void Stop() {
  if(!PostThreadMessage(threadId,0x12,UIntPtr.Zero,IntPtr.Zero))throw new Win32Exception();
  if(!worker.Join(5000))throw new TimeoutException("GUI fixture cleanup.");
  if(error!=null)throw error;
 }
}
'@
$before = [FesTermApplicationWindow]::LastInputTick()
$window = [SeparateGuiFixture]::Start()
try {
    [FesTermApplicationWindow]::Activate($window,$PID)
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $window) { throw 'Separate GUI queue remained inactive.' }
    if ([FesTermApplicationWindow]::LastInputTick() -ne $before) { throw 'Activation injected input.' }
    'activation regression passed'
} finally { [SeparateGuiFixture]::Stop() }
"""


if __name__ == "__main__":
    unittest.main()
