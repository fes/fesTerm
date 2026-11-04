use std::io;

pub const REQUIRED_ENVIRONMENT: [(&str, &str); 7] = [
    ("FESTERM_WINDOWS_JOB_DISPOSABLE_VM", "1"),
    ("FESTERM_WINDOWS_JOB_RUNNER_ENVIRONMENT", "github-hosted"),
    ("FESTERM_WINDOWS_JOB_RUNNER_OS", "Windows"),
    ("GITHUB_ACTIONS", "true"),
    ("GITHUB_JOB", "windows-job-ownership-probe"),
    ("RUNNER_ENVIRONMENT", "github-hosted"),
    ("RUNNER_OS", "Windows"),
];

pub fn require_with(windows: bool, read: impl Fn(&str) -> Option<String>) -> io::Result<()> {
    if !windows {
        return Err(io::Error::other(
            "ownership probe refused: Windows is required",
        ));
    }
    for (name, expected) in REQUIRED_ENVIRONMENT {
        if read(name).as_deref() != Some(expected) {
            return Err(io::Error::other(format!(
                "ownership probe refused: {name} must be {expected}"
            )));
        }
    }
    Ok(())
}

pub fn require_current() -> io::Result<()> {
    require_with(cfg!(windows), |name| std::env::var(name).ok())
}

pub fn child_environment() -> Vec<u16> {
    let mut result = Vec::new();
    for (name, value) in REQUIRED_ENVIRONMENT {
        result.extend(name.encode_utf16());
        result.push(b'=' as u16);
        result.extend(value.encode_utf16());
        result.push(0);
    }
    result.push(0);
    result
}
