//! Publisher authentication for immutable helper releases (ADR 0084 §9).
//!
//! No network, filesystem extraction, service switching, or caller-selected key.
//! Downloaders must apply these bounds while receiving bytes, not just afterwards.

use std::io::{Cursor, Read};

use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_MANIFEST_BYTES: usize = 4096;
pub const MAX_SIGNATURE_BYTES: usize = 4096;
pub const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_NOTICE_BYTES: u64 = 16 * 1024 * 1024;
const PUBLIC_KEY: &str = include_str!("../../../packaging/updater.pub");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    X86_64,
    Aarch64,
}

impl Architecture {
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

/// This schema deliberately does not claim that a host-management CLI is bundled.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Package {
    #[serde(rename = "festerm-helpers")]
    Helpers,
}

/// The signature authenticates the exact JSON bytes, not reserialized JSON.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub package: Package,
    pub version: String,
    pub platform: Platform,
    pub architecture: Architecture,
    pub filename: String,
    pub length: u64,
    pub sha256: String,
}

/// Values selected by trusted installation policy, never copied from the manifest.
/// The caller must enforce downgrade policy before choosing an expected version.
pub struct ExpectedArtifact<'a> {
    pub version: &'a str,
    pub platform: Platform,
    pub architecture: Architecture,
    pub package: Package,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactError {
    Bounds,
    Signature,
    Manifest,
    Target,
    Length,
    Hash,
    Archive,
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "artifact verification failed: {self:?}")
    }
}

impl std::error::Error for ArtifactError {}

/// Immutable, authenticated, validated regular file bytes. Never reopen the
/// downloaded archive by path after verification (that would introduce TOCTOU).
#[derive(Debug)]
pub struct VerifiedEntry {
    name: String,
    bytes: Vec<u8>,
    executable: bool,
}

impl VerifiedEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn executable(&self) -> bool {
        self.executable
    }
}

/// Only `verify` can construct this staging capability. A staging owner must
/// create private files exclusively/no-follow, sync, then publish atomically.
/// This descriptor is not authorization to switch a service or execute a helper.
#[derive(Debug)]
pub struct VerifiedArtifact {
    manifest: Manifest,
    entries: Vec<VerifiedEntry>,
    manifest_bytes: Vec<u8>,
    signature_bytes: Vec<u8>,
    archive_bytes: Vec<u8>,
}

impl VerifiedArtifact {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn entries(&self) -> &[VerifiedEntry] {
        &self.entries
    }

    /// Retain the original signed bytes; JSON reserialization is not equivalent.
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    pub fn signature_bytes(&self) -> &[u8] {
        &self.signature_bytes
    }

    /// Retain with the signed metadata for independent restart verification.
    /// The archive hash does not authenticate separately modified staged files.
    pub fn archive_bytes(&self) -> &[u8] {
        &self.archive_bytes
    }
}

pub fn filename(expected: &ExpectedArtifact<'_>) -> String {
    let extension = if expected.platform == Platform::Windows {
        "zip"
    } else {
        "tar.gz"
    };
    format!(
        "fesTerm-tunnel-{}-{}-{}.{extension}",
        expected.version,
        expected.platform.name(),
        expected.architecture.name()
    )
}

/// Authenticate metadata and bytes, then validate the exact archive allowlist.
/// `.sig` uses cargo-packager's base64-encoded Minisign text with the existing
/// embedded updater key. Missing/legacy unsigned artifacts are always errors.
pub fn verify(
    manifest: &[u8],
    signature: &[u8],
    archive: &[u8],
    expected: &ExpectedArtifact<'_>,
) -> Result<VerifiedArtifact, ArtifactError> {
    verify_with_key(manifest, signature, archive, expected, PUBLIC_KEY)
}

