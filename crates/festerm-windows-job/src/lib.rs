//! Narrow safe ownership of a Windows Job Object.
//!
//! This crate isolates the Win32 FFI required to ensure that closing or
//! terminating a job also terminates every process assigned to that job.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod imp {
    use std::{io, os::windows::io::RawHandle, ptr};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
    };

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
            let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self { handle };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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
}

#[cfg(windows)]
pub use imp::WindowsJob;
