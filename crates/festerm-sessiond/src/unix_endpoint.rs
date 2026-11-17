//! Generation-bound Unix socket addressing. Durable state stays in runtime_root.
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

fn invalid() -> io::Error {
    io::ErrorKind::InvalidInput.into()
}

fn original(root: &Path, generation: &str) -> io::Result<PathBuf> {
    let valid = generation.split_once('-').is_some_and(|(pid, time)| {
        pid.parse::<u32>()
            .ok()
            .is_some_and(|value| value != 0 && value.to_string() == pid)
            && time
                .parse::<u128>()
                .ok()
                .is_some_and(|value| value != 0 && value.to_string() == time)
    });
    if !valid || root.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(invalid());
    }
    Ok(root.join(format!("{generation}.sock")))
}

#[cfg(any(target_os = "macos", test))]
fn short_path(root: &Path, generation: &str, cache: &Path) -> io::Result<PathBuf> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    original(root, generation)?;
    if !root.is_absolute() || !cache.is_absolute() {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    hash.update(b"fesTerm.sessiond.unix-endpoint.v1\0");
    for bytes in [root.as_os_str().as_encoded_bytes(), generation.as_bytes()] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    let name = format!(
        "f{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.finalize())
    );
    let path = cache.join(name).join("s");
    std::os::unix::net::SocketAddr::from_pathname(&path)?;
    Ok(path)
}

fn address(root: &Path, generation: &str) -> io::Result<(PathBuf, Option<PathBuf>)> {
    let path = original(root, generation)?;
    match std::os::unix::net::SocketAddr::from_pathname(&path) {
        Ok(_) => Ok((path, None)),
        Err(error) => {
            #[cfg(target_os = "macos")]
            {
                let _ = error;
                let cache = festerm_unix_security::current_user_cache_directory()?;
                Ok((short_path(root, generation, &cache)?, Some(cache)))
            }
            #[cfg(not(target_os = "macos"))]
            {
                Err(error)
            }
        }
    }
}

/// May resolve/initialize the OS cache base on macOS overflow, but creates no
/// generation child. Once the base is resolved, short_path is pure derivation.
pub fn generation_path(root: &Path, generation: &str) -> io::Result<PathBuf> {
    address(root, generation).map(|a| a.0)
}

/// Recognizes exact original or deterministic overflow endpoints, never a bare
/// suffix. New /s endpoints must still use the original-root generation lease.
pub fn matches_generation(root: &Path, generation: &str, endpoint: &Path) -> io::Result<bool> {
    if endpoint == original(root, generation)? {
        return Ok(true);
    }
    if endpoint.file_name() != Some(std::ffi::OsStr::new("s")) {
        return Ok(false);
    }
    let (path, cache) = address(root, generation)?;
    Ok(cache.is_some() && path == endpoint)
}

/// The existing original-root generation lease must be held before preparation
/// and through this retained directory's lifetime. Existing children refuse.
pub struct SocketDirectory {
    base: File,
    directory: File,
    name: std::ffi::OsString,
}

pub fn prepare(root: &Path, generation: &str) -> io::Result<Option<SocketDirectory>> {
    let (path, cache) = address(root, generation)?;
    let Some(cache) = cache else {
        return Ok(None);
    };
    let _runtime = crate::private_storage::private_directory(root, false)?;
    let base = crate::private_storage::private_directory(&cache, false)?;
    let name = path
        .parent()
        .and_then(Path::file_name)
        .ok_or_else(invalid)?
        .to_owned();
    let directory = festerm_unix_security::create_private_directory(&base, Path::new(&name))?;
    let owned = SocketDirectory {
        base,
        directory,
        name,
    };
    owned.base.sync_all()?;
    Ok(Some(owned))
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        // Never unlink a socket or another entry. Normal cleanup reports
        // errors; unwind can only retire this same empty owned child.
        let _ = festerm_unix_security::remove_empty_private_directory(
            &self.base,
            Path::new(&self.name),
            &self.directory,
        );
    }
}