fn verify_with_key(
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
    archive: &[u8],
    expected: &ExpectedArtifact<'_>,
    key: &str,
) -> Result<VerifiedArtifact, ArtifactError> {
    if manifest_bytes.is_empty()
        || manifest_bytes.len() > MAX_MANIFEST_BYTES
        || signature_bytes.is_empty()
        || signature_bytes.len() > MAX_SIGNATURE_BYTES
        || archive.is_empty()
        || archive.len() > MAX_ARCHIVE_BYTES
    {
        return Err(ArtifactError::Bounds);
    }
    let decode = |bytes: &[u8]| {
        let bytes = STANDARD
            .decode(bytes)
            .map_err(|_| ArtifactError::Signature)?;
        String::from_utf8(bytes).map_err(|_| ArtifactError::Signature)
    };
    let public_key =
        PublicKey::decode(&decode(key.trim().as_bytes())?).map_err(|_| ArtifactError::Signature)?;
    let signature =
        Signature::decode(&decode(signature_bytes)?).map_err(|_| ArtifactError::Signature)?;
    public_key
        .verify(manifest_bytes, &signature, false)
        .map_err(|_| ArtifactError::Signature)?;
    let manifest: Manifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| ArtifactError::Manifest)?;
    if manifest.schema != 1
        || !stable_version(&manifest.version)
        || manifest.sha256.len() != 64
        || !manifest
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || manifest.length == 0
        || manifest.length > MAX_ARCHIVE_BYTES as u64
    {
        return Err(ArtifactError::Manifest);
    }
    if manifest.package != expected.package
        || manifest.version != expected.version
        || manifest.platform != expected.platform
        || manifest.architecture != expected.architecture
        || manifest.filename != filename(expected)
    {
        return Err(ArtifactError::Target);
    }
    if manifest.length != archive.len() as u64 {
        return Err(ArtifactError::Length);
    }
    let digest: String = Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if manifest.sha256 != digest {
        return Err(ArtifactError::Hash);
    }
    let entries = validate_archive(archive, &manifest)?;
    Ok(VerifiedArtifact {
        manifest,
        entries,
        manifest_bytes: manifest_bytes.to_vec(),
        signature_bytes: signature_bytes.to_vec(),
        archive_bytes: archive.to_vec(),
    })
}

fn stable_version(version: &str) -> bool {
    version.len() <= 32 && {
        let parts: Vec<_> = version.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && (part.len() == 1 || !part.starts_with('0'))
                    && part.bytes().all(|byte| byte.is_ascii_digit())
            })
    }
}

fn allowed_names(platform: Platform) -> Vec<String> {
    let suffix = if platform == Platform::Windows {
        ".exe"
    } else {
        ""
    };
    let mut names: Vec<_> = [
        "festerm-sessiond",
        "festerm-tunnel-bridge",
        "festerm-tunnel-setup",
    ]
    .map(|name| format!("{name}{suffix}"))
    .into();
    if platform == Platform::Windows {
        names.push("festerm-tunnel-worker.exe".into());
    }
    names.extend(["VERSION", "LICENSE", "THIRD_PARTY_NOTICES.txt"].map(str::to_owned));
    names
}

fn read_entry(
    reader: impl Read,
    name: &str,
    size: u64,
    mode: u32,
    manifest: &Manifest,
    entries: &mut Vec<VerifiedEntry>,
) -> Result<(), ArtifactError> {
    let allowed = allowed_names(manifest.platform);
    let executable = name.starts_with("festerm-");
    let limit = if executable {
        MAX_ENTRY_BYTES
    } else {
        MAX_NOTICE_BYTES
    };
    if !allowed.iter().any(|allowed| allowed == name)
        || entries.iter().any(|entry| entry.name == name)
        || entries.len() >= allowed.len()
        || size == 0
        || size > limit
        || mode != if executable { 0o755 } else { 0o644 }
    {
        return Err(ArtifactError::Archive);
    }
    if size
        + entries
            .iter()
            .map(|entry| entry.bytes.len() as u64)
            .sum::<u64>()
        > MAX_EXPANDED_BYTES
    {
        return Err(ArtifactError::Bounds);
    }
    let mut bytes = Vec::new();
    reader
        .take(size + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ArtifactError::Archive)?;
    if bytes.len() as u64 != size
        || (name == "VERSION" && bytes != format!("{}\n", manifest.version).as_bytes())
    {
        return Err(ArtifactError::Archive);
    }
    entries.push(VerifiedEntry {
        name: name.to_owned(),
        bytes,
        executable,
    });
    Ok(())
}

