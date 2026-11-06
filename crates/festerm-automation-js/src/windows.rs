//! Windows worker containment: a zero-capability AppContainer process, created
//! suspended and placed in a kill-on-close Job Object before it runs, with a
//! hard 128MiB commit ceiling, one active process, a CPU-time ceiling, and UI
//! restrictions. No handles are inherited. The worker connects to a single
//! instance, PID-named local pipe whose DACL admits only the current user and
//! this AppContainer, and the supervisor verifies the client process ID.

use crate::{Error, CPU_SECONDS, PROCESS_BYTES};
use std::{
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::{
        LocalFree, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
        ERROR_PIPE_LISTENING, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
            GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SDDL_REVISION_1, SE_FILE_OBJECT, TRUSTEE_IS_SID,
            TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        FreeSid, GetTokenInformation,
        Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName},
        TokenCapabilities, TokenIsAppContainer, TokenUser, ACL, DACL_SECURITY_INFORMATION,
        NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
        TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, CREATE_NEW, FILE_FLAG_DELETE_ON_CLOSE,
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, OPEN_EXISTING,
        PIPE_ACCESS_DUPLEX,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
            JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
            JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS,
            JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
            JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
        },
        Memory::{VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE},
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId,
            SetNamedPipeHandleState, PIPE_NOWAIT, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE,
        },
        SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW},
        Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetCurrentProcessId,
            GetExitCodeProcess, GetProcessTimes, InitializeProcThreadAttributeList,
            OpenProcessToken, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
            EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
        },
        WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
    },
};

const CONTAINER_NAME: &str = "festerm.automation.javascript-worker";
const PIPE_BUFFER: u32 = 128 * 1024;
const REQUIRED_LIMITS: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    | JOB_OBJECT_LIMIT_PROCESS_MEMORY
    | JOB_OBJECT_LIMIT_JOB_MEMORY
    | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
    | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
    | JOB_OBJECT_LIMIT_PROCESS_TIME;

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain([0]).collect()
}

fn pipe_name(pid: u32) -> Vec<u16> {
    wide(format!(r"\\.\pipe\festerm-automation-js-{pid}"))
}

fn last_error() -> Error {
    Error::Io(io::Error::last_os_error())
}

fn api_error(api: &str) -> Error {
    let error = io::Error::last_os_error();
    Error::Io(io::Error::new(
        error.kind(),
        format!(
            "{api} failed (OS error {}): {error}",
            error.raw_os_error().unwrap_or(0)
        ),
    ))
}

fn unavailable(reason: &'static str) -> Error {
    Error::Unavailable(reason)
}

fn owned(handle: HANDLE) -> Option<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        None
    } else {
        // SAFETY: the caller transfers a fresh, valid handle it exclusively owns.
        Some(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

/// An AppContainer SID, freed with the allocator that produced it.
struct ContainerSid(PSID);

impl ContainerSid {
    fn get() -> Result<Self, Error> {
        let name = wide(CONTAINER_NAME);
        let mut sid = ptr::null_mut();
        // SAFETY: NUL-terminated names; no capabilities; sid is an out pointer.
        let created = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &mut sid,
            )
        };
        if created >= 0 && !sid.is_null() {
            return Ok(Self(sid));
        }
        let exists = (0x8007_0000u32 | ERROR_ALREADY_EXISTS) as i32;
        if created != exists {
            return Err(unavailable("AppContainer profile unavailable"));
        }
        // SAFETY: NUL-terminated name; sid is an out pointer.
        if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0
            || sid.is_null()
        {
            return Err(unavailable("AppContainer profile unavailable"));
        }
        Ok(Self(sid))
    }

    fn string(&self) -> Result<String, Error> {
        sid_string(self.0)
    }
}

impl Drop for ContainerSid {
    fn drop(&mut self) {
        // SAFETY: both userenv functions return a SID released with FreeSid.
        unsafe { FreeSid(self.0) };
    }
}

fn sid_string(sid: PSID) -> Result<String, Error> {
    let mut text = ptr::null_mut();
    // SAFETY: sid is valid for this call; text is released with LocalFree.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err(last_error());
    }
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated wide string.
    let length = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
    // SAFETY: length counted above within the returned allocation.
    let value = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(value)
}

