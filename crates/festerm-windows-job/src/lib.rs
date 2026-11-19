//! Narrow safe ownership of a Windows Job Object.
//!
//! This crate isolates the Win32 FFI required to ensure that closing or
//! terminating a job also terminates every process assigned to that job.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod imp {
    use std::{io, os::windows::io::RawHandle, ptr};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, STILL_ACTIVE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        },
        System::Threading::{
            GetCurrentProcess, GetExitCodeProcess, OpenProcess, TerminateProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        },
    };

    struct ProcessHandle(HANDLE);

    impl ProcessHandle {
        fn open(pid: u32, access: u32) -> io::Result<Self> {
            let handle = unsafe { OpenProcess(access, 0, pid) };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(handle))
            }
        }
    }

    /// Child creation mode allowed by the immediate Job. This is only a launch
    /// choice: nested ancestor Jobs may still retain the child.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ChildJobPolicy {
        NoJob,
        ExplicitBreakaway,
        SilentBreakaway,
        Blocked,
    }

    fn child_job_policy(flags: u32) -> ChildJobPolicy {
        if flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0 {
            ChildJobPolicy::ExplicitBreakaway
        } else if flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK != 0 {
            ChildJobPolicy::SilentBreakaway
        } else {
            ChildJobPolicy::Blocked
        }
    }

    /// Checks membership in any Job, including all inherited ancestor Jobs.
    pub fn current_process_is_in_job() -> io::Result<bool> {
        let mut in_job = 0;
        if unsafe { IsProcessInJob(GetCurrentProcess(), ptr::null_mut(), &raw mut in_job) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(in_job != 0)
    }

    /// Reads the immediate Job's child creation policy without changing it.
    /// Neither permissive result proves escape from every ancestor Job.
    pub fn current_process_child_job_policy() -> io::Result<ChildJobPolicy> {
        if !current_process_is_in_job()? {
            return Ok(ChildJobPolicy::NoJob);
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let queried = unsafe {
            QueryInformationJobObject(
                ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                (&raw mut limits).cast(),
                u32::try_from(std::mem::size_of_val(&limits))
                    .expect("Job Object limit structure fits in u32"),
                ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(child_job_policy(limits.BasicLimitInformation.LimitFlags))
    }

    /// Reports a known restriction, not proof that a later start will succeed.
    pub fn current_process_blocks_breakaway() -> io::Result<bool> {
        Ok(current_process_child_job_policy()? == ChildJobPolicy::Blocked)
    }

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    /// Returns whether a process exists and has not exited.
    pub fn process_is_alive(pid: u32) -> bool {
        let Ok(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            return false;
        };
        let mut exit_code = 0;
        let queried = unsafe { GetExitCodeProcess(process.0, &raw mut exit_code) != 0 };
        queried && exit_code == STILL_ACTIVE as u32
    }

    /// Terminates a process by identifier.
    pub fn terminate_process(pid: u32) -> io::Result<()> {
        if !process_is_alive(pid) {
            return Ok(());
        }
        let process = ProcessHandle::open(pid, PROCESS_TERMINATE)?;
        if unsafe { TerminateProcess(process.0, 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Owns a Windows Job Object with `KILL_ON_JOB_CLOSE` enabled.
    pub struct WindowsJob {
        handle: HANDLE,
    }

    // A Job Object handle may be used by the PTY reader and control threads.
    unsafe impl Send for WindowsJob {}
    unsafe impl Sync for WindowsJob {}

    impl WindowsJob {
        /// Creates a job and assigns an already-started process to it.
        pub fn assign_to_process(process: RawHandle) -> io::Result<Self> {
            Self::assign_with_limits(process, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
        }

        /// Controlled native-test fixture only: permits an inner breakaway
        /// while a separately owned outer Job still prevents complete escape.
        #[cfg(feature = "test-support")]
        pub fn assign_test_breakaway_inner(process: RawHandle) -> io::Result<Self> {
            Self::assign_with_limits(
                process,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK,
            )
        }

        fn assign_with_limits(process: RawHandle, flags: u32) -> io::Result<Self> {
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self { handle };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = flags;
            let configured = unsafe {
                SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    u32::try_from(std::mem::size_of_val(&limits))
                        .expect("Job Object limit structure fits in u32"),
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            let assigned = unsafe { AssignProcessToJobObject(job.handle, process.cast()) };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }

        /// Queries this exact owned Job, not merely membership in any OS Job.
        pub fn contains_process(&self, pid: u32) -> io::Result<bool> {
            let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
            let mut member = 0;
            if unsafe { IsProcessInJob(process.0, self.handle, &raw mut member) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(member != 0)
        }

        /// Terminates every process currently assigned to this job.
        pub fn terminate(&self) -> io::Result<()> {
            let terminated = unsafe { TerminateJobObject(self.handle, 1) };
            if terminated == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    impl Drop for WindowsJob {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn silent_job_does_not_request_explicit_breakaway() {
            assert_eq!(
                child_job_policy(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK),
                ChildJobPolicy::SilentBreakaway
            );
            assert_eq!(
                child_job_policy(JOB_OBJECT_LIMIT_BREAKAWAY_OK),
                ChildJobPolicy::ExplicitBreakaway
            );
            assert_eq!(
                child_job_policy(
                    JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK
                ),
                ChildJobPolicy::ExplicitBreakaway
            );
        }

        #[test]
        fn immediate_job_only_refuses_when_both_breakaway_modes_are_absent() {
            assert_eq!(child_job_policy(0), ChildJobPolicy::Blocked);
            assert_eq!(
                child_job_policy(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE),
                ChildJobPolicy::Blocked
            );
            for permitted in [
                JOB_OBJECT_LIMIT_BREAKAWAY_OK,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
            ] {
                assert_ne!(
                    child_job_policy(permitted | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE),
                    ChildJobPolicy::Blocked
                );
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{
    current_process_blocks_breakaway, current_process_child_job_policy, current_process_is_in_job,
    process_is_alive, terminate_process, ChildJobPolicy, WindowsJob,
};
