//! Reading and writing local documents on behalf of the editor (ADR 0034 §5).
//!
//! Every load and every successful save records a [`Generation`]: the strongest
//! combination of identity, modification time, and size the filesystem will
//! tell us about. A save revalidates that generation before it writes, so the
//! only way to overwrite somebody else's change is to be shown it first and
//! decide to.
//!
//! Replacement uses private same-directory staging and two no-overwrite moves,
//! with existing access metadata preserved and bytes durably flushed first.
//! The target name can be briefly absent after its prior entry is captured;
//! ambiguous in-process failures retain both versions and never report success.

#[cfg(not(windows))]
use std::fs::Metadata;
use std::fs::{self, File};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
#[cfg(test)]
use std::{
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use festerm_document::{DocumentBounds, RefusalReason, TextDocument};

/// How many distinct temporary names to try before giving up.
const TEMPORARY_FILE_ATTEMPTS: u32 = 16;

#[cfg(test)]
static NEXT_TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
thread_local! {
    static AFTER_SAVE_DIRECTORY_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SAVE_WRITE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_STAGING_PAYLOAD_CREATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_STAGING_DIRECTORY_CREATE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SAVE_VERIFICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static BEFORE_SAVE_REPLACEMENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_TARGET_CAPTURE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SAVE_REPLACEMENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
    static AFTER_SECURITY_METADATA_COPY: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

fn after_save_directory_capture() {
    #[cfg(test)]
    AFTER_SAVE_DIRECTORY_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn before_save_write() {
    #[cfg(test)]
    BEFORE_SAVE_WRITE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn before_staging_payload_create() {
    #[cfg(test)]
    BEFORE_STAGING_PAYLOAD_CREATE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn after_staging_directory_create() {
    #[cfg(test)]
    AFTER_STAGING_DIRECTORY_CREATE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn before_save_verification() {
    #[cfg(test)]
    BEFORE_SAVE_VERIFICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn before_save_replacement() {
    #[cfg(test)]
    BEFORE_SAVE_REPLACEMENT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn after_save_replacement() {
    #[cfg(test)]
    AFTER_SAVE_REPLACEMENT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn after_target_capture() {
    #[cfg(test)]
    AFTER_TARGET_CAPTURE.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

fn after_security_metadata_copy() {
    #[cfg(test)]
    AFTER_SECURITY_METADATA_COPY.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

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

/// What the Save As picker observed at the exact destination when the user
/// confirmed it. Absence and an existing exact generation are deliberately
/// distinct so later appearance or replacement cannot be interpreted as
/// permission to overwrite a different object.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DestinationExpectation {
    Absent,
    Existing(Generation),
}

/// Picker-confirmed destination authority. The parent identity and canonical
/// name prevent the save from switching to a replacement directory between
/// confirmation, open-document resolution, and publication.
#[derive(Clone, Debug)]
pub(crate) struct ConfirmedDestination {
    authority: LocalSourceAuthority,
    expectation: DestinationExpectation,
}

impl ConfirmedDestination {
    pub(crate) const fn expectation(&self) -> DestinationExpectation {
        self.expectation
    }

    pub(crate) fn matches_source_authority(&self, authority: &LocalSourceAuthority) -> bool {
        self.authority == *authority
    }

    pub(crate) fn matches_requested_path(&self, path: &Path) -> bool {
        let Some(file_name) = path.file_name() else {
            return false;
        };
        let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        else {
            return false;
        };
        fs::canonicalize(parent)
            .map(|parent| parent.join(file_name) == self.authority.canonical_path)
            .unwrap_or(false)
    }
}

/// The authority a save is allowed to mutate.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SaveExpectation<'a> {
    Loaded {
        generation: Generation,
        authority: &'a LocalSourceAuthority,
    },
    Destination(&'a ConfirmedDestination),
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
        #[cfg(not(windows))]
        {
            use cap_fs_ext::MetadataExt;

            let metadata = directory.dir_metadata()?;
            Ok(Self(FileIdentity {
                volume: metadata.dev(),
                file: metadata.ino(),
            }))
        }
        #[cfg(windows)]
        {
            let directory = directory.try_clone()?.into_std_file();
            let information = winapi_util::file::information(&directory)?;
            Ok(Self(FileIdentity {
                volume: information.volume_serial_number(),
                file: information.file_index(),
            }))
        }
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
    RecoveryRequired,
    MetadataPreservation,
    EncryptedFile,
    NamedStreams,
    UnsupportedFilesystem,
    CrossVolume,
    UnsafeDestinationFolder,
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
            Self::RecoveryRequired => "Saving needs manual recovery",
            Self::MetadataPreservation => "This file's access cannot be preserved",
            Self::EncryptedFile => "This file's encryption cannot be preserved",
            Self::NamedStreams => "This file's Windows data streams cannot be preserved",
            Self::UnsupportedFilesystem => "This disk cannot support safe saving",
            Self::CrossVolume => "This destination crossed a filesystem boundary",
            Self::UnsafeDestinationFolder => "This folder cannot protect save staging",
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
            Self::NotAFile => {
                "Nothing was written. For a symbolic link or reparse point, choose the regular file it points to; otherwise choose a file, not a folder or special device."
            }
            Self::Interrupted => {
                "The previous contents are unchanged. Try saving again, or use Save As…."
            }
            Self::RecoveryRequired => {
                "Publication could not be verified. The destination may contain the new bytes or be absent. In the private .festerm-save-* folder beside it, original is the private prior version, displaced (if present) is Windows' captured prior file, and prepared or payload contains the new bytes. Compare every retained version before recovering or saving there again."
            }
            Self::MetadataPreservation => {
                "The file's owner, group, ACL, security labels, attributes, or extended metadata cannot be preserved safely. Use Save As to choose a destination with compatible access metadata."
            }
            Self::EncryptedFile => {
                "fesTerm cannot safely preserve Windows EFS encryption during replacement. Use Save As to choose a new destination, or edit it with an EFS-aware tool."
            }
            Self::NamedStreams => {
                "The file has NTFS alternate data streams such as Zone.Identifier that fesTerm cannot safely preserve yet. Use Save As to choose a new destination."
            }
            Self::UnsupportedFilesystem => {
                "This disk cannot provide private staging and no-overwrite publication. Use Save As on a different local disk."
            }
            Self::CrossVolume => {
                "Nothing was written. A mount point, junction, or reparse point redirected the destination to another disk. Choose a regular destination on the intended disk."
            }
            Self::UnsafeDestinationFolder => {
                "Nothing was written. Another account may be able to replace temporary save entries in this folder. Use Save As to choose a folder protected from other writers."
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
    #[cfg(not(unix))]
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
        #[cfg(unix)]
        {
            let parent = directory.try_clone()?.into_std_file();
            let child =
                festerm_unix_security::open_directory_search_nofollow(&parent, Path::new(name))?;
            directory = cap_std::fs::Dir::from_std_file(child);
        }
        #[cfg(not(unix))]
        {
            directory = directory.open_dir_nofollow(name)?;
        }
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

/// Observes the exact Save As destination at confirmation time.
pub(crate) fn observe_destination(path: &Path) -> Result<ConfirmedDestination, SaveFailure> {
    let parent = parent_directory(path)?;
    let save_directory = source_authority_for_save(path, parent)?;
    let expectation = match open_original_file(&save_directory.directory, &save_directory.target)? {
        Some(original) => DestinationExpectation::Existing(original.generation),
        None => DestinationExpectation::Absent,
    };
    Ok(ConfirmedDestination {
        authority: save_directory.source_authority,
        expectation,
    })
}

/// Conditionally publishes bytes under the authority represented by
/// `expectation`. Ordinary Save uses the loaded generation and parent
/// capability; Save As uses the absent-or-exact destination observation made
/// when the picker was confirmed.
pub fn save(
    path: &Path,
    bytes: &[u8],
    expectation: SaveExpectation<'_>,
) -> Result<SavedDocument, SaveFailure> {
    let save_directory = match expectation {
        SaveExpectation::Loaded { authority, .. } => {
            save_directory_from_loaded_authority(authority)?
        }
        SaveExpectation::Destination(destination) if destination.matches_requested_path(path) => {
            save_directory_from_loaded_authority(&destination.authority)?
        }
        SaveExpectation::Destination(_) => return Err(SaveFailure::Interrupted),
    };
    after_save_directory_capture();
    let expected_generation = match expectation {
        SaveExpectation::Loaded { generation, .. } => Some(generation),
        SaveExpectation::Destination(destination) => match destination.expectation {
            DestinationExpectation::Existing(generation) => Some(generation),
            DestinationExpectation::Absent => None,
        },
    };
    if let Some(expected) = expected_generation {
        match freshness_in_directory(&save_directory.directory, &save_directory.target, expected) {
            Freshness::Unchanged => {}
            Freshness::Changed(current) => return Err(SaveFailure::Conflict(current)),
            Freshness::Gone(LoadFailure::PermissionDenied) => {
                return Err(SaveFailure::PermissionDenied);
            }
            Freshness::Gone(_) => return Err(SaveFailure::Gone),
        }
    }

    let original = open_original_file(&save_directory.directory, &save_directory.target)?;
    #[cfg(windows)]
    let mut original = original;
    match (expected_generation, original.as_ref()) {
        (Some(expected), Some(original)) if original.generation == expected => {}
        (Some(_), None) => return Err(SaveFailure::Gone),
        (Some(_), Some(original)) | (None, Some(original)) => {
            return Err(SaveFailure::Conflict(original.generation));
        }
        (None, None) => {}
    }
    #[cfg(windows)]
    let security_metadata = original
        .as_ref()
        .map(|original| {
            festerm_windows_security::security_metadata(&original.file)
                .map_err(classify_write_error)
        })
        .transpose()?;
    #[cfg(windows)]
    if security_metadata
        .as_ref()
        .is_some_and(festerm_windows_security::SecurityMetadata::is_encrypted)
    {
        return Err(SaveFailure::EncryptedFile);
    }
    #[cfg(windows)]
    if security_metadata
        .as_ref()
        .is_some_and(festerm_windows_security::SecurityMetadata::has_named_streams)
    {
        return Err(SaveFailure::NamedStreams);
    }

    let mut temporary = TemporaryFile::create(&save_directory.directory)?;
    before_save_write();
    write_all_durably(temporary.file_mut(), bytes)?;
    #[cfg(unix)]
    let security_metadata = original
        .as_ref()
        .map(|original| preserve_security_metadata(&original.file, temporary.file_mut()))
        .transpose()?;
    #[cfg(unix)]
    after_security_metadata_copy();
    #[cfg(not(windows))]
    let read_only = temporary
        .file_mut()
        .metadata()
        .map_err(|_| SaveFailure::Interrupted)?
        .permissions()
        .readonly();
    #[cfg(windows)]
    let read_only = original.as_ref().is_some_and(|original| original.read_only);
    let temporary_generation =
        Generation::from_file(temporary.file_mut()).map_err(|_| SaveFailure::Interrupted)?;
    before_save_verification();
    temporary.verify_name()?;
    #[cfg(unix)]
    let publication = publish_temporary(
        &save_directory.directory,
        &mut temporary,
        &save_directory.target,
        original.as_ref().map(|original| original.generation),
        temporary_generation,
        original.as_ref().map(|original| &original.file),
        security_metadata.as_ref(),
    );
    #[cfg(windows)]
    let publication = {
        let original_generation = original.as_ref().map(|original| original.generation);
        let original_file = original.take().map(|original| original.file);
        publish_temporary(
            &save_directory.directory,
            &mut temporary,
            &save_directory.target,
            original_generation,
            temporary_generation,
            original_file,
            security_metadata.as_ref(),
        )
    };
    sync_directory(&save_directory.directory);
    let generation = publication?;

    Ok(SavedDocument {
        generation,
        source_authority: save_directory.source_authority,
        read_only,
    })
}

struct OriginalFile {
    file: File,
    generation: Generation,
    read_only: bool,
}

fn open_original_file(
    directory: &cap_std::fs::Dir,
    target: &Path,
) -> Result<Option<OriginalFile>, SaveFailure> {
    let file = match open_named_file(directory, target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(SaveFailure::NotAFile);
        }
        Err(error) => return Err(classify_write_error(error)),
    };
    let metadata = file.metadata().map_err(classify_write_error)?;
    if !metadata.is_file() {
        return Err(SaveFailure::NotAFile);
    }
    Ok(Some(OriginalFile {
        generation: Generation::from_file(&file).map_err(|_| SaveFailure::Interrupted)?,
        read_only: metadata.permissions().readonly(),
        file,
    }))
}

fn generation_in_directory(
    directory: &cap_std::fs::Dir,
    target: &Path,
) -> Result<Generation, std::io::Error> {
    let file = open_named_file(directory, target)?;
    Generation::from_file(&file)
}

fn open_named_file(directory: &cap_std::fs::Dir, target: &Path) -> Result<File, std::io::Error> {
    #[cfg(windows)]
    {
        let directory = directory.try_clone()?.into_std_file();
        return festerm_windows_security::open_file_no_reparse(&directory, target);
    }
    #[cfg(unix)]
    {
        let directory = directory.try_clone()?.into_std_file();
        festerm_unix_security::open_file_nofollow(&directory, target)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true);
        directory
            .open_with(target, &options)
            .map(cap_std::fs::File::into_std)
    }
}

struct SaveDirectory {
    directory: cap_std::fs::Dir,
    target: PathBuf,
    source_authority: LocalSourceAuthority,
}

fn save_directory_from_loaded_authority(
    authority: &LocalSourceAuthority,
) -> Result<SaveDirectory, SaveFailure> {
    let parent = authority
        .canonical_path
        .parent()
        .ok_or(SaveFailure::NoDirectory)?;
    let target = authority
        .canonical_path
        .file_name()
        .map(PathBuf::from)
        .ok_or(SaveFailure::NoDirectory)?;
    let directory = open_canonical_directory(parent).map_err(classify_write_error)?;
    if !authority.parent_identity.matches_directory(&directory) {
        return Err(SaveFailure::Gone);
    }
    Ok(SaveDirectory {
        directory,
        target,
        source_authority: authority.clone(),
    })
}

fn source_authority_for_save(path: &Path, parent: &Path) -> Result<SaveDirectory, SaveFailure> {
    let file_name = path.file_name().ok_or(SaveFailure::NoDirectory)?;
    let canonical_parent = fs::canonicalize(parent).map_err(classify_write_error)?;
    let directory = open_canonical_directory(&canonical_parent).map_err(classify_write_error)?;
    let parent_identity =
        DirectoryIdentity::from_directory(&directory).map_err(classify_write_error)?;
    let target = PathBuf::from(file_name);
    Ok(SaveDirectory {
        directory,
        source_authority: LocalSourceAuthority {
            canonical_path: canonical_parent.join(file_name),
            parent_identity,
        },
        target,
    })
}

fn freshness_in_directory(
    directory: &cap_std::fs::Dir,
    target: &Path,
    known: Generation,
) -> Freshness {
    let file = match open_named_file(directory, target) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return Freshness::Gone(LoadFailure::NotAFile);
        }
        Err(error) => return Freshness::Gone(classify_read_error(error)),
    };
    match file.metadata() {
        Ok(metadata) if !metadata.is_file() => Freshness::Gone(LoadFailure::NotAFile),
        Ok(_) => match Generation::from_file(&file) {
            Ok(current) if current == known => Freshness::Unchanged,
            Ok(current) => Freshness::Changed(current),
            Err(error) => Freshness::Gone(classify_read_error(error)),
        },
        Err(error) => Freshness::Gone(classify_read_error(error)),
    }
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
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidData => {
            SaveFailure::UnsupportedFilesystem
        }
        std::io::ErrorKind::CrossesDevices => SaveFailure::CrossVolume,
        _ => SaveFailure::Interrupted,
    }
}

fn classify_metadata_error(error: std::io::Error) -> SaveFailure {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        SaveFailure::MetadataPreservation
    } else if matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported
            | std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::InvalidData
    ) {
        SaveFailure::UnsupportedFilesystem
    } else {
        classify_write_error(error)
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

#[cfg(unix)]
fn preserve_security_metadata(
    original: &File,
    temporary: &mut File,
) -> Result<festerm_unix_security::SecurityMetadata, SaveFailure> {
    festerm_unix_security::preserve_security_metadata(original, temporary)
        .map_err(classify_metadata_error)
}

fn open_staging_directory(
    directory: &cap_std::fs::Dir,
    name: &Path,
) -> Result<cap_std::fs::Dir, std::io::Error> {
    #[cfg(unix)]
    {
        let parent = directory.try_clone()?.into_std_file();
        let child = festerm_unix_security::open_directory_read_nofollow(&parent, name)?;
        Ok(cap_std::fs::Dir::from_std_file(child))
    }
    #[cfg(not(unix))]
    {
        directory.open_dir(name)
    }
}

/// A file that deletes itself unless it is explicitly kept, so a save that
/// fails half way through leaves no debris beside the user's file.
struct TemporaryFile<'a> {
    directory: &'a cap_std::fs::Dir,
    staging: cap_std::fs::Dir,
    staging_identity: DirectoryIdentity,
    staging_directory: PathBuf,
    file: Option<File>,
    persist: bool,
}

impl<'a> TemporaryFile<'a> {
    fn create(directory: &'a cap_std::fs::Dir) -> Result<Self, SaveFailure> {
        #[cfg(unix)]
        let parent_security = {
            let parent = directory
                .try_clone()
                .map(cap_std::fs::Dir::into_std_file)
                .map_err(classify_write_error)?;
            festerm_unix_security::secure_staging_parent(&parent).map_err(|error| {
                if error.kind() == std::io::ErrorKind::Unsupported {
                    SaveFailure::UnsafeDestinationFolder
                } else {
                    classify_write_error(error)
                }
            })?
        };
        for _ in 0..TEMPORARY_FILE_ATTEMPTS {
            let staging_directory = temporary_directory_path()?;
            #[cfg(unix)]
            {
                use cap_std::fs::DirBuilderExt;
                let mut builder = cap_std::fs::DirBuilder::new();
                builder.mode(0o700);
                match directory.create_dir_with(&staging_directory, &builder) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(classify_write_error(error)),
                }
                after_staging_directory_create();
            }
            #[cfg(windows)]
            let staging = {
                let root = directory
                    .try_clone()
                    .map(cap_std::fs::Dir::into_std_file)
                    .map_err(classify_write_error)?;
                match festerm_windows_security::create_current_user_only_directory(
                    &root,
                    &staging_directory,
                ) {
                    Ok(staging) => cap_std::fs::Dir::from_std_file(staging),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        let _ = directory.remove_dir(&staging_directory);
                        return Err(classify_write_error(error));
                    }
                }
            };
            #[cfg(unix)]
            let staging = match open_staging_directory(directory, &staging_directory) {
                Ok(staging) => staging,
                Err(error) => {
                    let _ = directory.remove_dir(&staging_directory);
                    return Err(classify_write_error(error));
                }
            };
            #[cfg(unix)]
            {
                let parent = directory
                    .try_clone()
                    .map(cap_std::fs::Dir::into_std_file)
                    .map_err(classify_write_error)?;
                let matches = match festerm_unix_security::staging_parent_matches(
                    &parent,
                    &parent_security,
                ) {
                    Ok(matches) => matches,
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => false,
                    Err(error) => return Err(classify_write_error(error)),
                };
                if !matches {
                    drop(staging);
                    tracing::warn!(
                        path = %staging_directory.display(),
                        "save staging was retained after the parent security state changed"
                    );
                    return Err(SaveFailure::Interrupted);
                }
            }
            let staging_identity = match DirectoryIdentity::from_directory(&staging) {
                Ok(identity) => identity,
                Err(error) => {
                    drop(staging);
                    let _ = directory.remove_dir(&staging_directory);
                    return Err(classify_write_error(error));
                }
            };
            #[cfg(unix)]
            {
                let private = staging
                    .try_clone()
                    .map(cap_std::fs::Dir::into_std_file)
                    .and_then(|staging| festerm_unix_security::make_private_directory(&staging));
                if let Err(error) = private {
                    drop(staging);
                    remove_staging_if_matches(directory, &staging_directory, staging_identity);
                    return Err(classify_write_error(error));
                }
            }
            if let Err(error) = sync_directory_durably(directory) {
                drop(staging);
                remove_staging_if_matches(directory, &staging_directory, staging_identity);
                return Err(classify_write_error(error));
            }
            let payload = Path::new("payload");
            before_staging_payload_create();
            #[cfg(unix)]
            let file = {
                use cap_std::fs::OpenOptionsExt;
                let mut options = cap_std::fs::OpenOptions::new();
                options.read(true).write(true).create_new(true).mode(0o600);
                staging
                    .open_with(payload, &options)
                    .map(|file| file.into_std())
            };
            #[cfg(windows)]
            let file = staging
                .try_clone()
                .map(cap_std::fs::Dir::into_std_file)
                .and_then(|directory| {
                    festerm_windows_security::create_current_user_only_file_exclusive(
                        &directory, payload,
                    )
                });
            #[cfg(not(any(unix, windows)))]
            let file = {
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                staging
                    .open_with(payload, &options)
                    .map(|file| file.into_std())
            };
            match file {
                Ok(file) => {
                    #[cfg(unix)]
                    if let Err(error) = festerm_unix_security::make_private(&file) {
                        let _ = staging.remove_file(payload);
                        if open_staging_directory(directory, &staging_directory)
                            .is_ok_and(|current| staging_identity.matches_directory(&current))
                        {
                            let _ = directory.remove_dir(&staging_directory);
                        }
                        return Err(classify_write_error(error));
                    }
                    let temporary = Self {
                        directory,
                        staging,
                        staging_identity,
                        staging_directory,
                        file: Some(file),
                        persist: false,
                    };
                    temporary.verify_name()?;
                    return Ok(temporary);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let _ = staging.remove_file(payload);
                    drop(staging);
                    remove_staging_if_matches(directory, &staging_directory, staging_identity);
                    continue;
                }
                Err(error) => {
                    let _ = staging.remove_file(payload);
                    drop(staging);
                    remove_staging_if_matches(directory, &staging_directory, staging_identity);
                    return Err(classify_write_error(error));
                }
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

    fn persist(&mut self) {
        self.persist = true;
    }

    fn create_private_staging_file(&self, name: &Path) -> Result<File, SaveFailure> {
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true).mode(0o600);
            let file = self
                .staging
                .open_with(name, &options)
                .map(cap_std::fs::File::into_std)
                .map_err(classify_write_error)?;
            festerm_unix_security::make_private(&file).map_err(classify_write_error)?;
            Ok(file)
        }
        #[cfg(windows)]
        {
            let staging = self
                .staging
                .try_clone()
                .map(cap_std::fs::Dir::into_std_file)
                .map_err(classify_write_error)?;
            festerm_windows_security::create_current_user_only_file(&staging, name)
                .map_err(classify_write_error)
        }
    }

    fn copy_to_private_staging_file(
        &self,
        source: &mut File,
        name: &Path,
    ) -> Result<(), SaveFailure> {
        let mut recovery = self.create_private_staging_file(name)?;
        let expected_length = source.metadata().map_err(classify_write_error)?.len();
        source.rewind().map_err(classify_write_error)?;
        let copied = std::io::copy(source, &mut recovery).map_err(classify_write_error)?;
        if copied != expected_length {
            return Err(SaveFailure::Interrupted);
        }
        recovery.sync_all().map_err(classify_write_error)
    }

    fn retain_recovery_copy(&mut self) -> Result<(), SaveFailure> {
        let mut prepared = self.file.take().ok_or(SaveFailure::Interrupted)?;
        let result = self.copy_to_private_staging_file(&mut prepared, Path::new("prepared"));
        self.file = Some(prepared);
        result?;
        sync_directory_durably(&self.staging).map_err(classify_write_error)
    }

    #[cfg(windows)]
    fn retain_original_recovery_copy(&self, source: &mut File) -> Result<(), SaveFailure> {
        self.copy_to_private_staging_file(source, Path::new("original"))?;
        sync_directory_durably(&self.staging).map_err(classify_write_error)
    }

    #[cfg(windows)]
    fn original_recovery_copy_matches(&self, source: &mut File) -> Result<bool, SaveFailure> {
        let mut recovery =
            open_named_file(&self.staging, Path::new("original")).map_err(classify_write_error)?;
        source.rewind().map_err(classify_write_error)?;
        recovery.rewind().map_err(classify_write_error)?;
        let mut source_buffer = [0_u8; 64 * 1024];
        let mut recovery_buffer = [0_u8; 64 * 1024];
        loop {
            let source_read = source
                .read(&mut source_buffer)
                .map_err(classify_write_error)?;
            let recovery_read = recovery
                .read(&mut recovery_buffer)
                .map_err(classify_write_error)?;
            if source_read != recovery_read
                || source_buffer[..source_read] != recovery_buffer[..recovery_read]
            {
                return Ok(false);
            }
            if source_read == 0 {
                return Ok(true);
            }
        }
    }

    fn finish(&mut self) {
        self.close_file();
        #[cfg(windows)]
        let cleanup_names = ["original", "displaced", "prepared"].as_slice();
        #[cfg(not(windows))]
        let cleanup_names = ["original", "prepared"].as_slice();
        for name in cleanup_names {
            if let Err(error) = self.staging.remove_file(name) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %self.staging_directory.display(),
                        %error,
                        "a private save staging file could not be removed after publication"
                    );
                }
            }
        }
        if !self.staging_name_matches() {
            tracing::warn!(
                path = %self.staging_directory.display(),
                "an empty private save staging directory was renamed before cleanup"
            );
        } else if let Err(error) = self.directory.remove_dir(&self.staging_directory) {
            tracing::warn!(
                path = %self.staging_directory.display(),
                %error,
                "an empty private save staging directory could not be removed"
            );
        }
        self.persist();
    }

    fn recovery_required(&mut self) -> SaveFailure {
        #[cfg(unix)]
        if let Some(file) = &self.file {
            if let Err(error) = festerm_unix_security::make_private(file) {
                tracing::error!(
                    path = %self.staging_directory.display(),
                    %error,
                    "a retained save recovery file could not be restricted to the current user"
                );
            }
        }
        #[cfg(windows)]
        if let Some(file) = &self.file {
            if let Err(error) = festerm_windows_security::restrict_to_current_user(file) {
                tracing::error!(
                    path = %self.staging_directory.display(),
                    %error,
                    "a retained Windows save recovery file could not be restricted to the current user"
                );
            }
        }
        self.persist();
        SaveFailure::RecoveryRequired
    }

    fn original_generation(&self) -> Result<Generation, std::io::Error> {
        generation_in_directory(&self.staging, Path::new("original"))
    }

    #[cfg(windows)]
    fn publish_new(&self, target: &Path) -> Result<(), std::io::Error> {
        let target_directory = self.directory.try_clone()?.into_std_file();
        let exact_source = self
            .file
            .as_ref()
            .ok_or_else(|| std::io::Error::other("the save payload is closed"))?;
        let staging = self.staging.try_clone()?.into_std_file();
        let source = festerm_windows_security::open_file_no_reparse_for_move(
            &staging,
            Path::new("payload"),
        )?;
        if !festerm_windows_security::same_file_identity(exact_source, &source)? {
            return Err(std::io::Error::other(
                "the staged payload name no longer identifies its retained handle",
            ));
        }
        festerm_windows_security::rename_file_noreplace(&source, &target_directory, target)
    }

    fn staging_name_matches(&self) -> bool {
        open_staging_directory(self.directory, &self.staging_directory)
            .is_ok_and(|directory| self.staging_identity.matches_directory(&directory))
    }

    fn verify_name(&self) -> Result<(), SaveFailure> {
        #[cfg(windows)]
        {
            // Native relative creation already binds this exact non-reparse
            // handle to the retained staging directory. Reopening by name
            // would require sharing reads and writes with a staged payload.
            return self
                .file
                .as_ref()
                .map(|_| ())
                .ok_or(SaveFailure::Interrupted);
        }
        #[cfg(not(windows))]
        {
            if !self.staging_name_matches() {
                return Err(SaveFailure::Interrupted);
            }
            let reopened = open_named_file(&self.staging, Path::new("payload"))
                .map_err(classify_write_error)?;
            let open = self
                .file
                .as_ref()
                .ok_or(SaveFailure::Interrupted)
                .and_then(|file| {
                    Generation::from_file(file).map_err(|_| SaveFailure::Interrupted)
                })?;
            let reopened =
                Generation::from_file(&reopened).map_err(|_| SaveFailure::Interrupted)?;
            if open == reopened {
                Ok(())
            } else {
                Err(SaveFailure::Interrupted)
            }
        }
    }
}

fn remove_staging_if_matches(
    directory: &cap_std::fs::Dir,
    staging_directory: &Path,
    expected: DirectoryIdentity,
) {
    if open_staging_directory(directory, staging_directory)
        .is_ok_and(|current| expected.matches_directory(&current))
    {
        let _ = directory.remove_dir(staging_directory);
    }
}

impl Drop for TemporaryFile<'_> {
    fn drop(&mut self) {
        if !self.persist {
            self.close_file();
            #[cfg(windows)]
            let cleanup_names = ["payload", "original", "displaced", "prepared"].as_slice();
            #[cfg(not(windows))]
            let cleanup_names = ["payload", "prepared"].as_slice();
            for name in cleanup_names {
                if let Err(error) = self.staging.remove_file(name) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            path = %self.staging_directory.display(),
                            %error,
                            "an unpublished save staging file could not be removed"
                        );
                    }
                }
            }
            if self.staging_name_matches() {
                if let Err(error) = self.directory.remove_dir(&self.staging_directory) {
                    tracing::warn!(
                        path = %self.staging_directory.display(),
                        %error,
                        "an unpublished save staging directory could not be removed"
                    );
                }
            }
        }
    }
}

