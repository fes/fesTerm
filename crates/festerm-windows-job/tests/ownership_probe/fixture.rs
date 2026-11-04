//! Experimental, test-only evidence. This is not a worker launcher.
//!
//! Run only on an isolated, disposable Windows VM. No observation here proves
//! cleanup if the creator dies inside `CreateProcessW`.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
pub mod probe {
    use std::{
        alloc::{alloc_zeroed, dealloc, Layout},
        ffi::OsStr,
        io,
        marker::PhantomData,
        mem::{size_of, transmute, zeroed},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::Path,
        ptr::{self, NonNull},
        sync::OnceLock,
        thread,
        time::{Duration, Instant},
    };

    use crate::{role::Role, runner_gate};

    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, DuplicateHandle, GetLastError, LocalFree, ERROR_INSUFFICIENT_BUFFER,
            HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
                JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Threading::{
                CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetCurrentThread,
                GetExitCodeProcess, GetProcessId, InitializeProcThreadAttributeList, ResumeThread,
                SuspendThread, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
                CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
                EXTENDED_STARTUPINFO_PRESENT, PROCESS_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTUPINFOEXW, STARTUPINFOW,
            },
        },
    };

    const BOOT: &str = "festerm-probe-boot";
    const OUTER_READY: &str = "festerm-probe-outer-ready";
    const DROP_OK: &str = "festerm-probe-drop-ok";
    const DROP_ACK: &str = "festerm-probe-drop-ack";
    const DEATH_READY: &str = "festerm-probe-death-ready";
    const CREATOR_KILLED: u32 = 0x7ace_0001;
    const CLEANUP_EXIT: u32 = 0x7ace_0002;
    // JOB_OBJECT_QUERY, documented in Job Object Security and Access Rights.
    const JOB_QUERY_ACCESS: u32 = 0x0004;
    const PHASE_TIMEOUT: Duration = Duration::from_secs(30);
    const CLEANUP_TIMEOUT_MS: u32 = 10_000;

    struct Job(OwnedHandle);

    impl Job {
        fn new() -> io::Result<Self> {
            let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if unsafe {
                SetInformationJobObject(
                    job.raw(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        fn raw(&self) -> HANDLE {
            self.0.as_raw_handle()
        }

        fn contains(&self, process: HANDLE) -> io::Result<bool> {
            let mut inside = 0;
            if unsafe { IsProcessInJob(process, self.raw(), &mut inside) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(inside != 0)
        }
    }

    struct Attributes<'a> {
        buffer: NonNull<u8>,
        layout: Layout,
        job_handles: Box<[HANDLE; 1]>,
        _job: PhantomData<&'a Job>,
    }

    impl<'a> Attributes<'a> {
        fn new(job: &'a Job) -> io::Result<Self> {
            let mut bytes = 0;
            let result =
                unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes) };
            if result != 0
                || bytes == 0
                || bytes > 65_536
                || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
            {
                return Err(io::Error::other("attribute-list size query failed"));
            }
            let layout = Layout::from_size_align(bytes, 16)
                .map_err(|_| io::Error::other("invalid attribute-list size"))?;
            let buffer = NonNull::new(unsafe { alloc_zeroed(layout) })
                .ok_or_else(|| io::Error::other("attribute-list allocation failed"))?;
            if unsafe {
                InitializeProcThreadAttributeList(buffer.as_ptr().cast(), 1, 0, &mut bytes)
            } == 0
            {
                let error = io::Error::last_os_error();
                unsafe { dealloc(buffer.as_ptr(), layout) };
                return Err(error);
            }
            let mut attributes = Self {
                buffer,
                layout,
                job_handles: Box::new([job.raw()]),
                _job: PhantomData,
            };
            if unsafe {
                UpdateProcThreadAttribute(
                    attributes.buffer.as_ptr().cast(),
                    0,
                    PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                    attributes.job_handles.as_mut_ptr().cast(),
                    size_of::<HANDLE>(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(attributes)
        }
    }

    impl Drop for Attributes<'_> {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.buffer.as_ptr().cast());
                dealloc(self.buffer.as_ptr(), self.layout);
            }
        }
    }

    struct Child {
        process: OwnedHandle,
        thread: OwnedHandle,
    }

    impl Child {
        fn process(&self) -> HANDLE {
            self.process.as_raw_handle()
        }

        fn thread(&self) -> HANDLE {
            self.thread.as_raw_handle()
        }

        fn terminate(&self) -> io::Result<()> {
            match unsafe { WaitForSingleObject(self.process(), 0) } {
                WAIT_OBJECT_0 => return Ok(()),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => return Err(io::Error::last_os_error()),
                _ => return Err(io::Error::other("unexpected process wait result")),
            }
            if unsafe { TerminateProcess(self.process(), CLEANUP_EXIT) } == 0 {
                let error = io::Error::last_os_error();
                if unsafe { WaitForSingleObject(self.process(), 0) } != WAIT_OBJECT_0 {
                    return Err(error);
                }
            }
            wait_exit(self.process())
        }
    }

    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.terminate();
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain([0]).collect()
    }

    type GetDescription = unsafe extern "system" fn(HANDLE, *mut *mut u16) -> i32;
    type SetDescription = unsafe extern "system" fn(HANDLE, *const u16) -> i32;

    struct ThreadDescriptions {
        get: GetDescription,
        set: SetDescription,
    }

    fn thread_descriptions() -> io::Result<&'static ThreadDescriptions> {
        static FUNCTIONS: OnceLock<Option<ThreadDescriptions>> = OnceLock::new();
        FUNCTIONS
            .get_or_init(|| unsafe {
                let module = GetModuleHandleW(wide(OsStr::new("KernelBase.dll")).as_ptr());
                if module.is_null() {
                    return None;
                }
                let get = GetProcAddress(module, c"GetThreadDescription".as_ptr().cast())?;
                let set = GetProcAddress(module, c"SetThreadDescription".as_ptr().cast())?;
                // The trusted probe does not unload KernelBase; the signatures
                // match the documented thread-description APIs.
                Some(ThreadDescriptions {
                    get: transmute::<unsafe extern "system" fn() -> isize, GetDescription>(get),
                    set: transmute::<unsafe extern "system" fn() -> isize, SetDescription>(set),
                })
            })
            .as_ref()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "thread-description APIs are unavailable",
                )
            })
    }

    fn spawn(role: Role, job: Option<&Job>, executable: Option<&Path>) -> io::Result<Child> {
        let exe = match executable {
            Some(path) => path.to_owned(),
            None => std::env::current_exe()?,
        };
        if !exe.is_absolute() {
            return Err(io::Error::other("fixture executable path is not absolute"));
        }
        let directory = exe
            .parent()
            .ok_or_else(|| io::Error::other("fixture executable has no directory"))?;
        let application = wide(exe.as_os_str());
        let current_directory = wide(directory.as_os_str());
        let mut command = vec![b'"' as u16];
        command.extend(exe.as_os_str().encode_wide());
        command.extend([b'"' as u16, b' ' as u16]);
        command.extend(role.argument().encode_utf16());
        command.push(0);
        let mut environment = runner_gate::child_environment();
        let mut information: PROCESS_INFORMATION = unsafe { zeroed() };
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        let attributes = job.map(Attributes::new).transpose()?;
        let mut flags = CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS;
        if let Some(attributes) = &attributes {
            flags |= CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT;
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attributes.buffer.as_ptr().cast();
        } else {
            startup.StartupInfo.cb = size_of::<STARTUPINFOW>() as u32;
        }
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                flags,
                environment.as_mut_ptr().cast(),
                current_directory.as_ptr(),
                &startup.StartupInfo,
                &mut information,
            )
        };
        let error = (created == 0).then(io::Error::last_os_error);
        drop(attributes);
        if let Some(error) = error {
            return Err(error);
        }
        if information.hProcess.is_null() || information.hThread.is_null() {
            if !information.hProcess.is_null() {
                unsafe {
                    TerminateProcess(information.hProcess, CLEANUP_EXIT);
                    WaitForSingleObject(information.hProcess, CLEANUP_TIMEOUT_MS);
                    CloseHandle(information.hProcess);
                }
            }
            if !information.hThread.is_null() {
                unsafe { CloseHandle(information.hThread) };
            }
            return Err(io::Error::other(
                "process creation returned incomplete handles",
            ));
        }
        Ok(Child {
            process: unsafe { OwnedHandle::from_raw_handle(information.hProcess) },
            thread: unsafe { OwnedHandle::from_raw_handle(information.hThread) },
        })
    }

    fn set_marker(thread: HANDLE, marker: &str) -> io::Result<()> {
        let marker = wide(OsStr::new(marker));
        let status = unsafe { (thread_descriptions()?.set)(thread, marker.as_ptr()) };
        if status < 0 {
            Err(io::Error::other(format!(
                "SetThreadDescription failed: {status:#x}"
            )))
        } else {
            Ok(())
        }
    }

    fn marker(thread: HANDLE) -> io::Result<String> {
        let mut description = ptr::null_mut();
        let status = unsafe { (thread_descriptions()?.get)(thread, &mut description) };
        if status < 0 {
            return Err(io::Error::other(format!(
                "GetThreadDescription failed: {status:#x}"
            )));
        }
        if description.is_null() {
            return Ok(String::new());
        }
        let mut length = 0;
        while length < 128 && unsafe { *description.add(length) } != 0 {
            length += 1;
        }
        let result =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(description, length) });
        unsafe { LocalFree(description.cast()) };
        Ok(result)
    }

    fn wait_marker(thread: HANDLE, process: Option<HANDLE>, expected: &str) -> io::Result<()> {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        loop {
            if marker(thread)? == expected {
                return Ok(());
            }
            if let Some(process) = process {
                if unsafe { WaitForSingleObject(process, 0) } != WAIT_TIMEOUT {
                    return Err(io::Error::other(format!(
                        "peer exited before {expected} handshake"
                    )));
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{expected} handshake timed out"),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_bootstrap(thread: HANDLE) -> io::Result<(OwnedHandle, OwnedHandle)> {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        loop {
            let description = marker(thread)?;
            if let Some(handles) = description.strip_prefix(&format!("{OUTER_READY}:")) {
                let (parent, outer) = handles
                    .split_once(':')
                    .ok_or_else(|| io::Error::other("invalid bootstrap handles"))?;
                let parent = usize::from_str_radix(parent, 16)
                    .map_err(|_| io::Error::other("invalid observer handle"))?;
                let outer = usize::from_str_radix(outer, 16)
                    .map_err(|_| io::Error::other("invalid outer-job handle"))?;
                if parent == 0
                    || outer == 0
                    || parent == usize::MAX
                    || outer == usize::MAX
                    || parent == outer
                {
                    return Err(io::Error::other("invalid bootstrap handles"));
                }
                return Ok(unsafe {
                    (
                        OwnedHandle::from_raw_handle(parent as HANDLE),
                        OwnedHandle::from_raw_handle(outer as HANDLE),
                    )
                });
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "bootstrap timed out",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_exit(process: HANDLE) -> io::Result<()> {
        match unsafe { WaitForSingleObject(process, CLEANUP_TIMEOUT_MS) } {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process did not exit",
            )),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Err(io::Error::other("unexpected process wait result")),
        }
    }

    fn verified_suspended(job: &Job) -> io::Result<Child> {
        let child = spawn(Role::Worker, Some(job), None)?;
        if !job.contains(child.process())? {
            return Err(io::Error::other("fixture is not in its exact job"));
        }
        let previous = unsafe { SuspendThread(child.thread()) };
        if previous == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if previous == 0 {
            return Err(io::Error::other("fixture was not suspended"));
        }
        let resumed = unsafe { ResumeThread(child.thread()) };
        if resumed != previous + 1 {
            return Err(io::Error::other("fixture suspension count changed"));
        }
        if unsafe { WaitForSingleObject(child.process(), 0) } != WAIT_TIMEOUT {
            return Err(io::Error::other("suspended fixture already exited"));
        }
        Ok(child)
    }

    pub fn creator() -> io::Result<()> {
        runner_gate::require_current()?;
        let this_thread = unsafe { GetCurrentThread() };
        set_marker(this_thread, BOOT)?;
        let (observer, outer) = wait_bootstrap(this_thread)?;
        let mut inside = 0;
        if unsafe { IsProcessInJob(GetCurrentProcess(), outer.as_raw_handle(), &mut inside) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if inside == 0 {
            return Err(io::Error::other("creator is not in the supplied outer job"));
        }
        drop(outer);
        if unsafe { WaitForSingleObject(observer.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return Err(io::Error::other("observer exited before worker launch"));
        }

        let job = Job::new()?;
        let child = verified_suspended(&job)?;
        drop(job);
        wait_exit(child.process())?;
        drop(child);
        set_marker(this_thread, DROP_OK)?;
        wait_marker(this_thread, Some(observer.as_raw_handle()), DROP_ACK)?;

        let job = Job::new()?;
        let child = verified_suspended(&job)?;
        set_marker(
            this_thread,
            &format!("{DEATH_READY}:{:x}", child.process() as usize),
        )?;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if unsafe { WaitForSingleObject(observer.as_raw_handle(), 0) } != WAIT_TIMEOUT {
                return Err(io::Error::other("observer exited"));
            }
            if unsafe { WaitForSingleObject(child.process(), 0) } != WAIT_TIMEOUT {
                return Err(io::Error::other("suspended fixture exited unexpectedly"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "observer did not terminate creator",
        ))
    }

    fn pin_worker(outer: &Job, creator: &Child) -> io::Result<OwnedHandle> {
        let deadline = Instant::now() + PHASE_TIMEOUT;
        loop {
            let description = marker(creator.thread())?;
            if let Some(handle) = description.strip_prefix(&format!("{DEATH_READY}:")) {
                let handle = usize::from_str_radix(handle, 16)
                    .map_err(|_| io::Error::other("invalid fixture handle"))?;
                if handle == 0 || handle == usize::MAX {
                    return Err(io::Error::other("invalid fixture handle"));
                }
                let mut raw = ptr::null_mut();
                if unsafe {
                    DuplicateHandle(
                        creator.process(),
                        handle as HANDLE,
                        GetCurrentProcess(),
                        &mut raw,
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        0,
                        0,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let worker = unsafe { OwnedHandle::from_raw_handle(raw) };
                let creator_id = unsafe { GetProcessId(creator.process()) };
                let worker_id = unsafe { GetProcessId(worker.as_raw_handle()) };
                if creator_id == 0 || worker_id == 0 || creator_id == worker_id {
                    return Err(io::Error::other("fixture identity is invalid"));
                }
                if !outer.contains(worker.as_raw_handle())?
                    || unsafe { WaitForSingleObject(worker.as_raw_handle(), 0) } != WAIT_TIMEOUT
                {
                    return Err(io::Error::other("fixture is not alive in the outer job"));
                }
                return Ok(worker);
            }
            if unsafe { WaitForSingleObject(creator.process(), 0) } != WAIT_TIMEOUT {
                return Err(io::Error::other("creator exited before worker was pinned"));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "fixture handle was not handed off",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_empty(job: &Job) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(CLEANUP_TIMEOUT_MS.into());
        loop {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { zeroed() };
            if unsafe {
                QueryInformationJobObject(
                    job.raw(),
                    JobObjectBasicAccountingInformation,
                    (&raw mut info).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "outer job not empty",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn duplicate_into(target: HANDLE, source: HANDLE, access: u32) -> io::Result<usize> {
        let mut remote = ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                source,
                target,
                &mut remote,
                access,
                0,
                0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(remote as usize)
    }

    pub fn observer(helper: &Path) -> io::Result<()> {
        runner_gate::require_current()?;
        thread_descriptions()?;
        let outer = Job::new()?;
        // The trusted creator starts unsuspended and cannot launch a fixture
        // until this observer has assigned and verified it in the outer job.
        let creator = spawn(Role::Creator, None, Some(helper))?;
        let mut observed_worker = None;
        let result = (|| {
            wait_marker(creator.thread(), Some(creator.process()), BOOT)?;
            if unsafe { AssignProcessToJobObject(outer.raw(), creator.process()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if !outer.contains(creator.process())? {
                return Err(io::Error::other("creator is not in the outer job"));
            }
            let observer = duplicate_into(
                creator.process(),
                unsafe { GetCurrentProcess() },
                PROCESS_SYNCHRONIZE,
            )?;
            let outer_query = duplicate_into(creator.process(), outer.raw(), JOB_QUERY_ACCESS)?;
            set_marker(
                creator.thread(),
                &format!("{OUTER_READY}:{observer:x}:{outer_query:x}"),
            )?;
            wait_marker(creator.thread(), Some(creator.process()), DROP_OK)?;
            set_marker(creator.thread(), DROP_ACK)?;
            observed_worker = Some(pin_worker(&outer, &creator)?);
            if unsafe { WaitForSingleObject(creator.process(), 0) } != WAIT_TIMEOUT {
                return Err(io::Error::other("creator exited before termination"));
            }
            if unsafe { TerminateProcess(creator.process(), CREATOR_KILLED) } == 0 {
                return Err(io::Error::last_os_error());
            }
            wait_exit(creator.process())?;
            let mut code = 0;
            if unsafe { GetExitCodeProcess(creator.process(), &mut code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if code != CREATOR_KILLED {
                return Err(io::Error::other(
                    "creator did not die by the requested termination",
                ));
            }
            let worker = observed_worker
                .as_ref()
                .ok_or_else(|| io::Error::other("worker handle missing"))?;
            wait_exit(worker.as_raw_handle())?;
            Ok(())
        })();
        if let Err(error) = result {
            let job_cleanup = if unsafe { TerminateJobObject(outer.raw(), CLEANUP_EXIT) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            let creator_cleanup = creator.terminate();
            let worker_cleanup = observed_worker
                .as_ref()
                .map(|worker| wait_exit(worker.as_raw_handle()))
                .transpose();
            drop(observed_worker);
            drop(creator);
            let empty = wait_empty(&outer);
            return match (job_cleanup, creator_cleanup, worker_cleanup, empty) {
                (Ok(()), Ok(()), Ok(_), Ok(())) => Err(error),
                (job, creator, worker, empty) => Err(io::Error::other(format!(
                    "{error}; cleanup: job={job:?}, creator={creator:?}, worker={worker:?}, empty={empty:?}"
                ))),
            };
        }
        // ActiveProcesses may include exited processes while handles to them
        // remain open. Release the pinned identities only after observing exit.
        drop(observed_worker);
        drop(creator);
        if let Err(error) = wait_empty(&outer) {
            let cleanup = if unsafe { TerminateJobObject(outer.raw(), CLEANUP_EXIT) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                wait_empty(&outer)
            };
            return Err(io::Error::other(format!(
                "{error}; outer-job cleanup: {cleanup:?}"
            )));
        }
        Ok(())
    }
}
