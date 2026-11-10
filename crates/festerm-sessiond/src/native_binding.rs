//! Generation-local challenge/response for explicit in-place HOST binding.
//! The daemon creates the private capability before publication and retains it
//! in memory. Its separate creation owner explicitly retires the disk key before
//! releasing the generation lease. Readers never own cleanup.
//! Possession is not owner consent: SessionControl separately requires
//! current protected policy at each durable publication.
use crate::{host_lease::DaemonWatch, private_storage as storage, NativeSessionIdentity};
use festerm_host_protocol::{Epoch, HostId, LogicalSessionId, OperationId, PrincipalId};
use fs2::FileExt;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const LIMIT: usize = 16 * 1024;
const MAGIC: &[u8; 4] = b"FSB1";

struct Deadline<'a, S> {
    stream: &'a mut S,
    until: std::time::Instant,
}
impl<S: Read> Read for Deadline<'_, S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            if std::time::Instant::now() >= self.until {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native.binding_deadline",
                ));
            }
            match self.stream.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue
                }
                result => return result,
            }
        }
    }
}
impl<S: Write> Write for Deadline<'_, S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if std::time::Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native.binding_deadline",
            ));
        }
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn invalid() -> io::Error {
    io::Error::other("native.binding_untrusted_stale_or_unsupported")
}

fn random() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

fn tag(key: &[u8], domain: &[u8], nonce: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts every key length");
    mac.update(domain);
    mac.update(nonce);
    mac.update(bytes);
    mac.finalize().into_bytes().into()
}

fn verify(
    key: &[u8],
    domain: &[u8],
    nonce: &[u8],
    bytes: &[u8],
    expected: &[u8],
) -> io::Result<()> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| invalid())?;
    mac.update(domain);
    mac.update(nonce);
    mac.update(bytes);
    mac.verify_slice(expected).map_err(|_| invalid())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub operation: OperationId,
    pub host: HostId,
    pub installation: String,
    pub account: String,
    pub host_key_fingerprint: String,
    pub principal: PrincipalId,
    pub owner_epoch: Epoch,
    pub host_epoch: Epoch,
    pub issuance_epoch: Epoch,
    pub namespace: OperationId,
    pub control_root: PathBuf,
    pub logical: LogicalSessionId,
    pub source_installation: String,
    pub source_host: HostId,
    pub source_key_fingerprint: String,
    pub source_receipt_digest: String,
    pub source_root: PathBuf,
    pub source_profile: bool,
    pub source_installation_digest: String,
    pub source_ownership_digest: String,
    pub source_key_digest: String,
    pub source_claim_digest: String,
    pub runtime_root: PathBuf,
    pub identity: NativeSessionIdentity,
    pub spawn_fingerprint: String,
}

impl Binding {
    pub fn validate(&self) -> io::Result<()> {
        for id in [&self.installation, &self.source_installation] {
            let value = uuid::Uuid::parse_str(id).map_err(|_| invalid())?;
            if value.get_version_num() != 4 || value.to_string() != *id {
                return Err(invalid());
            }
        }
        for digest in [
            &self.source_installation_digest,
            &self.source_ownership_digest,
            &self.source_key_digest,
            &self.source_claim_digest,
        ] {
            if !Self::canonical_digest(digest) {
                return Err(invalid());
            }
        }
        if !self.runtime_root.is_absolute()
            || !self.source_root.is_absolute()
            || !self.control_root.is_absolute()
            || self.identity.pid == 0
            || self.identity.created_at_unix_ms == 0
            || self.identity.created_at_unix_ms > u64::MAX as u128
            || self
                .account
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .is_none_or(|value| value.to_string() != self.account)
            || self.host_key_fingerprint.is_empty()
            || self.source_key_fingerprint.is_empty()
            || [&self.host_key_fingerprint, &self.source_key_fingerprint]
                .iter()
                .any(|value| value.len() > 256 || value.chars().any(char::is_control))
            || self.spawn_fingerprint.len() != 67
            || !self.spawn_fingerprint.starts_with("v1:")
            || !Self::canonical_digest(&self.source_receipt_digest)
            || !Self::canonical_digest(&self.spawn_fingerprint[3..])
            || [&self.runtime_root, &self.source_root, &self.control_root]
                .iter()
                .any(|path| {
                    path.components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                })
            || festerm_ssh::PersistentSessionName::new(&self.identity.name).is_err()
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn canonical_digest(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    pub(crate) fn source_fence(&self) -> io::Result<SourceFence> {
        let directory = storage::private_directory(&self.source_root, false)?;
        let mut locks = Vec::new();
        let lock = storage::open_private_file(&directory, Path::new("installer.lock"), false)?;
        FileExt::try_lock_shared(&lock)?;
        locks.push(lock);
        if self.source_profile {
            let lock =
                storage::open_private_file(&directory, Path::new("profile-consent.lock"), false)?;
            FileExt::try_lock_shared(&lock)?;
            locks.push(lock);
        } else {
            let parent =
                storage::private_directory(self.source_root.parent().ok_or_else(invalid)?, false)?;
            let lock = storage::open_private_file(&parent, Path::new("installer.lock"), false)?;
            FileExt::try_lock_shared(&lock)?;
            locks.push(lock);
        }
        for (name, expected) in [
            ("installation.json", &self.source_installation_digest),
            (
                if self.source_profile {
                    "profile-consent.json"
                } else {
                    "script-ownership.json"
                },
                &self.source_ownership_digest,
            ),
            ("host-identity.key", &self.source_key_digest),
            ("host-migration-claim.json", &self.source_claim_digest),
        ] {
            let bytes = storage::read_private(&directory, Path::new(name), 64 * 1024)?;
            let actual = format!("{:x}", Sha256::digest(&*bytes));
            if &actual != expected {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "native.binding_source_changed_or_revoked",
                ));
            }
        }
        Ok(SourceFence { _locks: locks })
    }
}

