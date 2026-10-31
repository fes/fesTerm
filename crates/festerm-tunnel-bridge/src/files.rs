//! Explicit read-only capability under the bridge's OS user. No shell, SFTP,
//! pathname reopen, link traversal, device paths, or remote writes.
use crate::protocol::invalid;
use serde::{Deserialize, Serialize};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
#[cfg(feature = "relay")]
use std::{
    io::Read,
    path::{Component, Path},
};
#[cfg(feature = "relay")]
use tokio::sync::Semaphore;

pub use crate::protocol::{FILE_LIMIT, FILE_READ, FILE_RESULT};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStyle {
    #[default]
    Posix,
    Windows,
}
impl PathStyle {
    pub fn native() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadRequest {
    pub id: u64,
    pub path: String,
}

#[cfg_attr(not(feature = "relay"), allow(dead_code))]
pub(crate) struct FileCall {
    pub request: ReadRequest,
    pub reply: flume::Sender<io::Result<Vec<u8>>>,
}
#[cfg_attr(not(feature = "relay"), allow(dead_code))]
pub(crate) type PendingFile = Arc<std::sync::Mutex<Option<FileCall>>>;

#[derive(Clone)]
pub struct FileRequestor {
    pub(crate) sender: flume::Sender<FileCall>,
    pub(crate) busy: Arc<AtomicBool>,
    pub(crate) next: Arc<AtomicU64>,
    pub(crate) stopped: tokio_util::sync::CancellationToken,
    pub path_style: PathStyle,
    pub endpoint_identity: String,
    pub generation: u64,
}
impl std::fmt::Debug for FileRequestor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DevTunnelFileRequestor(<bound live endpoint>)")
    }
}
impl FileRequestor {
    pub fn available(&self) -> bool {
        !self.stopped.is_cancelled() && !self.sender.is_disconnected()
    }
    pub fn read(&self, path: &str, maximum: usize) -> io::Result<Vec<u8>> {
        if !self.available() || maximum > FILE_LIMIT || !valid_path(path, self.path_style) {
            return Err(invalid());
        }
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| invalid())?;
        struct Busy(Arc<AtomicBool>);
        impl Drop for Busy {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _busy = Busy(self.busy.clone());
        let id = crate::reserve_sequence(&self.next)?;
        let (reply, result) = flume::bounded(1);
        self.sender
            .try_send(FileCall {
                request: ReadRequest {
                    id,
                    path: path.into(),
                },
                reply,
            })
            .map_err(|_| invalid())?;
        let bytes = result
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| invalid())??;
        if !self.available() || bytes.len() > maximum {
            return Err(invalid());
        }
        Ok(bytes)
    }
}

pub fn valid_path(path: &str, style: PathStyle) -> bool {
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return false;
    }
    let normalized = path.replace('\\', "/");
    if normalized.starts_with("//")
        || normalized.starts_with("/??/")
        || normalized.starts_with("??/")
    {
        return false;
    }
    let components = match style {
        PathStyle::Windows => {
            let bytes = normalized.as_bytes();
            if bytes.len() < 4 || !bytes[0].is_ascii_alphabetic() || bytes[1..3] != *b":/" {
                return false;
            }
            &normalized[3..]
        }
        PathStyle::Posix => {
            if !path.starts_with('/') || path.contains('\\') {
                return false;
            }
            &path[1..]
        }
    };
    components.split('/').all(|part| {
        if part.is_empty() || matches!(part, "." | "..") {
            return false;
        }
        if style == PathStyle::Windows {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            !part.contains([':', '*', '?', '"', '<', '>', '|'])
                && !part.ends_with(['.', ' '])
                && !matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                )
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit())
        } else {
            true
        }
    })
}

