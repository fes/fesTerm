if (-not ('FesTermApplicationWindow' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class FesTermApplicationWindow {
    private delegate bool EnumWindowProc(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool EnumWindows(EnumWindowProc callback, IntPtr parameter);
    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);
    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")]
    private static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern int GetWindowLong(IntPtr window, int index);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern int GetClassName(IntPtr window, StringBuilder name, int count);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(IntPtr window, uint message,
        UIntPtr wparam, IntPtr lparam, uint flags, uint milliseconds, out UIntPtr result);
    [StructLayout(LayoutKind.Sequential)]
    private struct LastInputInfo { public uint Size, Time; }
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetLastInputInfo(ref LastInputInfo info);
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr window);
    [StructLayout(LayoutKind.Sequential)]
    private struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)]
    private struct Point { public int X, Y; }
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool GetUserObjectInformationW(IntPtr handle, int index,
        StringBuilder value, uint length, out uint required);
    [DllImport("dwmapi.dll")]
    private static extern int DwmIsCompositionEnabled(out bool enabled);
    [DllImport("wtsapi32.dll", SetLastError = true)]
    private static extern bool WTSQuerySessionInformationW(IntPtr server, int session,
        int kind, out IntPtr buffer, out int bytes);
    [DllImport("wtsapi32.dll")]
    private static extern void WTSFreeMemory(IntPtr buffer);
    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool AttachThreadInput(uint thread, uint target, bool attach);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(IntPtr window, IntPtr z, int x, int y,
        int width, int height, uint flags);

    public static bool Matches(IntPtr window, int processId) {
        uint owner;
        if (GetWindowThreadProcessId(window, out owner) == 0 || owner != processId ||
            !IsWindowVisible(window) || GetWindow(window, 4) != IntPtr.Zero) return false;
        // Winit's event target is visible for WM_PAINT delivery, but is a
        // transparent, non-activating tool window, not the application UI.
        const int toolWindow = 0x80, noActivate = 0x08000000;
        return (GetWindowLong(window, -20) & (toolWindow | noActivate)) == 0;
    }

    public static IntPtr Find(int processId) {
        if (processId <= 0) throw new ArgumentOutOfRangeException("processId");
        var matches = new List<IntPtr>();
        if (!EnumWindows((window, parameter) => {
            if (Matches(window, processId)) matches.Add(window);
            return true;
        }, IntPtr.Zero)) throw new Win32Exception();
        if (matches.Count > 1)
            throw new InvalidOperationException("Expected one application window; found " + matches.Count + ".");
        return matches.Count == 0 ? IntPtr.Zero : matches[0];
    }

    public static string ClassName(IntPtr window) {
        var name = new StringBuilder(256);
        if (GetClassName(window, name, name.Capacity) == 0) throw new Win32Exception();
        return name.ToString();
    }

    public static void Activate(IntPtr window, int processId) {
        RequireResponsive(window, processId);
        if (GetForegroundWindow() == window) return;
        if (!SetForegroundWindow(window)) {
            if (!SetWindowPos(window, IntPtr.Zero, 0, 0, 0, 0, 0x43))
                throw new Win32Exception();
            if (GetForegroundWindow() != window && !SetForegroundWindow(window)) {
                uint owner;
                uint foregroundThread = GetWindowThreadProcessId(GetForegroundWindow(), out owner);
                uint currentThread = GetCurrentThreadId();
                if (foregroundThread == 0 || foregroundThread == currentThread)
                    throw new InvalidOperationException("Foreground activation is unavailable.");
                if (!AttachThreadInput(currentThread, foregroundThread, true))
                    throw new Win32Exception();
                try {
                    if (!SetForegroundWindow(window))
                        throw new InvalidOperationException("Could not activate the owned application window.");
                } finally {
                    if (!AttachThreadInput(currentThread, foregroundThread, false))
                        throw new Win32Exception();
                }
            }
        }
        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (GetForegroundWindow() != window && clock.ElapsedMilliseconds < 2000)
            System.Threading.Thread.Sleep(50);
        if (GetForegroundWindow() != window)
            throw new InvalidOperationException("Could not activate the application window.");
    }

    public static void Close(IntPtr window, int processId) {
        if (!Matches(window, processId))
            throw new InvalidOperationException("The application window identity changed.");
        if (!PostMessage(window, 0x0010, IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception();
    }

    public static void RequireResponsive(IntPtr window, int processId) {
        if (!Matches(window, processId))
            throw new InvalidOperationException("The application window identity changed.");
        UIntPtr result;
        if (SendMessageTimeout(window, 0, UIntPtr.Zero, IntPtr.Zero, 3, 2000, out result) == IntPtr.Zero)
            throw new InvalidOperationException("The application window did not respond within two seconds.");
    }

    public static uint LastInputTick() {
        var info = new LastInputInfo { Size = (uint)Marshal.SizeOf(typeof(LastInputInfo)) };
        if (!GetLastInputInfo(ref info)) throw new Win32Exception();
        return info.Time;
    }

    public static void RequireInteractiveDesktop() {
        if (!Environment.UserInteractive)
            throw new InvalidOperationException("An interactive logged-in desktop is required.");
        IntPtr buffer;
        int bytes, session = System.Diagnostics.Process.GetCurrentProcess().SessionId;
        if (!WTSQuerySessionInformationW(IntPtr.Zero, session, 8, out buffer, out bytes))
            throw new Win32Exception();
        int state;
        try { state = Marshal.ReadInt32(buffer); } finally { WTSFreeMemory(buffer); }
        if (state != 0)
            throw new InvalidOperationException("The desktop session is not WTSActive: " + state);
        var desktop = OpenInputDesktop(0, false, 1);
        if (desktop == IntPtr.Zero) throw new Win32Exception();
        try {
            var name = new StringBuilder(256);
            uint required;
            if (!GetUserObjectInformationW(desktop, 2, name, 512, out required))
                throw new Win32Exception();
            if (name.ToString() != "Default")
                throw new InvalidOperationException("The input desktop is locked or unsuitable.");
        } finally { if (!CloseDesktop(desktop)) throw new Win32Exception(); }
        bool enabled;
        int result = DwmIsCompositionEnabled(out enabled);
        if (result != 0) Marshal.ThrowExceptionForHR(result);
        if (!enabled) throw new InvalidOperationException("DWM composition is disabled.");
    }

    public static int[] ClientGeometry(IntPtr window, int processId) {
        RequireResponsive(window, processId);
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero) throw new Win32Exception();
        try {
            Rect rect;
            var point = new Point();
            if (!GetClientRect(window, out rect) || !ClientToScreen(window, ref point))
                throw new Win32Exception();
            uint dpi = GetDpiForWindow(window);
            if (dpi == 0 || rect.Right <= 0 || rect.Bottom <= 0)
                throw new InvalidOperationException("The client geometry is unavailable.");
            return new[] { point.X, point.Y, rect.Right, rect.Bottom, (int)dpi };
        } finally {
            if (SetThreadDpiAwarenessContext(previous) == IntPtr.Zero) throw new Win32Exception();
        }
    }

    public static IntPtr BeginPhysicalPixels() {
        var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        if (previous == IntPtr.Zero) throw new Win32Exception();
        return previous;
    }

    public static void RestorePixelContext(IntPtr previous) {
        if (SetThreadDpiAwarenessContext(previous) == IntPtr.Zero) throw new Win32Exception();
    }
}
'@
}

