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
        SetForegroundWindow(window);
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
}
'@
}