fn same(left: &File, right: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

/// Returns false for ordinary original-root endpoints. Callers hold the exact
/// dead-generation lease. Own failed starts pass their retained created dir;
/// published-record cleanup may reopen only the exact derived private path.
pub fn cleanup_short(
    root: &Path,
    generation: &str,
    socket_owned: bool,
    expected: Option<&SocketDirectory>,
) -> io::Result<bool> {
    let (path, cache) = address(root, generation)?;
    let Some(cache) = cache else {
        return Ok(false);
    };
    if !socket_owned && expected.is_none() {
        return Ok(true);
    }
    let _runtime = crate::private_storage::private_directory(root, false)?;
    let base = crate::private_storage::private_directory(&cache, false)?;
    let name = path
        .parent()
        .and_then(Path::file_name)
        .ok_or_else(invalid)?;
    if let Some(expected) = expected {
        if !same(&base, &expected.base)? || name != expected.name {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    let directory =
        match festerm_unix_security::open_directory_read_nofollow(&base, Path::new(name)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound && expected.is_none() => {
                return Ok(true)
            }
            Err(error) => return Err(error),
        };
    if !festerm_unix_security::is_current_user_only(&directory, true)? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if let Some(expected) = expected {
        if !same(&directory, &expected.directory)? {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    if socket_owned {
        crate::private_storage::remove_private_socket(&directory, Path::new("s"))?;
    }
    festerm_unix_security::remove_empty_private_directory(&base, Path::new(name), &directory)?;
    base.sync_all()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_endpoint_and_full_digest_domain_are_preserved() {
        let root = Path::new("/owned/runtime");
        assert_eq!(
            generation_path(root, "123-456").unwrap(),
            root.join("123-456.sock")
        );
        use std::os::unix::ffi::OsStringExt;
        let nul = PathBuf::from(std::ffi::OsString::from_vec(b"/bad\0root".to_vec()));
        assert!(generation_path(&nul, "1-2").is_err());
        let cache = Path::new("/c");
        let first = short_path(root, "123-456", cache).unwrap();
        assert_eq!(first.parent().unwrap().file_name().unwrap().len(), 44);
        assert_ne!(first, short_path(root, "123-457", cache).unwrap());
        assert_ne!(
            first,
            short_path(Path::new("/other/runtime"), "123-456", cache).unwrap()
        );
        for name in ["", "0-1", "1-0", "01-2", "1-02", "1-2/3", "1--2"] {
            assert!(generation_path(root, name).is_err());
        }
        assert!(short_path(root, "1-2", Path::new("/c").join("x".repeat(120)).as_path()).is_err());
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;
        use std::{
            fs,
            net::Shutdown,
            os::unix::{
                fs::{symlink, PermissionsExt},
                net::{UnixListener, UnixStream},
            },
            time::{SystemTime, UNIX_EPOCH},
        };

        struct Fixture {
            root: PathBuf,
            generation: String,
            time: u128,
            lease: File,
        }
        impl Fixture {
            fn new() -> Self {
                let root = std::env::current_dir().unwrap().join(format!(
                    "endpoint-{}-{}",
                    uuid::Uuid::new_v4().simple(),
                    "x".repeat(80)
                ));
                let dir = crate::private_storage::private_directory(&root, true).unwrap();
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
                    * 1000
                    + NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u128;
                let generation = format!("{}-{time}", std::process::id());
                let lease = crate::private_storage::open_private_file(
                    &dir,
                    Path::new(&format!("lease-{generation}")),
                    true,
                )
                .unwrap();
                fs2::FileExt::lock_exclusive(&lease).unwrap();
                Self {
                    root,
                    generation,
                    time,
                    lease,
                }
            }
            fn path(&self) -> PathBuf {
                generation_path(&self.root, &self.generation).unwrap()
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn macos_short_endpoint_real_bind_retains_original_lease_and_native_evidence() {
            let fixture = Fixture::new();
            let path = fixture.path();
            assert!(!path.parent().unwrap().exists());
            let owned = prepare(&fixture.root, &fixture.generation)
                .unwrap()
                .unwrap();
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let listener = UnixListener::bind(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let client = UnixStream::connect(&path).unwrap();
            let (server, _) = listener.accept().unwrap();
            client.shutdown(Shutdown::Both).unwrap();
            drop(server);
            drop(client);
            assert!(crate::daemon_generation_is_live(
                &fixture.root,
                std::process::id(),
                fixture.time,
                path.to_str().unwrap()
            )
            .unwrap());
            let identity = crate::NativeSessionIdentity {
                name: "owned".into(),
                pid: std::process::id(),
                created_at_unix_ms: fixture.time,
                endpoint: path.to_string_lossy().into(),
            };
            let fingerprint = format!("v1:{}", "a".repeat(64));
            let root = crate::private_storage::private_directory(&fixture.root, false).unwrap();
            let marker = format!("fesTerm:{}:{}", identity.pid, identity.created_at_unix_ms);
            let registry = serde_json::to_vec(&serde_json::json!({"sessions":{"owned":{
                "name":"owned", "pid":identity.pid, "created_at_unix_ms":identity.created_at_unix_ms,
                "socket":identity.endpoint, "spawn_fingerprint":fingerprint,
                "correlation_id":marker, "protocol_version":crate::PROTOCOL_VERSION,
                "snapshot_schema_version":crate::RECOVERY_SNAPSHOT_SCHEMA_VERSION
            }}})).unwrap();
            crate::private_storage::write_new_private(&root, Path::new("registry.json"), &registry)
                .unwrap();
            crate::verify_spawn_fingerprint_in(&fixture.root, &identity, &fingerprint).unwrap();
            assert_eq!(
                crate::session_correlation_in(&fixture.root, &identity)
                    .unwrap()
                    .as_deref(),
                Some(marker.as_str())
            );
            let (_binding, capability) = crate::native_binding::DaemonBinding::create(
                &fixture.root,
                identity.clone(),
                fingerprint.clone(),
            )
            .unwrap();
            assert!(crate::verify_spawn_fingerprint_in(
                &fixture.root,
                &identity,
                &format!("v1:{}", "b".repeat(64))
            )
            .is_err());
            let mut forged = identity.clone();
            forged.endpoint = path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("foreign/s")
                .to_string_lossy()
                .into();
            assert!(crate::session_correlation_in(&fixture.root, &forged).is_err());
            assert!(crate::daemon_generation_is_live(
                &fixture.root,
                identity.pid,
                fixture.time,
                &forged.endpoint
            )
            .is_err());
            capability.cleanup().unwrap();
            fs2::FileExt::unlock(&fixture.lease).unwrap();
            assert!(!crate::daemon_generation_is_live(
                &fixture.root,
                identity.pid,
                fixture.time,
                &identity.endpoint
            )
            .unwrap());
            drop(listener);
            assert!(cleanup_short(&fixture.root, &fixture.generation, true, Some(&owned)).unwrap());
            assert!(!path.parent().unwrap().exists());
            assert_eq!(
                fs::read(fixture.root.join("registry.json")).unwrap(),
                registry
            );
        }

        #[test]
        fn macos_short_endpoint_refuses_existing_symlink_nonempty_and_replaced_directory() {
            let fixture = Fixture::new();
            let path = fixture.path();
            let parent = path.parent().unwrap();
            fs::create_dir(parent).unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(prepare(&fixture.root, &fixture.generation).is_err());
            fs::remove_dir(parent).unwrap();
            symlink(&fixture.root, parent).unwrap();
            assert!(prepare(&fixture.root, &fixture.generation).is_err());
            fs::remove_file(parent).unwrap();
            let owned = prepare(&fixture.root, &fixture.generation)
                .unwrap()
                .unwrap();
            fs::write(parent.join("foreign"), b"preserve").unwrap();
            assert!(
                cleanup_short(&fixture.root, &fixture.generation, false, Some(&owned)).is_err()
            );
            assert_eq!(fs::read(parent.join("foreign")).unwrap(), b"preserve");
            fs::remove_file(parent.join("foreign")).unwrap();
            let moved = parent.with_extension("owned-old");
            fs::rename(parent, &moved).unwrap();
            fs::create_dir(parent).unwrap();
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                cleanup_short(&fixture.root, &fixture.generation, false, Some(&owned)).is_err()
            );
            assert!(parent.exists() && moved.exists());
            fs::remove_dir(parent).unwrap();
            fs::rename(&moved, parent).unwrap();
            assert!(
                cleanup_short(&fixture.root, &fixture.generation, false, Some(&owned)).unwrap()
            );
            assert!(!parent.exists());
        }

        #[test]
        fn macos_short_endpoint_refuses_unowned_socket_entry_and_never_creates_preflight_children()
        {
            let fixture = Fixture::new();
            let first = fixture.path();
            let another = generation_path(
                &fixture.root,
                &format!("{}-{}", std::process::id(), fixture.time + 1),
            )
            .unwrap();
            assert!(!first.parent().unwrap().exists() && !another.parent().unwrap().exists());
            assert!(matches_generation(&fixture.root, &fixture.generation, &first).unwrap());
            assert!(!matches_generation(&fixture.root, &fixture.generation, &another).unwrap());
            let owned = prepare(&fixture.root, &fixture.generation)
                .unwrap()
                .unwrap();
            fs::write(&first, b"not a socket").unwrap();
            fs::set_permissions(&first, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(cleanup_short(&fixture.root, &fixture.generation, true, Some(&owned)).is_err());
            assert_eq!(fs::read(&first).unwrap(), b"not a socket");
            fs::remove_file(&first).unwrap();
            assert!(
                cleanup_short(&fixture.root, &fixture.generation, false, Some(&owned)).unwrap()
            );
            assert!(!first.parent().unwrap().exists() && !another.parent().unwrap().exists());
        }
    }
}
