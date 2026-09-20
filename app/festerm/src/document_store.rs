//! Reading and writing local documents on behalf of the editor (ADR 0034 §5).
//!
//! Every load and every successful save records a [`Generation`]: the strongest
//! combination of identity, modification time, and size the filesystem will
//! tell us about. A save revalidates that generation before it writes, so the
//! only way to overwrite somebody else's change is to be shown it first and
//! decide to.
//!
//! Replacement is write-to-temporary-then-rename inside the target's own
//! directory, with the original's permissions carried over and the bytes
//! durably flushed before the rename. A write that is interrupted anywhere
//! along that path leaves the previous file exactly as it was, and never
//! reports success.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use festerm_document::{DocumentBounds, RefusalReason, TextDocument};

/// How many distinct temporary names to try before giving up.
const TEMPORARY_FILE_ATTEMPTS: u32 = 16;

static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// What we knew about the file the last time we read or wrote it.
///
/// Two generations comparing equal is the claim "this is still the same file
/// with the same contents as far as the filesystem can tell us". The identity
/// component is what distinguishes a file that was edited in place from one
/// that was replaced by an atomic save somewhere else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generation {
    size: u64,
    modified: Option<SystemTime>,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    /// The device on Unix, or the volume serial number on Windows.
    volume: u64,
    /// The inode on Unix, or the file index on Windows.
    file: u64,
}

impl Generation {
    /// Reads metadata and identity from the same open file.
    fn at(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            identity: file_identity(&file, &metadata)?,
        })
    }

    /// The size the filesystem last reported, for the status bar's fallback
    /// wording when the source goes away.
    pub const fn size(self) -> u64 {
        self.size
    }
}

#[cfg(unix)]
fn file_identity(_file: &File, metadata: &Metadata) -> std::io::Result<Option<FileIdentity>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    }))
}

#[cfg(windows)]
fn file_identity(file: &File, _metadata: &Metadata) -> std::io::Result<Option<FileIdentity>> {
    // Creation times can collide or survive replacement through NTFS tunneling.
    let information = winapi_util::file::information(file)?;
    Ok(Some(FileIdentity {
        volume: information.volume_serial_number(),
        file: information.file_index(),
    }))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File, _metadata: &Metadata) -> std::io::Result<Option<FileIdentity>> {
    Ok(None)
}

/// A document as it was on disk, with the generation that reading it observed.
#[derive(Clone, Debug)]
pub struct LoadedDocument {
    pub document: TextDocument,
    pub generation: Generation,
    /// True when the file's permissions say we would not be able to save over
    /// it, which the editor shows before the user has typed anything.
    pub read_only: bool,
}

/// Why a file could not be opened for editing.
///
/// These are deliberately coarse: the editor shows the user a sentence, not an
/// operating-system error string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadFailure {
    NotFound,
    NotAFile,
    PermissionDenied,
    Refused(RefusalReason),
    Unreadable,
}

impl LoadFailure {
    /// The sentence shown in place of the document.
    pub fn headline(&self) -> String {
        match self {
            Self::NotFound => "This file no longer exists".to_owned(),
            Self::NotAFile => "This is not a file".to_owned(),
            Self::PermissionDenied => "This file cannot be read".to_owned(),
            Self::Refused(reason) => reason.headline().to_owned(),
            Self::Unreadable => "This file could not be read".to_owned(),
        }
    }

    /// What can be done about it, without quoting the file's contents.
    pub fn detail(&self) -> String {
        match self {
            Self::NotFound => {
                "It may have been moved, renamed, or deleted since it was listed.".to_owned()
            }
            Self::NotAFile => {
                "Only regular files can be opened in the editor. Directories and devices cannot."
                    .to_owned()
            }
            Self::PermissionDenied => {
                "Your account does not have permission to read it.".to_owned()
            }
            Self::Refused(reason) => reason.detail(),
            Self::Unreadable => "Reading it failed. It may be on a disconnected volume.".to_owned(),
        }
    }
}

