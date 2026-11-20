// stats_alloc measures the whole process, so each target uses its own single-
// thread main. Preserve Cargo and nextest's listing and filtering interface.
pub(crate) fn run(name: &str, check: fn()) {
    let mut args = std::env::args().skip(1);
    let mut filters = Vec::new();
    let mut skips = Vec::new();
    let mut list = false;
    let mut exact = false;
    let mut ignored = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--exact" => exact = true,
            "--ignored" => ignored = true,
            "--include-ignored" | "--nocapture" | "--show-output" | "--quiet" | "-q" => {}
            "--format" => {
                let value = args.next().expect("--format requires a value");
                assert!(matches!(value.as_str(), "terse" | "pretty"));
            }
            "--test-threads" => assert_eq!(args.next().as_deref(), Some("1")),
            "--test-threads=1" => {}
            "--skip" => skips.push(args.next().expect("--skip requires a filter")),
            _ if arg.starts_with('-') => panic!("unsupported allocation-test argument: {arg}"),
            _ => filters.push(arg),
        }
    }
    let matches = |filter: &str| {
        if exact {
            name == filter
        } else {
            name.contains(filter)
        }
    };
    let selected = !ignored
        && (filters.is_empty() || filters.iter().any(|filter| matches(filter)))
        && !skips.iter().any(|filter| matches(filter));
    if list {
        if selected {
            println!("{name}: test");
        }
        return;
    }
    if selected {
        check();
        println!("test {name} ... ok");
    }
    println!(
        "test result: ok. {} passed; 0 failed",
        usize::from(selected)
    );
}