function Save-FesTermWindowCapture {
    param(
        [Parameter(Mandatory)][IntPtr] $Window,
        [Parameter(Mandatory)][int] $ProcessId,
        [Parameter(Mandatory)][string] $Path
    )

    [FesTermApplicationWindow]::RequireInteractiveDesktop()
    if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $Window) {
        throw 'External capture requires the foreground test-owned application window.'
    }
    if (Test-Path -LiteralPath $Path) { throw 'A capture must not overwrite prior evidence.' }
    Add-Type -AssemblyName System.Drawing
    $previous = [FesTermApplicationWindow]::BeginPhysicalPixels()
    $bitmap = $null
    $graphics = $null
    try {
        $geometry = [FesTermApplicationWindow]::ClientGeometry($Window, $ProcessId)
        $bitmap = [Drawing.Bitmap]::new($geometry[2], $geometry[3])
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($geometry[0], $geometry[1], 0, 0, $bitmap.Size)
        if ([FesTermApplicationWindow]::GetForegroundWindow() -ne $Window -or
            ($geometry -join ',') -ne
                ([FesTermApplicationWindow]::ClientGeometry($Window, $ProcessId) -join ',')) {
            throw 'The test-owned window changed during external desktop capture.'
        }
        $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
    } finally {
        if ($null -ne $graphics) { $graphics.Dispose() }
        if ($null -ne $bitmap) { $bitmap.Dispose() }
        [FesTermApplicationWindow]::RestorePixelContext($previous)
    }
    [pscustomobject]@{
        Method = 'external desktop CopyFromScreen; owned client rectangle'
        Geometry = $geometry
        Sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    }
}