fn token_information(class: i32) -> Result<Vec<u64>, Error> {
    let mut token = ptr::null_mut();
    // SAFETY: queries this process's own token; token is an out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(last_error());
    }
    let token = owned(token).ok_or_else(last_error)?;
    let mut needed = 0;
    // SAFETY: size query with a null buffer.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    // u64 storage keeps the returned structures suitably aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: buffer is writable for the stated byte length.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(buffer)
}

/// Permit only the current user and this AppContainer, at low integrity so
/// the AppContainer's integrity level can write to it.
fn pipe_security(container: &ContainerSid) -> Result<SecurityDescriptor, Error> {
    let user = token_information(TokenUser)?;
    // SAFETY: TokenUser returns a TOKEN_USER at the start of the buffer.
    let user = sid_string(unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid })?;
    let sddl = wide(format!(
        "D:P(A;;GA;;;{user})(A;;GRGW;;;{})S:(ML;;NW;;;LW)",
        container.string()?
    ));
    let mut descriptor = ptr::null_mut();
    // SAFETY: sddl is NUL-terminated; descriptor is released with LocalFree.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(SecurityDescriptor(descriptor))
}

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

/// An AppContainer must be able to read and execute its image. Per-user
/// installs and build directories lack ALL APPLICATION PACKAGES, so grant
/// this container SID read/execute on the install-owned worker executable
/// only. Program Files already grants packages; a refusal there is ignored
/// and any real denial fails worker qualification.
fn grant_image_access(program: &Path, container: &ContainerSid) {
    let path = wide(program);
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: path is NUL-terminated; out pointers are valid; descriptor owns dacl.
    if unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    } != 0
    {
        return;
    }
    let descriptor = SecurityDescriptor(descriptor);
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: container.0.cast(),
        },
    };
    let mut updated = ptr::null_mut();
    // SAFETY: access and the existing dacl are valid; updated uses LocalFree.
    if unsafe { SetEntriesInAclW(1, &access, dacl, &mut updated) } != 0 {
        return;
    }
    // SAFETY: path and updated are valid for this synchronous call.
    unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            updated,
            ptr::null(),
        );
        LocalFree(updated.cast());
    }
    drop(descriptor);
}

fn create_job() -> Result<OwnedHandle, Error> {
    // SAFETY: anonymous, non-inheritable job with default security.
    let job =
        owned(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) }).ok_or_else(last_error)?;
    // SAFETY: zero is a valid initial value for this plain C structure.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = REQUIRED_LIMITS;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.BasicLimitInformation.PerProcessUserTimeLimit = (CPU_SECONDS * 10_000_000) as i64;
    limits.ProcessMemoryLimit = PROCESS_BYTES as usize;
    limits.JobMemoryLimit = PROCESS_BYTES as usize;
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    // SAFETY: each structure is initialized and sized for its class.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
        || unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                std::mem::size_of_val(&ui) as u32,
            )
        } == 0
    {
        return Err(unavailable("worker Job Object limits unavailable"));
    }
    Ok(job)
}

/// Supervisor-owned contained worker process.
pub(crate) struct Child {
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
    pipe: Arc<OwnedHandle>,
    pub(crate) stdin: Option<PipeEnd>,
    pub(crate) stdout: Option<PipeEnd>,
}

