fn main() {
    let mut args = std::env::args_os();
    let _program = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--automation-worker"))
        || args.next().is_some()
    {
        std::process::exit(64);
    }
    if let Err(error) =
        festerm_automation_js::worker_entry(std::io::stdin().lock(), std::io::stdout().lock())
    {
        // Never format an exception, request, source or authorized result here.
        use festerm_automation_js::{
            protocol::{write_frame, WorkerFailure, WorkerMessage},
            Error,
        };
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
        std::process::exit(70);
    }
}