function Get-FesTermOwnedProcessTree {
    param([Parameter(Mandatory)][Diagnostics.Process] $Process)

    $Process.Refresh()
    if ($Process.HasExited) { return }
    $pending = [Collections.Generic.Queue[int]]::new()
    $pending.Enqueue($Process.Id)
    while ($pending.Count -gt 0) {
        $parent = $pending.Dequeue()
        foreach ($child in @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$parent")) {
            $owned = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
            if ($null -ne $owned) {
                $pending.Enqueue($owned.Id)
                [pscustomobject]@{
                    ProcessId = $owned.Id
                    ParentProcessId = $parent
                    StartedUtc = $owned.StartTime.ToUniversalTime().ToString('o')
                    Executable = $owned.Path
                }
            }
        }
    }
}

function Close-FesTermOwnedApplication {
    param(
        [Parameter(Mandatory)][Diagnostics.Process] $Process,
        [Parameter(Mandatory)][IntPtr] $Window,
        [switch] $ConfirmQuit
    )

    $descendants = @(Get-FesTermOwnedProcessTree -Process $Process)
    $confirmed = $false
    $forced = $false
    $closeError = $null
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $Process.Refresh()
    if (-not $Process.HasExited) {
        try {
            [FesTermApplicationWindow]::Close($Window, $Process.Id)
            if ($ConfirmQuit) {
                Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
                $condition = [Windows.Automation.AndCondition]::new(
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::NameProperty, 'Quit fesTerm'),
                    [Windows.Automation.PropertyCondition]::new(
                        [Windows.Automation.AutomationElement]::ControlTypeProperty,
                        [Windows.Automation.ControlType]::Button))
                do {
                    $Process.Refresh()
                    if ($Process.HasExited) { break }
                    [FesTermApplicationWindow]::RequireResponsive($Window, $Process.Id)
                    $root = [Windows.Automation.AutomationElement]::FromHandle($Window)
                    $buttons = $root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
                    if ($buttons.Count -gt 1) { throw 'Ambiguous test-owned quit confirmation.' }
                    if ($buttons.Count -eq 1) {
                        if ($buttons[0].Current.ProcessId -ne $Process.Id) {
                            throw 'The quit confirmation owner changed.'
                        }
                        $invoke = $buttons[0].GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern)
                        $invoke.Invoke()
                        $confirmed = $true
                        break
                    }
                    Start-Sleep -Milliseconds 50
                } while ($clock.Elapsed.TotalSeconds -lt 4)
            }
        } catch {
            $closeError = $_.ToString()
        }
        [void]$Process.WaitForExit(4000)
        $Process.Refresh()
        if (-not $Process.HasExited) {
            $forced = $true
            Stop-Process -Id $Process.Id
            if (-not $Process.WaitForExit(5000)) { throw 'Owned application cleanup did not finish.' }
        }
    }
    $remaining = @()
    foreach ($child in $descendants) {
        $alive = Get-Process -Id $child.ProcessId -ErrorAction SilentlyContinue
        if ($null -ne $alive -and
            $alive.StartTime.ToUniversalTime().ToString('o') -eq $child.StartedUtc) {
            [void]$alive.WaitForExit(2000)
            $alive.Refresh()
            if ($alive.HasExited) { continue }
            $remaining += $child.ProcessId
            if ($alive.Path -ne $child.Executable) { throw 'An owned descendant identity changed.' }
            Stop-Process -Id $alive.Id
            if (-not $alive.WaitForExit(5000)) { throw 'Owned descendant cleanup did not finish.' }
        }
    }
    [pscustomobject]@{
        ProcessId = $Process.Id
        Forced = $forced
        CloseError = $closeError
        QuitConfirmed = $confirmed
        ExitCode = $Process.ExitCode
        ElapsedSeconds = $clock.Elapsed.TotalSeconds
        Descendants = $descendants
        RemainingDescendantPids = $remaining
        NormalExit = (-not $forced -and $null -eq $closeError -and
            $Process.ExitCode -eq 0 -and $remaining.Count -eq 0)
    }
}
