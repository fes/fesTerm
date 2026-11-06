//! Private imported copies, never direct writes to Storage Access Framework URIs.

use super::{
    DocumentFileError as Error, DocumentFileEvent as Event, DocumentFileSnapshot as Snapshot,
};
use crate::{document_writer::try_document_save, MAX_DOCUMENT_BYTES, MAX_NATIVE_DOCUMENTS};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

static DOCUMENTS: AtomicUsize = AtomicUsize::new(0);
static WRITES: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "android")]
#[derive(Default)]
pub struct DocumentPicker {
    active: std::cell::Cell<bool>,
}

#[cfg(target_os = "android")]
impl DocumentPicker {
    pub fn open(&self) -> Result<(), Error> {
        if self.active.get() {
            return Err(Error::Busy);
        }
        festerm_android_platform::pick_document().map_err(|_| Error::Unavailable)?;
        self.active.set(true);
        Ok(())
    }

    pub fn take_result(&self) -> Option<Result<DocumentFile, Error>> {
        let result = festerm_android_platform::take_picked_document()?;
        self.active.set(false);
        Some(
            result
                .map_err(|error| match error {
                    festerm_android_platform::PickError::Cancelled => Error::Cancelled,
                    festerm_android_platform::PickError::Unavailable => Error::Unavailable,
                })
                .and_then(|path| {
                    let root = festerm_android_platform::storage_directory()
                        .map_err(|_| Error::Unavailable)?
                        .join("Documents/Imports");
                    DocumentFile::new(
                        path,
                        &root,
                        Arc::new(|| {
                            let _ = festerm_android_platform::request_repaint();
                        }),
                    )
                }),
        )
    }
}

pub struct DocumentFile {
    state: Arc<State>,
}

struct State {
    path: PathBuf,
    busy: AtomicBool,
    events: Mutex<Vec<Event>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Drop for State {
    fn drop(&mut self) {
        DOCUMENTS.fetch_sub(1, Ordering::AcqRel);
    }
}

impl DocumentFile {
    fn new(path: PathBuf, root: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, Error> {
        let canonical = path.canonicalize().map_err(|_| Error::Unavailable)?;
        let root = root.canonicalize().map_err(|_| Error::Unavailable)?;
        if canonical != path || !canonical.starts_with(root) {
            return Err(Error::Unavailable);
        }
        read(&canonical)?;
        loop {
            let count = DOCUMENTS.load(Ordering::Acquire);
            if count >= MAX_NATIVE_DOCUMENTS {
                return Err(Error::Busy);
            }
            if DOCUMENTS
                .compare_exchange(count, count + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        Ok(Self {
            state: Arc::new(State {
                path,
                busy: AtomicBool::new(false),
                events: Mutex::new(Vec::new()),
                wake,
            }),
        })
    }

    fn start(&self, job: impl FnOnce(&Path) -> Event + Send + 'static) -> Result<(), Error> {
        if !self.state.events.lock().map_err(|_| Error::Io)?.is_empty()
            || self.state.busy.swap(true, Ordering::AcqRel)
        {
            return Err(Error::Busy);
        }
        let state = self.state.clone();
        if std::thread::Builder::new()
            .name("festerm-document".into())
            .spawn(move || {
                let event = job(&state.path);
                state
                    .events
                    .lock()
                    .expect("document event state poisoned")
                    .push(event);
                state.busy.store(false, Ordering::Release);
                (state.wake)();
            })
            .is_err()
        {
            self.state.busy.store(false, Ordering::Release);
            return Err(Error::Io);
        }
        Ok(())
    }

    pub fn open(&self) -> Result<(), Error> {
        self.refresh()
    }

    pub fn refresh(&self) -> Result<(), Error> {
        self.start(|path| match read(path) {
            Ok(snapshot) => Event::Loaded(snapshot),
            Err(error) => Event::Unavailable(error),
        })
    }

    pub fn save(&self, operation: u64, expected: [u8; 32], bytes: Vec<u8>) -> Result<(), Error> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(Error::TooLarge);
        }
        let permit = try_document_save(self.state.wake.clone())?;
        self.start(move |path| {
            let result = publish(path, expected, &bytes);
            drop(permit);
            match result {
                Ok(version) => Event::Saved { operation, version },
                Err(error) => Event::SaveFailed { operation, error },
            }
        })
    }

    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(
            &mut *self
                .state
                .events
                .lock()
                .expect("document event state poisoned"),
        )
    }
}

fn read(path: &Path) -> Result<Snapshot, Error> {
    let metadata = fs::symlink_metadata(path).map_err(|_| Error::Unavailable)?;
    if !metadata.file_type().is_file() {
        return Err(Error::NotAFile);
    }
    if metadata.len() > MAX_DOCUMENT_BYTES as u64 {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| Error::Io)?;
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file() {
        return Err(Error::NotAFile);
    }
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Io)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::TooLarge);
    }
    Ok(Snapshot {
        path: path.to_owned(),
        version: Sha256::digest(&bytes).into(),
        bytes,
        writable: !metadata.permissions().readonly(),
    })
}

