//! Operator-controlled native evidence only; never changes the owner policy.
use super::*;
use festerm_tunnel_bridge::file_client;
use festerm_tunnel_bridge::file_protocol::HostScope;
use serde::Serialize;
use std::{fs::File, io::Write, time::SystemTime};

const MAX_MS: u64 = 600_000;
const MAX_READS: u32 = 300;
const CANARY: &[u8] = b"fesTerm public boundary canary\n";

fn require_canary(bytes: &[u8]) {
    assert!(bytes == CANARY, "public canary bytes mismatch");
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevocationFixture {
    files: FilesBoundaryFixture,
    nonce: String,
    deadline_unix_ms: u64,
    off_scope: HostScope,
}

impl RevocationFixture {
    fn valid(&self, now: u64) -> bool {
        self.files.valid()
            && self.nonce.len() == 32
            && self
                .nonce
                .bytes()
                .all(|v| v.is_ascii_hexdigit() && !v.is_ascii_uppercase())
            && self
                .deadline_unix_ms
                .checked_sub(now)
                .is_some_and(|v| v > 0 && v <= MAX_MS)
            && self.off_scope.validate().is_ok()
            && self.off_scope.host == self.files.scope.host
            && self.off_scope.installation == self.files.scope.installation
            && self
                .files
                .scope
                .epoch
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_add(2))
                == self.off_scope.epoch.parse::<u64>().ok()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema_version: u32,
    nonce: String,
    original_scope: HostScope,
    off_scope: HostScope,
    stage: String,
    unix_ms: u64,
}

impl Marker {
    fn valid(&self, fixture: &RevocationFixture, stage: &str, earliest: u64, now: u64) -> bool {
        self.schema_version == 1
            && self.nonce == fixture.nonce
            && self.original_scope == fixture.files.scope
            && self.off_scope == fixture.off_scope
            && self.stage == stage
            && earliest <= self.unix_ms
            && self.unix_ms <= now
            && now < fixture.deadline_unix_ms
    }
}

fn now_ms() -> u64 {
    require(
        require(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH))
            .as_millis()
            .try_into(),
    )
}

/// Retain the existing nofollow/private directory capability for every marker.
struct Controls(File);
impl Controls {
    fn open(record: &Path) -> Self {
        use festerm_unix_security as security;
        let mut directory = require(File::open("/"));
        for component in record.parent().unwrap_or_else(|| blocked()).components() {
            match component {
                std::path::Component::Normal(part) => {
                    directory = require(security::open_directory_read_nofollow(
                        &directory,
                        Path::new(part),
                    ));
                    require(security::trusted_private_storage_ancestor(&directory));
                }
                std::path::Component::RootDir => {}
                _ => blocked(),
            }
        }
        assert!(require(security::is_current_user_only(&directory, true)));
        Self(directory)
    }
    fn read(&self, name: &str) -> Option<Marker> {
        use festerm_unix_security as security;
        let mut file = match security::open_private_file(&self.0, Path::new(name), false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) => blocked(),
        };
        assert!(require(security::is_current_user_only(&file, false)));
        let mut bytes = Vec::new();
        require(Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes));
        assert!(bytes.len() <= 4096);
        Some(require(serde_json::from_slice(&bytes)))
    }
    fn require_absent(&self, name: &str) {
        match festerm_unix_security::open_private_file(&self.0, Path::new(name), false) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => blocked(),
        }
    }
    fn write(&self, name: &str, value: &impl Serialize) {
        use festerm_unix_security as security;
        let bytes = require(serde_json::to_vec(value));
        assert!(bytes.len() <= 4096);
        let mut file = require(security::open_private_file(&self.0, Path::new(name), true));
        assert!(require(security::is_current_user_only(&file, false)));
        require(file.write_all(&bytes));
        require(file.sync_all());
    }
}

#[derive(Default)]
struct Reads {
    count: u32,
    failed: bool,
}
impl Reads {
    fn begin(&mut self, before_deadline: bool) -> bool {
        if self.failed || !before_deadline || self.count >= MAX_READS {
            return false;
        }
        self.count += 1;
        true
    }
    fn first_failure(&mut self, issued_before_request: bool) -> bool {
        self.failed = true;
        issued_before_request
    }
}

// Finite classification only: never Display/Debug errors, recovery paths or payloads.
fn failure_kind(error: &file_client::Error) -> &'static str {
    match error {
        file_client::Error::Closed => "closed",
        file_client::Error::Cancelled => "cancelled",
        file_client::Error::Deadline => "deadline",
        file_client::Error::Transport(_) => "transport",
        file_client::Error::Remote {
            code: festerm_tunnel_bridge::file_protocol::ErrorCode::Denied,
            ..
        } => "remote_denied",
        file_client::Error::Remote { .. } => "remote_other",
        _ => "other",
    }
}

fn retirement_refusal(error: &file_client::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error,
        file_client::Error::Closed
            | file_client::Error::Transport(
                ErrorKind::UnexpectedEof
                    | ErrorKind::BrokenPipe
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::NotConnected
            )
            | file_client::Error::Remote {
                code: festerm_tunnel_bridge::file_protocol::ErrorCode::Denied,
                ..
            }
    )
}

