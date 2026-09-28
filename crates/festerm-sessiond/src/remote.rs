use std::collections::BTreeSet;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use festerm_session::SessionEventNotifier;
use festerm_ssh::{
    PersistentSessionName, SshAuthentication, SshConnectionProfile, SshRawExecOptions,
    SshRawExecSession,
};
use serde::Deserialize;

use crate::{
    DiscoveredSession, DiscoveryInventory, DiscoveryStatus, PersistentSession,
    PersistentSessionError, MACHINE_READABLE_SCHEMA_VERSION, MAX_DISCOVERY_RECORD_COUNT,
    PROTOCOL_VERSION, RECOVERY_SNAPSHOT_SCHEMA_VERSION,
};

const DISCOVERY_DEADLINE: Duration = Duration::from_secs(30);
const MAX_DISCOVERY_BYTES: usize = 512 * 1024;

/// The authenticated SSH destination and helper used for both discovery and attach.
/// A fingerprint must be verified independently before constructing this endpoint.
#[derive(Clone, Debug)]
pub struct RemoteSshEndpoint {
    profile: SshConnectionProfile,
    fingerprint: String,
    helper: String,
}

impl RemoteSshEndpoint {
    pub fn new(
        profile: SshConnectionProfile,
        fingerprint: impl Into<String>,
        helper: impl Into<String>,
    ) -> Result<Self, PersistentSessionError> {
        let fingerprint = fingerprint.into();
        let helper = helper.into();
        SshRawExecOptions::new()
            .with_known_host_fingerprint(fingerprint.clone())
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        // SSH exec still invokes the server's configured shell. Restrict the
        // helper to a command token usable unchanged with POSIX, cmd and pwsh.
        if helper.is_empty()
            || helper.len() > 128
            || !helper.as_bytes()[0].is_ascii_alphanumeric()
            || !helper
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(PersistentSessionError::new(
                "remote helper must be a simple executable name on the remote PATH",
            ));
        }
        Ok(Self {
            profile,
            fingerprint,
            helper,
        })
    }

    pub fn profile(&self) -> &SshConnectionProfile {
        &self.profile
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn helper(&self) -> &str {
        &self.helper
    }

    fn open(
        &self,
        authentication: SshAuthentication,
        command: String,
    ) -> Result<SshRawExecSession, PersistentSessionError> {
        let options = SshRawExecOptions::new()
            .with_known_host_fingerprint(self.fingerprint.clone())
            .map_err(|error| PersistentSessionError::new(error.to_string()))?;
        SshRawExecSession::connect(self.profile.clone(), authentication, command, options).map_err(
            |error| PersistentSessionError::new(format!("remote SSH connection failed: {error}")),
        )
    }

    /// Performs bounded, read-only discovery. Call on a background worker.
    pub fn discover(
        &self,
        authentication: SshAuthentication,
    ) -> Result<RemoteSessionInventory, PersistentSessionError> {
        self.discover_with_cancellation(authentication, Arc::new(AtomicBool::new(false)))
    }

    /// Performs bounded, read-only discovery until completion, deadline, size
    /// limit, transport failure, or caller cancellation.
    ///
    /// Cancellation is cooperative with the raw-exec stream timeout: once the
    /// flag is observed, the exec stream is dropped and no further bytes are
    /// read from the helper.
    pub fn discover_with_cancellation(
        &self,
        authentication: SshAuthentication,
        cancelled: Arc<AtomicBool>,
    ) -> Result<RemoteSessionInventory, PersistentSessionError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(PersistentSessionError::new(
                "remote discovery was cancelled",
            ));
        }
        let mut stream = self.open(authentication, format!("{} discover --json", self.helper))?;
        if cancelled.load(Ordering::Acquire) {
            drop(stream);
            return Err(PersistentSessionError::new(
                "remote discovery was cancelled",
            ));
        }
        let bytes = read_discovery_with_cancellation(
            &mut stream,
            Instant::now() + DISCOVERY_DEADLINE,
            || cancelled.load(Ordering::Acquire),
        )?;
        RemoteSessionInventory::decode(self.clone(), &bytes)
    }
}

/// A bounded inventory scoped to the same SSH host, account, key and source policy.
#[derive(Clone, Debug)]
pub struct RemoteSessionInventory {
    endpoint: RemoteSshEndpoint,
    sessions: Vec<DiscoveredSession>,
}