fn temporary_path() -> Result<PathBuf, SaveFailure> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| SaveFailure::Interrupted)?;
    let mut encoded = String::with_capacity(random.len() * 2);
    for byte in random {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(PathBuf::from(format!(".festerm-save-{encoded}.tmp")))
}

fn temporary_directory_path() -> Result<PathBuf, SaveFailure> {
    temporary_path().map(|path| path.with_extension("stage"))
}

#[cfg(unix)]
fn publish_temporary(
    directory: &cap_std::fs::Dir,
    temporary: &mut TemporaryFile<'_>,
    target: &Path,
    original_generation: Option<Generation>,
    temporary_generation: Generation,
    original_file: Option<&File>,
    security_metadata: Option<&festerm_unix_security::SecurityMetadata>,
) -> Result<Generation, SaveFailure> {
    temporary.retain_recovery_copy()?;
    before_save_replacement();
    let Some(original_generation) = original_generation else {
        rename_noreplace(&temporary.staging, Path::new("payload"), directory, target).map_err(
            |error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    match generation_in_directory(directory, target) {
                        Ok(generation) => SaveFailure::Conflict(generation),
                        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                            SaveFailure::NotAFile
                        }
                        Err(error) => classify_write_error(error),
                    }
                } else {
                    classify_write_error(error)
                }
            },
        )?;
        temporary.close_file();
        if let Err(error) = sync_directory_durably(directory) {
            tracing::error!(%error, "the newly published save directory could not be synchronized");
            return Err(temporary.recovery_required());
        }
        after_save_replacement();
        let published = match generation_in_directory(directory, target) {
            Ok(published) => published,
            Err(error) => {
                tracing::error!(%error, "a newly published save could not be verified");
                return Err(temporary.recovery_required());
            }
        };
        if published != temporary_generation {
            return Err(temporary.recovery_required());
        }
        temporary.finish();
        return Ok(published);
    };

    let original_file = original_file.ok_or(SaveFailure::Interrupted)?;
    let security_metadata = security_metadata.ok_or(SaveFailure::Interrupted)?;
    let current = open_named_file(directory, target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SaveFailure::Gone
        } else if error.kind() == std::io::ErrorKind::InvalidInput {
            SaveFailure::NotAFile
        } else {
            classify_write_error(error)
        }
    })?;
    let current_generation =
        Generation::from_file(&current).map_err(|_| SaveFailure::Interrupted)?;
    if current_generation != original_generation {
        return Err(SaveFailure::Conflict(current_generation));
    }
    if !festerm_unix_security::security_metadata_matches(&current, security_metadata)
        .map_err(classify_write_error)?
    {
        return Err(SaveFailure::Conflict(current_generation));
    }
    if !festerm_unix_security::security_metadata_matches(original_file, security_metadata)
        .map_err(classify_write_error)?
    {
        return Err(SaveFailure::Conflict(original_generation));
    }
    rename_noreplace(directory, target, &temporary.staging, Path::new("original")).map_err(
        |error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SaveFailure::Gone
            } else {
                classify_write_error(error)
            }
        },
    )?;
    if let Err(error) =
        sync_directory_durably(&temporary.staging).and_then(|()| sync_directory_durably(directory))
    {
        tracing::error!(%error, "the captured original could not be synchronized");
        return Err(temporary.recovery_required());
    }
    let displaced = match temporary.original_generation() {
        Ok(generation) => generation,
        Err(_) => return Err(temporary.recovery_required()),
    };
    if displaced != original_generation {
        if rename_noreplace(&temporary.staging, Path::new("original"), directory, target).is_ok() {
            if let Err(error) = sync_directory_durably(directory)
                .and_then(|()| sync_directory_durably(&temporary.staging))
            {
                tracing::error!(%error, "the restored conflict target could not be synchronized");
                return Err(temporary.recovery_required());
            }
            return Err(SaveFailure::Conflict(displaced));
        }
        return Err(temporary.recovery_required());
    }
    after_target_capture();
    if let Err(error) =
        rename_noreplace(&temporary.staging, Path::new("payload"), directory, target)
    {
        if rename_noreplace(&temporary.staging, Path::new("original"), directory, target).is_ok() {
            if let Err(sync_error) = sync_directory_durably(directory)
                .and_then(|()| sync_directory_durably(&temporary.staging))
            {
                tracing::error!(
                    %sync_error,
                    "the restored target after failed publication could not be synchronized"
                );
                return Err(temporary.recovery_required());
            }
            return Err(classify_write_error(error));
        }
        tracing::error!(%error, "a concurrent target prevented conditional save publication");
        return Err(temporary.recovery_required());
    }
    temporary.close_file();
    if let Err(error) =
        sync_directory_durably(directory).and_then(|()| sync_directory_durably(&temporary.staging))
    {
        tracing::error!(%error, "the published save state could not be synchronized");
        return Err(temporary.recovery_required());
    }
    after_save_replacement();
    let published_file = match open_named_file(directory, target) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, "the replaced Unix save target could not be reopened");
            return Err(temporary.recovery_required());
        }
    };
    let published = match Generation::from_file(&published_file) {
        Ok(generation) => generation,
        Err(error) => {
            tracing::error!(%error, "the replaced Unix save target could not be identified");
            return Err(temporary.recovery_required());
        }
    };
    let retained_original = match Generation::from_file(original_file) {
        Ok(generation) => generation,
        Err(error) => {
            tracing::error!(%error, "the retained Unix original could not be identified");
            return Err(temporary.recovery_required());
        }
    };
    let published_security_matches = match festerm_unix_security::security_metadata_matches(
        &published_file,
        security_metadata,
    ) {
        Ok(matches) => matches,
        Err(error) => {
            tracing::error!(%error, "the replaced Unix target metadata could not be verified");
            return Err(temporary.recovery_required());
        }
    };
    let original_security_matches = match festerm_unix_security::security_metadata_matches(
        original_file,
        security_metadata,
    ) {
        Ok(matches) => matches,
        Err(error) => {
            tracing::error!(%error, "the retained Unix original metadata could not be verified");
            return Err(temporary.recovery_required());
        }
    };
    if published != temporary_generation
        || retained_original != original_generation
        || !published_security_matches
        || !original_security_matches
    {
        return Err(temporary.recovery_required());
    }
    temporary.finish();
    Ok(published)
}

