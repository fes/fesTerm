//! Local, user-selected workflow snapshot intake. This is not an approval.

use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
};

use festerm_automation_js::snapshot::{ApprovedBundle, SourceSnapshot, MAX_SNAPSHOT_BYTES};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", windows))]
const MAX_SELECTED_PATH_BYTES: usize = 4096;

/// Intake failures do not contain the selected path or source text.
#[derive(Debug)]
pub enum IntakeError {
    UnsupportedPlatform,
    InvalidPath(&'static str),
    Open(io::Error),
    NotRegularFile,
    Read(io::Error),
    TooLarge,
    InvalidUtf8 { valid_up_to: usize },
    InvalidJson { line: usize, column: usize },
    InvalidSnapshot,
}

impl fmt::Display for IntakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                formatter.write_str("local workflow bundle intake is unavailable on this platform")
            }
            Self::InvalidPath(reason) => {
                write!(formatter, "invalid selected workflow path: {reason}")
            }
            Self::Open(error) => write!(formatter, "cannot safely open selected workflow: {error}"),
            Self::NotRegularFile => formatter.write_str("selected workflow is not a regular file"),
            Self::Read(error) => write!(formatter, "cannot read selected workflow: {error}"),
            Self::TooLarge => write!(
                formatter,
                "selected workflow JSON exceeds {MAX_SNAPSHOT_BYTES} bytes"
            ),
            Self::InvalidUtf8 { valid_up_to } => write!(
                formatter,
                "selected workflow is not UTF-8 at byte {valid_up_to}"
            ),
            Self::InvalidJson { line, column } => write!(
                formatter,
                "invalid workflow JSON or schema (including duplicate/unknown fields) at line {line}, column {column}"
            ),
            Self::InvalidSnapshot => {
                formatter.write_str("invalid workflow manifest or module graph")
            }
        }
    }
}

impl std::error::Error for IntakeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) | Self::Read(error) => Some(error),
            _ => None,
        }
    }
}

/// Captures a single JSON [`SourceSnapshot`] selected by the local user.
///
/// The caller must obtain the path from an explicit native user selection, not
/// from an MCP request. Opening and parsing may block and belong off the UI
/// thread. The caller must bound concurrent captures and keep each slot until
/// its I/O actually finishes, even if the request was cancelled. This returns
/// validated, frozen source, not a grant or an approval.
/// It does not establish private storage or publisher provenance. On iOS the
/// caller must retain security-scoped access for the entire capture. Display
/// and approve the returned snapshot; never reopen the file to execute.
pub fn load_user_selected_bundle(path: &Path) -> Result<ApprovedBundle, IntakeError> {
    let file = open_selected_file(path)?;
    let metadata = file.metadata().map_err(IntakeError::Open)?;
    if !metadata.is_file() {
        return Err(IntakeError::NotRegularFile);
    }
    if metadata.len() > MAX_SNAPSHOT_BYTES as u64 {
        return Err(IntakeError::TooLarge);
    }
    let bytes = read_bounded(file)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| IntakeError::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let snapshot: SourceSnapshot =
        serde_json::from_str(text).map_err(|error| IntakeError::InvalidJson {
            line: error.line(),
            column: error.column(),
        })?;
    drop(bytes);
    ApprovedBundle::new(snapshot).map_err(|_| IntakeError::InvalidSnapshot)
}

