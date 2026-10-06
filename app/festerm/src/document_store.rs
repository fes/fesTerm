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

#[cfg(not(windows))]
use std::fs::Metadata;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
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

/// The stable filesystem identity of the directory that supplied a local file.
///
/// Directory timestamps and sizes change when an image beside a Markdown file
/// changes, so only the OS identity participates in this authority check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryIdentity(FileIdentity);

impl DirectoryIdentity {
    fn from_directory(directory: &cap_std::fs::Dir) -> Result<Self, std::io::Error> {
        use cap_fs_ext::MetadataExt;

        let metadata = directory.dir_metadata()?;
        Ok(Self(FileIdentity {
            volume: metadata.dev(),
            file: metadata.ino(),
        }))
    }

    pub(crate) fn matches_directory(self, directory: &cap_std::fs::Dir) -> bool {
        Self::from_directory(directory).is_ok_and(|current| current == self)
    }
}

/// The path and parent identity that supplied one loaded local document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalSourceAuthority {
    canonical_path: PathBuf,
    parent_identity: DirectoryIdentity,
}

impl LocalSourceAuthority {
    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) const fn parent_identity(&self) -> DirectoryIdentity {
        self.parent_identity
    }
}

impl Generation {
    #[cfg(not(windows))]
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            identity: file_identity(metadata),
        }
    }

    #[cfg(not(windows))]
    fn at(path: &Path) -> Result<Self, std::io::Error> {
        File::open(path).and_then(|file| Self::from_file(&file))
    }

    #[cfg(not(windows))]
    fn from_file(file: &File) -> Result<Self, std::io::Error> {
        file.metadata()
            .map(|metadata| Self::from_metadata(&metadata))
    }

    /// Reads metadata and identity from the same open file.
    #[cfg(windows)]
    fn at(path: &Path) -> Result<Self, std::io::Error> {
        let file = File::open(path)?;
        Self::from_file(&file)
    }

    #[cfg(windows)]
    fn from_file(file: &File) -> Result<Self, std::io::Error> {
        let metadata = file.metadata()?;
        // Creation times can collide or survive replacement through NTFS tunneling.
        let information = winapi_util::file::information(file)?;
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            identity: Some(FileIdentity {
                volume: information.volume_serial_number(),
                file: information.file_index(),
            }),
        })
    }

    /// The size the filesystem last reported, for the status bar's fallback
    /// wording when the source goes away.
    pub const fn size(self) -> u64 {
        self.size
    }

    pub(crate) fn matches_file(self, file: &File) -> bool {
        Self::from_file(file).is_ok_and(|current| current == self)
    }
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &Metadata) -> Option<FileIdentity> {
    None
}

/// A document as it was on disk, with the generation that reading it observed.
#[derive(Clone, Debug)]
pub struct LoadedDocument {
    pub document: TextDocument,
    pub generation: Generation,
    pub(crate) source_authority: LocalSourceAuthority,
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
    NotAFile,
    Interrupted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SavedDocument {
    pub(crate) generation: Generation,
    pub(crate) source_authority: LocalSourceAuthority,
    pub(crate) read_only: bool,
}

impl SaveFailure {
    pub fn headline(&self) -> &'static str {
        match self {
            Self::Conflict(_) => "This file changed somewhere else",
            Self::Gone => "This file is no longer there",
            Self::PermissionDenied => "This file could not be written",
            Self::NoDirectory => "This folder is no longer there",
            Self::NotAFile => "This destination is not a regular file",
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
            Self::NoDirectory => "Nothing was written. Use Save As… to write it somewhere else.",
            Self::NotAFile => "Nothing was written. Choose a file, not a folder or special device.",
            Self::Interrupted => {
                "The previous contents are unchanged. Try saving again, or use Save As…."
            }
        }
    }
}

/// Reads a file into an editable document.
pub fn load(path: &Path, bounds: DocumentBounds) -> Result<LoadedDocument, LoadFailure> {
    let canonical_path = fs::canonicalize(path).map_err(classify_read_error)?;
    let (source_authority, mut file) =
        open_canonical_source(&canonical_path).map_err(classify_read_error)?;
    let metadata = file.metadata().map_err(classify_read_error)?;
    if !metadata.is_file() {
        return Err(LoadFailure::NotAFile);
    }
    let declared = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    bounds
        .check_declared_size(declared)
        .map_err(LoadFailure::Refused)?;

    let before = Generation::from_file(&file).map_err(classify_read_error)?;
    let mut bytes = Vec::with_capacity(declared);
    Read::by_ref(&mut file)
        .take(bounds.max_bytes() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(classify_read_error)?;
    bounds
        .check_declared_size(bytes.len())
        .map_err(LoadFailure::Refused)?;
    let generation = Generation::from_file(&file).map_err(classify_read_error)?;
    if generation != before {
        return Err(LoadFailure::Unreadable);
    }
    let document = TextDocument::from_bytes(&bytes, bounds).map_err(LoadFailure::Refused)?;

    Ok(LoadedDocument {
        document,
        generation,
        source_authority,
        read_only: metadata.permissions().readonly(),
    })
}

pub(crate) fn open_canonical_directory(parent: &Path) -> Result<cap_std::fs::Dir, std::io::Error> {
    use cap_fs_ext::DirExt;
    use std::io;

    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "The local source directory must be a canonical absolute path.",
        )
    };
    if !parent.is_absolute() {
        return Err(invalid());
    }
    let root = parent.ancestors().last().ok_or_else(invalid)?;
    let relative = parent.strip_prefix(root).map_err(|_| invalid())?;
    let mut directory = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(invalid());
        };
        directory = directory.open_dir_nofollow(name)?;
    }
    Ok(directory)
}