#[test]
fn files_revocation_control_rejects_stale_wrong_scope_and_unbounded_fixture() {
    let scope = HostScope {
        host: Default::default(),
        installation: Default::default(),
        epoch: "83".into(),
    };
    let off_scope = HostScope {
        epoch: "85".into(),
        ..scope.clone()
    };
    let raw = serde_json::json!({"files":{"schema_version":1,"account":"fes-festerm1",
        "configuration":"/private/config.toml","runtime_root":"/private/runtime",
        "locator":"fixture.region","scope":scope,
        "owned_leaf":"festerm-boundary-0123456789abcdef","allow_direct_server_probes":true,"existing_ready_keys_verified":true},
        "nonce":"0123456789abcdef0123456789abcdef","deadline_unix_ms":601000,
        "off_scope":off_scope});
    let mut fixture: RevocationFixture = serde_json::from_value(raw.clone()).unwrap();
    assert!(fixture.valid(1000));
    assert!(!fixture.valid(999));
    assert!(!fixture.valid(601000));
    let marker = |stage: &str, at| Marker {
        schema_version: 1,
        nonce: fixture.nonce.clone(),
        original_scope: fixture.files.scope.clone(),
        off_scope: fixture.off_scope.clone(),
        stage: stage.into(),
        unix_ms: at,
    };
    assert!(marker("disable_submitted", 2000).valid(&fixture, "disable_submitted", 1500, 2000));
    assert!(!marker("disable_submitted", 1499).valid(&fixture, "disable_submitted", 1500, 2000));
    assert!(!marker("disable_submitted", 2001).valid(&fixture, "disable_submitted", 1500, 2000));
    assert!(!marker("off_settled", 2000).valid(&fixture, "disable_submitted", 1500, 2000));
    let mut wrong = marker("disable_submitted", 2000);
    wrong.nonce = "f".repeat(32);
    assert!(!wrong.valid(&fixture, "disable_submitted", 1500, 2000));
    wrong.nonce = fixture.nonce.clone();
    wrong.off_scope.epoch = "87".into();
    assert!(!wrong.valid(&fixture, "disable_submitted", 1500, 2000));
    wrong.off_scope = fixture.off_scope.clone();
    wrong.original_scope.epoch = "82".into();
    assert!(!wrong.valid(&fixture, "disable_submitted", 1500, 2000));
    let mut malformed_marker = serde_json::to_value(marker("disable_submitted", 2000)).unwrap();
    malformed_marker["command"] = serde_json::json!("anything");
    assert!(serde_json::from_value::<Marker>(malformed_marker).is_err());
    fixture.off_scope.epoch = "84".into();
    assert!(!fixture.valid(1000));
    let mut malformed = raw;
    malformed["command"] = serde_json::json!("anything");
    assert!(serde_json::from_value::<RevocationFixture>(malformed).is_err());
}

#[test]
fn files_revocation_canary_mismatch_diagnostic_is_content_free() {
    require_canary(CANARY);
    let panic = std::panic::catch_unwind(|| require_canary(b"unexpected private remote bytes"))
        .expect_err("mismatched canary must refuse");
    let text = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str));
    assert!(
        text == Some("public canary bytes mismatch"),
        "content-free diagnostic required"
    );
}

#[test]
fn files_revocation_reads_stop_after_first_failure_deadline_or_count() {
    let mut reads = Reads::default();
    assert!(reads.begin(true));
    assert!(!reads.first_failure(false));
    assert!(!reads.begin(true));
    let mut reads = Reads::default();
    assert!(reads.begin(true));
    assert!(reads.first_failure(true));
    assert!(!reads.begin(true));
    let mut reads = Reads::default();
    assert!(!reads.begin(false));
    assert_eq!(reads.count, 0);
    for _ in 0..MAX_READS {
        assert!(reads.begin(true));
    }
    assert!(!reads.begin(true));
    assert_eq!(reads.count, MAX_READS);
    assert!(retirement_refusal(&file_client::Error::Closed));
    assert!(retirement_refusal(&file_client::Error::Transport(
        std::io::ErrorKind::UnexpectedEof
    )));
    for error in [
        file_client::Error::Deadline,
        file_client::Error::Cancelled,
        file_client::Error::Protocol,
        file_client::Error::Invalid,
        file_client::Error::Transport(std::io::ErrorKind::TimedOut),
    ] {
        assert!(!retirement_refusal(&error));
    }
}

