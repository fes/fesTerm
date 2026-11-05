use crate::{
    protocol::{write_frame, WorkerFailure, WorkerMessage},
    worker_entry, Error,
};

/// Run the early worker branch with inherited standard streams, under the
/// same preconditions as [`worker_entry`]. A failure is reported as a
/// content-free protocol frame when the pipe is still writable; the caller
/// must also exit unsuccessfully if the frame cannot be delivered.
pub fn worker_stdio() -> Result<(), Error> {
    let result = worker_entry(std::io::stdin().lock(), std::io::stdout().lock());
    if let Err(error) = &result {
        let code = match error {
            Error::Unavailable(_) => WorkerFailure::Unavailable,
            Error::Snapshot => WorkerFailure::InvalidSnapshot,
            Error::Protocol => WorkerFailure::Protocol,
            Error::Deadline => WorkerFailure::Deadline,
            _ => WorkerFailure::Io,
        };
        let _ = write_frame(
            &mut std::io::stdout().lock(),
            &WorkerMessage::Failed { code },
        );
    }
    result
}