fn read_bounded(source: impl Read) -> Result<Vec<u8>, IntakeError> {
    let mut bytes = Vec::new();
    source
        .take(MAX_SNAPSHOT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(IntakeError::Read)?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(IntakeError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", windows))]
fn selected_components(path: &Path) -> Result<Vec<&std::ffi::OsStr>, IntakeError> {
    use std::path::Component;

    validate_native_path(path)?;
    let mut parts = path.components();
    #[cfg(windows)]
    if !matches!(
        parts.next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_))
    ) {
        return Err(IntakeError::InvalidPath("a local drive root is required"));
    }
    if parts.next() != Some(Component::RootDir) {
        return Err(IntakeError::InvalidPath("an absolute path is required"));
    }
    let names = parts
        .map(|part| match part {
            Component::Normal(name) => Ok(name),
            _ => Err(IntakeError::InvalidPath("path components must be normal")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.is_empty() {
        return Err(IntakeError::InvalidPath("a file name is required"));
    }
    Ok(names)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
fn validate_native_path(path: &Path) -> Result<(), IntakeError> {
    use std::os::unix::ffi::OsStrExt;

    let raw = path.as_os_str().as_bytes();
    if raw.len() > MAX_SELECTED_PATH_BYTES {
        return Err(IntakeError::InvalidPath("the path is too long"));
    }
    let Some(rest) = raw.strip_prefix(b"/") else {
        return Err(IntakeError::InvalidPath("an absolute path is required"));
    };
    if rest
        .split(|byte| *byte == b'/')
        .any(|part| part.is_empty() || part == b"." || part == b".." || part.contains(&0))
    {
        return Err(IntakeError::InvalidPath(
            "empty, dot, traversal and NUL components are not allowed",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_native_path(path: &Path) -> Result<(), IntakeError> {
    let raw = path
        .to_str()
        .ok_or(IntakeError::InvalidPath("the path must be valid Unicode"))?;
    let bytes = raw.as_bytes();
    if bytes.len() > MAX_SELECTED_PATH_BYTES {
        return Err(IntakeError::InvalidPath("the path is too long"));
    }
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return Err(IntakeError::InvalidPath(
            "an absolute local drive path is required",
        ));
    }
    if raw[3..].split(['\\', '/']).any(|part| {
        part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with(['.', ' '])
            || part.contains([':', '*', '?', '"', '<', '>', '|'])
            || part.chars().any(char::is_control)
            || windows_reserved_name(part)
    }) {
        return Err(IntakeError::InvalidPath(
            "unsupported, ambiguous or traversal path component",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_reserved_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) || ["COM", "LPT"].into_iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
fn open_native_root(_path: &Path) -> Result<File, IntakeError> {
    File::open("/").map_err(IntakeError::Open)
}

#[cfg(windows)]
fn open_native_root(path: &Path) -> Result<File, IntakeError> {
    use std::path::{Component, Prefix};

    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return Err(IntakeError::InvalidPath("a local drive root is required"));
    };
    let Prefix::Disk(letter) = prefix.kind() else {
        return Err(IntakeError::InvalidPath("a local drive root is required"));
    };
    let root = format!("{}:\\", char::from(letter));
    let root_path = Path::new(&root);
    festerm_windows_security::require_local_disk_root(root_path).map_err(IntakeError::Open)?;
    let directory = festerm_windows_security::lock_directory_path_without_delete_sharing(root_path)
        .map_err(IntakeError::Open)?;
    let opened =
        festerm_windows_security::opened_file_path(&directory).map_err(IntakeError::Open)?;
    // A substituted drive can otherwise hide traversal before this anchor.
    let expected = format!(r"\\?\{root}");
    if !opened
        .to_str()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
    {
        return Err(IntakeError::InvalidPath(
            "the drive does not resolve to its own volume root",
        ));
    }
    Ok(directory)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", windows))]
fn open_selected_file(path: &Path) -> Result<File, IntakeError> {
    let names = selected_components(path)?;
    let (name, directories) = names
        .split_last()
        .ok_or(IntakeError::InvalidPath("a file name is required"))?;
    let mut parent = open_native_root(path)?;
    for directory in directories {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
        let opened =
            festerm_unix_security::open_directory_search_nofollow(&parent, Path::new(*directory));
        #[cfg(windows)]
        let opened =
            festerm_windows_security::open_directory_no_reparse(&parent, Path::new(*directory));
        parent = opened.map_err(IntakeError::Open)?;
    }
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
    let file = festerm_unix_security::open_file_nofollow(&parent, Path::new(*name));
    #[cfg(windows)]
    let file = festerm_windows_security::open_file_no_reparse(&parent, Path::new(*name));
    file.map_err(IntakeError::Open)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
fn open_selected_file(_path: &Path) -> Result<File, IntakeError> {
    Err(IntakeError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_intake_never_reads_more_than_the_limit_plus_one() {
        struct Endless {
            read: usize,
        }

        impl Read for Endless {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                buffer.fill(b'x');
                self.read += buffer.len();
                Ok(buffer.len())
            }
        }

        let mut source = Endless { read: 0 };
        assert!(matches!(
            read_bounded(&mut source),
            Err(IntakeError::TooLarge)
        ));
        assert_eq!(source.read, MAX_SNAPSHOT_BYTES + 1);

        struct FailedRead;
        impl Read for FailedRead {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        }
        assert!(matches!(
            read_bounded(FailedRead),
            Err(IntakeError::Read(_))
        ));
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", windows)))]
    #[test]
    fn bundle_intake_fails_closed_on_unsupported_platforms() {
        assert!(matches!(
            load_user_selected_bundle(Path::new("/workflow.json")),
            Err(IntakeError::UnsupportedPlatform)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios", windows))]
    mod supported {
        use super::*;
        use festerm_automation::Scope;
        use std::{
            fs,
            path::PathBuf,
            sync::atomic::{AtomicU64, Ordering},
        };

        const VALID_JSON: &str = r#"{"manifest":{"version":1,"apiVersion":1,"name":"local","entry":"main.js","modules":["helper.js","main.js"],"parameters":{},"capabilities":[]},"modules":{"helper.js":"export const n = 1;","main.js":"import { n } from 'helper.js'; export function main() { return n; }"}}"#;

        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

        struct FixtureDirectory(PathBuf);

        impl FixtureDirectory {
            fn new() -> Self {
                let base = std::env::current_dir().unwrap();
                #[cfg(unix)]
                let base = base.canonicalize().unwrap();
                let directory = base.join(format!(
                    ".bundle-intake-test-{}-{}",
                    std::process::id(),
                    NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&directory).unwrap();
                Self(directory)
            }

            fn path(&self, name: &str) -> PathBuf {
                self.0.join(name)
            }
        }

        impl Drop for FixtureDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        fn invocation(bundle: ApprovedBundle) -> crate::FrozenInvocation {
            crate::FrozenInvocation::new(
                bundle,
                Default::default(),
                std::iter::empty(),
                Scope {
                    invoke_workflows: true,
                    ..Scope::default()
                },
            )
            .unwrap()
        }

        #[test]
        fn selected_bundle_and_invocation_remain_frozen_after_file_changes() {
            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            fs::write(&path, VALID_JSON).unwrap();
            let frozen = invocation(load_user_selected_bundle(&path).unwrap());
            let binding = frozen.binding();
            let digest = frozen.bundle().digest().to_owned();

            let changed = VALID_JSON.replace("export const n = 1;", "export const n = 2;");
            fs::write(&path, changed).unwrap();
            let newer = invocation(load_user_selected_bundle(&path).unwrap());
            assert_ne!(digest.as_str(), newer.bundle().digest());
            assert_ne!(binding.invocation, newer.binding().invocation);
            assert_eq!(frozen.binding(), binding);
            assert_eq!(frozen.bundle().digest(), digest.as_str());
            assert_eq!(
                frozen.bundle().snapshot().modules["helper.js"],
                "export const n = 1;"
            );
        }

        #[test]
        fn selected_file_handle_survives_path_replacement() {
            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            fs::write(&path, VALID_JSON).unwrap();
            let opened = open_selected_file(&path).unwrap();
            fs::rename(&path, fixture.path("original.json")).unwrap();
            fs::write(&path, "replacement").unwrap();

            assert_eq!(read_bounded(opened).unwrap(), VALID_JSON.as_bytes());
            assert!(matches!(
                load_user_selected_bundle(&path),
                Err(IntakeError::InvalidJson { .. })
            ));
        }

        #[test]
        fn bundle_intake_hashes_exact_decoded_source_not_json_spelling() {
            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            let escaped = VALID_JSON.replace("export const n = 1;", r"export const n = '\u00e9';");
            fs::write(&path, escaped).unwrap();
            let first = load_user_selected_bundle(&path).unwrap();
            let literal = VALID_JSON.replace("export const n = 1;", "export const n = 'é';");
            fs::write(&path, literal).unwrap();
            let second = load_user_selected_bundle(&path).unwrap();
            assert_eq!(first.digest(), second.digest());
            assert_eq!(
                first.snapshot().modules["helper.js"],
                "export const n = 'é';"
            );
            let decomposed =
                VALID_JSON.replace("export const n = 1;", "export const n = 'e\u{301}';");
            fs::write(&path, decomposed).unwrap();
            let third = load_user_selected_bundle(&path).unwrap();
            assert_ne!(first.digest(), third.digest());
        }

        #[test]
        fn bundle_intake_enforces_the_encoded_size_limit() {
            use festerm_automation_js::snapshot::MAX_SOURCE_BYTES;

            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            let mut bytes = VALID_JSON.as_bytes().to_vec();
            bytes.resize(MAX_SNAPSHOT_BYTES, b' ');
            fs::write(&path, &bytes).unwrap();
            assert!(load_user_selected_bundle(&path).is_ok());

            bytes.push(b' ');
            fs::write(&path, bytes).unwrap();
            assert!(matches!(
                load_user_selected_bundle(&path),
                Err(IntakeError::TooLarge)
            ));

            let oversized_source =
                VALID_JSON.replace("export const n = 1;", &"x".repeat(MAX_SOURCE_BYTES + 1));
            fs::write(&path, oversized_source).unwrap();
            assert!(matches!(
                load_user_selected_bundle(&path),
                Err(IntakeError::InvalidSnapshot)
            ));
        }

        #[test]
        fn bundle_intake_rejects_malformed_duplicate_and_invalid_snapshots() {
            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            fs::write(&path, [0xff]).unwrap();
            assert!(matches!(
                load_user_selected_bundle(&path),
                Err(IntakeError::InvalidUtf8 { valid_up_to: 0 })
            ));

            let bad_json = [
                "{".to_owned(),
                format!("{VALID_JSON} true"),
                VALID_JSON.replacen(r#""version":1"#, r#""version":1,"version":1"#, 1),
                VALID_JSON.replacen(
                    r#""modules":{"helper.js":"#,
                    r#""modules":{},"modules":{"helper.js":"#,
                    1,
                ),
                VALID_JSON.replacen(
                    r#""helper.js":"export const n = 1;""#,
                    r#""helper.js":"export const n = 1;","helper.js":"export const n = 2;""#,
                    1,
                ),
                VALID_JSON.replace(
                    r#""parameters":{}"#,
                    r#""parameters":{"mode":{"type":"boolean"},"mode":{"type":"boolean"}}"#,
                ),
                VALID_JSON.replacen(
                    r#""manifest":"#,
                    r#""private-source-marker":"secret-value","manifest":"#,
                    1,
                ),
            ];
            for contents in bad_json {
                assert_ne!(contents, VALID_JSON);
                fs::write(&path, contents).unwrap();
                let error = load_user_selected_bundle(&path).unwrap_err();
                assert!(
                    matches!(&error, IntakeError::InvalidJson { .. }),
                    "{error:?}"
                );
                let diagnostic = format!("{error:?} {error}");
                assert!(!diagnostic.contains("private-source-marker"));
                assert!(!diagnostic.contains("secret-value"));
            }

            let invalid_snapshots = [
                VALID_JSON.replace(
                    r#""modules":["helper.js","main.js"]"#,
                    r#""modules":["helper.js","main.js","main.js"]"#,
                ),
                VALID_JSON.replace(r#""entry":"main.js""#, r#""entry":"missing.js""#),
                VALID_JSON.replace("from 'helper.js'", "from '../helper.js'"),
                VALID_JSON
                    .replace(
                        r#""modules":["helper.js","main.js"]"#,
                        r#""modules":["extra.js","helper.js","main.js"]"#,
                    )
                    .replacen(
                        r#""modules":{"helper.js":"#,
                        r#""modules":{"extra.js":"export const x = 1;","helper.js":"#,
                        1,
                    ),
            ];
            for contents in invalid_snapshots {
                assert_ne!(contents, VALID_JSON);
                fs::write(&path, contents).unwrap();
                assert!(matches!(
                    load_user_selected_bundle(&path),
                    Err(IntakeError::InvalidSnapshot)
                ));
            }
        }

        #[test]
        fn bundle_intake_rejects_traversal_aliases_and_nonregular_files() {
            let fixture = FixtureDirectory::new();
            let path = fixture.path("workflow.json");
            fs::write(&path, VALID_JSON).unwrap();
            let mut invalid_paths = vec![
                PathBuf::from("workflow.json"),
                fixture.path("missing").join("..").join("workflow.json"),
                fixture.0.join(".").join("workflow.json"),
                fixture.path(&"x".repeat(MAX_SELECTED_PATH_BYTES)),
            ];
            #[cfg(unix)]
            {
                let mut doubled = fixture.0.as_os_str().to_os_string();
                doubled.push("//workflow.json");
                invalid_paths.push(PathBuf::from(doubled));
                let mut nul = fixture.0.as_os_str().to_os_string();
                nul.push("/bad\0name.json");
                invalid_paths.push(PathBuf::from(nul));
            }
            #[cfg(windows)]
            invalid_paths.push(fixture.path("bad\0name.json"));
            for invalid in invalid_paths {
                assert!(
                    matches!(
                        load_user_selected_bundle(&invalid),
                        Err(IntakeError::InvalidPath(_))
                    ),
                    "{invalid:?}"
                );
            }
            assert!(matches!(
                load_user_selected_bundle(&fixture.0),
                Err(IntakeError::Open(_) | IntakeError::NotRegularFile)
            ));
            assert!(matches!(
                load_user_selected_bundle(&fixture.path("missing.json")),
                Err(IntakeError::Open(_))
            ));

            #[cfg(unix)]
            {
                let socket = fixture.path("socket");
                let relative_socket = Path::new(fixture.0.file_name().unwrap()).join("socket");
                let _listener = std::os::unix::net::UnixListener::bind(relative_socket).unwrap();
                assert!(matches!(
                    load_user_selected_bundle(&socket),
                    Err(IntakeError::Open(_) | IntakeError::NotRegularFile)
                ));
            }
        }

        #[test]
        fn bundle_intake_rejects_file_and_ancestor_links() {
            let fixture = FixtureDirectory::new();
            let real_directory = fixture.path("real");
            fs::create_dir(&real_directory).unwrap();
            let real_file = real_directory.join("workflow.json");
            fs::write(&real_file, VALID_JSON).unwrap();
            let file_link = fixture.path("file-link.json");
            let directory_link = fixture.path("directory-link");

            #[cfg(unix)]
            let file_result = std::os::unix::fs::symlink(&real_file, &file_link);
            #[cfg(windows)]
            let file_result = std::os::windows::fs::symlink_file(&real_file, &file_link);
            #[cfg(windows)]
            if file_result
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(1314))
            {
                eprintln!("Windows symlink test deferred: symlink privilege is unavailable");
                return;
            }
            file_result.unwrap();
            assert!(matches!(
                load_user_selected_bundle(&file_link),
                Err(IntakeError::Open(_))
            ));

            #[cfg(unix)]
            std::os::unix::fs::symlink(&real_directory, &directory_link).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&real_directory, &directory_link).unwrap();
            assert!(matches!(
                load_user_selected_bundle(&directory_link.join("workflow.json")),
                Err(IntakeError::Open(_))
            ));
        }

        #[cfg(target_os = "ios")]
        #[test]
        fn bundle_intake_keeps_ios_private_storage_checks_closed() {
            use std::os::unix::fs::MetadataExt;

            let fixture = FixtureDirectory::new();
            let file_path = fixture.path("existing");
            fs::write(&file_path, "content").unwrap();
            let directory = fs::File::open(&fixture.0).unwrap();
            let file = fs::File::open(&file_path).unwrap();
            let directory_mode = directory.metadata().unwrap().mode();
            let file_mode = file.metadata().unwrap().mode();
            let errors = [
                festerm_unix_security::security_metadata(&file).unwrap_err(),
                festerm_unix_security::secure_staging_parent(&directory).unwrap_err(),
                festerm_unix_security::is_current_user_only(&file, false).unwrap_err(),
                festerm_unix_security::make_private(&file).unwrap_err(),
                festerm_unix_security::make_private_directory(&directory).unwrap_err(),
                festerm_unix_security::create_private_directory(&directory, Path::new("new-dir"))
                    .unwrap_err(),
                festerm_unix_security::open_private_file(&directory, Path::new("new-file"), true)
                    .unwrap_err(),
                festerm_unix_security::replace_private_file(
                    &directory,
                    Path::new("existing"),
                    Path::new("replacement"),
                )
                .unwrap_err(),
                festerm_unix_security::remove_private_staging_file(
                    &directory,
                    Path::new("existing"),
                )
                .unwrap_err(),
            ];
            assert!(errors
                .iter()
                .all(|error| error.kind() == io::ErrorKind::Unsupported));
            assert_eq!(directory.metadata().unwrap().mode(), directory_mode);
            assert_eq!(file.metadata().unwrap().mode(), file_mode);
            assert!(!fixture.path("new-dir").exists());
            assert!(!fixture.path("new-file").exists());
            assert!(file_path.exists());
            assert!(!fixture.path("replacement").exists());
        }

        #[cfg(windows)]
        #[test]
        fn bundle_intake_rejects_windows_device_network_and_stream_paths() {
            for path in [
                r"C:relative.json",
                r"\rooted.json",
                r"\\server\share\workflow.json",
                r"\\?\C:\workflow.json",
                r"\\.\pipe\workflow.json",
                r"C:\folder\..\workflow.json",
                r"C:\folder\.\workflow.json",
                r"C:\folder\\workflow.json",
                r"C:\folder\workflow.json:stream",
                r"C:\folder\NUL.json",
                r"C:\folder\COM¹.json",
                r"C:\folder\trailing. ",
            ] {
                assert!(
                    matches!(
                        load_user_selected_bundle(Path::new(path)),
                        Err(IntakeError::InvalidPath(_))
                    ),
                    "{path}"
                );
            }
        }
    }
}