pub(crate) struct SourceFence {
    _locks: Vec<File>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BindingPhase {
    Prepared,
    Active,
    Revoked,
    Released,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub version: u8,
    pub binding: Binding,
    pub phase: BindingPhase,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    version: u8,
    identity: NativeSessionIdentity,
    fingerprint: String,
    key: [u8; 32],
}

impl Drop for Capability {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}

fn capability_name(identity: &NativeSessionIdentity) -> String {
    format!(
        "native-binding-{}-{}.key",
        identity.pid, identity.created_at_unix_ms
    )
}
fn receipt_name(identity: &NativeSessionIdentity) -> String {
    format!(
        "native-binding-{}-{}.json",
        identity.pid, identity.created_at_unix_ms
    )
}

/// Only exclusive creation yields this resource owner; deserializing a client
/// capability cannot. Cleanup is explicit so failures reach daemon diagnostics.
pub struct CapabilityOwner {
    directory: File,
    file: File,
    name: PathBuf,
}

impl CapabilityOwner {
    pub fn cleanup(self) -> io::Result<()> {
        storage::remove_private_file_if_same(&self.directory, &self.name, &self.file)
    }

    fn publish(mut self, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<Self> {
        let result = write(&mut self.file).and_then(|()| self.file.sync_all());
        #[cfg(unix)]
        let result = result.and_then(|()| self.directory.sync_all());
        if let Err(error) = result {
            return match self.cleanup() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(io::Error::other(format!(
                    "{error}; native binding capability cleanup failed: {cleanup}"
                ))),
            };
        }
        Ok(self)
    }
}

/// A single, non-recursive acquisition of a retired generation's lifetime lease.
/// Keeping its handle private prevents cleanup consumers from relocking it.
pub struct DeadGenerationLease(File);

impl DeadGenerationLease {
    pub fn acquire(file: File) -> io::Result<Self> {
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }

    pub fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self.0.metadata()
    }
}