impl Child {
    pub(crate) fn spawn(program: &Path) -> Result<Self, Error> {
        let container = ContainerSid::get()?;
        grant_image_access(program, &container);
        let job = create_job()?;
        let mut capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: container.0,
            Capabilities: ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let child_policy: u32 = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
        let mut size = 0;
        // SAFETY: size query for a two-attribute list.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut size) };
        let mut storage = vec![0u64; size.div_ceil(8)];
        let list: LPPROC_THREAD_ATTRIBUTE_LIST = storage.as_mut_ptr().cast();
        // SAFETY: storage is at least `size` bytes and outlives the list.
        if unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) } == 0 {
            return Err(last_error());
        }
        struct List(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for List {
            fn drop(&mut self) {
                // SAFETY: initialized above; deleted once.
                unsafe { DeleteProcThreadAttributeList(self.0) };
            }
        }
        let list = List(list);
        // SAFETY: attribute values outlive CreateProcessW below.
        if unsafe {
            UpdateProcThreadAttribute(
                list.0,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                (&raw mut capabilities).cast(),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
            || unsafe {
                UpdateProcThreadAttribute(
                    list.0,
                    0,
                    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize,
                    (&raw const child_policy).cast(),
                    std::mem::size_of::<u32>(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            } == 0
        {
            return Err(unavailable("AppContainer process attributes unavailable"));
        }
        // SAFETY: zero is a valid initial value for these C structures.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // The worker has no standard handles and speaks only on its verified pipe.
        startup.lpAttributeList = list.0;
        let application = wide(program);
        let mut command_line = wide(format!("\"{}\" --automation-worker", program.display()));
        let environment = worker_environment()?;
        let mut current_directory = windows_directory()?;
        current_directory.push(0);
        // SAFETY: zero is a valid initial value for this output structure.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: strings are NUL-terminated and writable where required;
        // no handles are inherited and the environment block is terminated.
        if unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                0,
                CREATE_SUSPENDED
                    | DETACHED_PROCESS
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_ptr().cast(),
                current_directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        } == 0
        {
            return Err(api_error("CreateProcessW"));
        }
        drop(list);
        let process = owned(info.hProcess).ok_or_else(last_error)?;
        let thread = owned(info.hThread).ok_or_else(last_error)?;
        let kill = |error: Error| {
            // SAFETY: the suspended process is owned by this function.
            unsafe { TerminateProcess(process.as_raw_handle(), 1) };
            error
        };
        // SAFETY: both handles are live and owned here.
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process.as_raw_handle()) } == 0 {
            return Err(kill(unavailable("worker Job Object assignment failed")));
        }
        let pipe = (|| {
            let security = pipe_security(&container)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: security.0,
                bInheritHandle: 0,
            };
            let name = pipe_name(info.dwProcessId);
            // SAFETY: name and attributes remain valid for this call.
            let raw = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    PIPE_BUFFER,
                    PIPE_BUFFER,
                    0,
                    &attributes,
                )
            };
            owned(raw).ok_or_else(last_error)
        })();
        let pipe = match pipe {
            Ok(pipe) => Arc::new(pipe),
            Err(error) => return Err(kill(error)),
        };
        // SAFETY: resumes the suspended primary thread of the owned process.
        if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
            return Err(kill(api_error("ResumeThread")));
        }
        Ok(Self {
            process,
            job,
            pid: info.dwProcessId,
            stdin: Some(PipeEnd(pipe.clone())),
            stdout: Some(PipeEnd(pipe.clone())),
            pipe,
        })
    }

    /// Accept only this worker process as the pipe's single client.
    pub(crate) fn poll_connected(&mut self) -> Result<bool, Error> {
        let handle = self.pipe.as_raw_handle();
        // SAFETY: PIPE_NOWAIT server handle; no OVERLAPPED.
        if unsafe { ConnectNamedPipe(handle, ptr::null_mut()) } == 0 {
            match io::Error::last_os_error().raw_os_error().map(|c| c as u32) {
                Some(ERROR_PIPE_LISTENING) => return Ok(false),
                Some(ERROR_PIPE_CONNECTED) => {}
                _ => return Err(unavailable("worker pipe connection failed")),
            }
        }
        let mut client = 0;
        // SAFETY: handle is live; client is an out pointer.
        if unsafe { GetNamedPipeClientProcessId(handle, &mut client) } == 0 || client != self.pid {
            return Err(unavailable(
                "worker pipe client is not the contained worker",
            ));
        }
        Ok(true)
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<u32>> {
        // SAFETY: zero-timeout wait on the owned process handle.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } != WAIT_OBJECT_0 {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: the process has exited; code is an out pointer.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(code))
    }

    pub(crate) fn kill(&mut self) -> io::Result<()> {
        // SAFETY: terminates every process in this owned job.
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A synchronous `PIPE_NOWAIT` endpoint: an empty pipe reads as `WouldBlock`
/// and a full pipe accepts zero bytes, also reported as `WouldBlock`.
pub(crate) struct PipeEnd(Arc<OwnedHandle>);

fn nowait_read(handle: HANDLE, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let mut read = 0;
    // SAFETY: synchronous PIPE_NOWAIT read into a live buffer.
    if unsafe {
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            buffer.len().min(u32::MAX as usize) as u32,
            &mut read,
            ptr::null_mut(),
        )
    } != 0
    {
        return Ok(read as usize);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|c| c as u32) {
        Some(ERROR_NO_DATA | ERROR_PIPE_LISTENING) => Err(io::ErrorKind::WouldBlock.into()),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) => Ok(0),
        _ => Err(error),
    }
}

