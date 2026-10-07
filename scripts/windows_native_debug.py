"""Content-free, root-process-only Win32 debug events (no SDK or dumps)."""

import ctypes as C
from ctypes import wintypes as W
import os
from pathlib import Path
import platform
import struct
import time
import uuid


DWORD = C.c_uint32
QWORD = C.c_uint64
HANDLE = C.c_void_p
SIZE = C.c_size_t


class STARTUPINFO(C.Structure):
    _fields_ = [
        ("cb", DWORD), ("reserved", W.LPWSTR), ("desktop", W.LPWSTR),
        ("title", W.LPWSTR), ("x", DWORD), ("y", DWORD), ("cx", DWORD),
        ("cy", DWORD), ("chars_x", DWORD), ("chars_y", DWORD),
        ("fill", DWORD), ("flags", DWORD), ("show", W.WORD),
        ("reserved_size", W.WORD), ("reserved_bytes", HANDLE),
        ("stdin", HANDLE), ("stdout", HANDLE), ("stderr", HANDLE),
    ]


class PROCESS_INFORMATION(C.Structure):
    _fields_ = [
        ("process", HANDLE), ("thread", HANDLE), ("pid", DWORD), ("tid", DWORD),
    ]


class EXCEPTION_RECORD(C.Structure):
    pass


EXCEPTION_RECORD._fields_ = [
    ("code", DWORD), ("flags", DWORD), ("record", HANDLE), ("address", HANDLE),
    ("parameter_count", DWORD), ("parameters", SIZE * 15),
]


class EXCEPTION_INFO(C.Structure):
    _fields_ = [("record", EXCEPTION_RECORD), ("first_chance", DWORD)]


class CREATE_PROCESS_INFO(C.Structure):
    _fields_ = [
        ("file", HANDLE), ("process", HANDLE), ("thread", HANDLE),
        ("base", HANDLE), ("debug_offset", DWORD), ("debug_size", DWORD),
        ("tls", HANDLE), ("start", HANDLE), ("image_name", HANDLE),
        ("unicode", W.WORD),
    ]


class CREATE_THREAD_INFO(C.Structure):
    _fields_ = [("thread", HANDLE), ("tls", HANDLE), ("start", HANDLE)]


class LOAD_DLL_INFO(C.Structure):
    _fields_ = [
        ("file", HANDLE), ("base", HANDLE), ("debug_offset", DWORD),
        ("debug_size", DWORD), ("image_name", HANDLE), ("unicode", W.WORD),
    ]


class EVENT_DATA(C.Union):
    _fields_ = [
        ("exception", EXCEPTION_INFO), ("process", CREATE_PROCESS_INFO),
        ("thread", CREATE_THREAD_INFO), ("dll", LOAD_DLL_INFO),
        ("exit_code", DWORD), ("unload_base", HANDLE),
    ]


class DEBUG_EVENT(C.Structure):
    _fields_ = [("kind", DWORD), ("pid", DWORD), ("tid", DWORD), ("data", EVENT_DATA)]


class ADDRESS64(C.Structure):
    _fields_ = [("offset", QWORD), ("segment", W.WORD), ("mode", DWORD)]


class KDHELP64(C.Structure):
    _fields_ = [
        ("thread", QWORD), ("callback_stack", DWORD), ("callback_store", DWORD),
        ("next_callback", DWORD), ("frame_pointer", DWORD),
        ("call_user", QWORD), ("user_dispatcher", QWORD), ("system_range", QWORD),
        ("dispatcher", QWORD), ("stack_base", QWORD), ("stack_limit", QWORD),
        ("build_version", DWORD), ("retpoline_table_size", DWORD),
        ("retpoline_table", QWORD), ("retpoline_offset", DWORD),
        ("retpoline_size", DWORD), ("reserved", QWORD * 2),
    ]


class STACKFRAME64(C.Structure):
    _fields_ = [
        ("pc", ADDRESS64), ("return_address", ADDRESS64), ("frame", ADDRESS64),
        ("stack", ADDRESS64), ("backing_store", ADDRESS64),
        ("function_table", HANDLE), ("params", QWORD * 4), ("far", W.BOOL),
        ("virtual", W.BOOL), ("reserved", QWORD * 3), ("kdhelp", KDHELP64),
    ]


class PROCESSENTRY32(C.Structure):
    _fields_ = [
        ("size", DWORD), ("usage", DWORD), ("pid", DWORD), ("heap", SIZE),
        ("module", DWORD), ("threads", DWORD), ("parent", DWORD),
        ("priority", C.c_long), ("flags", DWORD), ("exe", W.WCHAR * 260),
    ]