#[test]
#[ignore = "native owner Disable/FilesOFF/restore plus same-account Files stream; operator-controlled only"]
fn controlled_github_files_live_policy_revocation() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "native SDK client is Linux-only"
    );
    assert!(
        std::env::var("FESTERM_RUN_DEV_TUNNEL_FILES_REVOCATION").as_deref() == Ok("1"),
        "explicit revocation opt-in required"
    );
    let record = PathBuf::from(
        std::env::var_os("FESTERM_DEV_TUNNEL_FILES_REVOCATION_RECORD").unwrap_or_else(|| blocked()),
    );
    let fixture: RevocationFixture = require(serde_json::from_slice(&private_bytes(&record)));
    let started = now_ms();
    assert!(fixture.valid(started));
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(fixture.deadline_unix_ms - started);
    let controls = Controls::open(&record);
    for name in [
        "ready.json",
        "disable-submitted.json",
        "off-settled.json",
        "first-failure.json",
    ] {
        controls.require_absent(name);
    }
    assert!(
        std::env::var_os("FESTERM_SESSIOND_RUNTIME_ROOT").map(PathBuf::from)
            == Some(fixture.files.runtime_root.clone()),
        "exact configured runtime root required"
    );
    let (identity, guard) = native_account(&fixture.files.configuration, &fixture.files.account);
    let runtime = require(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
    );
    runtime.block_on(async {
        let result = tokio::time::timeout_at(deadline, async {
            let cancel = CancellationToken::new();
            require(identity.confirm_account(&require(Https::new()), &cancel).await);
            let hosts = require(service::list_hosts(&identity, &cancel).await);
            assert!(hosts.iter().any(|host| host.locator == fixture.files.locator));
            let mut files = require(ordinary::open_files_reviewed(
                &identity,
                &fixture.files.locator,
                require(OrdinaryKeyIndex::native()),
                &host_client::Budgets::default(),
                guard.clone(),
                &cancel,
                Some(&fixture.files.scope),
            ).await).unwrap_or_else(|| blocked());
            assert!(files.scope() == &fixture.files.scope);
            assert!(files.client().capabilities().write && files.delete_advertised());
            let canary = format!("/{}/canary.txt", fixture.files.owned_leaf);
            let (handle, metadata) = require(files.client().open_read(&canary).await);
            assert!(metadata.size == CANARY.len().to_string(), "public canary size mismatch");
            require(files.client().ping().await);
            let baseline = require(files.client().read(&handle, 0, 128).await);
            require_canary(&baseline.bytes);
            assert!(baseline.eof);
            let ready = now_ms();
            controls.write("ready.json", &Marker {
                schema_version: 1,
                nonce: fixture.nonce.clone(),
                original_scope: fixture.files.scope.clone(),
                off_scope: fixture.off_scope.clone(),
                stage: "ready".into(),
                unix_ms: ready,
            });
            let mut reads = Reads { count: 1, failed: false };
            let mut issued = None;
            let mut last_success = Some(ready);
            loop {
                if issued.is_none() {
                    if let Some(marker) = controls.read("disable-submitted.json") {
                        assert!(marker.valid(&fixture, "disable_submitted", ready, now_ms()));
                        assert!(marker.unix_ms.saturating_sub(last_success.unwrap()) <= 5000,
                            "fresh pre-Disable liveness required");
                        issued = Some(marker.unix_ms);
                    }
                }
                let off = controls.read("off-settled.json");
                if let Some(marker) = &off {
                    assert!(marker.valid(&fixture, "off_settled",
                        issued.unwrap_or_else(|| blocked()), now_ms()));
                }
                assert!(reads.begin(tokio::time::Instant::now() < deadline));
                let request_at = now_ms();
                let outcome = files.client().read(&handle, 0, 128).await;
                assert!(tokio::time::Instant::now() < deadline);
                match outcome {
                    Ok(chunk) => {
                        assert!(off.is_none(), "new post-barrier read unexpectedly succeeded");
                        require_canary(&chunk.bytes);
                        assert!(chunk.eof);
                        last_success = Some(now_ms());
                    }
                    Err(error) => {
                        let failed_at = now_ms();
                        let qualified = reads.first_failure(issued.is_some() && last_success.is_some());
                        controls.write("first-failure.json", &serde_json::json!({
                            "nonce": fixture.nonce,
                            "unix_ms": failed_at,
                            "request_unix_ms": request_at,
                            "last_success_unix_ms": last_success,
                            "reads": reads.count,
                            "kind": failure_kind(&error),
                            "transition_submitted_before_request": qualified,
                        }));
                        println!(
                            "FESTERM_FILES_REVOCATION_FIRST_FAILURE kind={} reads={} unix_ms={} issued_before_request={}",
                            failure_kind(&error), reads.count, failed_at, qualified);
                        require(guard.check());
                        assert!(!cancel.is_cancelled(),
                            "local cancellation is not owner-revocation evidence");
                        assert!(qualified, "early connection failure is unqualified");
                        assert!(retirement_refusal(&error), "unqualified first error retained");
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            // No Files request, reconnect, close, Finish or Delete after first error.
            loop {
                if let Some(marker) = controls.read("off-settled.json") {
                    assert!(marker.valid(&fixture, "off_settled", issued.unwrap(), now_ms()));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            require(guard.check());
            assert!(!cancel.is_cancelled());
            files.client().cancel();
            println!("FESTERM_FILES_REVOCATION_PASS original_stream_retired=true no_requests_after_failure=true");
        }).await;
        require(result);
    });
}