/// Caller retains the exact generation's exclusive lifetime lease after exit.
/// A missing key is idempotent; foreign/corrupt/untrusted entries are not absence.
pub fn cleanup_dead_capability(
    directory: &File,
    identity: &NativeSessionIdentity,
    fingerprint: Option<&str>,
    lifetime_lease: Option<&DeadGenerationLease>,
) -> io::Result<()> {
    let name = PathBuf::from(capability_name(identity));
    let mut file = match storage::open_private_file(directory, &name, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    lifetime_lease.ok_or_else(invalid)?;
    if file.metadata()?.len() > 4096 {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(4097));
    Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
    let capability: Capability = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if capability.version != 1
        || capability.identity != *identity
        || Some(capability.fingerprint.as_str()) != fingerprint
    {
        return Err(invalid());
    }
    storage::remove_private_file_if_same(directory, &name, &file)
}

/// Created by the actual daemon while holding its original lifetime lease.
pub struct DaemonBinding {
    directory: File,
    root: PathBuf,
    capability: Capability,
    receipt: Option<Receipt>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request {
    Probe {},
    Bind { binding: Box<Binding> },
    Release { binding: Box<Binding> },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    supports_binding: bool,
    receipt: Option<Receipt>,
}

impl DaemonBinding {
    pub fn imported_remote_revoked(&self) -> bool {
        self.receipt.as_ref().is_some_and(|receipt| {
            matches!(
                receipt.phase,
                BindingPhase::Released | BindingPhase::Revoked
            )
        })
    }

    fn publish(&self, previous: Option<&Receipt>, next: &Receipt) -> io::Result<()> {
        let name = receipt_name(&next.binding.identity);
        let bytes = serde_json::to_vec(next).map_err(|_| invalid())?;
        match storage::read_private(&self.directory, Path::new(&name), LIMIT) {
            Ok(existing) => {
                let current: Receipt = serde_json::from_slice(&existing).map_err(|_| invalid())?;
                if current == *next {
                    return Ok(());
                }
                if Some(&current) != previous {
                    return Err(invalid());
                }
                storage::replace_private(&self.directory, Path::new(&name), &bytes)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && previous.is_none() => {
                storage::write_new_private(&self.directory, Path::new(&name), &bytes)
            }
            Err(error) => Err(error),
        }
    }

    fn recover_publication(
        &mut self,
        request: &Request,
        watch: &mut Option<DaemonWatch>,
    ) -> io::Result<()> {
        let bytes = match storage::read_private(
            &self.directory,
            Path::new(&receipt_name(&self.capability.identity)),
            LIMIT,
        ) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound && self.receipt.is_none() => {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        receipt.binding.validate()?;
        if let Some(previous) = &self.receipt {
            if receipt.version != 1
                || receipt.binding != previous.binding
                || receipt.phase < previous.phase
            {
                return Err(invalid());
            }
            match receipt.phase {
                BindingPhase::Released => *watch = None,
                BindingPhase::Revoked => watch.as_ref().ok_or_else(invalid)?.seal_binding(),
                _ => {}
            }
            self.receipt = Some(receipt);
            return Ok(());
        }
        if receipt.version != 1
            || receipt.binding.identity != self.capability.identity
            || receipt.binding.runtime_root != self.root
            || receipt.binding.spawn_fingerprint != self.capability.fingerprint
            || watch.is_some()
        {
            return Err(invalid());
        }
        if receipt.phase != BindingPhase::Released {
            if receipt.phase != BindingPhase::Prepared {
                return Err(invalid());
            }
            let candidate = DaemonWatch::open_binding(
                &self.root,
                &receipt.binding.identity.name,
                receipt.binding.identity.created_at_unix_ms,
            )?;
            if matches!(request, Request::Release { binding } if **binding == receipt.binding) {
                candidate.verify_released_binding(&receipt.binding)?;
            } else {
                let _source = receipt.binding.source_fence()?;
                candidate.verify_binding(&receipt.binding)?;
            }
            *watch = Some(candidate);
        }
        self.receipt = Some(receipt);
        Ok(())
    }

    pub fn create(
        root: &Path,
        identity: NativeSessionIdentity,
        fingerprint: String,
    ) -> io::Result<(Self, CapabilityOwner)> {
        let directory = storage::private_directory(root, false)?;
        let capability = Capability {
            version: 1,
            identity,
            fingerprint,
            key: random(),
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&capability).map_err(|_| invalid())?);
        let name = PathBuf::from(capability_name(&capability.identity));
        let owner = CapabilityOwner {
            directory: directory.try_clone()?,
            file: storage::open_private_file(&directory, &name, true)?,
            name,
        }
        .publish(|file| file.write_all(&bytes))?;
        Ok((
            Self {
                directory,
                root: root.into(),
                capability,
                receipt: None,
            },
            owner,
        ))
    }

    fn apply(&mut self, request: Request, watch: &mut Option<DaemonWatch>) -> io::Result<Reply> {
        if let Err(error) = self.recover_publication(&request, watch) {
            if !crate::lock_is_contended(&error) {
                if let Some(watch) = watch.as_ref() {
                    watch.seal_binding();
                }
            }
            return Err(error);
        }
        match request {
            Request::Probe {} => {}
            Request::Bind { binding } => {
                binding.validate()?;
                if binding.identity != self.capability.identity
                    || binding.runtime_root != self.root
                    || binding.spawn_fingerprint != self.capability.fingerprint
                {
                    return Err(invalid());
                }
                if let Some(previous) = &self.receipt {
                    if previous.binding != *binding
                        || matches!(
                            previous.phase,
                            BindingPhase::Released | BindingPhase::Revoked
                        )
                    {
                        return Err(invalid());
                    }
                } else {
                    if watch.is_some() {
                        return Err(invalid());
                    }
                    let _source = binding.source_fence()?;
                    let candidate = DaemonWatch::open_binding(
                        &self.root,
                        &binding.identity.name,
                        binding.identity.created_at_unix_ms,
                    )?;
                    candidate.verify_binding(&binding)?;
                    let receipt = Receipt {
                        version: 1,
                        binding: *binding,
                        phase: BindingPhase::Prepared,
                    };
                    self.publish(None, &receipt)?;
                    *watch = Some(candidate);
                    self.receipt = Some(receipt);
                }
            }
            Request::Release { binding } => {
                binding.validate()?;
                if binding.identity != self.capability.identity
                    || binding.runtime_root != self.root
                    || binding.spawn_fingerprint != self.capability.fingerprint
                {
                    return Err(invalid());
                }
                if self.receipt.is_none() {
                    if watch.is_some() {
                        return Err(invalid());
                    }
                    let receipt = Receipt {
                        version: 1,
                        binding: *binding.clone(),
                        phase: BindingPhase::Released,
                    };
                    self.publish(None, &receipt)?;
                    self.receipt = Some(receipt);
                }
                let receipt = self.receipt.as_ref().ok_or_else(invalid)?;
                if receipt.binding != *binding {
                    return Err(invalid());
                }
                if receipt.phase != BindingPhase::Released {
                    watch
                        .as_ref()
                        .ok_or_else(invalid)?
                        .verify_released_binding(&binding)?;
                    let mut next = receipt.clone();
                    next.phase = BindingPhase::Released;
                    self.publish(Some(receipt), &next)?;
                    self.receipt = Some(next);
                    *watch = None;
                }
            }
        }
        if let Some(receipt) = self.receipt.as_ref() {
            if !matches!(
                receipt.phase,
                BindingPhase::Released | BindingPhase::Revoked
            ) {
                let phase = if watch
                    .as_ref()
                    .ok_or_else(invalid)?
                    .binding_revoked(&receipt.binding)?
                {
                    BindingPhase::Revoked
                } else if watch
                    .as_ref()
                    .ok_or_else(invalid)?
                    .binding_active(&receipt.binding)?
                {
                    BindingPhase::Active
                } else {
                    BindingPhase::Prepared
                };
                if phase < receipt.phase {
                    watch.as_ref().ok_or_else(invalid)?.seal_binding();
                    return Err(invalid());
                }
                if receipt.phase != phase {
                    let mut next = receipt.clone();
                    next.phase = phase;
                    self.publish(Some(receipt), &next)?;
                    self.receipt = Some(next);
                }
            }
        }
        Ok(Reply {
            supports_binding: self.receipt.is_some() || watch.is_none(),
            receipt: self.receipt.clone(),
        })
    }

    /// Runs on the existing sole writer loop, but never enters recovery or
    /// replacement admission. The stream already has a bounded native timeout.
    pub fn serve(
        &mut self,
        header: &[u8; 29],
        stream: &mut (impl Read + Write),
        watch: &mut Option<DaemonWatch>,
    ) -> io::Result<()> {
        let mut bounded = Deadline {
            stream,
            until: std::time::Instant::now() + std::time::Duration::from_secs(2),
        };
        let stream = &mut bounded;
        if &header[..4] != MAGIC
            || header[4..8] != self.capability.identity.pid.to_be_bytes()
            || header[8..24] != self.capability.identity.created_at_unix_ms.to_be_bytes()
        {
            return Err(invalid());
        }
        let length = u32::from_be_bytes(header[24..28].try_into().map_err(|_| invalid())?) as usize;
        if length == 0 || length > LIMIT || header[28] != 0 {
            return Err(invalid());
        }
        let nonce = random();
        stream.write_all(&nonce)?;
        let mut bytes = vec![0; length];
        let mut signature = [0; 32];
        stream.read_exact(&mut signature)?;
        stream.read_exact(&mut bytes)?;
        verify(
            &self.capability.key,
            b"native-bind-request-v1",
            &nonce,
            &bytes,
            &signature,
        )?;
        let request: Request = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let reply = self.apply(request, watch)?;
        let bytes = serde_json::to_vec(&reply).map_err(|_| invalid())?;
        if bytes.len() > LIMIT {
            return Err(invalid());
        }
        stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
        stream.write_all(&tag(
            &self.capability.key,
            b"native-bind-reply-v1",
            &nonce,
            &bytes,
        ))?;
        stream.write_all(&bytes)
    }
}

fn exchange(
    root: &Path,
    identity: &NativeSessionIdentity,
    fingerprint: &str,
    request: &Request,
) -> io::Result<Option<Receipt>> {
    let directory = storage::private_directory(root, false)?;
    let bytes = storage::read_private(&directory, Path::new(&capability_name(identity)), 4096)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "native.binding_older_helper_defer",
                )
            } else {
                error
            }
        })?;
    let capability: Capability = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if capability.version != 1
        || capability.identity != *identity
        || capability.fingerprint != fingerprint
    {
        return Err(invalid());
    }
    crate::verify_spawn_fingerprint_in(root, identity, fingerprint).map_err(|_| invalid())?;
    #[cfg(unix)]
    let mut stream = {
        let stream = std::os::unix::net::UnixStream::connect(&identity.endpoint)?;
        stream.set_read_timeout(Some(std::time::Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
        stream
    };
    #[cfg(windows)]
    let mut stream = {
        let mut stream = festerm_windows_security::named_pipe::Pipe::connect(
            &identity.endpoint,
            std::time::Duration::from_secs(2),
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        stream.set_read_timeout(std::time::Duration::from_millis(100));
        stream.set_write_timeout(std::time::Duration::from_secs(2));
        stream
    };
    #[cfg(any(unix, windows))]
    {
        let mut stream = Deadline {
            stream: &mut stream,
            until: std::time::Instant::now() + std::time::Duration::from_secs(2),
        };
        let bytes = serde_json::to_vec(request).map_err(|_| invalid())?;
        if bytes.len() > LIMIT {
            return Err(invalid());
        }
        let mut header = [0; 29];
        header[..4].copy_from_slice(MAGIC);
        header[4..8].copy_from_slice(&identity.pid.to_be_bytes());
        header[8..24].copy_from_slice(&identity.created_at_unix_ms.to_be_bytes());
        header[24..28].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        stream.write_all(&header)?;
        let mut nonce = [0; 32];
        stream.read_exact(&mut nonce)?;
        stream.write_all(&tag(
            &capability.key,
            b"native-bind-request-v1",
            &nonce,
            &bytes,
        ))?;
        stream.write_all(&bytes)?;
        let mut length = [0; 4];
        stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > LIMIT {
            return Err(invalid());
        }
        let mut signature = [0; 32];
        let mut response = vec![0; length];
        stream.read_exact(&mut signature)?;
        stream.read_exact(&mut response)?;
        verify(
            &capability.key,
            b"native-bind-reply-v1",
            &nonce,
            &response,
            &signature,
        )?;
        let reply: Reply = serde_json::from_slice(&response).map_err(|_| invalid())?;
        if !reply.supports_binding {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native.binding_current_daemon_already_managed",
            ));
        }
        if let Some(receipt) = &reply.receipt {
            receipt.binding.validate()?;
            if receipt.version != 1
                || receipt.binding.identity != *identity
                || receipt.binding.runtime_root != root
            {
                return Err(invalid());
            }
        }
        Ok(reply.receipt)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = request;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native.binding_transport_unqualified",
        ))
    }
}