fn validate_archive(
    archive: &[u8],
    manifest: &Manifest,
) -> Result<Vec<VerifiedEntry>, ArtifactError> {
    let mut entries = Vec::new();
    if manifest.platform == Platform::Windows {
        // Reject ZIP64/multi-disk/comments and bound central-directory work
        // before ZipArchive allocates from attacker-controlled entry counts.
        // The deterministic release producer emits precisely this ZIP subset.
        let eocd = archive
            .get(
                archive
                    .len()
                    .checked_sub(22)
                    .ok_or(ArtifactError::Archive)?..,
            )
            .ok_or(ArtifactError::Archive)?;
        let count = allowed_names(manifest.platform).len() as u16;
        if eocd[..4] != *b"PK\x05\x06"
            || eocd[4..8] != [0; 4]
            || u16::from_le_bytes([eocd[8], eocd[9]]) != count
            || u16::from_le_bytes([eocd[10], eocd[11]]) != count
            || eocd[20..22] != [0; 2]
        {
            return Err(ArtifactError::Archive);
        }
        let directory_size = u32::from_le_bytes(eocd[12..16].try_into().unwrap()) as usize;
        let directory_offset = u32::from_le_bytes(eocd[16..20].try_into().unwrap()) as usize;
        if directory_size > 16 * 1024
            || directory_offset.checked_add(directory_size) != Some(archive.len() - 22)
        {
            return Err(ArtifactError::Archive);
        }
        let mut zip =
            zip::ZipArchive::new(Cursor::new(archive)).map_err(|_| ArtifactError::Archive)?;
        if zip.len() != allowed_names(manifest.platform).len() {
            return Err(ArtifactError::Archive);
        }
        for index in 0..zip.len() {
            let entry = zip.by_index(index).map_err(|_| ArtifactError::Archive)?;
            let mode = entry.unix_mode().ok_or(ArtifactError::Archive)?;
            if mode & 0o170000 != 0o100000 || entry.encrypted() {
                return Err(ArtifactError::Archive);
            }
            let name = std::str::from_utf8(entry.name_raw())
                .map_err(|_| ArtifactError::Archive)?
                .to_owned();
            let size = entry.size();
            read_entry(entry, &name, size, mode & 0o7777, manifest, &mut entries)?;
        }
    } else {
        // Bound the *whole* decompressed stream too, including padding/headers.
        let mut expanded = Vec::new();
        let mut decoder = flate2::bufread::GzDecoder::new(archive);
        decoder
            .by_ref()
            .take(MAX_EXPANDED_BYTES + 1)
            .read_to_end(&mut expanded)
            .map_err(|_| ArtifactError::Archive)?;
        if expanded.len() as u64 > MAX_EXPANDED_BYTES {
            return Err(ArtifactError::Bounds);
        }
        if !decoder.into_inner().is_empty() {
            return Err(ArtifactError::Archive);
        }
        let mut tar = tar::Archive::new(Cursor::new(expanded));
        for entry in tar.entries().map_err(|_| ArtifactError::Archive)?.raw(true) {
            let entry = entry.map_err(|_| ArtifactError::Archive)?;
            if !entry.header().entry_type().is_file() {
                return Err(ArtifactError::Archive);
            }
            let path = entry.path_bytes();
            let name = std::str::from_utf8(&path)
                .map_err(|_| ArtifactError::Archive)?
                .to_owned();
            let size = entry.size();
            let mode = entry.header().mode().map_err(|_| ArtifactError::Archive)?;
            read_entry(entry, &name, size, mode, manifest, &mut entries)?;
        }
    }
    if entries.len() != allowed_names(manifest.platform).len() {
        return Err(ArtifactError::Archive);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use std::io::Write;

    fn expected(platform: Platform, architecture: Architecture) -> ExpectedArtifact<'static> {
        ExpectedArtifact {
            version: "1.2.3",
            platform,
            architecture,
            package: Package::Helpers,
        }
    }

    // Repository-owned, deterministic test identity; never a release secret.
    fn sign(data: &[u8], seed: u8) -> (String, Vec<u8>) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = [seed; 8];
        let public = [b"Ed".as_slice(), &id, key.verifying_key().as_bytes()].concat();
        let public = STANDARD.encode(format!(
            "untrusted comment: synthetic test key\n{}\n",
            STANDARD.encode(public)
        ));
        let digest = <blake2::Blake2b512 as blake2::Digest>::digest(data);
        let sig = key.sign(&digest).to_bytes();
        let packet = [b"ED".as_slice(), &id, &sig].concat();
        let comment = "test fixture only";
        let global = key.sign(&[sig.as_slice(), comment.as_bytes()].concat());
        let signature = STANDARD
            .encode(format!(
                "untrusted comment: synthetic signature\n{}\ntrusted comment: {comment}\n{}\n",
                STANDARD.encode(packet),
                STANDARD.encode(global.to_bytes())
            ))
            .into_bytes();
        (public, signature)
    }

    fn tar(entries: &[(String, Vec<u8>, u32, tar::EntryType)]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, bytes, mode, kind) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_mode(*mode);
            header.set_size(bytes.len() as u64);
            header.set_entry_type(*kind);
            if kind.is_symlink() || kind.is_hard_link() {
                header.set_link_name("VERSION").unwrap();
            }
            header.set_cksum();
            builder
                .append_data(&mut header, name, bytes.as_slice())
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn files(platform: Platform) -> Vec<(String, Vec<u8>, u32, tar::EntryType)> {
        allowed_names(platform)
            .into_iter()
            .map(|name| {
                let bytes = if name == "VERSION" {
                    b"1.2.3\n".to_vec()
                } else {
                    format!("synthetic {name}").into_bytes()
                };
                let mode = if name.starts_with("festerm-") {
                    0o755
                } else {
                    0o644
                };
                (name, bytes, mode, tar::EntryType::Regular)
            })
            .collect()
    }

    fn archive(platform: Platform) -> Vec<u8> {
        if platform != Platform::Windows {
            return tar(&files(platform));
        }
        zip(&files(platform))
    }

    fn zip(entries: &[(String, Vec<u8>, u32, tar::EntryType)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes, mode, kind) in entries {
            let options = zip::write::SimpleFileOptions::default().unix_permissions(*mode);
            if kind.is_symlink() {
                writer.add_symlink(name, "VERSION", options).unwrap();
            } else {
                writer.start_file(name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn manifest(archive: &[u8], expected: &ExpectedArtifact<'_>) -> Manifest {
        Manifest {
            schema: 1,
            package: Package::Helpers,
            version: expected.version.into(),
            platform: expected.platform,
            architecture: expected.architecture,
            filename: filename(expected),
            length: archive.len() as u64,
            sha256: Sha256::digest(archive)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    fn verify_fixture(
        manifest: &Manifest,
        archive: &[u8],
        expected: &ExpectedArtifact<'_>,
    ) -> Result<VerifiedArtifact, ArtifactError> {
        let data = serde_json::to_vec(manifest).unwrap();
        let (key, signature) = sign(&data, 42);
        verify_with_key(&data, &signature, archive, expected, &key)
    }

    #[test]
    fn authenticates_six_targets_and_returns_only_immutable_allowlisted_bytes() {
        for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
            for architecture in [Architecture::X86_64, Architecture::Aarch64] {
                let expected = expected(platform, architecture);
                let archive = archive(platform);
                let metadata = manifest(&archive, &expected);
                let verified = verify_fixture(&metadata, &archive, &expected).unwrap();
                assert_eq!(verified.manifest(), &metadata);
                assert_eq!(verified.archive_bytes(), archive);
                assert_eq!(
                    verified.manifest_bytes(),
                    serde_json::to_vec(&metadata).unwrap()
                );
                let (key, signature) = sign(verified.manifest_bytes(), 42);
                assert_eq!(verified.signature_bytes(), signature);
                verify_with_key(
                    verified.manifest_bytes(),
                    verified.signature_bytes(),
                    verified.archive_bytes(),
                    &expected,
                    &key,
                )
                .unwrap();
                for entry in verified.entries() {
                    assert!(allowed_names(platform).contains(&entry.name().to_owned()));
                    assert!(!entry.bytes().is_empty());
                    assert_eq!(entry.executable(), entry.name().starts_with("festerm-"));
                }
            }
        }
    }

    #[test]
    fn rejects_unsigned_wrong_key_and_tampered_metadata_before_archive_parsing() {
        let expected = expected(Platform::Linux, Architecture::X86_64);
        let archive = archive(expected.platform);
        let data = serde_json::to_vec(&manifest(&archive, &expected)).unwrap();
        let (key, signature) = sign(&data, 42);
        let (wrong_key, _) = sign(&data, 43);
        assert_eq!(
            verify_with_key(&data, b"", &archive, &expected, &key).unwrap_err(),
            ArtifactError::Bounds
        );
        for bad_signature in [b"not base64".as_slice(), b"YWJj"] {
            assert_eq!(
                verify_with_key(&data, bad_signature, b"not an archive", &expected, &key)
                    .unwrap_err(),
                ArtifactError::Signature
            );
        }
        assert_eq!(
            verify_with_key(&data, &signature, &archive, &expected, &wrong_key).unwrap_err(),
            ArtifactError::Signature
        );
        // The shipping API cannot be redirected to a test/downloaded key.
        assert_eq!(
            verify(&data, &signature, &archive, &expected).unwrap_err(),
            ArtifactError::Signature
        );
        let mut tampered = data;
        tampered.push(b' ');
        assert_eq!(
            verify_with_key(&tampered, &signature, &archive, &expected, &key).unwrap_err(),
            ArtifactError::Signature
        );
    }

    #[test]
    fn rejects_wrong_version_arch_platform_filename_length_and_hash() {
        let expected = expected(Platform::Linux, Architecture::X86_64);
        let archive = archive(expected.platform);
        let original = manifest(&archive, &expected);
        for change in ["version", "arch", "platform", "filename", "length", "hash"] {
            let mut metadata = original.clone();
            let error = match change {
                "version" => {
                    metadata.version = "1.2.2".into();
                    ArtifactError::Target
                }
                "arch" => {
                    metadata.architecture = Architecture::Aarch64;
                    ArtifactError::Target
                }
                "platform" => {
                    metadata.platform = Platform::Windows;
                    ArtifactError::Target
                }
                "filename" => {
                    metadata.filename = "../other.tar.gz".into();
                    ArtifactError::Target
                }
                "length" => {
                    metadata.length += 1;
                    ArtifactError::Length
                }
                _ => {
                    metadata.sha256 = "0".repeat(64);
                    ArtifactError::Hash
                }
            };
            assert_eq!(
                verify_fixture(&metadata, &archive, &expected).unwrap_err(),
                error,
                "{change}"
            );
        }
        let mut modified = archive.clone();
        modified[0] ^= 1;
        assert_eq!(
            verify_fixture(&original, &modified, &expected).unwrap_err(),
            ArtifactError::Hash
        );
        assert_eq!(
            verify_fixture(&original, &archive[..archive.len() - 1], &expected).unwrap_err(),
            ArtifactError::Length
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_out_of_bounds_manifest_fields() {
        let expected = expected(Platform::Linux, Architecture::X86_64);
        let archive = archive(expected.platform);
        let original = manifest(&archive, &expected);
        for field in [
            r#""unexpected":true,"#,
            r#""package":"other-product","#,
            r#""schema":2,"#,
            r#""version":"01.2.3","#,
        ] {
            let data = serde_json::to_string(&original).unwrap();
            let data = format!("{{{field}{}", &data[1..]);
            let (key, sig) = sign(data.as_bytes(), 42);
            assert_eq!(
                verify_with_key(data.as_bytes(), &sig, &archive, &expected, &key).unwrap_err(),
                ArtifactError::Manifest
            );
        }
        for change in ["schema", "length", "version", "hash"] {
            let mut metadata = manifest(&archive, &expected);
            match change {
                "schema" => metadata.schema = 2,
                "length" => metadata.length = MAX_ARCHIVE_BYTES as u64 + 1,
                "version" => metadata.version = "1.2.3-rc.1".into(),
                _ => metadata.sha256 = "ABCDEF".repeat(10),
            }
            assert_eq!(
                verify_fixture(&metadata, &archive, &expected).unwrap_err(),
                ArtifactError::Manifest,
                "{change}"
            );
        }
        assert_eq!(
            verify(&vec![0; MAX_MANIFEST_BYTES + 1], b"x", b"x", &expected).unwrap_err(),
            ArtifactError::Bounds
        );
        assert_eq!(
            verify(b"x", &vec![0; MAX_SIGNATURE_BYTES + 1], b"x", &expected).unwrap_err(),
            ArtifactError::Bounds
        );
    }

    #[test]
    fn rejects_signed_unsafe_tar_entries_duplicates_missing_files_and_wrong_version() {
        let expected = expected(Platform::Linux, Architecture::X86_64);
        for change in [
            "extra",
            "duplicate",
            "missing",
            "symlink",
            "hardlink",
            "mode",
            "version",
            "empty",
        ] {
            let mut files = files(expected.platform);
            match change {
                "extra" => files.push((
                    "extra".into(),
                    b"x".to_vec(),
                    0o644,
                    tar::EntryType::Regular,
                )),
                "duplicate" => files.push(files[0].clone()),
                "missing" => {
                    files.pop();
                }
                "symlink" => files[0].3 = tar::EntryType::Symlink,
                "hardlink" => files[0].3 = tar::EntryType::Link,
                "mode" => files[0].2 = 0o4755,
                "version" => files[3].1 = b"1.2.2\n".to_vec(),
                _ => files[0].1.clear(),
            }
            let archive = tar(&files);
            assert_eq!(
                verify_fixture(&manifest(&archive, &expected), &archive, &expected).unwrap_err(),
                ArtifactError::Archive,
                "{change}"
            );
        }
        // tar::Builder itself refuses traversal; mutate a synthetic raw header
        // and repair its checksum so our consumer (not the builder) rejects it.
        for name in [
            "../escape",
            "/absolute",
            "sub/file",
            "./VERSION",
            "C:\\escape",
        ] {
            let mut raw = Vec::new();
            flate2::read::GzDecoder::new(tar(&files(expected.platform)).as_slice())
                .read_to_end(&mut raw)
                .unwrap();
            let mut header = tar::Header::from_byte_slice(&raw[..512]).clone();
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            raw[..512].copy_from_slice(header.as_bytes());
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&raw).unwrap();
            let archive = encoder.finish().unwrap();
            assert_eq!(
                verify_fixture(&manifest(&archive, &expected), &archive, &expected).unwrap_err(),
                ArtifactError::Archive,
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_signed_zip_paths_symlinks_modes_and_duplicates() {
        let expected = expected(Platform::Windows, Architecture::Aarch64);
        for change in [
            "../escape",
            "/absolute",
            "sub/file",
            "symlink",
            "mode",
            "missing",
            "version",
        ] {
            let mut files = files(expected.platform);
            match change {
                "symlink" => files[0].3 = tar::EntryType::Symlink,
                "mode" => files[0].2 = 0o644,
                "missing" => {
                    files.pop();
                }
                "version" => files[4].1 = b"1.2.2\n".to_vec(),
                name => files[0].0 = name.into(),
            }
            let archive = zip(&files);
            assert_eq!(
                verify_fixture(&manifest(&archive, &expected), &archive, &expected).unwrap_err(),
                ArtifactError::Archive,
                "{change}"
            );
        }
        let mut duplicate = archive(expected.platform);
        for index in 0..duplicate.len() - 7 {
            if &duplicate[index..index + 7] == b"VERSION" {
                duplicate[index..index + 7].copy_from_slice(b"LICENSE");
            }
        }
        assert_eq!(
            verify_fixture(&manifest(&duplicate, &expected), &duplicate, &expected).unwrap_err(),
            ArtifactError::Archive
        );
        let mut oversized = archive(expected.platform);
        let central = oversized
            .windows(4)
            .position(|bytes| bytes == b"PK\x01\x02")
            .unwrap();
        oversized[central + 24..central + 28]
            .copy_from_slice(&((MAX_ENTRY_BYTES + 1) as u32).to_le_bytes());
        assert_eq!(
            verify_fixture(&manifest(&oversized, &expected), &oversized, &expected).unwrap_err(),
            ArtifactError::Archive
        );
    }

    #[test]
    fn rejects_oversized_headers_without_allocating_entry_and_trailing_gzip() {
        let expected = expected(Platform::Linux, Architecture::X86_64);
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(archive(expected.platform).as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        let mut header = tar::Header::from_byte_slice(&raw[..512]).clone();
        header.set_size(MAX_ENTRY_BYTES + 1);
        header.set_cksum();
        raw[..512].copy_from_slice(header.as_bytes());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        let oversized = encoder.finish().unwrap();
        assert_eq!(
            verify_fixture(&manifest(&oversized, &expected), &oversized, &expected).unwrap_err(),
            ArtifactError::Archive
        );
        let mut trailing = archive(expected.platform);
        trailing.extend_from_slice(b"trailing junk");
        assert_eq!(
            verify_fixture(&manifest(&trailing, &expected), &trailing, &expected).unwrap_err(),
            ArtifactError::Archive
        );
    }
}