/// What a freshness check found at the document's path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// The file is still the one we loaded, unchanged.
    Unchanged,
    /// The file is still there but is not the one we loaded any more.
    Changed(Generation),
    /// The path no longer resolves to a readable regular file.
    Gone(LoadFailure),
}

/// Why a save did not happen, or did not complete.
///
/// `Conflict` is the case the ADR cares most about: the file changed under us,
/// so nothing was written and the newer generation is handed back so the
/// document can enter Conflict and offer Compare.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveFailure {
    Conflict(Generation),
    Gone,
    PermissionDenied,
    NoDirectory,
    Interrupted,
}

impl SaveFailure {
    pub fn headline(&self) -> &'static str {
        match self {
            Self::Conflict(_) => "This file changed somewhere else",
            Self::Gone => "This file is no longer there",
            Self::PermissionDenied => "This file could not be written",
            Self::NoDirectory => "This folder is no longer there",
            Self::Interrupted => "Saving did not complete",
        }
    }

    pub fn detail(&self) -> &'static str {
        match self {
            Self::Conflict(_) => {
                "Nothing was written. Compare the two versions before deciding what to keep."
            }
            Self::Gone => "Nothing was written. Use Save As… to write it somewhere else.",
            Self::PermissionDenied => {
                "Your account does not have permission to replace it. Use Save As… to write it somewhere else."
            }
            Self::NoDirectory => {
                "Nothing was written. Use Save As… to write it somewhere else."
            }
            Self::Interrupted => {
                "The previous contents are unchanged. Try saving again, or use Save As…."
            }
        }
    }
}

/// Reads a file into an editable document.
pub fn load(path: &Path, bounds: DocumentBounds) -> Result<LoadedDocument, LoadFailure> {
    let metadata = fs::metadata(path).map_err(classify_read_error)?;
    if !metadata.is_file() {
        return Err(LoadFailure::NotAFile);
    }
    let declared = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    bounds
        .check_declared_size(declared)
        .map_err(LoadFailure::Refused)?;

    let bytes = fs::read(path).map_err(classify_read_error)?;
    // Re-stat after the read so the generation describes the bytes we hold
    // rather than the file as it was before someone else touched it.
    let generation = Generation::at(path).map_err(classify_read_error)?;
    let document = TextDocument::from_bytes(&bytes, bounds).map_err(LoadFailure::Refused)?;

    Ok(LoadedDocument {
        document,
        generation,
        read_only: metadata.permissions().readonly(),
    })
}

/// Asks whether a path still holds the generation we loaded.
pub fn freshness(path: &Path, known: Generation) -> Freshness {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => Freshness::Gone(LoadFailure::NotAFile),
        Ok(_) => {
            let current = match Generation::at(path) {
                Ok(current) => current,
                Err(error) => return Freshness::Gone(classify_read_error(error)),
            };
            if current == known {
                Freshness::Unchanged
            } else {
                Freshness::Changed(current)
            }
        }
        Err(error) => Freshness::Gone(classify_read_error(error)),
    }
}

/// Replaces a file's contents atomically, refusing if it changed since
/// `expected`.
///
/// `expected` is `None` for a path we have not loaded — a Save As to a new
/// name — in which case any existing file is replaced, because the user has
/// already been warned and pressed Save.
pub fn save(
    path: &Path,
    bytes: &[u8],
    expected: Option<Generation>,
) -> Result<Generation, SaveFailure> {
    if let Some(expected) = expected {
        match freshness(path, expected) {
            Freshness::Unchanged => {}
            Freshness::Changed(current) => return Err(SaveFailure::Conflict(current)),
            Freshness::Gone(LoadFailure::PermissionDenied) => {
                return Err(SaveFailure::PermissionDenied);
            }
            Freshness::Gone(_) => return Err(SaveFailure::Gone),
        }
    }

    let parent = parent_directory(path)?;
    let original = fs::metadata(path).ok();

    let mut temporary = TemporaryFile::create(parent)?;
    write_all_durably(temporary.file_mut(), bytes)?;
    if let Some(original) = &original {
        // Best effort: a filesystem that will not carry permissions over is
        // not a reason to refuse an otherwise complete save.
        let _ = fs::set_permissions(temporary.path(), original.permissions());
    }
    temporary.close_file();

    replace_file(temporary.path(), path)?;
    temporary.persist();
    sync_directory(parent);

    Generation::at(path).map_err(|_| SaveFailure::Interrupted)
}

