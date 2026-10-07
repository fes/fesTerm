# Supervisor-only metadata for the explicitly owned aging child, never stack or
# private-byte attribution. Thread handles pin identity while metadata is read.
if (-not ('FesTermAging.ThreadOwnership' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace FesTermAging {
    public sealed class ThreadObservation {
        public string Status = "observed", Reason;
        public long? CreationFileTime100ns;
        public double? CpuMilliseconds;
        public string DescriptionStatus, Description, DescriptionReason;
        public string StartStatus, StartReason;
        public ulong? StartAddress;
    }
    public static class ThreadOwnership {
        [StructLayout(LayoutKind.Sequential)]
        private struct FileTime {
            public uint Low, High;
            public ulong Value { get { return ((ulong)High << 32) | Low; } }
        }
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenThread(uint access, bool inherit, uint threadId);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint GetProcessIdOfThread(IntPtr thread);
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetThreadTimes(IntPtr thread, out FileTime creation,
            out FileTime exit, out FileTime kernel, out FileTime user);
        [DllImport("kernel32.dll")]
        private static extern int GetThreadDescription(IntPtr thread, out IntPtr description);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr LocalFree(IntPtr memory);
        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool CloseHandle(IntPtr handle);
        [DllImport("ntdll.dll")]
        private static extern int NtQueryInformationThread(IntPtr thread, int informationClass,
            out IntPtr address, int length, out int returnedLength);

        public static ThreadObservation Read(uint processId, uint threadId, int labelLimit) {
            var result = new ThreadObservation();
            var thread = OpenThread(0x0040 | 0x0800, false, threadId);
            if (thread == IntPtr.Zero) {
                int error = Marshal.GetLastWin32Error();
                if (error != 87 && error != 5)
                    throw new Win32Exception(error, "OpenThread failed for owned child inventory");
                result.Status = error == 87 ? "raced" : "unavailable";
                result.Reason = error == 87 ? "thread-exited-before-open" : "OpenThread:access-denied";
                return result;
            }
            try {
                uint owner = GetProcessIdOfThread(thread);
                if (owner == 0) throw new Win32Exception(Marshal.GetLastWin32Error(), "GetProcessIdOfThread failed");
                if (owner != processId) {
                    result.Status = "raced";
                    result.Reason = "tid-recycled-outside-owned-child";
                    return result;
                }
                FileTime creation, exit, kernel, user;
                if (!GetThreadTimes(thread, out creation, out exit, out kernel, out user))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "GetThreadTimes failed");
                if (creation.Value == 0 || creation.Value > long.MaxValue)
                    throw new InvalidOperationException("Owned thread creation identity is invalid");
                result.CreationFileTime100ns = (long)creation.Value;
                result.CpuMilliseconds = ((double)kernel.Value + user.Value) / 10000.0;
                if (exit.Value != 0) {
                    result.Status = "raced";
                    result.Reason = "thread-exited-before-metadata";
                    return result;
                }
                IntPtr description = IntPtr.Zero;
                try {
                    int status = GetThreadDescription(thread, out description);
                    if (status < 0) {
                        result.DescriptionStatus = "unavailable";
                        result.DescriptionReason = "GetThreadDescription:hresult:0x" + ((uint)status).ToString("X8");
                    } else {
                        string text = description == IntPtr.Zero ? "" : Marshal.PtrToStringUni(description);
                        if (text == null) throw new InvalidOperationException("Unreadable successful thread description");
                        if (text.Length > labelLimit) {
                            result.DescriptionStatus = "unavailable";
                            result.DescriptionReason = "description-over-limit:" + text.Length;
                        } else {
                            result.DescriptionStatus = text.Length == 0 ? "unnamed" : "named";
                            result.Description = text.Length == 0 ? null : text;
                        }
                    }
                } catch (EntryPointNotFoundException) {
                    result.DescriptionStatus = "unavailable";
                    result.DescriptionReason = "api-unavailable:GetThreadDescription";
                } finally {
                    if (description != IntPtr.Zero && LocalFree(description) != IntPtr.Zero)
                        throw new Win32Exception(Marshal.GetLastWin32Error(), "Thread description LocalFree failed");
                }
                try {
                    IntPtr address;
                    int returned;
                    int status = NtQueryInformationThread(thread, 9, out address, IntPtr.Size, out returned);
                    if (status != 0) {
                        result.StartStatus = "unavailable";
                        result.StartReason = "ThreadQuerySetWin32StartAddress:ntstatus:0x" + ((uint)status).ToString("X8");
                    } else if (returned != IntPtr.Size) {
                        result.StartStatus = "unavailable";
                        result.StartReason = "unexpected-start-address-length:" + returned;
                    } else if (address == IntPtr.Zero) {
                        result.StartStatus = "unavailable";
                        result.StartReason = "zero-start-address";
                    } else {
                        result.StartStatus = "available";
                        result.StartAddress = (ulong)address.ToInt64();
                    }
                } catch (EntryPointNotFoundException) {
                    result.StartStatus = "unavailable";
                    result.StartReason = "api-unavailable:NtQueryInformationThread";
                }
                return result;
            } finally {
                if (!CloseHandle(thread))
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "Owned thread CloseHandle failed");
            }
        }
    }
}
'@
}