impl RemoteSessionInventory {
    pub fn decode(
        endpoint: RemoteSshEndpoint,
        bytes: &[u8],
    ) -> Result<Self, PersistentSessionError> {
        #[derive(Deserialize)]
        struct WireInventory {
            schema_version: u32,
            inventory: DiscoveryInventory,
            sessions: Vec<DiscoveredSession>,
        }
        if bytes.len() > MAX_DISCOVERY_BYTES {
            return Err(PersistentSessionError::new(
                "remote discovery exceeds the byte limit",
            ));
        }
        let wire: WireInventory = serde_json::from_slice(bytes).map_err(|_| {
            PersistentSessionError::new("invalid remote session discovery document")
        })?;
        if wire.schema_version != MACHINE_READABLE_SCHEMA_VERSION
            || wire.sessions.len() > MAX_DISCOVERY_RECORD_COUNT
            || wire.inventory.record_count != wire.sessions.len()
        {
            return Err(PersistentSessionError::new(
                "remote discovery schema or record count is unsupported",
            ));
        }
        let mut names = BTreeSet::new();
        for session in &wire.sessions {
            if !names.insert(session.name.as_str()) {
                return Err(PersistentSessionError::new(
                    "duplicate remote session identity",
                ));
            }
        }
        Ok(Self {
            endpoint,
            sessions: wire.sessions,
        })
    }

    pub fn sessions(&self) -> &[DiscoveredSession] {
        &self.sessions
    }

    pub fn endpoint(&self) -> &RemoteSshEndpoint {
        &self.endpoint
    }

    pub fn select(&self, name: &str) -> Result<RemoteSessionTarget, PersistentSessionError> {
        let session = self
            .sessions
            .iter()
            .find(|session| session.name == name)
            .ok_or_else(|| {
                PersistentSessionError::new("selected remote session was not discovered")
            })?;
        PersistentSessionName::new(session.name.clone())
            .map_err(|_| PersistentSessionError::new("invalid remote session name"))?;
        if !session.validated_name
            || !matches!(
                session.status,
                DiscoveryStatus::Available | DiscoveryStatus::Attached
            )
            || session.daemon_protocol.version != Some(PROTOCOL_VERSION)
            || session.recovery_snapshot_schema.version != Some(RECOVERY_SNAPSHOT_SCHEMA_VERSION)
            || !session.daemon_protocol.supported
            || !session.recovery_snapshot_schema.supported
        {
            return Err(PersistentSessionError::new(
                "selected remote session is unavailable or has incompatible recovery metadata",
            ));
        }
        let pid = session
            .pid
            .filter(|pid| *pid > 0)
            .ok_or_else(|| PersistentSessionError::new("missing remote process identity"))?;
        let generation = session
            .created_at_unix_ms
            .filter(|generation| *generation > 0)
            .ok_or_else(|| PersistentSessionError::new("missing remote generation identity"))?;
        Ok(RemoteSessionTarget {
            endpoint: self.endpoint.clone(),
            name: session.name.clone(),
            pid,
            generation,
        })
    }
}

/// An exact discovered generation, never a request to start or replace a shell.
#[derive(Clone, Debug)]
pub struct RemoteSessionTarget {
    endpoint: RemoteSshEndpoint,
    name: String,
    pid: u32,
    generation: u128,
}

impl RemoteSessionTarget {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn generation(&self) -> u128 {
        self.generation
    }

    pub fn endpoint(&self) -> &RemoteSshEndpoint {
        &self.endpoint
    }

    fn bridge_command(&self) -> String {
        format!(
            "{} bridge --name {} --pid {} --generation {} --protocol {} --snapshot-schema {} --allow-takeover",
            self.endpoint.helper, self.name, self.pid, self.generation,
            PROTOCOL_VERSION, RECOVERY_SNAPSHOT_SCHEMA_VERSION,
        )
    }

