use super::*;
use crate::imp::{
    create_current_user_only_directory, create_current_user_only_file, delete_directory_by_handle,
    open_absolute_data_directory, open_contained_data_directory, remove_reviewed_empty_directory,
    remove_reviewed_file,
};
use std::{
    os::windows::fs::{symlink_dir, symlink_file},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::{
    Wdk::Storage::FileSystem::FILE_DIRECTORY_FILE,
    Win32::{
        Foundation::{
            ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PRIVILEGE_NOT_HELD,
            ERROR_SHARING_VIOLATION, GENERIC_WRITE,
        },
        Storage::FileSystem::{DELETE, FILE_WRITE_ATTRIBUTES},
        System::Ioctl::FSCTL_SET_REPARSE_POINT,
    },
};

const ALL_SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const JUNCTION: &str = "junction";

fn make_payload(tag: u32, flags: u32, substitute: &str, print: &str) -> Vec<u8> {
    let substitute: Vec<u8> = substitute
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let print: Vec<u8> = print.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let header_len = if tag == IO_REPARSE_TAG_SYMLINK { 12 } else { 8 };
    let data_len = header_len + substitute.len() + print.len() + 4;
    let mut payload = Vec::new();
    payload.extend(tag.to_le_bytes());
    payload.extend(u16::try_from(data_len).unwrap().to_le_bytes());
    payload.extend([0; 4]); // Reserved and substitute offset.
    payload.extend(u16::try_from(substitute.len()).unwrap().to_le_bytes());
    payload.extend(u16::try_from(substitute.len() + 2).unwrap().to_le_bytes());
    payload.extend(u16::try_from(print.len()).unwrap().to_le_bytes());
    if tag == IO_REPARSE_TAG_SYMLINK {
        payload.extend(flags.to_le_bytes());
    }
    payload.extend(substitute);
    payload.extend([0; 2]);
    payload.extend(print);
    payload.extend([0; 2]);
    payload
}

#[test]
fn parses_only_the_recognized_substitute_path() {
    let absolute = r"C:\fixture\target";
    for tag in [IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK] {
        let bytes = make_payload(tag, 0, &format!(r"\??\{absolute}"), r"\\elsewhere\print");
        assert!(
            matches!(parse_payload(&bytes).unwrap(), WindowsLinkTarget::AbsoluteDos(path) if path == Path::new(absolute))
        );
    }
    let bytes = make_payload(IO_REPARSE_TAG_SYMLINK, 1, r"..\target\é", "");
    assert!(
        matches!(parse_payload(&bytes).unwrap(), WindowsLinkTarget::ParentRelative(path) if path == Path::new(r"..\target\é"))
    );
}

#[test]
fn rejects_malformed_payloads_and_ambiguous_names() {
    let tag = IO_REPARSE_TAG_SYMLINK;
    for name in [
        r"\??\UNC\host\share",
        r"\??\GLOBALROOT\Device",
        r"\??\Volume{x}\target",
        r"\??\C:target",
        r"\Device\disk",
        r"C:\target",
        r"\??\C:\root\..\target",
        r"\??\C:\target:stream",
        "",
    ] {
        assert!(parse_payload(&make_payload(tag, 0, name, "")).is_err());
    }
    for name in [
        r"\rooted",
        r"C:relative",
        "with/slash",
        r"repeat\\separator",
        "dot.",
        "space ",
        "COM¹.txt",
        "NUL .txt",
        "has\0nul",
    ] {
        assert!(parse_payload(&make_payload(tag, 1, name, "")).is_err());
    }
    let long = vec!["€".repeat(200); 7].join("\\");
    assert!(parse_payload(&make_payload(tag, 1, &long, "")).is_err());
    assert!(parse_payload(&make_payload(tag, 2, "target", "")).is_err());
    assert!(parse_payload(&make_payload(IO_REPARSE_TAG_MOUNT_POINT, 0, "target", "")).is_err());
    assert!(parse_payload(&make_payload(0x9000_001a, 0, "target", "")).is_err()); // Cloud tag.

    let valid = make_payload(tag, 1, "target", "print");
    for end in 0..valid.len() {
        assert!(parse_payload(&valid[..end]).is_err());
    }
    for (index, value) in [
        (6, 1),
        (7, 1),
        (8, 1),
        (10, 1),
        (12, 0),
        (14, 0xfe),
        (21, 0xd8),
    ] {
        let mut malformed = valid.clone();
        malformed[index] = value;
        assert!(parse_payload(&malformed).is_err());
    }
}

fn set_payload(file: &File, payload: &[u8]) -> io::Result<()> {
    let mut returned = 0;
    // SAFETY: the synchronous handle identifies only the private fixture
    // junction. The complete test buffer and return count live across the call.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_SET_REPARSE_POINT,
            payload.as_ptr().cast(),
            payload.len() as u32,
            ptr::null_mut(),
            0,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct FixtureRoot {
    temp: File,
    _ancestors: Vec<File>,
    name: PathBuf,
    path: PathBuf,
    identity: (u32, u64),
}

impl FixtureRoot {
    fn create() -> (Self, File) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = PathBuf::from(format!(
            "fes-retained-link-{}-{ticks}-{sequence}",
            std::process::id()
        ));
        let temp_path = std::env::temp_dir();
        let (temp, ancestors) = open_absolute_data_directory(&temp_path).unwrap();
        let root = create_current_user_only_directory(&temp, &name).unwrap();
        let fixture = Self {
            temp,
            _ancestors: ancestors,
            path: temp_path.join(&name),
            name,
            identity: directory_identity(&root).unwrap(),
        };
        (fixture, root)
    }

    fn pin(&self) -> File {
        let (root, _) = open_contained_data_directory(&self.temp, &self.name).unwrap();
        assert_eq!(file_identity(&root).unwrap(), self.identity);
        root
    }

    fn open_for_cleanup(&self) -> io::Result<File> {
        let root = open_relative(
            &self.temp,
            &self.name,
            FILE_READ_ATTRIBUTES | DELETE,
            ALL_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            ptr::null(),
        )?;
        if directory_identity(&root)? != self.identity {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(root)
    }
}

struct JunctionFixture {
    root: FixtureRoot,
    paths: [PathBuf; 2],
    payloads: [Vec<u8>; 2],
    targets: Vec<File>,
    junction_id: Option<(u32, u64)>,
    initialized: bool,
}

impl JunctionFixture {
    fn new() -> Self {
        let (owned_root, root) = FixtureRoot::create();
        let paths = ["target-a", "target-b"].map(|target| owned_root.path.join(target));
        let mut fixture = Self {
            root: owned_root,
            paths,
            payloads: Default::default(),
            targets: Vec::new(),
            junction_id: None,
            initialized: false,
        };
        fixture.payloads = fixture.paths.each_ref().map(|path| {
            let target = path.to_str().unwrap();
            make_payload(
                IO_REPARSE_TAG_MOUNT_POINT,
                0,
                &format!(r"\??\{target}"),
                target,
            )
        });
        for name in ["target-a", "target-b"] {
            fixture
                .targets
                .push(create_current_user_only_directory(&root, Path::new(name)).unwrap());
        }
        let junction = create_current_user_only_directory(&root, Path::new(JUNCTION)).unwrap();
        fixture.junction_id = Some(file_identity(&junction).unwrap());
        set_payload(&junction, &fixture.payloads[0]).unwrap();
        fixture.initialized = true;
        fixture
    }

    fn clean(&mut self) -> io::Result<()> {
        let root = self.root.open_for_cleanup()?;
        if let Some(saved) = self.junction_id {
            let link = self.open_junction(&root, DELETE)?;
            if file_identity(&link)? != saved {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            if self.initialized {
                let payload = read_payload(&link)?;
                if !self
                    .payloads
                    .iter()
                    .any(|known| known.as_slice() == payload.as_ref())
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
            } else {
                directory_identity(&link)?;
            }
            delete_directory_by_handle(&link)?;
        }
        for target in self.targets.drain(..) {
            directory_identity(&target)?;
            delete_directory_by_handle(&target)?;
        }
        delete_directory_by_handle(&root)
    }

    fn open_junction(&self, root: &File, access: u32) -> io::Result<File> {
        open_relative(
            root,
            Path::new(JUNCTION),
            FILE_READ_ATTRIBUTES | access,
            ALL_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            ptr::null(),
        )
    }

    fn assert_target(&self, snapshot: &RetainedDataLink, target: usize) {
        assert!(
            matches!(snapshot.target(), WindowsLinkTarget::AbsoluteDos(path) if path == &self.paths[target])
        );
    }

    fn current_target(&self, junction: &File) -> usize {
        let payload = read_payload(junction).unwrap();
        self.payloads
            .iter()
            .position(|known| known.as_slice() == payload.as_ref())
            .expect("expected an owned target")
    }

    fn assert_pinned_edit(&self, case: &str, writer: &File, snapshot: &RetainedDataLink) {
        assert_eq!(file_identity(writer).unwrap(), self.junction_id.unwrap());
        match set_payload(writer, &self.payloads[1]) {
            Ok(()) => {
                eprintln!(
                    "{case}: attribute-only edit returned 0 and changed the retained junction"
                );
                assert_eq!(self.current_target(writer), 1);
                assert_eq!(
                    snapshot.revalidate().unwrap_err().kind(),
                    io::ErrorKind::PermissionDenied
                );
                self.assert_target(snapshot, 0);
            }
            Err(error) => {
                eprintln!(
                    "{case}: attribute-only edit returned {:?}",
                    error.raw_os_error()
                );
                assert!(attribute_edit_denied(&error));
                snapshot.revalidate().unwrap();
            }
        }
    }
}

impl Drop for JunctionFixture {
    fn drop(&mut self) {
        if let Err(error) = self.clean() {
            if std::thread::panicking() {
                eprintln!("retained junction fixture cleanup failed: {error}");
            } else {
                panic!("retained junction fixture cleanup failed: {error}");
            }
        }
    }
}

fn assert_sharing_violation<T>(case: &str, opened: io::Result<T>) {
    let code = opened
        .err()
        .expect("restricted open succeeded")
        .raw_os_error();
    eprintln!("{case}: open returned {code:?}");
    assert_eq!(code, Some(ERROR_SHARING_VIOLATION as i32));
}

fn attribute_edit_denied(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|n| n as u32),
        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
    )
}

#[test]
fn retained_ntfs_junction_extracts_and_revalidates_with_live_writers() {
    let fixture = JunctionFixture::new();
    let root = fixture.root.pin();
    let name = Path::new(JUNCTION);
    for bad in [
        "junction:stream",
        "junction.",
        "junction ",
        "junction\\",
        "junction/.",
    ] {
        assert_eq!(
            RetainedDataLink::open(&root, Path::new(bad))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    // The fixture root is an ordinary directory held with compatible read sharing.
    let plain = RetainedDataLink::open(&fixture.root.temp, &fixture.root.name)
        .err()
        .expect("plain fixture directory produced a link snapshot");
    assert_eq!(plain.kind(), io::ErrorKind::Unsupported, "{plain:?}");
    assert_eq!(plain.raw_os_error(), None, "{plain:?}");

    let snapshot = RetainedDataLink::open(&root, name).unwrap();
    fixture.assert_target(&snapshot, 0);
    drop(root);
    assert_sharing_violation(
        "parent DELETE access with the snapshot retained",
        open_relative(
            &fixture.root.temp,
            &fixture.root.name,
            DELETE,
            ALL_SHARE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
            ptr::null(),
        ),
    );
    snapshot.revalidate().unwrap();
    let root = fixture.root.pin();
    // Rename requires DELETE access.
    assert_sharing_violation(
        "replacement DELETE access",
        fixture.open_junction(&root, DELETE),
    );
    assert_sharing_violation(
        "subsequent broad writer",
        fixture.open_junction(&root, GENERIC_WRITE),
    );
    match fixture.open_junction(&root, FILE_WRITE_ATTRIBUTES) {
        Ok(writer) => fixture.assert_pinned_edit("subsequent writer", &writer, &snapshot),
        Err(error) => {
            assert_sharing_violation::<File>("subsequent attribute-only writer", Err(error));
            snapshot.revalidate().unwrap();
        }
    }
    drop(snapshot);

    let broad = fixture.open_junction(&root, GENERIC_WRITE).unwrap();
    assert_sharing_violation(
        "pin with pre-existing broad writer",
        RetainedDataLink::open(&root, name),
    );
    set_payload(&broad, &fixture.payloads[0]).unwrap();
    drop(broad);

    let writer = fixture.open_junction(&root, FILE_WRITE_ATTRIBUTES).unwrap();
    match RetainedDataLink::open(&root, name) {
        Ok(snapshot) => fixture.assert_pinned_edit("pre-existing writer", &writer, &snapshot),
        Err(error) => {
            assert_sharing_violation::<RetainedDataLink>(
                "pin with pre-existing attribute-only writer",
                Err(error),
            );
        }
    }
    let before = fixture.current_target(&writer);
    match set_payload(&writer, &fixture.payloads[1 - before]) {
        Ok(()) => {
            eprintln!("released pin: attribute-only edit returned 0");
            assert_eq!(fixture.current_target(&writer), 1 - before);
        }
        Err(error) => {
            eprintln!(
                "released pin: attribute-only edit returned {:?}",
                error.raw_os_error()
            );
            assert!(attribute_edit_denied(&error));
            assert_eq!(fixture.current_target(&writer), before);
        }
    }
    drop(writer);
    let broad = fixture.open_junction(&root, GENERIC_WRITE).unwrap();
    let next = 1 - fixture.current_target(&broad);
    set_payload(&broad, &fixture.payloads[next]).unwrap();
    eprintln!("released pin: broad-writer edit returned 0");
    drop(broad);
    let fresh = RetainedDataLink::open(&root, name).unwrap();
    fixture.assert_target(&fresh, next);
    fresh.revalidate().unwrap();
}

const TARGET_FILE: &str = "target-file";
const TARGET_DIR: &str = "target-dir";
const FILE_LINK: &str = "relative-file-link";
const DIR_LINK: &str = "absolute-directory-link";

struct OwnedSymlink {
    name: &'static str,
    identity: (u32, u64),
    directory: bool,
    payload: Box<[u8]>,
    disposed: bool,
}

fn open_fixture_leaf(root: &File, name: &str, access: u32) -> io::Result<File> {
    open_relative(
        root,
        Path::new(name),
        FILE_READ_ATTRIBUTES | access,
        ALL_SHARE,
        FILE_OPEN,
        0,
        ptr::null(),
    )
}

struct SymlinkFixture {
    root: FixtureRoot,
    file: Option<File>,
    directory: Option<File>,
    links: Vec<OwnedSymlink>,
}

impl SymlinkFixture {
    fn new() -> Self {
        let (owned_root, root) = FixtureRoot::create();
        let mut fixture = Self {
            root: owned_root,
            file: None,
            directory: None,
            links: Vec::new(),
        };
        fixture.file = Some(create_current_user_only_file(&root, Path::new(TARGET_FILE)).unwrap());
        fixture.directory =
            Some(create_current_user_only_directory(&root, Path::new(TARGET_DIR)).unwrap());
        fixture
    }

    fn record_link(&mut self, root: &File, name: &'static str, directory: bool) {
        let link = open_fixture_leaf(root, name, 0).unwrap();
        let payload = read_payload(&link).unwrap();
        assert!(payload.starts_with(&IO_REPARSE_TAG_SYMLINK.to_le_bytes()));
        assert_eq!(
            file_information(&link).unwrap().dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            directory
        );
        // The fixed symbolic-link header carries tag, lengths, offsets and flags.
        eprintln!(
            "{name} native header {:?}, bytes {}",
            &payload[..payload.len().min(20)],
            payload.len()
        );
        self.links.push(OwnedSymlink {
            name,
            identity: file_identity(&link).unwrap(),
            directory,
            payload,
            disposed: false,
        });
    }

    fn remove_links(&mut self, root: &File) -> io::Result<()> {
        while let Some(saved) = self.links.last_mut() {
            if !saved.disposed {
                let link = open_fixture_leaf(root, saved.name, DELETE)?;
                let directory =
                    file_information(&link)?.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                if file_identity(&link)? != saved.identity
                    || directory != saved.directory
                    || read_payload(&link)? != saved.payload
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                // Remember a successful disposition even if absence verification
                // fails. A retry may only verify absence, never delete a reused name.
                delete_directory_by_handle(&link)?;
                saved.disposed = true;
                drop(link);
            }
            match open_fixture_leaf(root, saved.name, 0) {
                Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {}
                Err(error) => return Err(error),
                Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            }
            eprintln!("{} released pin allowed exact alias removal", saved.name);
            self.links.pop();
        }
        Ok(())
    }

    fn assert_originals(&self, root: &File) {
        // Verify the originals directly by metadata. This never opens through an alias.
        let file = open_fixture_leaf(root, TARGET_FILE, 0).unwrap();
        assert_eq!(
            file_identity(&file).unwrap(),
            file_identity(self.file.as_ref().unwrap()).unwrap()
        );
        let info = file_information(&file).unwrap();
        assert_eq!(
            info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT),
            0
        );
        assert_eq!((info.nFileSizeHigh, info.nFileSizeLow), (0, 0));
        let directory = open_fixture_leaf(root, TARGET_DIR, 0).unwrap();
        assert_eq!(
            directory_identity(&directory).unwrap(),
            directory_identity(self.directory.as_ref().unwrap()).unwrap()
        );
    }

    fn clean(&mut self) -> io::Result<()> {
        let root = self.root.open_for_cleanup()?;
        self.remove_links(&root)?;
        if let Some(file) = self.file.take() {
            remove_reviewed_file(&file)?;
        }
        if let Some(directory) = self.directory.take() {
            remove_reviewed_empty_directory(&directory)?;
        }
        delete_directory_by_handle(&root)
    }
}

impl Drop for SymlinkFixture {
    fn drop(&mut self) {
        if let Err(error) = self.clean() {
            if std::thread::panicking() {
                eprintln!("retained symlink fixture cleanup failed: {error}");
            } else {
                panic!("retained symlink fixture cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn retained_ntfs_symlinks_extract_relative_file_and_absolute_directory() {
    let mut fixture = SymlinkFixture::new();
    let root = fixture.root.pin();
    if let Err(error) = symlink_file(TARGET_FILE, fixture.root.path.join(FILE_LINK)) {
        if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32)
            && std::env::var_os("FESTERM_REQUIRE_WINDOWS_SYMLINKS")
                .is_none_or(|value| value.is_empty())
        {
            eprintln!("Native Windows symlink qualification deferred; creation returned 1314");
            return;
        }
        panic!("Native relative file symlink creation failed: {error:?}");
    }
    fixture.record_link(&root, FILE_LINK, false);
    let absolute = fixture.root.path.join(TARGET_DIR);
    symlink_dir(&absolute, fixture.root.path.join(DIR_LINK)).unwrap();
    fixture.record_link(&root, DIR_LINK, true);

    let expected = [
        (
            WindowsLinkTarget::ParentRelative(TARGET_FILE.into()),
            fixture.file.as_ref().unwrap(),
        ),
        (
            WindowsLinkTarget::AbsoluteDos(absolute),
            fixture.directory.as_ref().unwrap(),
        ),
    ];
    assert_eq!(fixture.links.len(), expected.len());
    fixture.assert_originals(&root);
    for (saved, (target, original)) in fixture.links.iter().zip(expected) {
        let snapshot = RetainedDataLink::open(&root, Path::new(saved.name)).unwrap();
        assert_eq!(file_identity(&snapshot.link).unwrap(), saved.identity);
        assert_ne!(saved.identity, file_identity(original).unwrap());
        assert!(
            snapshot.target() == &target,
            "{} substitute changed",
            saved.name
        );
        snapshot.revalidate().unwrap();
        assert_sharing_violation(saved.name, open_fixture_leaf(&root, saved.name, DELETE));
        snapshot.revalidate().unwrap();
    }
    fixture.remove_links(&root).unwrap();
    fixture.assert_originals(&root);
}
