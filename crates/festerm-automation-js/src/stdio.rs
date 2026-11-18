use crate::{
    protocol::{write_frame, WorkerFailure, WorkerMessage},
    worker_entry, Error,
};

/// Run the early worker branch with inherited standard streams, under the
/// same preconditions as [`worker_entry`]. A failure is reported as a
/// content-free protocol frame when the pipe is still writable; the caller
/// must also exit unsuccessfully if the frame cannot be delivered.
///
/// On Windows the worker has no standard handles; it connects to the
/// supervisor's PID-named pipe, which verifies this process as its client.
pub fn worker_stdio() -> Result<(), Error> {
    #[cfg(windows)]
    let pipe = crate::windows::WorkerPipe::connect()?;
    #[cfg(windows)]
    let (input, mut output) = (&pipe, &pipe);
    #[cfg(not(windows))]
    let (input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    let result = worker_entry(input, &mut output);
    if let Err(error) = &result {
        let code = match error {
            Error::Unavailable(_) => WorkerFailure::Unavailable,
            Error::Snapshot => WorkerFailure::InvalidSnapshot,
            Error::Protocol => WorkerFailure::Protocol,
            Error::Deadline => WorkerFailure::Deadline,
            _ => WorkerFailure::Io,
        };
        let _ = write_frame(&mut output, &WorkerMessage::Failed { code });
    }
    result
}

/// Run the distinct terminal-program worker mode on inherited stdio.
#[cfg(all(feature = "javascript-program-dev", not(target_os = "ios")))]
pub fn program_worker_stdio() -> Result<(), Error> {
    #[cfg(target_os = "ios")]
    return Err(Error::Unavailable(
        "iOS has no qualified separate JavaScript program worker",
    ));
    #[cfg(not(target_os = "ios"))]
    {
        #[cfg(windows)]
        let pipe = crate::windows::WorkerPipe::connect()?;
        #[cfg(windows)]
        let (input, output) = (&pipe, &pipe);
        #[cfg(not(windows))]
        let (input, output) = (std::io::stdin().lock(), std::io::stdout().lock());
        crate::program_worker::program_worker_entry(input, output)
    }
}