fn nowait_write(handle: HANDLE, buffer: &[u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    // SAFETY: synchronous PIPE_NOWAIT write; the buffer is not retained.
    if unsafe {
        WriteFile(
            handle,
            buffer.as_ptr(),
            buffer.len().min(u32::MAX as usize) as u32,
            &mut written,
            ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|c| c as u32) {
            Some(ERROR_PIPE_LISTENING) => Err(io::ErrorKind::WouldBlock.into()),
            _ => Err(error),
        };
    }
    if written == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    Ok(written as usize)
}

impl Read for PipeEnd {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        nowait_read(self.0.as_raw_handle(), buffer)
    }
}

impl Write for PipeEnd {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        nowait_write(self.0.as_raw_handle(), buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The worker's end of its supervisor pipe.
pub struct WorkerPipe(OwnedHandle);

impl WorkerPipe {
    pub(crate) fn connect() -> Result<Self, Error> {
        // SAFETY: scalar query.
        let name = pipe_name(unsafe { GetCurrentProcessId() });
        // SAFETY: NUL-terminated name; no sharing, security attributes, or template.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        let handle = owned(raw).ok_or(Error::Unavailable("worker pipe unavailable"))?;
        let mode = PIPE_READMODE_BYTE | PIPE_NOWAIT;
        // SAFETY: live client handle opened with write-attribute access.
        if unsafe {
            SetNamedPipeHandleState(handle.as_raw_handle(), &mode, ptr::null(), ptr::null())
        } == 0
        {
            return Err(Error::Unavailable("worker pipe unavailable"));
        }
        Ok(Self(handle))
    }
}

impl Read for &WorkerPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        nowait_read(self.0.as_raw_handle(), buffer)
    }
}

impl Write for &WorkerPipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        nowait_write(self.0.as_raw_handle(), buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn system_path(directory: unsafe extern "system" fn(*mut u16, u32) -> u32, leaf: &str) -> Vec<u16> {
    let mut buffer = [0u16; 260];
    // SAFETY: buffer is writable for its stated length.
    let length = unsafe { directory(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    let mut path: Vec<u16> = buffer[..length.min(buffer.len())].to_vec();
    path.extend(leaf.encode_utf16());
    path.push(0);
    path
}

fn windows_directory() -> Result<Vec<u16>, Error> {
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: buffer is writable for its stated length.
    let length = unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(unavailable("Windows directory unavailable"));
    }
    buffer.truncate(length);
    Ok(buffer)
}

// CreateProcess needs SystemRoot for the child runtime; inherit nothing else.
fn worker_environment() -> Result<Vec<u16>, Error> {
    let mut environment: Vec<_> = "SystemRoot=".encode_utf16().collect();
    environment.extend(windows_directory()?);
    environment.extend([0, 0]);
    Ok(environment)
}

fn worker_environment_isolated() -> bool {
    let Ok(system_root) = windows_directory() else {
        return false;
    };
    let mut variables = std::env::vars_os();
    let Some((key, value)) = variables.next() else {
        return false;
    };
    key == "SystemRoot"
        && value.encode_wide().eq(system_root.iter().copied())
        && variables.next().is_none()
}

/// Worker-side qualification before Ready: verify the containment the
/// supervisor established, then require representative native denials.
pub(crate) fn confine() -> Result<(), Error> {
    if !worker_environment_isolated() {
        return Err(unavailable(
            "worker environment was not cleared by its supervisor",
        ));
    }
    let container = token_information(TokenIsAppContainer)?;
    if container[0] as u32 == 0 {
        return Err(unavailable("worker is not in an AppContainer"));
    }
    let capabilities = token_information(TokenCapabilities)?;
    // SAFETY: TokenCapabilities returns a TOKEN_GROUPS at the buffer start.
    if unsafe { (*capabilities.as_ptr().cast::<TOKEN_GROUPS>()).GroupCount } != 0 {
        return Err(unavailable("worker AppContainer has capabilities"));
    }
    let mut in_job = 0;
    // SAFETY: queries this process; result is an out pointer.
    if unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &mut in_job) } == 0
        || in_job == 0
    {
        return Err(unavailable("worker is not in a Job Object"));
    }
    // SAFETY: zero is a valid initial value for this output structure.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a null job queries this process's job; limits is writable.
    if unsafe {
        QueryInformationJobObject(
            ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            std::mem::size_of_val(&limits) as u32,
            ptr::null_mut(),
        )
    } == 0
        || limits.BasicLimitInformation.LimitFlags & REQUIRED_LIMITS != REQUIRED_LIMITS
        || limits.BasicLimitInformation.ActiveProcessLimit != 1
        || limits.ProcessMemoryLimit as u64 > PROCESS_BYTES
        || limits.BasicLimitInformation.PerProcessUserTimeLimit > (CPU_SECONDS * 10_000_000) as i64
    {
        return Err(unavailable("worker Job Object limits not active"));
    }
    native_probes()
}