fn publish(path: &Path, expected: [u8; 32], bytes: &[u8]) -> Result<[u8; 32], Error> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(Error::TooLarge);
    }
    let before = read(path)?;
    if !before.writable {
        return Err(Error::ReadOnly);
    }
    if before.version != expected {
        return Err(Error::Changed);
    }
    let temporary = path.with_file_name(format!(
        ".festerm-save-{}-{}",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed),
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|_| Error::Io)?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| Error::Io)?;
        if read(path)?.version != expected {
            return Err(Error::Changed);
        }
        fs::rename(&temporary, path).map_err(|_| Error::Io)?;
        Ok(Sha256::digest(bytes).into())
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("fesTerm: document temporary cleanup failed");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_import_save_preserves_unicode_and_rejects_stale_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Imported.txt");
        fs::write(&path, "hello\r\n世界").unwrap();
        let before = read(&path).unwrap();
        publish(&path, before.version, "edited\r\n世界".as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited\r\n世界");
        assert_eq!(
            publish(&path, before.version, b"stale"),
            Err(Error::Changed)
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited\r\n世界");
    }

    #[test]
    fn android_import_rejects_outside_authority_and_oversized_sources() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        assert!(DocumentFile::new(outside, root.path(), Arc::new(|| {})).is_err());
        let path = root.path().join("large.txt");
        fs::write(&path, vec![b'x'; MAX_DOCUMENT_BYTES + 1]).unwrap();
        assert!(matches!(read(&path), Err(Error::TooLarge)));
    }

    #[cfg(unix)]
    #[test]
    fn android_import_refuses_symlink_replacement_without_touching_target() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Imported.txt");
        let target = root.path().join("other.txt");
        fs::write(&path, "before").unwrap();
        fs::write(&target, "untouched").unwrap();
        let version = read(&path).unwrap().version;
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(matches!(read(&path), Err(Error::NotAFile)));
        assert_eq!(publish(&path, version, b"overwrite"), Err(Error::NotAFile));
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
    }

    #[test]
    fn android_document_events_are_bounded_and_save_is_observable() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Imported.txt");
        fs::write(&path, "before").unwrap();
        let version = read(&path).unwrap().version;
        let (tx, rx) = std::sync::mpsc::channel();
        let file = DocumentFile::new(
            path.canonicalize().unwrap(),
            root.path(),
            Arc::new(move || {
                tx.send(()).unwrap();
            }),
        )
        .unwrap();
        file.open().unwrap();
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(file.refresh(), Err(Error::Busy));
        assert!(matches!(file.take_events().as_slice(), [Event::Loaded(_)]));
        file.save(7, version, b"after".to_vec()).unwrap();
        // Writer-admission completion also wakes; wait for the event itself.
        for _ in 0..2 {
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            if !file.state.busy.load(Ordering::Acquire) {
                break;
            }
        }
        assert!(matches!(
            file.take_events().as_slice(),
            [Event::Saved { operation: 7, .. }]
        ));
    }
}