pub fn probe(
    root: &Path,
    identity: &NativeSessionIdentity,
    fingerprint: &str,
) -> io::Result<Option<Receipt>> {
    exchange(root, identity, fingerprint, &Request::Probe {})
}

pub(crate) fn bind(binding: &Binding) -> io::Result<Receipt> {
    exchange(
        &binding.runtime_root,
        &binding.identity,
        &binding.spawn_fingerprint,
        &Request::Bind {
            binding: Box::new(binding.clone()),
        },
    )?
    .ok_or_else(invalid)
}

pub(crate) fn release(binding: &Binding) -> io::Result<Receipt> {
    exchange(
        &binding.runtime_root,
        &binding.identity,
        &binding.spawn_fingerprint,
        &Request::Release {
            binding: Box::new(binding.clone()),
        },
    )?
    .ok_or_else(invalid)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn descriptor(root: &Path) -> Binding {
        Binding {
            operation: OperationId::new(),
            host: HostId::new(),
            installation: uuid::Uuid::new_v4().to_string(),
            account: "42".into(),
            host_key_fingerprint: "SHA256:fixture".into(),
            principal: PrincipalId::new(),
            owner_epoch: Epoch::new(1).unwrap(),
            host_epoch: Epoch::new(1).unwrap(),
            issuance_epoch: Epoch::new(1).unwrap(),
            namespace: OperationId::new(),
            control_root: root.join("control"),
            logical: LogicalSessionId::new(),
            source_installation: uuid::Uuid::new_v4().to_string(),
            source_host: HostId::new(),
            source_key_fingerprint: "SHA256:source".into(),
            source_receipt_digest: "0".repeat(64),
            source_root: root.join("source"),
            source_profile: true,
            source_installation_digest: "0".repeat(64),
            source_ownership_digest: "0".repeat(64),
            source_key_digest: "0".repeat(64),
            source_claim_digest: "0".repeat(64),
            runtime_root: root.into(),
            identity: NativeSessionIdentity {
                name: "fixture".into(),
                pid: std::process::id(),
                created_at_unix_ms: 123,
                endpoint: root.join("fixture.sock").to_string_lossy().into(),
            },
            spawn_fingerprint: format!("v1:{}", "a".repeat(64)),
        }
    }

    #[test]
    fn authenticated_binding_request_roundtrips_exact_native_generation_without_buffered_u128_loss()
    {
        let binding = descriptor(&std::env::current_dir().unwrap());
        binding.validate().unwrap();
        let bytes = serde_json::to_vec(&Request::Bind {
            binding: Box::new(binding.clone()),
        })
        .unwrap();
        let Request::Bind { binding: decoded } = serde_json::from_slice::<Request>(&bytes).unwrap()
        else {
            panic!("wrong action");
        };
        assert_eq!(*decoded, binding);
        assert!(serde_json::from_slice::<Request>(br#"{"Probe":{"takeover":true}}"#).is_err());
        assert!(serde_json::from_slice::<Request>(br#"{"Restart":{}}"#).is_err());
    }

    #[test]
    fn authenticated_binding_receipt_publication_resumes_exact_commit_and_never_rewrites_released_or_missing_state(
    ) {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/binding-receipt-{}", uuid::Uuid::new_v4()));
        storage::private_directory(&root, true).unwrap();
        let binding = descriptor(&root);
        let (mut daemon, owner) = DaemonBinding::create(
            &root,
            binding.identity.clone(),
            binding.spawn_fingerprint.clone(),
        )
        .unwrap();
        let prepared = Receipt {
            version: 1,
            binding,
            phase: BindingPhase::Prepared,
        };
        daemon.publish(None, &prepared).unwrap();
        daemon.publish(None, &prepared).unwrap();
        let released = Receipt {
            phase: BindingPhase::Released,
            ..prepared.clone()
        };
        daemon.publish(Some(&prepared), &released).unwrap();
        let mut watch = None;
        daemon
            .recover_publication(&Request::Probe {}, &mut watch)
            .unwrap();
        assert!(watch.is_none());
        assert_eq!(daemon.receipt.as_ref().unwrap(), &released);
        assert!(daemon.imported_remote_revoked());
        assert!(daemon.publish(Some(&prepared), &prepared).is_err());
        assert!(daemon
            .apply(
                Request::Bind {
                    binding: Box::new(prepared.binding.clone())
                },
                &mut watch
            )
            .is_err());
        std::fs::remove_file(root.join(receipt_name(&released.binding.identity))).unwrap();
        assert!(daemon.publish(Some(&released), &released).is_err());
        assert!(daemon
            .recover_publication(&Request::Probe {}, &mut watch)
            .is_err());
        owner.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authenticated_binding_probe_refuses_already_managed_daemon_without_changing_original_watch()
    {
        use crate::host_lease::{DaemonLease, LeaseGuard};
        use festerm_host_protocol::{GenerationId, Lifetime, LogicalState, SessionGrant};
        let root = std::env::current_dir()
            .unwrap()
            .join(format!("target/binding-managed-{}", uuid::Uuid::new_v4()));
        storage::private_directory(&root, true).unwrap();
        let binding = descriptor(&root);
        let generation = GenerationId::new(binding.identity.created_at_unix_ms).unwrap();
        LeaseGuard::reserve(&root, generation)
            .unwrap()
            .create(&DaemonLease {
                version: 1,
                name: binding.identity.name.clone(),
                generation,
                revision: 1,
                lifetime: Lifetime::Persistent {},
                temporary: None,
                deadline_ms: None,
                connected: false,
                stopped: false,
                remote_revoked: false,
                pending: None,
                last_attachment: None,
                writer_epoch: Epoch::new(1).unwrap(),
                native_pid: Some(std::process::id()),
                grant: SessionGrant {
                    owner: binding.principal.clone(),
                    logical: binding.logical.clone(),
                    revision: Epoch::new(1).unwrap(),
                    remote_access: true,
                    state: LogicalState::Running(generation),
                },
            })
            .unwrap();
        let mut watch =
            Some(DaemonWatch::open(&root, &binding.identity.name, generation.get()).unwrap());
        let (mut daemon, owner) = DaemonBinding::create(
            &root,
            binding.identity.clone(),
            binding.spawn_fingerprint.clone(),
        )
        .unwrap();
        let reply = daemon.apply(Request::Probe {}, &mut watch).unwrap();
        assert!(!reply.supports_binding);
        assert!(reply.receipt.is_none());
        assert!(daemon
            .apply(
                Request::Bind {
                    binding: Box::new(binding)
                },
                &mut watch
            )
            .is_err());
        assert!(!watch.as_mut().unwrap().should_stop());
        assert!(daemon.receipt.is_none());
        owner.cleanup().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn authenticated_binding_mac_rejects_replay_foreign_key_domain_and_payload() {
        let signature = tag(&[1; 32], b"native-bind-request-v1", &[2; 32], b"bind");
        assert!(verify(
            &[1; 32],
            b"native-bind-request-v1",
            &[2; 32],
            b"bind",
            &signature
        )
        .is_ok());
        for (key, domain, nonce, body) in [
            (
                [3; 32],
                b"native-bind-request-v1".as_slice(),
                [2; 32],
                b"bind".as_slice(),
            ),
            (
                [1; 32],
                b"native-bind-reply-v1".as_slice(),
                [2; 32],
                b"bind".as_slice(),
            ),
            (
                [1; 32],
                b"native-bind-request-v1".as_slice(),
                [4; 32],
                b"bind".as_slice(),
            ),
            (
                [1; 32],
                b"native-bind-request-v1".as_slice(),
                [2; 32],
                b"release".as_slice(),
            ),
        ] {
            assert!(verify(&key, domain, &nonce, body, &signature).is_err());
        }
    }

    #[test]
    fn authenticated_binding_capability_owner_cleanup_preserves_live_readers_and_release_tombstone()
    {
        let root = storage::private_test_directory();
        let binding = descriptor(root.path());
        let (daemon, owner) = DaemonBinding::create(
            root.path(),
            binding.identity.clone(),
            binding.spawn_fingerprint.clone(),
        )
        .unwrap();
        let name = PathBuf::from(capability_name(&binding.identity));
        let bytes = storage::read_private(&daemon.directory, &name, 4096).unwrap();
        let reader: Capability = serde_json::from_slice(&bytes).unwrap();
        drop(reader);
        assert!(root.path().join(&name).exists());
        let released = Receipt {
            version: 1,
            binding,
            phase: BindingPhase::Released,
        };
        daemon.publish(None, &released).unwrap();
        assert!(
            root.path().join(&name).exists(),
            "live release must preserve capability"
        );
        owner.cleanup().unwrap();
        assert!(!root.path().join(&name).exists());
        assert!(root
            .path()
            .join(receipt_name(&released.binding.identity))
            .exists());
    }

    #[test]
    fn authenticated_binding_capability_failed_publication_cleans_partial_owned_key_and_reports_failure(
    ) {
        let root = storage::private_test_directory();
        let directory = storage::private_directory(root.path(), false).unwrap();
        let name = PathBuf::from("partial.key");
        let owner = CapabilityOwner {
            directory: directory.try_clone().unwrap(),
            file: storage::open_private_file(&directory, &name, true).unwrap(),
            name: name.clone(),
        };
        let error = owner
            .publish(|file| {
                file.write_all(b"partial-secret")?;
                Err(io::Error::other("fixture publication failure"))
            })
            .err()
            .unwrap();
        assert!(error.to_string().contains("fixture publication failure"));
        assert!(!root.path().join(&name).exists());

        let owner = CapabilityOwner {
            directory: directory.try_clone().unwrap(),
            file: storage::open_private_file(&directory, &name, true).unwrap(),
            name: name.clone(),
        };
        let error = owner
            .publish(|_| {
                storage::replace_private(&directory, &name, b"foreign")?;
                Err(io::Error::other("fixture publication failure"))
            })
            .err()
            .unwrap();
        assert!(error.to_string().contains("fixture publication failure"));
        assert!(error.to_string().contains("cleanup failed"));
        assert_eq!(
            &*storage::read_private(&directory, &name, 4096).unwrap(),
            b"foreign"
        );
    }

    #[test]
    fn authenticated_binding_capability_cleanup_refuses_replaced_foreign_corrupt_and_redirected_keys(
    ) {
        for replacement in [
            "same-bytes",
            "foreign",
            "corrupt",
            "redirected",
            "untrusted",
        ] {
            let root = storage::private_test_directory();
            let binding = descriptor(root.path());
            let (daemon, owner) = DaemonBinding::create(
                root.path(),
                binding.identity.clone(),
                binding.spawn_fingerprint.clone(),
            )
            .unwrap();
            let name = PathBuf::from(capability_name(&binding.identity));
            let lifetime = DeadGenerationLease::acquire(
                storage::open_private_file(&daemon.directory, Path::new("lifetime"), true).unwrap(),
            )
            .unwrap();
            let bytes = storage::read_private(&daemon.directory, &name, 4096).unwrap();
            match replacement {
                "same-bytes" => storage::replace_private(&daemon.directory, &name, &bytes).unwrap(),
                "foreign" => {
                    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    value["identity"]["created_at_unix_ms"] = serde_json::json!(999);
                    storage::replace_private(
                        &daemon.directory,
                        &name,
                        &serde_json::to_vec(&value).unwrap(),
                    )
                    .unwrap();
                }
                "corrupt" => {
                    storage::replace_private(&daemon.directory, &name, b"corrupt").unwrap()
                }
                #[cfg(unix)]
                "redirected" => {
                    std::fs::rename(root.path().join(&name), root.path().join("sentinel")).unwrap();
                    std::os::unix::fs::symlink("sentinel", root.path().join(&name)).unwrap();
                }
                #[cfg(unix)]
                "untrusted" => {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(
                        root.path().join(&name),
                        std::fs::Permissions::from_mode(0o644),
                    )
                    .unwrap();
                }
                _ => continue,
            }
            assert!(owner.cleanup().is_err(), "{replacement}");
            assert!(std::fs::symlink_metadata(root.path().join(&name)).is_ok());
            if replacement != "same-bytes" {
                assert!(
                    cleanup_dead_capability(
                        &daemon.directory,
                        &binding.identity,
                        Some(&binding.spawn_fingerprint),
                        Some(&lifetime)
                    )
                    .is_err(),
                    "{replacement}"
                );
            }
            assert!(std::fs::symlink_metadata(root.path().join(&name)).is_ok());
        }
    }

    #[test]
    fn authenticated_binding_capability_dead_cleanup_requires_exact_identity_fingerprint_and_dead_lease(
    ) {
        let root = storage::private_test_directory();
        let binding = descriptor(root.path());
        let (daemon, owner) = DaemonBinding::create(
            root.path(),
            binding.identity.clone(),
            binding.spawn_fingerprint.clone(),
        )
        .unwrap();
        let name = PathBuf::from(capability_name(&binding.identity));
        let lease =
            storage::open_private_file(&daemon.directory, Path::new("lifetime"), true).unwrap();
        let mut other_identity = binding.identity.clone();
        other_identity.created_at_unix_ms += 1;
        let (_other_daemon, other_owner) = DaemonBinding::create(
            root.path(),
            other_identity.clone(),
            binding.spawn_fingerprint.clone(),
        )
        .unwrap();
        lease.lock_exclusive().unwrap();
        let observer =
            storage::open_private_file(&daemon.directory, Path::new("lifetime"), false).unwrap();
        assert!(DeadGenerationLease::acquire(observer).is_err());
        assert!(cleanup_dead_capability(
            &daemon.directory,
            &binding.identity,
            Some(&binding.spawn_fingerprint),
            None
        )
        .is_err());
        assert!(root.path().join(&name).exists());
        drop(lease);
        let observer = DeadGenerationLease::acquire(
            storage::open_private_file(&daemon.directory, Path::new("lifetime"), false).unwrap(),
        )
        .unwrap();
        assert!(cleanup_dead_capability(
            &daemon.directory,
            &binding.identity,
            Some("foreign"),
            Some(&observer)
        )
        .is_err());
        assert!(root.path().join(&name).exists());
        cleanup_dead_capability(
            &daemon.directory,
            &binding.identity,
            Some(&binding.spawn_fingerprint),
            Some(&observer),
        )
        .unwrap();
        assert!(!root.path().join(&name).exists());
        assert!(root.path().join(capability_name(&other_identity)).exists());
        cleanup_dead_capability(
            &daemon.directory,
            &binding.identity,
            Some(&binding.spawn_fingerprint),
            Some(&observer),
        )
        .unwrap();
        owner.cleanup().unwrap();
        other_owner.cleanup().unwrap();
    }

    #[test]
    fn authenticated_binding_rejects_unknown_protocol_fields_and_actions() {
        assert!(
            serde_json::from_slice::<Request>(br#"{"action":"Probe","takeover":true}"#).is_err()
        );
        assert!(serde_json::from_slice::<Request>(br#"{"action":"Restart"}"#).is_err());
        assert!(serde_json::from_slice::<Request>(br#"{"action":"Bind"}"#).is_err());
    }

    #[test]
    fn authenticated_binding_deadline_expires_without_reading_or_writing() {
        let mut cursor = io::Cursor::new(vec![1]);
        let mut stream = Deadline {
            stream: &mut cursor,
            until: std::time::Instant::now(),
        };
        assert_eq!(
            stream.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            stream.write(&[2]).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(cursor.position(), 0);
    }
}
