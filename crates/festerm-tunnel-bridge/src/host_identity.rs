//! Dedicated unencrypted host signing identity, separate from public metadata.
use std::{io, path::Path};

use russh::keys::{Algorithm, HashAlg, PrivateKey};
use zeroize::Zeroizing;

use crate::setup_storage;

const MAGIC: &[u8; 16] = b"fesTerm-host-v1\0";
const MAX_IDENTITY_BYTES: usize = 4096;

// PrivateKey owns zeroizing keypair material. Intentionally no Debug/Serialize:
// only the dedicated private-file encoding can expose its private bytes.
pub struct HostIdentity {
    host_id: uuid::Uuid,
    key: PrivateKey,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "host identity refused")
}

impl HostIdentity {
    pub fn ephemeral() -> io::Result<Self> {
        Ok(Self {
            host_id: uuid::Uuid::new_v4(),
            key: PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519)
                .map_err(|_| invalid())?,
        })
    }
    /// Loads an existing identity only; a missing file is never regenerated.
    pub fn load(path: &Path) -> io::Result<Self> {
        let (directory, name) = Self::location(path)?;
        Self::decode(&setup_storage::read_private(
            &directory,
            name,
            MAX_IDENTITY_BYTES,
        )?)
    }

    fn location(path: &Path) -> io::Result<(std::fs::File, &Path)> {
        if !path.is_absolute() {
            return Err(invalid());
        }
        let parent = path.parent().ok_or_else(invalid)?;
        let name = Path::new(path.file_name().ok_or_else(invalid)?);
        Ok((setup_storage::private_directory(parent, false)?, name))
    }

    /// Loads the exact identity, or exclusively creates one when absent.
    /// The absolute parent must be an existing owner-only directory.
    /// A failed/malformed load is never repaired, rotated or overwritten.
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        let (directory, name) = Self::location(path)?;
        match setup_storage::read_private(&directory, name, MAX_IDENTITY_BYTES) {
            Ok(bytes) => Self::decode(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let identity = Self::ephemeral()?;
                let bytes = identity.encode()?;
                match setup_storage::write_new_private(&directory, name, &bytes) {
                    Ok(()) => Ok(identity),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        // Another creator may still be writing. Incomplete
                        // publication fails closed; no retry ever replaces it.
                        Self::decode(&setup_storage::read_private(
                            &directory,
                            name,
                            MAX_IDENTITY_BYTES,
                        )?)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn host_id(&self) -> String {
        self.host_id.to_string()
    }

    pub fn fingerprint(&self) -> io::Result<String> {
        Ok(self
            .key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string())
    }

    pub(crate) fn into_key(self) -> PrivateKey {
        self.key
    }

    fn encode(&self) -> io::Result<Zeroizing<Vec<u8>>> {
        let key = self.key.to_bytes().map_err(|_| invalid())?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAGIC.len() + 16 + key.len()));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(self.host_id.as_bytes());
        bytes.extend_from_slice(&key);
        if bytes.len() > MAX_IDENTITY_BYTES {
            return Err(invalid());
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() <= MAGIC.len() + 16
            || bytes.len() > MAX_IDENTITY_BYTES
            || &bytes[..MAGIC.len()] != MAGIC
        {
            return Err(invalid());
        }
        let host_id =
            uuid::Uuid::from_slice(&bytes[MAGIC.len()..MAGIC.len() + 16]).map_err(|_| invalid())?;
        if host_id.get_version_num() != 4 || host_id.get_variant() != uuid::Variant::RFC4122 {
            return Err(invalid());
        }
        let key = PrivateKey::from_bytes(&bytes[MAGIC.len() + 16..]).map_err(|_| invalid())?;
        if key.algorithm() != Algorithm::Ed25519 || key.is_encrypted() {
            return Err(invalid());
        }
        let identity = Self { host_id, key };
        // Require one canonical complete representation (no trailing bytes).
        if identity.encode()?.as_slice() != bytes {
            return Err(invalid());
        }
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::Arc};

    struct OwnedDirectory(PathBuf);
    impl OwnedDirectory {
        fn new() -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join(format!(".bridge-identity-test-{}", uuid::Uuid::new_v4()));
            setup_storage::private_directory(&path, true).unwrap();
            Self(path)
        }

        fn key(&self) -> PathBuf {
            self.0.join("host-identity.key")
        }
    }

    impl Drop for OwnedDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn identity_roundtrip_preserves_host_id_and_key_fingerprint() {
        let owned = OwnedDirectory::new();
        assert!(HostIdentity::load(&owned.key()).is_err());
        assert!(!owned.key().exists());
        let first = HostIdentity::load_or_create(&owned.key()).unwrap();
        let id = first.host_id();
        let fingerprint = first.fingerprint().unwrap();
        let encoded = first.encode().unwrap();
        assert_eq!(encoded.as_slice(), first.encode().unwrap().as_slice());
        assert!(fingerprint.starts_with("SHA256:"));
        drop(first);
        for _ in 0..3 {
            let reloaded = HostIdentity::load_or_create(&owned.key()).unwrap();
            assert_eq!(reloaded.host_id(), id);
            assert_eq!(reloaded.fingerprint().unwrap(), fingerprint);
            assert_eq!(reloaded.encode().unwrap().as_slice(), encoded.as_slice());
        }
        fs::remove_file(owned.key()).unwrap();
        assert!(HostIdentity::load(&owned.key()).is_err());
        assert!(!owned.key().exists());
    }

    #[test]
    fn deterministic_ed25519_identity_format_roundtrips_exactly() {
        let identity = HostIdentity {
            host_id: uuid::Uuid::from_bytes([
                0, 1, 2, 3, 4, 5, 0x46, 7, 0x88, 9, 10, 11, 12, 13, 14, 15,
            ]),
            key: PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(
                &[7; 32],
            )),
        };
        assert_eq!(identity.host_id(), "00010203-0405-4607-8809-0a0b0c0d0e0f");
        let bytes = identity.encode().unwrap();
        let decoded = HostIdentity::decode(&bytes).unwrap();
        assert_eq!(decoded.host_id(), identity.host_id());
        assert_eq!(
            decoded.fingerprint().unwrap(),
            identity.fingerprint().unwrap()
        );
        assert_eq!(decoded.encode().unwrap().as_slice(), bytes.as_slice());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn identity_rejects_extended_read_acl_without_repairing_it() {
        let owned = OwnedDirectory::new();
        HostIdentity::load_or_create(&owned.key()).unwrap();
        assert!(std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(owned.key())
            .status()
            .unwrap()
            .success());
        assert!(HostIdentity::load(&owned.key()).is_err());
        // A second load still refuses: verification must not remove the ACL.
        assert!(HostIdentity::load_or_create(&owned.key()).is_err());
    }

    #[test]
    fn identity_malformed_unknown_oversized_and_trailing_bytes_never_rotate() {
        let owned = OwnedDirectory::new();
        let identity = HostIdentity::ephemeral().unwrap();
        let original = identity.encode().unwrap();
        let mut unknown = original.clone();
        unknown[0] ^= 1;
        let mut trailing = original.clone();
        trailing.push(0);
        let malformed = [
            Zeroizing::new(Vec::new()),
            Zeroizing::new(b"ordinary metadata is not an identity".to_vec()),
            unknown,
            trailing,
            Zeroizing::new(vec![b'x'; MAX_IDENTITY_BYTES + 1]),
        ];
        let directory = setup_storage::private_directory(&owned.0, false).unwrap();
        for (index, bytes) in malformed.iter().enumerate() {
            let name = PathBuf::from(format!("malformed-{index}.key"));
            setup_storage::write_new_private(&directory, &name, bytes).unwrap();
            let path = owned.0.join(&name);
            assert!(HostIdentity::load_or_create(&path).is_err());
            assert_eq!(fs::read(path).unwrap().as_slice(), bytes.as_slice());
        }
        assert!(HostIdentity::load_or_create(Path::new("relative.key")).is_err());
        assert!(HostIdentity::load_or_create(&owned.0.join("absent").join("key")).is_err());
    }

    #[test]
    fn concurrent_identity_creation_never_replaces_the_winner() {
        let owned = OwnedDirectory::new();
        let path = Arc::new(owned.key());
        let start = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    HostIdentity::load_or_create(&path)
                        .ok()
                        .map(|identity| (identity.host_id(), identity.fingerprint().unwrap()))
                })
            })
            .collect();
        let successful: Vec<_> = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap())
            .collect();
        assert!(!successful.is_empty());
        let winner = HostIdentity::load_or_create(&owned.key()).unwrap();
        for identity in successful {
            assert_eq!(identity, (winner.host_id(), winner.fingerprint().unwrap()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn identity_rejects_permissive_files_directories_symlinks_and_hardlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let owned = OwnedDirectory::new();
        let identity = HostIdentity::load_or_create(&owned.key()).unwrap();
        let fingerprint = identity.fingerprint().unwrap();
        assert_eq!(
            fs::metadata(&owned.0).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        assert_eq!(
            fs::metadata(owned.key()).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        fs::set_permissions(owned.key(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(HostIdentity::load_or_create(&owned.key()).is_err());
        fs::set_permissions(owned.key(), fs::Permissions::from_mode(0o600)).unwrap();
        let alias = owned.0.join("alias.key");
        symlink(owned.key(), &alias).unwrap();
        assert!(HostIdentity::load_or_create(&alias).is_err());
        let linked = owned.0.join("hardlink.key");
        fs::hard_link(owned.key(), &linked).unwrap();
        assert!(HostIdentity::load_or_create(&owned.key()).is_err());
        fs::remove_file(linked).unwrap();
        let alias_directory = owned.0.join("alias-directory");
        symlink(&owned.0, &alias_directory).unwrap();
        assert!(HostIdentity::load_or_create(&alias_directory.join("host-identity.key")).is_err());
        fs::set_permissions(&owned.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(HostIdentity::load_or_create(&owned.key()).is_err());
        fs::set_permissions(&owned.0, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            HostIdentity::load_or_create(&owned.key())
                .unwrap()
                .fingerprint()
                .unwrap(),
            fingerprint
        );
    }
}