#[cfg(windows)]
fn restore_windows_displaced(
    displaced: &File,
    destination: &File,
    target: &Path,
    security_metadata: &festerm_windows_security::SecurityMetadata,
) -> bool {
    festerm_windows_security::apply_security_metadata(displaced, security_metadata)
        .and_then(|()| {
            festerm_windows_security::rename_file_noreplace(displaced, destination, target)
        })
        .is_ok()
}

#[cfg(windows)]
fn publish_temporary(
    directory: &cap_std::fs::Dir,
    temporary: &mut TemporaryFile<'_>,
    target: &Path,
    original_generation: Option<Generation>,
    temporary_generation: Generation,
    original_file: Option<File>,
    security_metadata: Option<&festerm_windows_security::SecurityMetadata>,
) -> Result<Generation, SaveFailure> {
    temporary.retain_recovery_copy()?;
    let directory_handle = directory
        .try_clone()
        .map(cap_std::fs::Dir::into_std_file)
        .map_err(classify_write_error)?;
    let Some(original_generation) = original_generation else {
        let expected_security = festerm_windows_security::security_metadata(temporary.file_mut())
            .map_err(classify_write_error)?;
        before_save_replacement();
        temporary.publish_new(target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                generation_in_directory(directory, target)
                    .map(SaveFailure::Conflict)
                    .unwrap_or(SaveFailure::Interrupted)
            } else {
                classify_write_error(error)
            }
        })?;
        let published_file = match festerm_windows_security::open_file_no_reparse_for_verification(
            &directory_handle,
            target,
        ) {
            Ok(file) => file,
            Err(error) => {
                tracing::error!(%error, "a newly published save could not be locked for verification");
                return Err(temporary.recovery_required());
            }
        };
        after_save_replacement();
        let published = match Generation::from_file(&published_file) {
            Ok(generation) => generation,
            Err(error) => {
                tracing::error!(%error, "a newly published save could not be verified");
                return Err(temporary.recovery_required());
            }
        };
        let exact_payload = temporary.file.as_ref().ok_or(SaveFailure::Interrupted)?;
        let exact_generation =
            Generation::from_file(exact_payload).map_err(|_| SaveFailure::Interrupted)?;
        let same_identity =
            festerm_windows_security::same_file_identity(exact_payload, &published_file)
                .unwrap_or(false);
        let security_matches =
            festerm_windows_security::security_metadata_matches(exact_payload, &expected_security)
                .unwrap_or(false);
        if published != temporary_generation
            || exact_generation != temporary_generation
            || !same_identity
            || !security_matches
        {
            return Err(temporary.recovery_required());
        }
        temporary.finish();
        return Ok(published);
    };

    let mut original_file = original_file.ok_or(SaveFailure::Interrupted)?;
    let security_metadata = security_metadata.ok_or(SaveFailure::Interrupted)?;
    if Generation::from_file(&original_file).map_err(|_| SaveFailure::Interrupted)?
        != original_generation
        || !festerm_windows_security::security_metadata_matches(&original_file, security_metadata)
            .map_err(classify_write_error)?
    {
        return Err(SaveFailure::Conflict(original_generation));
    }
    temporary.retain_original_recovery_copy(&mut original_file)?;
    festerm_windows_security::apply_security_metadata(temporary.file_mut(), security_metadata)
        .map_err(classify_metadata_error)?;
    drop(original_file);
    before_save_replacement();
    let staging_handle = temporary
        .staging
        .try_clone()
        .map(cap_std::fs::Dir::into_std_file)
        .map_err(classify_write_error)?;
    let mut current =
        festerm_windows_security::open_file_no_reparse_for_rename(&directory_handle, target)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    SaveFailure::Gone
                } else {
                    classify_write_error(error)
                }
            })?;
    let current_generation =
        Generation::from_file(&current).map_err(|_| SaveFailure::Interrupted)?;
    if current_generation != original_generation
        || !festerm_windows_security::security_metadata_matches(&current, security_metadata)
            .map_err(classify_write_error)?
    {
        return Err(SaveFailure::Conflict(current_generation));
    }
    festerm_windows_security::rename_file_noreplace(
        &current,
        &staging_handle,
        Path::new("displaced"),
    )
    .map_err(classify_write_error)?;
    let displaced = match Generation::from_file(&current) {
        Ok(generation) => generation,
        Err(_) => return Err(temporary.recovery_required()),
    };
    let displaced_security_matches =
        festerm_windows_security::security_metadata_matches(&current, security_metadata)
            .unwrap_or(false);
    if displaced != original_generation || !displaced_security_matches {
        if festerm_windows_security::rename_file_noreplace(&current, &directory_handle, target)
            .is_ok()
        {
            return Err(SaveFailure::Conflict(displaced));
        }
        return Err(temporary.recovery_required());
    }
    if let Err(error) = festerm_windows_security::restrict_to_current_user(&current) {
        tracing::error!(%error, "the displaced Windows original could not be made private");
        if restore_windows_displaced(&current, &directory_handle, target, security_metadata) {
            return Err(classify_metadata_error(error));
        }
        return Err(temporary.recovery_required());
    }
    if !temporary.original_recovery_copy_matches(&mut current)? {
        if restore_windows_displaced(&current, &directory_handle, target, security_metadata) {
            return Err(SaveFailure::Conflict(displaced));
        }
        return Err(temporary.recovery_required());
    }
    after_target_capture();
    if let Err(error) = temporary.publish_new(target) {
        if restore_windows_displaced(&current, &directory_handle, target, security_metadata) {
            return Err(classify_write_error(error));
        }
        tracing::error!(%error, "a concurrent target prevented conditional save publication");
        return Err(temporary.recovery_required());
    }
    let published_file = match festerm_windows_security::open_file_no_reparse_for_verification(
        &directory_handle,
        target,
    ) {
        Ok(file) => file,
        Err(error) => {
            tracing::error!(%error, "the replaced Windows save target could not be reopened");
            return Err(temporary.recovery_required());
        }
    };
    after_save_replacement();
    let published = match Generation::from_file(&published_file) {
        Ok(generation) => generation,
        Err(error) => {
            tracing::error!(%error, "the replaced Windows save target could not be identified");
            return Err(temporary.recovery_required());
        }
    };
    let retained_original = match Generation::from_file(&current) {
        Ok(generation) => generation,
        Err(error) => {
            tracing::error!(%error, "the retained Windows original could not be identified");
            return Err(temporary.recovery_required());
        }
    };
    let retained_payload = match temporary.file.as_ref().and_then(|file| {
        Generation::from_file(file)
            .ok()
            .map(|generation| (file, generation))
    }) {
        Some(payload) => payload,
        None => {
            tracing::error!("the retained Windows save payload could not be identified");
            return Err(temporary.recovery_required());
        }
    };
    let published_security_matches = match festerm_windows_security::security_metadata_matches(
        retained_payload.0,
        security_metadata,
    ) {
        Ok(matches) => matches,
        Err(error) => {
            tracing::error!(%error, "the replaced Windows target metadata could not be verified");
            return Err(temporary.recovery_required());
        }
    };
    let same_published_identity =
        festerm_windows_security::same_file_identity(retained_payload.0, &published_file)
            .unwrap_or(false);
    let original_copy_matches = temporary
        .original_recovery_copy_matches(&mut current)
        .unwrap_or(false);
    if published != temporary_generation
        || retained_payload.1 != temporary_generation
        || retained_original != original_generation
        || !same_published_identity
        || !published_security_matches
        || !original_copy_matches
    {
        return Err(temporary.recovery_required());
    }
    drop(current);
    temporary.finish();
    Ok(published)
}

