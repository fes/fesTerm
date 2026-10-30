use std::path::PathBuf;

pub(crate) fn private_directory(prefix: &str) -> PathBuf {
    #[cfg(windows)]
    let parents = [
        std::env::temp_dir(),
        // Runner profiles can grant another account direct administrator access.
        // Try system temp only if the real private-storage policy permits it.
        PathBuf::from(std::env::var_os("SystemRoot").expect("Windows system directory"))
            .join("Temp"),
    ];
    #[cfg(not(windows))]
    let parents = [std::env::current_dir().unwrap()];
    let name = format!("{prefix}-{}", uuid::Uuid::new_v4());
    let mut failures = Vec::new();
    for parent in parents {
        let path = parent.join(&name);
        match crate::setup_storage::private_directory(&path, true) {
            Ok(directory) => {
                drop(directory);
                return path;
            }
            Err(error) => {
                eprintln!(
                    "private fixture candidate refused: {}: {error}",
                    path.display()
                );
                failures.push((path, error));
            }
        }
    }
    panic!("no accessible policy-compliant private fixture directory: {failures:?}");
}