fn write_all_durably(file: &mut File, bytes: &[u8]) -> Result<(), SaveFailure> {
    file.write_all(bytes).map_err(classify_write_error)?;
    file.flush().map_err(classify_write_error)?;
    file.sync_all().map_err(classify_write_error)
}

fn classify_read_error(error: std::io::Error) -> LoadFailure {
    match error.kind() {
        std::io::ErrorKind::NotFound => LoadFailure::NotFound,
        std::io::ErrorKind::PermissionDenied => LoadFailure::PermissionDenied,
        _ => LoadFailure::Unreadable,
    }
}

fn classify_write_error(error: std::io::Error) -> SaveFailure {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => SaveFailure::PermissionDenied,
        _ => SaveFailure::Interrupted,
    }
}

fn parent_directory(path: &Path) -> Result<&Path, SaveFailure> {
    if path.file_name().is_none() {
        return Err(SaveFailure::NoDirectory);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent.is_dir() {
        Ok(parent)
    } else {
        Err(SaveFailure::NoDirectory)
    }
}

/// A file that deletes itself unless it is explicitly kept, so a save that
/// fails half way through leaves no debris beside the user's file.
struct TemporaryFile {
    path: PathBuf,
    file: Option<File>,
    persist: bool,
}

impl TemporaryFile {
    fn create(parent: &Path) -> Result<Self, SaveFailure> {
        for _ in 0..TEMPORARY_FILE_ATTEMPTS {
            let path = temporary_path(parent);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        persist: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(classify_write_error(error)),
            }
        }
        Err(SaveFailure::Interrupted)
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("the temporary file is open until it is closed for replacement")
    }

    fn close_file(&mut self) {
        self.file.take();
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn persist(&mut self) {
        self.persist = true;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.persist {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn temporary_path(parent: &Path) -> PathBuf {
    let identifier = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".festerm-save-{}-{identifier}.tmp", process::id()))
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), SaveFailure> {
    fs::rename(temporary, target).map_err(classify_write_error)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path) -> Result<(), SaveFailure> {
    match fs::rename(temporary, target) {
        Ok(()) => Ok(()),
        Err(error) if target.exists() => {
            let permission = error.kind() == std::io::ErrorKind::PermissionDenied;
            replace_existing_windows_file(temporary, target, permission)
        }
        Err(error) => Err(classify_write_error(error)),
    }
}

/// Windows will not rename over an existing file, so the target is moved aside
/// first and moved back if the replacement fails. The user's file is never the
/// thing that goes missing.
#[cfg(windows)]
fn replace_existing_windows_file(
    temporary: &Path,
    target: &Path,
    permission: bool,
) -> Result<(), SaveFailure> {
    let parent = parent_directory(target)?;
    let previous = temporary_path(parent).with_extension("previous");
    if fs::rename(target, &previous).is_err() {
        return Err(if permission {
            SaveFailure::PermissionDenied
        } else {
            SaveFailure::Interrupted
        });
    }
    if let Err(error) = fs::rename(temporary, target) {
        let _ = fs::rename(&previous, target);
        return Err(classify_write_error(error));
    }
    let _ = fs::remove_file(previous);
    Ok(())
}

#[cfg(unix)]
fn sync_directory(parent: &Path) {
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_parent: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            let identifier = NEXT_TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "festerm-document-store-{}-{label}-{identifier}",
                process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.path.join(name);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn bounds() -> DocumentBounds {
        DocumentBounds::DEFAULT
    }

    #[test]
    fn loading_reads_the_text_and_remembers_the_generation() {
        let directory = TemporaryDirectory::new("load");
        let path = directory.file("notes.md", "alpha\nbeta\n");

        let loaded = load(&path, bounds()).unwrap();

        assert_eq!(loaded.document.text(), "alpha\nbeta\n");
        assert_eq!(loaded.generation.size(), 11);
        assert_eq!(freshness(&path, loaded.generation), Freshness::Unchanged);
    }

    #[test]
    fn a_missing_file_and_a_directory_are_told_apart() {
        let directory = TemporaryDirectory::new("missing");

        assert_eq!(
            load(&directory.path.join("absent.md"), bounds()).unwrap_err(),
            LoadFailure::NotFound
        );
        assert_eq!(
            load(&directory.path, bounds()).unwrap_err(),
            LoadFailure::NotAFile
        );
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let directory = TemporaryDirectory::new("oversize");
        let path = directory.file("big.txt", &"x".repeat(200));

        let failure = load(&path, DocumentBounds::new(64, 16, 64)).unwrap_err();

        assert!(matches!(
            failure,
            LoadFailure::Refused(RefusalReason::TooLarge { .. })
        ));
        assert!(failure.detail().contains("Markdown viewer"));
    }

    #[test]
    fn a_binary_file_is_refused_with_a_reason_worth_showing() {
        let directory = TemporaryDirectory::new("binary");
        let path = directory.path.join("image.bin");
        fs::write(&path, [b'a', 0, b'b']).unwrap();

        let failure = load(&path, bounds()).unwrap_err();

        assert_eq!(failure, LoadFailure::Refused(RefusalReason::BinaryContent));
        assert_eq!(failure.headline(), "This file appears to be binary");
    }

    #[test]
    fn saving_replaces_the_contents_and_returns_the_new_generation() {
        let directory = TemporaryDirectory::new("save");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();

        let generation = save(&path, b"after\n", Some(loaded.generation)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(freshness(&path, generation), Freshness::Unchanged);
        assert_ne!(generation, loaded.generation);
    }

    #[test]
    fn saving_leaves_no_temporary_files_behind() {
        let directory = TemporaryDirectory::new("debris");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();

        save(&path, b"after\n", Some(loaded.generation)).unwrap();

        let entries: Vec<_> = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("notes.md")]);
    }

    #[test]
    fn a_file_changed_underneath_us_is_a_conflict_and_is_not_overwritten() {
        let directory = TemporaryDirectory::new("conflict");
        let path = directory.file("notes.md", "mine\n");
        let loaded = load(&path, bounds()).unwrap();

        fs::write(&path, "theirs, which is longer\n").unwrap();
        let failure = save(&path, b"mine, edited\n", Some(loaded.generation)).unwrap_err();

        assert!(matches!(failure, SaveFailure::Conflict(_)));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "theirs, which is longer\n"
        );
    }

    #[test]
    fn a_file_replaced_by_an_atomic_save_elsewhere_does_not_look_unchanged() {
        let directory = TemporaryDirectory::new("replaced");
        let path = directory.file("notes.md", "aaaaa\n");
        set_fixed_timestamps(&path);
        let loaded = load(&path, bounds()).unwrap();

        // What another editor's own atomic save looks like from here: the same
        // name, written somewhere else and renamed into place.
        let elsewhere = directory.file("notes.md.new", "bbbbb\n");
        set_fixed_timestamps(&elsewhere);
        fs::rename(&elsewhere, &path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().modified().ok(),
            loaded.generation.modified
        );

        match freshness(&path, loaded.generation) {
            Freshness::Changed(_) => {}
            other => panic!("a replaced file must not look unchanged: {other:?}"),
        }
    }

    fn set_fixed_timestamps(path: &Path) {
        let timestamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let times = fs::FileTimes::new().set_modified(timestamp);
        #[cfg(windows)]
        let times = {
            use std::os::windows::fs::FileTimesExt;
            times.set_created(timestamp)
        };
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
    }

    #[test]
    fn saving_refuses_an_atomic_replacement_with_matching_timestamps_and_size() {
        let directory = TemporaryDirectory::new("replaced-save");
        let path = directory.file("notes.md", "aaaaa\n");
        set_fixed_timestamps(&path);
        let loaded = load(&path, bounds()).unwrap();
        let elsewhere = directory.file("notes.md.new", "bbbbb\n");
        set_fixed_timestamps(&elsewhere);
        fs::rename(&elsewhere, &path).unwrap();

        assert!(matches!(
            save(&path, b"my edits\n", Some(loaded.generation)),
            Err(SaveFailure::Conflict(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "bbbbb\n");
    }

    #[test]
    fn identity_distinguishes_files_that_are_otherwise_indistinguishable() {
        let original = Generation {
            size: 6,
            modified: Some(SystemTime::UNIX_EPOCH),
            identity: Some(FileIdentity {
                volume: 1,
                file: 10,
            }),
        };
        let replacement = Generation {
            identity: Some(FileIdentity {
                volume: 1,
                file: 11,
            }),
            ..original
        };

        assert_ne!(original, replacement);
        assert_eq!(original, Generation { ..original });
    }

    #[test]
    fn saving_over_a_deleted_file_reports_it_rather_than_recreating_it() {
        let directory = TemporaryDirectory::new("deleted");
        let path = directory.file("notes.md", "mine\n");
        let loaded = load(&path, bounds()).unwrap();
        fs::remove_file(&path).unwrap();

        assert_eq!(
            save(&path, b"mine\n", Some(loaded.generation)).unwrap_err(),
            SaveFailure::Gone
        );
        assert!(!path.exists());
    }

    #[test]
    fn saving_without_a_known_generation_writes_a_new_file() {
        let directory = TemporaryDirectory::new("saveas");
        let path = directory.path.join("fresh.md");

        let generation = save(&path, b"new\n", None).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(generation.size(), 4);
    }

    #[test]
    fn saving_into_a_missing_folder_is_refused() {
        let directory = TemporaryDirectory::new("nofolder");
        let path = directory.path.join("absent").join("fresh.md");

        assert_eq!(
            save(&path, b"new\n", None).unwrap_err(),
            SaveFailure::NoDirectory
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_original_permissions_survive_a_save() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("permissions");
        let path = directory.file("script.sh", "echo before\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o750)).unwrap();
        let loaded = load(&path, bounds()).unwrap();

        save(&path, b"echo after\n", Some(loaded.generation)).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_file_is_reported_as_permission_denied() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("readonly");
        let path = directory.file("locked.md", "mine\n");
        let loaded = load(&path, bounds()).unwrap();
        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o500)).unwrap();

        if fs::write(directory.path.join("probe"), b"probe").is_ok() {
            // Running as a user the directory mode does not constrain, so the
            // case under test does not exist here.
            fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let failure = save(&path, b"edited\n", Some(loaded.generation)).unwrap_err();

        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(failure, SaveFailure::PermissionDenied);
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine\n");
    }

    #[test]
    // Clearing the flag again is the point: the temporary directory cannot be
    // removed on Windows while it holds a read-only file.
    #[allow(clippy::permissions_set_readonly_false)]
    fn a_read_only_file_is_reported_at_load_time() {
        let directory = TemporaryDirectory::new("readonlyflag");
        let path = directory.file("notes.md", "mine\n");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);

        fs::set_permissions(&path, permissions).unwrap();
        let loaded = load(&path, bounds()).unwrap();
        let mut restored = fs::metadata(&path).unwrap().permissions();
        restored.set_readonly(false);
        fs::set_permissions(&path, restored).unwrap();

        assert!(loaded.read_only);
    }

    #[test]
    fn a_file_deleted_after_loading_is_reported_as_gone() {
        let directory = TemporaryDirectory::new("gone");
        let path = directory.file("notes.md", "mine\n");
        let loaded = load(&path, bounds()).unwrap();

        fs::remove_file(&path).unwrap();

        assert_eq!(
            freshness(&path, loaded.generation),
            Freshness::Gone(LoadFailure::NotFound)
        );
    }
}