function Get-FesTermAgingThreadOwnership {
    param(
        [Parameter(Mandatory)][Diagnostics.Process] $Process,
        [Parameter(Mandatory)][object[]] $Threads,
        [Parameter(Mandatory)][hashtable] $Bounds
    )
    if ($Threads.Count -gt $Bounds.threads_per_sample) {
        throw "Owned thread inventory over-limit: $($Threads.Count) > $($Bounds.threads_per_sample)."
    }
    $moduleSnapshot = @{ status = 'observed'; count = $null; reason = $null }
    try { $modules = @($Process.Modules) }
    catch [ComponentModel.Win32Exception] {
        if ($_.Exception.NativeErrorCode -ne 5) { throw }
        $modules = @()
        $moduleSnapshot = @{ status = 'unavailable'; count = $null; reason = 'Process.Modules:access-denied' }
    }
    if ($modules.Count -gt $Bounds.modules_per_sample) {
        throw "Owned module inventory over-limit: $($modules.Count) > $($Bounds.modules_per_sample)."
    }
    if ($moduleSnapshot.status -eq 'observed') { $moduleSnapshot.count = $modules.Count }
    $observations = @(foreach ($thread in $Threads) {
        $metadata = [FesTermAging.ThreadOwnership]::Read(
            [uint32]$Process.Id, [uint32]$thread.Id, $Bounds.label_utf16_units)
        $description = @{ status = 'unavailable'; value = $null; reason = $metadata.Reason }
        $start = @{ status = 'unavailable'; value = $null; reason = $metadata.Reason }
        $module = @{ status = 'unavailable'; value = $null; reason = $metadata.Reason }
        if ($metadata.Status -eq 'observed') {
            $description = @{
                status = $metadata.DescriptionStatus
                value = $metadata.Description; reason = $metadata.DescriptionReason
            }
            $start = @{
                status = $metadata.StartStatus
                value = if ($null -ne $metadata.StartAddress) { '0x{0:X16}' -f $metadata.StartAddress } else { $null }
                reason = $metadata.StartReason
            }
            $module = @{ status = 'unavailable'; value = $null; reason = $metadata.StartReason }
            if ($metadata.StartStatus -eq 'available') {
                $matching = @($modules | Where-Object {
                    $base = [uint64]$_.BaseAddress.ToInt64()
                    $metadata.StartAddress -ge $base -and
                        ($metadata.StartAddress - $base) -lt [uint64]$_.ModuleMemorySize
                })
                if ($matching.Count -gt 1) { throw 'Ambiguous owned start-address module snapshot.' }
                $module = @{ status = 'unmapped'; value = $null; reason = 'address-not-in-owned-module-snapshot' }
                if ($moduleSnapshot.status -eq 'unavailable') {
                    $module = @{ status = 'unavailable'; value = $null; reason = $moduleSnapshot.reason }
                } elseif ($matching.Count -eq 1) {
                    $name = $matching[0].ModuleName
                    if ([string]::IsNullOrEmpty($name)) { throw 'Owned module snapshot has an empty name.' }
                    $module = if ($name.Length -gt $Bounds.label_utf16_units) {
                        @{ status = 'unavailable'; value = $null; reason = "module-name-over-limit:$($name.Length)" }
                    } else {
                        @{ status = 'mapped'; value = $name; reason = $null }
                    }
                }
            }
        }
        @{
            id = $thread.Id; cpu_ms = $metadata.CpuMilliseconds
            ownership = @{
                status = $metadata.Status; reason = $metadata.Reason
                creation_filetime_100ns = $metadata.CreationFileTime100ns
                description = $description; start_address = $start; start_module = $module
            }
        }
    })
    @{ threads = $observations; module_snapshot = $moduleSnapshot }
}