#[cfg(unix)]
fn rename_noreplace(
    source_directory: &cap_std::fs::Dir,
    source: &Path,
    target_directory: &cap_std::fs::Dir,
    target: &Path,
) -> Result<(), std::io::Error> {
    let source_directory = source_directory.try_clone()?.into_std_file();
    let target_directory = target_directory.try_clone()?.into_std_file();
    festerm_unix_security::rename_noreplace(&source_directory, source, &target_directory, target)
}

#[cfg(unix)]
fn sync_directory_durably(directory: &cap_std::fs::Dir) -> Result<(), std::io::Error> {
    let directory = directory.try_clone()?.into_std_file();
    festerm_unix_security::reopen_directory_read(&directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory_durably(_directory: &cap_std::fs::Dir) -> Result<(), std::io::Error> {
    Ok(())
}

fn sync_directory(directory: &cap_std::fs::Dir) {
    let _ = sync_directory_durably(directory);
}

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

    fn loaded_expectation(loaded: &LoadedDocument) -> SaveExpectation<'_> {
        SaveExpectation::Loaded {
            generation: loaded.generation,
            authority: &loaded.source_authority,
        }
    }

    fn absent_destination(path: &Path) -> ConfirmedDestination {
        let destination = observe_destination(path).unwrap();
        assert_eq!(destination.expectation(), DestinationExpectation::Absent);
        destination
    }

    fn replace_path(source: &Path, target: &Path) {
        #[cfg(windows)]
        fs::remove_file(target).unwrap();
        fs::rename(source, target).unwrap();
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

    #[cfg(unix)]
    #[test]
    fn loading_traverses_a_search_only_ancestor_without_requiring_read_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("search-only-ancestor");
        let ancestor = directory.path.join("search-only");
        fs::create_dir(&ancestor).unwrap();
        let writable_parent = ancestor.join("documents");
        fs::create_dir(&writable_parent).unwrap();
        let path = writable_parent.join("notes.md");
        fs::write(&path, b"alpha\n").unwrap();
        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o111)).unwrap();

        let loaded = load(&path, bounds());
        let saved = loaded.as_ref().map_err(|_| ()).and_then(|loaded| {
            save(&path, b"beta\n", loaded_expectation(loaded))
                .map(|_| ())
                .map_err(|_| ())
        });

        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
        let loaded = loaded.unwrap();
        saved.unwrap();
        assert_eq!(loaded.document.text(), "alpha\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "beta\n");
    }

    #[cfg(unix)]
    #[test]
    fn saving_refuses_a_non_sticky_shared_writable_parent_before_staging() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("shared-writable-parent");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o777)).unwrap();

        let failure = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap_err();

        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(failure, SaveFailure::UnsafeDestinationFolder);
        assert!(failure.detail().contains("folder"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn parent_security_change_after_staging_refuses_and_retains_the_private_directory() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("parent-security-change");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let parent = directory.path.clone();
        AFTER_STAGING_DIRECTORY_CREATE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
            }));
        });

        let failure = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap_err();

        fs::set_permissions(&directory.path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(failure, SaveFailure::Interrupted);
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        assert!(fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .any(|candidate| candidate.is_dir()));
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
    fn metadata_permission_failure_explains_the_safe_save_as_path() {
        let failure =
            classify_metadata_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert_eq!(failure, SaveFailure::MetadataPreservation);
        assert!(failure.headline().contains("access"));
        assert!(failure.detail().contains("Save As"));

        let unsupported =
            classify_metadata_error(std::io::Error::from(std::io::ErrorKind::Unsupported));
        assert_eq!(unsupported, SaveFailure::UnsupportedFilesystem);
        assert!(unsupported.detail().contains("different local disk"));

        let encrypted = SaveFailure::EncryptedFile;
        assert!(encrypted.headline().contains("encryption"));
        assert!(encrypted.detail().contains("EFS"));
        assert!(encrypted.detail().contains("Save As"));

        let streams = SaveFailure::NamedStreams;
        assert!(streams.headline().contains("data streams"));
        assert!(streams.detail().contains("Zone.Identifier"));
        assert!(streams.detail().contains("Save As"));
    }

    #[test]
    fn saving_replaces_the_contents_and_returns_the_new_generation() {
        let directory = TemporaryDirectory::new("save");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();

        let saved = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

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

        save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

        let entries: Vec<_> = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("notes.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_symlinked_source_updates_its_canonical_target() {
        let real = TemporaryDirectory::new("save-symlink-real");
        let links = TemporaryDirectory::new("save-symlink-links");
        let target = real.file("notes.md", "before\n");
        let alias = links.path.join("linked.md");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let loaded = load(&alias, bounds()).unwrap();

        save(&alias, b"after\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "after\n");
        assert!(
            fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink(),
            "ordinary Save must not replace the selected alias in its lexical parent"
        );
        assert_eq!(fs::read_to_string(&alias).unwrap(), "after\n");
    }

    #[test]
    fn a_file_changed_underneath_us_is_a_conflict_and_is_not_overwritten() {
        let directory = TemporaryDirectory::new("conflict");
        let path = directory.file("notes.md", "mine\n");
        let loaded = load(&path, bounds()).unwrap();

        fs::write(&path, "theirs, which is longer\n").unwrap();
        let failure = save(&path, b"mine, edited\n", loaded_expectation(&loaded)).unwrap_err();

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
            save(&path, b"my edits\n", loaded_expectation(&loaded),),
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
            save(&path, b"mine\n", loaded_expectation(&loaded),).unwrap_err(),
            SaveFailure::Gone
        );
        assert!(!path.exists());
    }

    #[test]
    fn saving_without_a_known_generation_writes_a_new_file() {
        let directory = TemporaryDirectory::new("saveas");
        let path = directory.path.join("fresh.md");
        let destination = absent_destination(&path);

        let saved = save(&path, b"new\n", SaveExpectation::Destination(&destination)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        assert_eq!(saved.generation.size(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn save_as_refuses_a_symlink_destination_without_moving_it() {
        let directory = TemporaryDirectory::new("saveas-symlink");
        let target = directory.file("target.md", "target\n");
        let link = directory.path.join("link.md");
        std::os::unix::fs::symlink("target.md", &link).unwrap();

        let failure = observe_destination(&link).unwrap_err();

        assert_eq!(failure, SaveFailure::NotAFile);
        assert_eq!(fs::read_link(&link).unwrap(), PathBuf::from("target.md"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "target\n");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 2);
    }

    #[test]
    fn save_as_refuses_a_destination_that_appears_after_confirmation() {
        let directory = TemporaryDirectory::new("saveas-appeared");
        let path = directory.path.join("fresh.md");
        let destination = observe_destination(&path).unwrap();
        assert_eq!(destination.expectation(), DestinationExpectation::Absent);
        fs::write(&path, b"winner\n").unwrap();

        let failure = save(
            &path,
            b"editor\n",
            SaveExpectation::Destination(&destination),
        )
        .unwrap_err();

        assert!(matches!(failure, SaveFailure::Conflict(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "winner\n");
    }

    #[test]
    fn unsupported_write_errors_are_non_retryable() {
        for kind in [
            std::io::ErrorKind::Unsupported,
            std::io::ErrorKind::InvalidData,
        ] {
            assert_eq!(
                classify_write_error(std::io::Error::from(kind)),
                SaveFailure::UnsupportedFilesystem
            );
        }
        assert_eq!(
            classify_write_error(std::io::Error::from(std::io::ErrorKind::CrossesDevices)),
            SaveFailure::CrossVolume
        );
    }

    #[test]
    fn save_as_refuses_a_destination_that_changes_after_confirmation() {
        let directory = TemporaryDirectory::new("saveas-changed");
        let path = directory.file("notes.md", "observed\n");
        let destination = observe_destination(&path).unwrap();
        assert!(matches!(
            destination.expectation(),
            DestinationExpectation::Existing(_)
        ));
        fs::write(&path, b"newer and different\n").unwrap();

        let failure = save(
            &path,
            b"editor\n",
            SaveExpectation::Destination(&destination),
        )
        .unwrap_err();

        assert!(matches!(failure, SaveFailure::Conflict(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer and different\n");
    }

    #[test]
    fn saved_authority_remains_bound_to_the_destination_parent() {
        let directory = TemporaryDirectory::new("saved-parent");
        let retained = TemporaryDirectory::new("saved-parent-retained");
        fs::remove_dir(&retained.path).unwrap();
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let saved = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

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
    fn save_uses_the_captured_parent_after_its_path_is_rebound() {
        let directory = TemporaryDirectory::new("save-captured-parent");
        let retained = TemporaryDirectory::new("save-captured-parent-retained");
        fs::remove_dir(&retained.path).unwrap();
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let original = directory.path.clone();
        let moved = retained.path.clone();
        AFTER_SAVE_DIRECTORY_CAPTURE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::rename(&original, &moved).unwrap();
                fs::create_dir(&original).unwrap();
                fs::write(original.join("notes.md"), "replacement\n").unwrap();
            }));
        });

        save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(
            fs::read_to_string(retained.path.join("notes.md")).unwrap(),
            "after\n",
            "the captured source parent receives the atomic replacement"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "replacement\n",
            "a later pathname occupant must not receive saved bytes"
        );
    }

    #[test]
    fn save_as_never_moves_or_replaces_a_directory() {
        let directory = TemporaryDirectory::new("directory-saveas");
        let folder = directory.path.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("sentinel.txt"), b"unchanged").unwrap();

        assert_eq!(
            observe_destination(&folder).unwrap_err(),
            SaveFailure::NotAFile
        );
        assert!(folder.is_dir());
        assert_eq!(fs::read(folder.join("sentinel.txt")).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[test]
    fn saving_into_a_missing_folder_is_refused() {
        let directory = TemporaryDirectory::new("nofolder");
        let path = directory.path.join("absent").join("fresh.md");

        assert_eq!(
            observe_destination(&path).unwrap_err(),
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

        save(&path, b"echo after\n", loaded_expectation(&loaded)).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750);
    }

    #[cfg(unix)]
    #[test]
    fn save_temporary_file_is_private_before_content_is_written() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("private-temporary");
        let path = directory.file("secret.md", "before\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let loaded = load(&path, bounds()).unwrap();
        let observed_directory = directory.path.clone();
        BEFORE_SAVE_WRITE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(&observed_directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".festerm-save-") && name.ends_with(".stage")
                            })
                    })
                    .expect("private save staging directory");
                let temporary = staging.join("payload");
                let metadata = fs::metadata(&temporary).unwrap();
                assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
                assert_eq!(metadata.len(), 0);
                assert_eq!(
                    fs::read_to_string(observed_directory.join("secret.md")).unwrap(),
                    "before\n"
                );
            }));
        });

        save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn payload_creation_collision_is_cleaned_before_retry() {
        let directory = TemporaryDirectory::new("payload-collision-cleanup");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let observed_directory = directory.path.clone();
        BEFORE_STAGING_PAYLOAD_CREATE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(&observed_directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".festerm-save-") && name.ends_with(".stage")
                            })
                    })
                    .expect("private save staging directory");
                fs::write(staging.join("payload"), b"collision\n").unwrap();
            }));
        });

        save(&path, b"after\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn staging_symlink_substitution_before_open_is_refused_without_mutation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("staging-open-substitution");
        let path = directory.file("notes.md", "before\n");
        let victim = directory.path.join("victim");
        fs::create_dir(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(victim.join("payload"), b"keep\n").unwrap();
        let loaded = load(&path, bounds()).unwrap();
        let observed_directory = directory.path.clone();
        let victim_for_hook = victim.clone();
        AFTER_STAGING_DIRECTORY_CREATE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(&observed_directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|candidate| {
                        candidate
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".festerm-save-") && name.ends_with(".stage")
                            })
                    })
                    .expect("new staging directory");
                fs::rename(&staging, observed_directory.join("stolen.stage")).unwrap();
                std::os::unix::fs::symlink(&victim_for_hook, &staging).unwrap();
            }));
        });

        let failure = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap_err();

        assert_eq!(failure, SaveFailure::Interrupted);
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        assert_eq!(
            fs::read_to_string(victim.join("payload")).unwrap(),
            "keep\n"
        );
        assert_eq!(
            fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[cfg(unix)]
    #[test]
    fn payload_symlink_substitution_before_verification_is_refused() {
        let directory = TemporaryDirectory::new("payload-verification-substitution");
        let path = directory.file("notes.md", "before\n");
        let loaded = load(&path, bounds()).unwrap();
        let observed_directory = directory.path.clone();
        BEFORE_SAVE_VERIFICATION.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(&observed_directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".festerm-save-") && name.ends_with(".stage")
                            })
                    })
                    .expect("private save staging directory");
                fs::rename(staging.join("payload"), staging.join("displaced")).unwrap();
                std::os::unix::fs::symlink("displaced", staging.join("payload")).unwrap();
            }));
        });

        let failure = save(&path, b"after\n", loaded_expectation(&loaded)).unwrap_err();

        assert_eq!(failure, SaveFailure::Interrupted);
        assert_eq!(fs::read_to_string(&path).unwrap(), "before\n");
        let staging = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|candidate| candidate.is_dir())
            .expect("the substituted staging payload is retained");
        assert_eq!(
            fs::read_to_string(staging.join("displaced")).unwrap(),
            "after\n"
        );
        assert!(!staging.join("payload").exists());
    }

    #[test]
    fn target_replacement_immediately_before_publication_conflicts_without_overwrite() {
        let directory = TemporaryDirectory::new("publication-conflict");
        let path = directory.file("notes.md", "loaded\n");
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        let replacement = directory.path.join("replacement.md");
        BEFORE_SAVE_REPLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::write(&replacement, b"newer\n").unwrap();
                replace_path(&replacement, &target);
            }));
        });

        let failure = save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap_err();

        assert!(matches!(failure, SaveFailure::Conflict(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer\n");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[test]
    fn target_created_after_capture_remains_visible_with_every_version_recoverable() {
        let directory = TemporaryDirectory::new("capture-race");
        let path = directory.file("notes.md", "loaded\n");
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        AFTER_TARGET_CAPTURE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::write(&target, b"winner\n").unwrap();
            }));
        });

        let failure = save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap_err();

        assert_eq!(failure, SaveFailure::RecoveryRequired);
        assert_eq!(fs::read_to_string(&path).unwrap(), "winner\n");
        let staging = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "stage")
            })
            .expect("private recovery directory");
        assert_eq!(
            fs::read_to_string(staging.join("original")).unwrap(),
            "loaded\n"
        );
        #[cfg(windows)]
        for recovery_name in ["original", "displaced"] {
            let recovery = File::open(staging.join(recovery_name)).unwrap();
            assert!(
                festerm_windows_security::is_current_user_only(&recovery).unwrap(),
                "{recovery_name} must be independently current-user-only"
            );
        }
        assert_eq!(
            fs::read_to_string(staging.join("payload")).unwrap(),
            "editor\n"
        );
        assert_eq!(
            fs::read_to_string(staging.join("prepared")).unwrap(),
            "editor\n"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn replacement_after_publication_keeps_the_later_winner_and_recovery_copies() {
        let directory = TemporaryDirectory::new("post-publication-conflict");
        let path = directory.file("notes.md", "loaded\n");
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        let replacement = directory.path.join("replacement.md");
        AFTER_SAVE_REPLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::write(&replacement, b"later\n").unwrap();
                replace_path(&replacement, &target);
            }));
        });

        let failure = save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap_err();

        assert_eq!(failure, SaveFailure::RecoveryRequired);
        assert_eq!(fs::read_to_string(&path).unwrap(), "later\n");
        let staging = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "stage")
            })
            .expect("private recovery directory");
        assert_eq!(
            fs::read_to_string(staging.join("original")).unwrap(),
            "loaded\n"
        );
        assert_eq!(
            fs::read_to_string(staging.join("prepared")).unwrap(),
            "editor\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn published_target_cannot_be_deleted_during_final_verification() {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        let directory = TemporaryDirectory::new("post-publication-lock");
        let path = directory.file("notes.md", "loaded\n");
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        AFTER_SAVE_REPLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let error = fs::remove_file(&target).unwrap_err();
                assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION as i32));
            }));
        });

        save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "editor\n");
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn staging_name_substitution_cannot_redirect_publication_or_cleanup() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("temporary-substitution");
        let path = directory.file("notes.md", "loaded\n");
        let loaded = load(&path, bounds()).unwrap();
        let observed_directory = directory.path.clone();
        let stolen = directory.path.join("stolen.stage");
        let stolen_for_hook = stolen.clone();
        BEFORE_SAVE_REPLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let staging = fs::read_dir(&observed_directory)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with(".festerm-save-") && name.ends_with(".stage")
                            })
                    })
                    .expect("private save staging directory");
                fs::rename(&staging, &stolen_for_hook).unwrap();
                fs::create_dir(&staging).unwrap();
                fs::write(staging.join("payload"), b"attacker\n").unwrap();
            }));
        });

        save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "editor\n");
        assert_eq!(
            fs::metadata(&stolen).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(!stolen.join("payload").exists());
        let substituted = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".festerm-save-"))
            })
            .expect("substituted staging directory");
        assert_eq!(
            fs::read_to_string(substituted.join("payload")).unwrap(),
            "attacker\n"
        );
        fs::remove_dir_all(stolen).unwrap();
        fs::remove_dir_all(substituted).unwrap();
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn security_metadata_change_before_publication_conflicts_without_overwrite() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("security-metadata-conflict");
        let path = directory.file("notes.md", "loaded\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        AFTER_SECURITY_METADATA_COPY.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            }));
        });

        let failure = save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap_err();

        assert!(matches!(failure, SaveFailure::Conflict(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "loaded\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&directory.path).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn security_metadata_change_after_publication_requires_recovery() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("published-security-metadata-conflict");
        let path = directory.file("notes.md", "loaded\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let loaded = load(&path, bounds()).unwrap();
        let target = path.clone();
        AFTER_SAVE_REPLACEMENT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
            }));
        });

        let failure = save(&path, b"editor\n", loaded_expectation(&loaded)).unwrap_err();

        assert_eq!(failure, SaveFailure::RecoveryRequired);
        assert_eq!(fs::read_to_string(&path).unwrap(), "editor\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let staging = fs::read_dir(&directory.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "stage")
            })
            .expect("private recovery directory");
        assert_eq!(
            fs::read_to_string(staging.join("original")).unwrap(),
            "loaded\n"
        );
        assert_eq!(
            fs::read_to_string(staging.join("prepared")).unwrap(),
            "editor\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_as_creates_a_private_new_file() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TemporaryDirectory::new("private-save-as");
        let path = directory.path.join("new.md");
        let destination = absent_destination(&path);

        save(
            &path,
            b"private\n",
            SaveExpectation::Destination(&destination),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "private\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
        let failure = save(&path, b"edited\n", loaded_expectation(&loaded)).unwrap_err();

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