    /// Explicitly authorizes takeover of this generation. Adoption remains
    /// transactional: obtain and install `take_recovered_terminal()` before
    /// sending input. Disconnect/drop detaches, and reconnect is never automatic.
    pub fn attach_with_takeover(
        &self,
        authentication: SshAuthentication,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<PersistentSession, PersistentSessionError> {
        let stream = self.endpoint.open(authentication, self.bridge_command())?;
        PersistentSession::from_stream_with_protocol(
            Box::new(stream),
            notifier,
            None,
            PROTOCOL_VERSION,
            RECOVERY_SNAPSHOT_SCHEMA_VERSION,
        )
    }
}

#[cfg(test)]
fn read_discovery(
    stream: &mut impl Read,
    deadline: Instant,
) -> Result<Vec<u8>, PersistentSessionError> {
    read_discovery_with_cancellation(stream, deadline, || false)
}

fn read_discovery_with_cancellation(
    stream: &mut impl Read,
    deadline: Instant,
    mut cancelled: impl FnMut() -> bool,
) -> Result<Vec<u8>, PersistentSessionError> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        if cancelled() {
            return Err(PersistentSessionError::new(
                "remote discovery was cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(PersistentSessionError::new(
                "remote discovery deadline exceeded",
            ));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                if cancelled() {
                    return Err(PersistentSessionError::new(
                        "remote discovery was cancelled",
                    ));
                }
                if bytes.len().saturating_add(count) > MAX_DISCOVERY_BYTES {
                    return Err(PersistentSessionError::new(
                        "remote discovery exceeds the byte limit",
                    ));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                return Err(PersistentSessionError::new(format!(
                    "remote discovery failed: {error}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_session::TerminalSize;
    use serde_json::json;

    fn endpoint() -> RemoteSshEndpoint {
        RemoteSshEndpoint::new(
            SshConnectionProfile::new(
                festerm_ssh::HostIdentity::new("localhost", 22).unwrap(),
                "fixture",
                "xterm-256color",
                TerminalSize::new(80, 24).unwrap(),
            )
            .unwrap(),
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "festerm-sessiond",
        )
        .unwrap()
    }

    fn document() -> serde_json::Value {
        json!({
            "schema_version": 1,
            "inventory": {"record_count": 1, "serialized_bytes": 300, "status_counts": {"available": 1}},
            "sessions": [{
                "name": "existing", "validated_name": true, "pid": 42,
                "created_at_unix_ms": 123456, "attached": false, "status": "available",
                "daemon_protocol": {"version": 2, "supported": true},
                "recovery_snapshot_schema": {"version": 2, "supported": true}
            }]
        })
    }

    #[test]
    fn remote_target_pins_generation_and_requires_explicit_takeover_command() {
        let inventory =
            RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&document()).unwrap())
                .unwrap();
        let target = inventory.select("existing").unwrap();
        assert_eq!(target.bridge_command(),
            "festerm-sessiond bridge --name existing --pid 42 --generation 123456 --protocol 2 --snapshot-schema 2 --allow-takeover");
        assert_eq!(
            target.endpoint().fingerprint(),
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert!(inventory.select("missing").is_err());
    }

    #[test]
    fn remote_inventory_refuses_unavailable_incompatible_or_invalid_generations() {
        for (key, value) in [
            ("pid", json!(0)),
            ("created_at_unix_ms", json!(null)),
            ("status", json!("stale")),
            ("status", json!("incompatible_protocol")),
            ("name", json!("session;whoami")),
            ("validated_name", json!(false)),
            ("daemon_protocol", json!({"version": 1, "supported": true})),
            (
                "recovery_snapshot_schema",
                json!({"version": 99, "supported": true}),
            ),
        ] {
            let mut wire = document();
            wire["sessions"][0][key] = value;
            let inventory =
                RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&wire).unwrap())
                    .unwrap();
            assert!(
                inventory
                    .select(inventory.sessions()[0].name.as_str())
                    .is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn remote_inventory_checks_schema_size_count_and_duplicate_identity() {
        let mut wire = document();
        wire["schema_version"] = json!(2);
        assert!(
            RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&wire).unwrap())
                .is_err()
        );
        let mut wire = document();
        wire["inventory"]["record_count"] = json!(2);
        assert!(
            RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&wire).unwrap())
                .is_err()
        );
        let original = wire["sessions"][0].clone();
        wire["sessions"].as_array_mut().unwrap().push(original);
        assert!(
            RemoteSessionInventory::decode(endpoint(), &serde_json::to_vec(&wire).unwrap())
                .is_err()
        );
        assert!(read_discovery(
            &mut io::repeat(b'x'),
            Instant::now() + Duration::from_secs(5)
        )
        .is_err());
        assert!(read_discovery(&mut io::empty(), Instant::now()).is_err());
    }

    #[test]
    fn remote_discovery_read_loop_observes_caller_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut bytes = io::Cursor::new(br#"{"unterminated":"#);
        let first = Arc::clone(&cancelled);
        assert!(read_discovery_with_cancellation(
            &mut bytes,
            Instant::now() + Duration::from_secs(5),
            move || first.swap(true, Ordering::AcqRel),
        )
        .is_err());

        let mut bytes = io::Cursor::new(br#"{}"#);
        let already = Arc::new(AtomicBool::new(true));
        assert!(read_discovery_with_cancellation(
            &mut bytes,
            Instant::now() + Duration::from_secs(5),
            move || already.load(Ordering::Acquire),
        )
        .is_err());
    }

    #[test]
    fn remote_helper_is_a_single_portable_executable_token() {
        for helper in [
            "",
            "../helper",
            "helper.exe & whoami",
            "$(whoami)",
            "-option",
            "helper\n",
        ] {
            let valid = endpoint();
            assert!(RemoteSshEndpoint::new(valid.profile, valid.fingerprint, helper).is_err());
        }
    }
}
