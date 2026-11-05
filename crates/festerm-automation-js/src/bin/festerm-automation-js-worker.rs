fn main() {
    let mut args = std::env::args_os();
    let _program = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--automation-worker"))
        || args.next().is_some()
    {
        std::process::exit(64);
    }
    if festerm_automation_js::worker_stdio().is_err() {
        std::process::exit(70);
    }
}