pub(crate) fn source_authority_is_current(
    authority: &LocalSourceAuthority,
    generation: Generation,
) -> bool {
    let Some(parent) = authority.canonical_path.parent() else {
        return false;
    };
    let Ok(directory) = open_canonical_directory(parent) else {
        return false;
    };
    authority.parent_identity.matches_directory(&directory)
        && open_canonical_file(&directory, &authority.canonical_path)
            .is_ok_and(|file| generation.matches_file(&file))
}

pub(crate) fn open_canonical_file(
    directory: &cap_std::fs::Dir,
    canonical_path: &Path,
) -> Result<File, std::io::Error> {
    if fs::canonicalize(canonical_path)? != canonical_path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The local source name is no longer canonical.",
        ));
    }
    let file_name = canonical_path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The local source has no file name.",
        )
    })?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOCTTY | nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW);
    }
    let file = directory
        .open_with(Path::new(file_name), &options)?
        .into_std();
    if fs::canonicalize(canonical_path)? != canonical_path {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The local source name changed while it was being opened.",
        ));
    }
    Ok(file)
}

fn open_canonical_source(
    canonical_path: &Path,
) -> Result<(LocalSourceAuthority, File), std::io::Error> {
    let parent = canonical_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The local source has no parent directory.",
        )
    })?;
    let directory = open_canonical_directory(parent)?;
    let parent_identity = DirectoryIdentity::from_directory(&directory)?;
    let file = open_canonical_file(&directory, canonical_path)?;
    Ok((
        LocalSourceAuthority {
            canonical_path: canonical_path.to_path_buf(),
            parent_identity,
        },
        file,
    ))
}

/// Asks whether a path still holds the generation we loaded.
pub fn freshness(path: &Path, known: Generation) -> Freshness {
    match fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => Freshness::Gone(LoadFailure::NotAFile),
        Ok(_metadata) => {
            #[cfg(not(windows))]
            let current = Generation::from_metadata(&_metadata);
            #[cfg(windows)]
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
) -> Result<SavedDocument, SaveFailure> {
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
    let source_authority = source_authority_for_save(path, parent)?;
    let original = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => return Err(SaveFailure::NotAFile),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(classify_write_error(error)),
    };

    let mut temporary = TemporaryFile::create(parent)?;
    write_all_durably(temporary.file_mut(), bytes)?;
    if let Some(original) = &original {
        // Best effort: a filesystem that will not carry permissions over is
        // not a reason to refuse an otherwise complete save.
        let _ = fs::set_permissions(temporary.path(), original.permissions());
    }
    let generation =
        Generation::from_file(temporary.file_mut()).map_err(|_| SaveFailure::Interrupted)?;
    let read_only = temporary
        .file_mut()
        .metadata()
        .map_err(|_| SaveFailure::Interrupted)?
        .permissions()
        .readonly();
    temporary.close_file();

    replace_file(temporary.path(), path)?;
    temporary.persist();
    sync_directory(parent);

    Ok(SavedDocument {
        generation,
        source_authority,
        read_only,
    })
}

fn source_authority_for_save(
    path: &Path,
    parent: &Path,
) -> Result<LocalSourceAuthority, SaveFailure> {
    let file_name = path.file_name().ok_or(SaveFailure::NoDirectory)?;
    let canonical_parent = fs::canonicalize(parent).map_err(classify_write_error)?;
    let directory = open_canonical_directory(&canonical_parent).map_err(classify_write_error)?;
    let parent_identity =
        DirectoryIdentity::from_directory(&directory).map_err(classify_write_error)?;
    Ok(LocalSourceAuthority {
        canonical_path: canonical_parent.join(file_name),
        parent_identity,
    })
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
    if !fs::metadata(target)
        .map_err(classify_write_error)?
        .is_file()
    {
        return Err(SaveFailure::NotAFile);
    }
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

        let saved = save(&path, b"after\n", Some(loaded.generation)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(freshness(&path, saved.generation), Freshness::Unchanged);
        assert_ne!(saved.generation, loaded.generation);
        assert_eq!(
            saved.source_authority.canonical_path(),
            fs::canonicalize(&path).unwrap()
        );
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

        let saved = save(&path, b"new\n", None).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(saved.generation.size(), 4);
    }

    #[test]
    fn saved_authority_remains_bound_to_the_destination_parent() {
        let directory = TemporaryDirectory::new("saved-parent");
        let retained = TemporaryDirectory::new("saved-parent-retained");
        fs::remove_dir(&retained.path).unwrap();
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let saved = save(&path, b"after\n", Some(loaded.generation)).unwrap();

        fs::rename(&directory.path, &retained.path).unwrap();
        fs::create_dir(&directory.path).unwrap();
        fs::hard_link(retained.path.join("notes.md"), &path).unwrap();
        let replacement =
            open_canonical_directory(&fs::canonicalize(&directory.path).unwrap()).unwrap();

        assert!(
            !saved
                .source_authority
                .parent_identity()
                .matches_directory(&replacement),
            "a later hard-link replacement must not change the parent captured by Save"
        );
    }

    #[test]
    fn save_as_never_moves_or_replaces_a_directory() {
        let directory = TemporaryDirectory::new("directory-saveas");
        let folder = directory.path.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("sentinel.txt"), b"unchanged").unwrap();

        assert_eq!(save(&folder, b"snapshot", None), Err(SaveFailure::NotAFile));
        assert!(folder.is_dir());
        assert_eq!(fs::read(folder.join("sentinel.txt")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
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