fn native_probes() -> Result<(), Error> {
    // Hard commit ceiling.
    // SAFETY: reservation probe; released immediately if wrongly admitted.
    let memory = unsafe {
        VirtualAlloc(
            ptr::null(),
            PROCESS_BYTES as usize + 1,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if !memory.is_null() {
        // SAFETY: memory was returned by VirtualAlloc above.
        unsafe { VirtualFree(memory, 0, MEM_RELEASE) };
        return Err(unavailable("hard process memory ceiling not enforced"));
    }
    // No file creation outside the container.
    let probe = system_path(
        GetWindowsDirectoryW,
        &format!(r"\Temp\festerm-js-probe-{}", std::process::id()),
    );
    // SAFETY: NUL-terminated path; an unexpected file is deleted on close.
    let file = unsafe {
        CreateFileW(
            probe.as_ptr(),
            GENERIC_WRITE,
            0,
            ptr::null(),
            CREATE_NEW,
            FILE_FLAG_DELETE_ON_CLOSE,
            ptr::null_mut(),
        )
    };
    if owned(file).is_some() {
        return Err(unavailable("active native denial probe failed"));
    }
    // No child processes.
    let application = system_path(GetSystemDirectoryW, r"\cmd.exe");
    let mut command_line = wide("cmd.exe /c exit 0");
    // SAFETY: zero is a valid initial value for these C structures.
    let mut startup: windows_sys::Win32::System::Threading::STARTUPINFOW =
        unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    // SAFETY: as above.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated strings; suspended so a wrongly admitted child
    // never runs before it is terminated.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_SUSPENDED | DETACHED_PROCESS,
            ptr::null(),
            ptr::null(),
            &startup,
            &mut info,
        )
    } != 0
    {
        // SAFETY: handles were returned by CreateProcessW above.
        unsafe { TerminateProcess(info.hProcess, 1) };
        drop(owned(info.hProcess));
        drop(owned(info.hThread));
        return Err(unavailable("active native denial probe failed"));
    }
    Ok(())
}

pub(crate) fn cpu_time() -> Option<std::time::Duration> {
    let mut created = Default::default();
    let mut exited = Default::default();
    let mut kernel = windows_sys::Win32::Foundation::FILETIME::default();
    let mut user = windows_sys::Win32::Foundation::FILETIME::default();
    // SAFETY: all out pointers are valid FILETIMEs.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    let ticks = |time: windows_sys::Win32::Foundation::FILETIME| {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    };
    let total = ticks(kernel).checked_add(ticks(user))?;
    Some(std::time::Duration::from_nanos(total.checked_mul(100)?))
}
