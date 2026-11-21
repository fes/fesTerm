fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [argument] if argument == "--automation-worker" => festerm_automation_js::worker_stdio(),
        #[cfg(feature = "javascript-program-dev")]
        [argument] if argument == "--javascript-program-worker" => {
            festerm_automation_js::program_worker_stdio()
        }
        _ => std::process::exit(64),
    };
    if result.is_err() {
        std::process::exit(70);
    }
}
