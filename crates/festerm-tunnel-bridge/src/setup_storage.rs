//! Owner-only, handle-relative storage for the explicitly managed host.
pub use festerm_sessiond::private_storage::*;

pub fn script_namespace() -> std::io::Result<std::path::PathBuf> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .map(|path| path.join("fesTerm-tunnels"));
    #[cfg(unix)]
    let root = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|path| path.join(".local").join("share").join("festerm-tunnels"));
    root.filter(|path| path.is_absolute())
        .ok_or_else(|| std::io::Error::other("management.private_namespace_unavailable"))
}