#[cfg(feature = "relay")]
#[derive(Clone)]
pub struct FileService {
    allowed: bool,
    slot: Arc<Semaphore>,
}
#[cfg(feature = "relay")]
impl FileService {
    pub fn allows_read(&self) -> bool {
        self.allowed
    }
    pub fn new(allowed: bool) -> Self {
        Self {
            allowed,
            slot: Arc::new(Semaphore::new(1)),
        }
    }
    pub async fn read(&self, path: String) -> io::Result<Vec<u8>> {
        if !self.allowed || !valid_path(&path, PathStyle::native()) {
            return Err(invalid());
        }
        let permit = self
            .slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| invalid())?;
        let job = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read_file(&path)
        });
        tokio::time::timeout(Duration::from_secs(30), job)
            .await
            .map_err(|_| invalid())?
            .map_err(|_| invalid())?
    }
}

#[cfg(feature = "relay")]
fn read_file(path: &str) -> io::Result<Vec<u8>> {
    if !valid_path(path, PathStyle::native()) {
        return Err(invalid());
    }
    let path = Path::new(path);
    let mut parts = path.components();
    #[cfg(windows)]
    let mut parent = {
        let Some(Component::Prefix(prefix)) = parts.next() else {
            return Err(invalid());
        };
        if parts.next() != Some(Component::RootDir) {
            return Err(invalid());
        }
        let mut root = prefix.as_os_str().to_os_string();
        root.push("\\");
        let root = std::path::PathBuf::from(root);
        festerm_windows_security::require_local_disk_root(&root)?;
        festerm_windows_security::lock_directory_path_without_delete_sharing(&root)?
    };
    #[cfg(unix)]
    let mut parent = {
        if parts.next() != Some(Component::RootDir) {
            return Err(invalid());
        }
        std::fs::File::open("/")?
    };
    let names: Vec<_> = parts
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(invalid()),
        })
        .collect::<io::Result<_>>()?;
    let (name, directories) = names.split_last().ok_or_else(invalid)?;
    for name in directories {
        #[cfg(windows)]
        {
            parent = festerm_windows_security::open_directory_no_reparse(&parent, Path::new(name))?;
        }
        #[cfg(unix)]
        {
            parent =
                festerm_unix_security::open_directory_search_nofollow(&parent, Path::new(name))?;
        }
    }
    #[cfg(windows)]
    let file = festerm_windows_security::open_file_no_reparse(&parent, Path::new(name))?;
    #[cfg(unix)]
    let file = festerm_unix_security::open_file_nofollow(&parent, Path::new(name))?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > FILE_LIMIT as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > FILE_LIMIT {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn bridge_regular_file_reads_are_opt_in_bounded_and_admitted_once() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::current_dir().unwrap().join(format!(
            "bridge-file-read-{}-{nonce}.txt",
            std::process::id()
        ));
        struct Owned(std::path::PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _owned = Owned(path.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, b"remote owned text").unwrap();
        file.sync_all().unwrap();
        let name = path.to_str().unwrap().to_owned();
        assert!(FileService::new(false).read(name.clone()).await.is_err());
        let service = FileService::new(true);
        assert_eq!(
            service.read(name.clone()).await.unwrap(),
            b"remote owned text"
        );
        let permit = service.slot.try_acquire().unwrap();
        assert!(service.read(name.clone()).await.is_err());
        drop(permit);
        file.set_len(FILE_LIMIT as u64 + 1).unwrap();
        assert!(service.read(name).await.is_err());
    }

    #[test]
    fn remote_path_style_rejects_namespaces_and_ambiguity() {
        for path in [
            "C:relative.txt",
            "/etc/passwd",
            r"\\server\share\x",
            r"C:\x\..\y",
            r"C:\x:secret",
            r"C:\NUL.txt",
            r"C:\x.",
        ] {
            assert!(!valid_path(path, PathStyle::Windows), "{path}");
        }
        for path in ["/etc/passwd", "/work/report.md"] {
            assert!(valid_path(path, PathStyle::Posix));
        }
        assert!(valid_path(r"C:\work\report.md", PathStyle::Windows));
        assert!(!valid_path(r"C:\work\report.md", PathStyle::Posix));
        assert!(!valid_path("//server/share/x", PathStyle::Posix));
    }
}
