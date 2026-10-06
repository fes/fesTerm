// The controlled 2173 calibration from diagnose/windows-direct2d-native-crash,
// extended for non-GPU runner regressions. Never run against a user process.
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Threading;

internal static class ExitProbe
{
    [DllImport("kernel32.dll")]
    private static extern bool TerminateProcess(IntPtr process, uint code);
    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")]
    private static extern IntPtr VirtualAlloc(IntPtr address, UIntPtr size, uint allocation, uint protection);
    [DllImport("ntdll.dll")]
    private static extern uint RtlGetNtGlobalFlags();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate void Fault();

    public static int Main(string[] args)
    {
        string mode = args.Length == 0 ? "exit" : args[0];
        Console.WriteLine("test fixture::completed ... ok");
        Console.WriteLine("private-payload-must-not-enter-receipts");
        if (mode == "pass") return 0;
        if (mode == "heap") {
            Console.WriteLine("DEBUG_HEAP_FLAGS=" + (RtlGetNtGlobalFlags() & 0x70));
            return 0;
        }
        if (mode == "environment") {
            return Environment.GetEnvironmentVariable("FESTERM_DIAGNOSTIC_ENV_CONTROL") ==
                "private-env-\ud83d\ude80-after" &&
                Environment.GetEnvironmentVariable("_NO_DEBUG_HEAP") == "1" ? 0 : 7;
        }
        if (mode == "failure") {
            Console.WriteLine("test fixture::assertion ... FAILED");
            return 101;
        }
        if (mode == "terminate") {
            TerminateProcess(GetCurrentProcess(), 0xc0000005);
            return 1;
        }
        if (mode == "exception") {
            // Run an actual unmanaged access violation, not a managed throw.
            IntPtr code = VirtualAlloc(IntPtr.Zero, (UIntPtr)4096, 0x3000, 0x40);
            if (code == IntPtr.Zero) return 3;
            byte[] machineCode = { 0x31, 0xc0, 0x89, 0x00, 0xc3 };
            Marshal.Copy(machineCode, 0, code, machineCode.Length);
            ((Fault)Marshal.GetDelegateForFunctionPointer(code, typeof(Fault)))();
            return 4;
        }
        if (mode == "tree") {
            Process child = Process.Start(new ProcessStartInfo {
                FileName = Process.GetCurrentProcess().MainModule.FileName,
                Arguments = "sleep", UseShellExecute = false
            });
            Console.WriteLine("FIXTURE_CHILD_PID=" + child.Id);
            Console.Out.Flush();
            Thread.Sleep(60000);
        }
        if (mode == "sleep") Thread.Sleep(60000);
        return 2173;
    }
}