class SYMBOL_INFO(C.Structure):
    _fields_ = [
        ("size", DWORD), ("type", DWORD), ("reserved", QWORD * 2),
        ("index", DWORD), ("symbol_size", DWORD), ("module", QWORD),
        ("flags", DWORD), ("value", QWORD), ("address", QWORD),
        ("register", DWORD), ("scope", DWORD), ("tag", DWORD),
        ("name_length", DWORD), ("max_name_length", DWORD), ("name", C.c_char * 1),
    ]


def bind(dll, name, result, *args):
    function = getattr(dll, name)
    function.restype = result
    function.argtypes = args
    return function


class NativeDebugger:
    """One debuggee, unchanged exception handling and original DWORD exit."""

    def __init__(self, record, event_limit=512):
        if os.name != "nt" or C.sizeof(HANDLE) != 8 or platform.machine().lower() not in ("amd64", "x86_64"):
            raise NotImplementedError("requires 64-bit Windows Python")
        # Explicit System32 loading; no PATH/working-directory DLL lookup.
        system = C.create_unicode_buffer(32768)
        kernel = C.WinDLL("kernel32", use_last_error=True)
        size = bind(kernel, "GetSystemDirectoryW", DWORD, W.LPWSTR, DWORD)(system, len(system))
        self.check(0 < size < len(system), "system_directory")
        self.k = C.WinDLL(str(Path(system.value) / "kernel32.dll"), use_last_error=True)
        self.d = C.WinDLL(str(Path(system.value) / "dbghelp.dll"), use_last_error=True)
        self.record = record
        self.event_limit = event_limit
        self.process = None
        self.threads = {}
        self.modules = {}
        self.breakpoints = {}
        self.retired_probes = {}
        self.symbols = False
        self.stack_captures = 0
        self._bind()

    def _bind(self):
        self.close = bind(self.k, "CloseHandle", W.BOOL, HANDLE)
        self.create = bind(
            self.k, "CreateProcessW", W.BOOL, W.LPCWSTR, W.LPWSTR, HANDLE,
            HANDLE, W.BOOL, DWORD, HANDLE, W.LPCWSTR, C.POINTER(STARTUPINFO),
            C.POINTER(PROCESS_INFORMATION),
        )
        self.wait = bind(self.k, "WaitForDebugEvent", W.BOOL, C.POINTER(DEBUG_EVENT), DWORD)
        self.continue_event = bind(self.k, "ContinueDebugEvent", W.BOOL, DWORD, DWORD, DWORD)
        self.context = bind(self.k, "GetThreadContext", W.BOOL, HANDLE, HANDLE)
        self.set_context = bind(self.k, "SetThreadContext", W.BOOL, HANDLE, HANDLE)
        self.read = bind(
            self.k, "ReadProcessMemory", W.BOOL, HANDLE, HANDLE, HANDLE, SIZE,
            C.POINTER(SIZE),
        )
        self.write = bind(
            self.k, "WriteProcessMemory", W.BOOL, HANDLE, HANDLE, HANDLE, SIZE,
            C.POINTER(SIZE),
        )
        self.flush = bind(self.k, "FlushInstructionCache", W.BOOL, HANDLE, HANDLE, SIZE)
        self.terminate = bind(self.k, "TerminateProcess", W.BOOL, HANDLE, DWORD)
        self.wait_process = bind(self.k, "WaitForSingleObject", DWORD, HANDLE, DWORD)
        self.get_exit = bind(self.k, "GetExitCodeProcess", W.BOOL, HANDLE, C.POINTER(DWORD))
        self.open_process = bind(self.k, "OpenProcess", HANDLE, DWORD, W.BOOL, DWORD)
        self.process_times = bind(
            self.k, "GetProcessTimes", W.BOOL, HANDLE, HANDLE, HANDLE, HANDLE, HANDLE,
        )
        self.sym_init = bind(self.d, "SymInitializeW", W.BOOL, HANDLE, W.LPCWSTR, W.BOOL)
        self.sym_cleanup = bind(self.d, "SymCleanup", W.BOOL, HANDLE)
        self.stack_walk = bind(
            self.d, "StackWalk64", W.BOOL, DWORD, HANDLE, HANDLE,
            C.POINTER(STACKFRAME64), HANDLE, HANDLE, HANDLE, HANDLE, HANDLE,
        )
        bind(self.d, "SymSetOptions", DWORD, DWORD)(0x00000004 | 0x00080000 | 0x1000 | 0x200)
        self.sym_load = bind(
            self.d, "SymLoadModuleExW", QWORD, HANDLE, HANDLE, W.LPCWSTR,
            W.LPCWSTR, QWORD, DWORD, HANDLE, DWORD,
        )
        self.sym_name = bind(self.d, "SymFromAddr", W.BOOL, HANDLE, QWORD, HANDLE, HANDLE)
        self.sym_table = C.cast(self.d.SymFunctionTableAccess64, HANDLE)
        self.sym_base = C.cast(self.d.SymGetModuleBase64, HANDLE)

    @staticmethod
    def check(ok, operation):
        if not ok:
            raise OSError(C.get_last_error(), operation)

    def note(self, kind, **fields):
        if len(self.record["events"]) < self.event_limit:
            self.record["events"].append({"kind": kind, **fields})
        else:
            self.record["events_truncated"] = True
            if kind in ("exception", "termination", "process_exited", "timeout"):
                self.record["events"].pop(0)
                self.record["events"].append({"kind": kind, **fields})

    def creation_time(self, handle):
        times = [QWORD() for _ in range(4)]
        self.check(self.process_times(handle, *(C.byref(t) for t in times)), "process_times")
        return times[0].value

    def memory(self, address, size):
        buffer = C.create_string_buffer(size)
        got = SIZE()
        self.check(self.read(self.process, address, buffer, size, C.byref(got)), "read_metadata")
        if got.value != size:
            raise OSError("short metadata read")
        return buffer.raw

    def patch(self, address, data):
        buffer = C.create_string_buffer(data)
        wrote = SIZE()
        self.check(self.write(self.process, address, buffer, len(data), C.byref(wrote)), "exit_probe_write")
        self.check(wrote.value == len(data), "exit_probe_short_write")
        self.check(self.flush(self.process, address, len(data)), "exit_probe_flush")

    def module(self, handle, base):
        name = "unavailable"
        try:
            if handle:
                path = C.create_unicode_buffer(32768)
                size = bind(
                    self.k, "GetFinalPathNameByHandleW", DWORD, HANDLE, W.LPWSTR, DWORD, DWORD,
                )(handle, path, len(path), 0)
                if 0 < size < len(path):
                    name = Path(path.value).name
            header = self.memory(base, 64)
            pe_offset = struct.unpack_from("<I", header, 60)[0]
            if pe_offset > 1024 * 1024:
                raise ValueError("invalid PE header offset")
            pe = self.memory(base + pe_offset, 84)
            if pe[:4] != b"PE\0\0":
                raise ValueError("invalid PE header")
            item = {
                "name": name, "base": hex(base),
                "timestamp": struct.unpack_from("<I", pe, 8)[0],
                "image_size": struct.unpack_from("<I", pe, 80)[0],
            }
            try:
                optional_magic = struct.unpack_from("<H", pe, 24)[0]
                directory_offset = 184 if optional_magic == 0x20b else 168
                directory = self.memory(base + pe_offset + directory_offset, 8)
                rva, size = struct.unpack("<II", directory)
                for offset in range(0, min(size, 28 * 32), 28):
                    debug = self.memory(base + rva + offset, 28)
                    if struct.unpack_from("<I", debug, 12)[0] == 2:
                        codeview = self.memory(base + struct.unpack_from("<I", debug, 20)[0], 24)
                        if codeview[:4] == b"RSDS":
                            item["pdb_guid"] = str(uuid.UUID(bytes_le=codeview[4:20]))
                            item["pdb_age"] = struct.unpack_from("<I", codeview, 20)[0]
                            break
            except (OSError, ValueError):
                item["pdb_identity"] = "unavailable"
            if len(self.record["modules"]) < 512:
                self.record["modules"].append(item)
                self.modules[base] = item
            else:
                self.record["modules_truncated"] = True
            if self.symbols:
                # Unwind metadata, not PDB contents, locals, parameters or symbols.
                loaded = self.sym_load(self.process, handle, None, None, base, item["image_size"], None, 0)
                item["unwind_registration"] = "available" if loaded else "unavailable"
                if not loaded:
                    item["unwind_win32_error"] = C.get_last_error()
            if name.lower() == "ntdll.dll":
                self.install_exit_probes(base)
        except (OSError, ValueError) as error:
            self.note("module_metadata_unavailable", error_type=type(error).__name__)
        finally:
            if handle:
                self.close(handle)

    def install_exit_probes(self, remote_base):
        local = bind(self.k, "GetModuleHandleW", HANDLE, W.LPCWSTR)("ntdll.dll")
        address = bind(self.k, "GetProcAddress", HANDLE, HANDLE, C.c_char_p)
        for name, register in (("RtlExitUserProcess", 128), ("NtTerminateProcess", 136)):
            try:
                exported = address(local, name.encode("ascii"))
                if not exported:
                    raise OSError("exit export unavailable")
                location = remote_base + exported - local
                saved = self.memory(location, 1)
                if saved == b"\xcc":
                    raise OSError("exit entry already has a breakpoint")
                self.breakpoints[location] = (saved, name, register)
                self.patch(location, b"\xcc")
            except OSError:
                self.note("termination_probe_unavailable", probe=name)
        self.record["termination_probes"] = [p[1] for p in self.breakpoints.values()]

    def thread_context(self, thread):
        # Windows AMD64 CONTEXT is 1232 bytes, 16-byte aligned. Only control/
        # integer registers are requested; no register values are serialized.
        buffer = C.create_string_buffer(1248)
        address = (C.addressof(buffer) + 15) & ~15
        struct.pack_into("<I", buffer, address - C.addressof(buffer) + 48, 0x00100003)
        self.check(self.context(thread, address), "thread_context")
        return buffer, address

    def frames(self, tid):
        thread = self.threads.get(tid)
        if not thread:
            return {"tid": tid, "status": "thread_unavailable", "frames": []}
        try:
            buffer, context = self.thread_context(thread)
            raw = C.string_at(context, 256)
            frame = STACKFRAME64()
            frame.pc.offset = struct.unpack_from("<Q", raw, 248)[0]
            frame.stack.offset = struct.unpack_from("<Q", raw, 152)[0]
            frame.frame.offset = struct.unpack_from("<Q", raw, 160)[0]
            frame.pc.mode = frame.stack.mode = frame.frame.mode = 3
            addresses = [frame.pc.offset]
            unwind_error = None
            if self.symbols:
                for step in range(24):
                    if not self.stack_walk(
                        0x8664, self.process, thread, C.byref(frame), context, None,
                        self.sym_table, self.sym_base, None,
                    ):
                        unwind_error = C.get_last_error()
                        break
                    if not frame.pc.offset:
                        break
                    if frame.pc.offset == addresses[-1]:
                        # StackWalk64's first successful call initializes the
                        # frame at the supplied PC rather than advancing it.
                        if step == 0:
                            continue
                        break
                    addresses.append(frame.pc.offset)
            frames = []
            for address in addresses:
                entry = {"pc": hex(address)}
                for base, module in self.modules.items():
                    if base <= address < base + module["image_size"]:
                        entry.update(module=module["name"], offset=hex(address - base))
                        break
                if self.symbols:
                    symbol_buffer = C.create_string_buffer(C.sizeof(SYMBOL_INFO) + 512)
                    symbol = SYMBOL_INFO.from_buffer(symbol_buffer)
                    symbol.size = C.sizeof(SYMBOL_INFO)
                    symbol.max_name_length = 512
                    displacement = QWORD()
                    if self.sym_name(self.process, address, C.byref(displacement), symbol_buffer):
                        name = C.string_at(
                            C.addressof(symbol_buffer) + SYMBOL_INFO.name.offset,
                            min(symbol.name_length, 512),
                        ).decode("ascii", errors="replace")
                        # Symbol names only: no source filenames, locals, frame
                        # parameters, exception parameters or arbitrary strings.
                        if "/" not in name and "\\" not in name and all(c.isprintable() for c in name):
                            entry.update(symbol=name, symbol_offset=hex(displacement.value))
                frames.append(entry)
            return {
                "tid": tid, "status": "unwound" if len(frames) > 1 else "pc_only",
                "frames": frames, "unwind_win32_error": unwind_error,
            }
        except OSError:
            return {"tid": tid, "status": "context_unavailable", "frames": []}

    def stacks(self, event_tid):
        if self.stack_captures >= 8:
            self.record["stacks_truncated"] = True
            return []
        self.stack_captures += 1
        tids = [event_tid] + [t for t in self.threads if t != event_tid]
        return [self.frames(t) for t in tids[:64]]

    def cleanup_tree(self):
        """Snapshot descendants, pin handles/start times, never kill by name."""
        snapshot = bind(self.k, "CreateToolhelp32Snapshot", HANDLE, DWORD, DWORD)(2, 0)
        if snapshot == HANDLE(-1).value:
            raise OSError(C.get_last_error(), "tree_snapshot")
        entries = []
        try:
            entry = PROCESSENTRY32()
            entry.size = C.sizeof(entry)
            first = bind(self.k, "Process32FirstW", W.BOOL, HANDLE, C.POINTER(PROCESSENTRY32))
            next_entry = bind(self.k, "Process32NextW", W.BOOL, HANDLE, C.POINTER(PROCESSENTRY32))
            more = first(snapshot, C.byref(entry))
            while more:
                entries.append((entry.pid, entry.parent))
                more = next_entry(snapshot, C.byref(entry))
        finally:
            self.close(snapshot)
        owned = {self.pid: self.started}
        handles = []
        try:
            for _ in range(32):
                added = False
                for pid, parent in entries:
                    if pid in owned or parent not in owned:
                        continue
                    handle = self.open_process(0x00100000 | 0x1000 | 1, False, pid)
                    if not handle:
                        continue
                    try:
                        born = self.creation_time(handle)
                        if born < owned[parent]:
                            self.close(handle)
                            continue
                    except OSError:
                        self.close(handle)
                        continue
                    owned[pid] = born
                    handles.append((pid, handle))
                    added = True
                if not added:
                    break
            for pid, handle in reversed(handles):
                if self.wait_process(handle, 0) == 0x102:
                    self.check(self.terminate(handle, 124), "descendant_terminate")
                    self.note("owned_descendant_terminated", pid=pid)
            cleanup_deadline = time.monotonic() + 5
            for _, handle in handles:
                remaining = max(0, int((cleanup_deadline - time.monotonic()) * 1000))
                self.check(self.wait_process(handle, remaining) == 0, "descendant_exit_wait")
            if self.wait_process(self.process, 0) == 0x102:
                self.check(self.terminate(self.process, 124), "root_terminate")
            self.record["cleanup"] = "owned_tree_termination_requested"
        finally:
            for _, handle in handles:
                self.close(handle)

    def drain_cleanup_exit(self):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            event = DEBUG_EVENT()
            if not self.wait(C.byref(event), 100):
                if C.get_last_error() in (121, 0):
                    continue
                raise OSError(C.get_last_error(), "cleanup_debug_wait")
            if event.kind in (3, 6):
                file = event.data.process.file if event.kind == 3 else event.data.dll.file
                if file:
                    self.close(file)
            continuation = 0x80010001 if event.kind == 1 else 0x00010002
            self.check(self.continue_event(event.pid, event.tid, continuation), "cleanup_debug_continue")
            if event.kind == 5:
                self.record["cleanup"] = "owned_tree_exited_after_runner_error"
                return
        raise TimeoutError("cleanup_debug_exit_deadline")

    def run(self, executable, arguments, cwd, timeout, stdout, stderr, stdin):
        import subprocess

        info = STARTUPINFO()
        info.cb = C.sizeof(info)
        info.flags = 0x100
        info.stdin, info.stdout, info.stderr = stdin, stdout, stderr
        process = PROCESS_INFORMATION()
        command = C.create_unicode_buffer(subprocess.list2cmdline([executable, *arguments]))
        # DEBUG_ONLY_THIS_PROCESS excludes shells, SSH, daemons and all children
        # from metadata capture. Suspension closes the startup/exit race.
        self.check(
            self.create(executable, command, None, None, True, 2 | 4, None, cwd,
                        C.byref(info), C.byref(process)),
            "debug_process_create",
        )
        self.process, self.pid = process.process, process.pid
        self.record["pid"] = self.pid
        deadline = time.monotonic() + timeout
        timed_out = False
        initial_breakpoint = True
        exited = False
        initial_thread_closed = False
        try:
            self.started = self.creation_time(self.process)
            resume = bind(self.k, "ResumeThread", DWORD, HANDLE)
            try:
                self.check(resume(process.thread) != 0xffffffff, "debug_process_resume")
            finally:
                self.close(process.thread)
                initial_thread_closed = True
            while not exited:
                if not timed_out and time.monotonic() >= deadline:
                    self.note("timeout")
                    self.record["outcome"] = "timeout"
                    self.cleanup_tree()
                    timed_out = True
                    deadline = time.monotonic() + 10
                elif timed_out and time.monotonic() >= deadline:
                    raise TimeoutError("debug_exit_after_termination")
                event = DEBUG_EVENT()
                if not self.wait(C.byref(event), 100):
                    if C.get_last_error() in (121, 0):
                        continue
                    raise OSError(C.get_last_error(), "debug_event_wait")
                continuation = 0x00010002
                try:
                    if event.pid != self.pid:
                        raise OSError("foreign debug event")
                    kind, data = event.kind, event.data
                    if kind == 3:
                        self.threads[event.tid] = data.process.thread
                        # The loader is stopped at CREATE_PROCESS; an eager
                        # module enumeration can fail before it is initialized.
                        self.symbols = bool(self.sym_init(self.process, "", False))
                        self.record["stack_tool"] = "dbghelp" if self.symbols else "unavailable"
                        if not self.symbols:
                            self.record["stack_tool_win32_error"] = C.get_last_error()
                        self.module(data.process.file, data.process.base)
                        self.note("process_created", tid=event.tid)
                    elif kind == 2:
                        self.threads[event.tid] = data.thread.thread
                        self.note("thread_created", tid=event.tid)
                    elif kind == 4:
                        self.note("thread_exited", tid=event.tid, exit_code=data.exit_code)
                        self.threads.pop(event.tid, None)
                    elif kind == 6:
                        self.module(data.dll.file, data.dll.base)
                    elif kind == 7:
                        self.modules.pop(data.unload_base, None)
                    elif kind == 1:
                        exception = data.exception
                        code = exception.record.code
                        location = exception.record.address
                        if code == 0x80000003 and (
                            location in self.breakpoints or location in self.retired_probes
                        ):
                            probe = self.breakpoints.pop(location, None)
                            if probe is not None:
                                self.retired_probes[location] = probe
                            saved, name, register = self.retired_probes[location]
                            buffer, context = self.thread_context(self.threads[event.tid])
                            exit_code = DWORD.from_address(context + register).value
                            if exit_code:
                                target_handle = QWORD.from_address(context + 128).value
                                self.note(
                                    "termination", api=name, exit_code=exit_code,
                                    tid=event.tid, stacks=self.stacks(event.tid),
                                    target="current_process" if name == "RtlExitUserProcess" or
                                    target_handle in (0, 0xffffffffffffffff) else "unresolved_handle",
                                )
                            self.patch(location, saved)
                            QWORD.from_address(context + 248).value = location
                            self.check(
                                self.set_context(self.threads[event.tid], context),
                                "exit_probe_restore_context",
                            )
                        elif code == 0x80000003 and initial_breakpoint:
                            initial_breakpoint = False
                            self.note("debugger_initial_breakpoint", tid=event.tid)
                        else:
                            continuation = 0x80010001
                            self.note(
                                "exception", code=f"0x{code:08x}",
                                first_chance=bool(exception.first_chance),
                                address=hex(location or 0), tid=event.tid,
                                stacks=self.stacks(event.tid) if not exception.first_chance else [],
                            )
                    elif kind == 5:
                        self.record["exit_code_unsigned"] = data.exit_code
                        self.note("process_exited", tid=event.tid, exit_code=data.exit_code)
                        exited = True
                    # OUTPUT_DEBUG_STRING is deliberately not read or persisted.
                finally:
                    self.check(
                        self.continue_event(event.pid, event.tid, continuation),
                        "debug_event_continue",
                    )
            self.record.setdefault("cleanup", "root_exited")
            if not self.symbols or len(self.record.get("termination_probes", [])) < 2:
                self.record["capture_status"] = "partial"
            return self.record["exit_code_unsigned"]
        finally:
            try:
                if not exited:
                    if hasattr(self, "started"):
                        self.cleanup_tree()
                    else:
                        self.check(self.terminate(self.process, 125), "cleanup_root_terminate")
                    self.drain_cleanup_exit()
            finally:
                if not initial_thread_closed:
                    self.close(process.thread)
                if self.symbols:
                    self.sym_cleanup(self.process)
                # Debug event process/thread handles are closed by Windows when
                # their exit event is continued; CreateProcess handles are ours.
                self.close(self.process)
                self.process = None


def cancel_reader(thread):
    if not thread.is_alive():
        return
    kernel = C.WinDLL("kernel32", use_last_error=True)
    handle = bind(kernel, "OpenThread", HANDLE, DWORD, W.BOOL, DWORD)(
        1, False, thread.native_id,
    )
    if handle:
        try:
            bind(kernel, "CancelSynchronousIo", W.BOOL, HANDLE)(handle)
        finally:
            bind(kernel, "CloseHandle", W.BOOL, HANDLE)(handle)
    thread.join(1)
