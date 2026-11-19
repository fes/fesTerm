//! Real GitHub/Dev Tunnels/native-vault integration; opt-in on a dedicated lab user.
//! No password login, owner enrollment, token export, global cleanup or GUI claim.
#![cfg(feature = "experimental-dev-tunnels")]

use festerm_config::Configuration;
use festerm_core::{CellWidth, Dimensions, Terminal};
use festerm_host_protocol::{
    AttachmentId, ContractError, Epoch, Lifetime, LogicalSessionId, OperationId, Recipe, Refusal,
};
use festerm_secret_store::native_store;
use festerm_session::{
    noop_session_event_notifier, Session, SessionEvent, SessionLifecycle, ShutdownResult,
};
use festerm_tunnel_bridge::host_client::{
    self,
    ordinary::{self, OrdinaryAccountGuard, OrdinaryContext, OrdinaryKeyIndex},
    originals::{OriginalId, OriginalStore},
    ApprovedAttachment, ClientContext, Completion, CreationIntent, CreationMode, Inventory,
    Operation, Outcome, Scope, Target, Worker,
};
use festerm_tunnel_client::{
    host_control::Error,
    identity::{self, Identity},
    service, Https,
};
use serde::Deserialize;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema_version: u32,
    account: String,
    configuration: PathBuf,
    runtime_root: PathBuf,
    locator: String,
    shared_profile_name: String,
    private_profile_name: String,
    private_logical: LogicalSessionId,
    private_revision: Epoch,
    generic_logical: LogicalSessionId,
    denied_logical: LogicalSessionId,
    creation_allowed: bool,
    allow_shell_input: bool,
    #[serde(default)]
    stopped_shared_profile_name: Option<String>,
    shell: Shell,
}

// Inventory is assembled from all correlated pages; uncertainty is never absence.
fn not_started(inventory: &Inventory, logical: &LogicalSessionId) -> bool {
    !inventory
        .sessions
        .iter()
        .any(|session| &session.target.logical == logical)
        && !inventory
            .unknown
            .iter()
            .any(|target| &target.logical == logical)
}

fn stopped_profile<'a>(
    fixture: &Fixture,
    inventory: &'a Inventory,
) -> Result<Option<&'a festerm_host_protocol::ProfileSummary>, ()> {
    let Some(name) = &fixture.stopped_shared_profile_name else {
        return Ok(None);
    };
    if fixture.creation_allowed
        || name.trim().is_empty()
        || name.len() > 128
        || name == &fixture.shared_profile_name
        || name == &fixture.private_profile_name
    {
        return Err(());
    }
    let mut candidates = inventory
        .profiles
        .iter()
        .filter(|profile| &profile.name == name);
    let profile = candidates.next().ok_or(())?;
    if candidates.next().is_some()
        || !not_started(inventory, &profile.logical)
        || [
            &fixture.private_logical,
            &fixture.generic_logical,
            &fixture.denied_logical,
        ]
        .contains(&&profile.logical)
        || inventory
            .profiles
            .iter()
            .filter(|other| other.logical == profile.logical)
            .count()
            != 1
    {
        return Err(());
    }
    Ok(Some(profile))
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Shell {
    Posix,
    Powershell,
    Cmd,
}

fn probe(shell: Shell, nonce: &str) -> (Vec<u8>, Vec<u8>) {
    let (first, second) = nonce.split_at(nonce.len() / 2);
    let marker = format!("FESTERM_LAB_{nonce}");
    let command = match shell {
        Shell::Posix => format!("printf '\\n%s%s\\n' 'FESTERM_LAB_{first}' '{second}'\r"),
        Shell::Powershell => format!("[Console]::WriteLine(('FESTERM_LAB_{first}'+'{second}'))\r"),
        Shell::Cmd => format!(
            "set \"FESTERM_VM_LAB_A=FESTERM_LAB_{first}\"\rset \"FESTERM_VM_LAB_B={second}\"\recho %FESTERM_VM_LAB_A%%FESTERM_VM_LAB_B%\rset FESTERM_VM_LAB_A=\rset FESTERM_VM_LAB_B=\r"
        ),
    };
    assert!(
        !command.contains(&marker),
        "proof must not be present in echoed input"
    );
    (command.into_bytes(), marker.into_bytes())
}

fn proof_received(terminal: &Terminal, marker: &[u8]) -> bool {
    // ConPTY may redraw a completed line using VT cursor movement rather than
    // emitting a standalone raw marker + CRLF. Inspect the same parsed screen
    // as the app, requiring a whole, unwrapped row behind the current cursor.
    // The random marker is absent from every supported probe's echoed input.
    !marker.is_empty()
        && marker.is_ascii()
        && (0..terminal.cursor().row()).any(|row| {
            let screen = terminal.screen();
            if screen.row_soft_wrapped(row) != Some(false)
                || (row > 0 && screen.row_soft_wrapped(row - 1) != Some(false))
            {
                return false;
            }
            // Compare complete cell text: character() alone drops combining
            // marks. Reject wide/continuation cells and ignore only plain
            // trailing padding, never a prefix or decorated/suffixed marker.
            marker.iter().enumerate().all(|(column, expected)| {
                terminal.cell_ref(column, row).is_some_and(|cell| {
                    cell.width() == CellWidth::Single
                        && cell.text().as_bytes() == std::slice::from_ref(expected)
                })
            }) && (marker.len()..screen.dimensions().columns()).all(|column| {
                terminal
                    .cell_ref(column, row)
                    .is_some_and(|cell| cell.width() == CellWidth::Single && cell.text() == " ")
            })
        })
}

#[test]
fn roundtrip_proof_is_absent_from_every_supported_input_and_requires_complete_line() {
    for shell in [Shell::Posix, Shell::Powershell, Shell::Cmd] {
        let (input, marker) = probe(shell, "0123456789abcdef0123456789abcdef");
        assert!(!input.windows(marker.len()).any(|bytes| bytes == marker));
        let mut echoed = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
        echoed.ingest(&input);
        assert!(!proof_received(&echoed, &marker));
        let mut terminal = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
        terminal.ingest(b"\r\n");
        terminal.ingest(&marker);
        assert!(!proof_received(&terminal, &marker));
        terminal.ingest(b"-wrong\r\n");
        assert!(!proof_received(&terminal, &marker));
        terminal.ingest(&marker);
        terminal.ingest(b"\r\n");
        assert!(proof_received(&terminal, &marker));
    }
}

#[test]
fn roundtrip_proof_accepts_chunked_vt_redraw_without_raw_newline() {
    let marker = b"FESTERM_LAB_0123456789abcdef0123456789abcdef";
    let mut terminal = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
    let mut redraw = b"\x1b[2;1H\x1b[0K\x1b[32m".to_vec();
    redraw.extend(marker);
    redraw.extend(b"\x1b[0m");
    for byte in redraw {
        terminal.ingest(&[byte]);
        assert!(!proof_received(&terminal, marker));
    }
    // A subsequent prompt on the following row establishes completed output.
    terminal.ingest(b"\x1b[3;1HC:\\lab>");
    assert!(proof_received(&terminal, marker));
}

#[test]
fn roundtrip_proof_refuses_prefixed_suffixed_and_wrapped_screen_rows() {
    let marker = b"FESTERM_LAB_0123456789abcdef0123456789abcdef";
    for prefix in [b"echo ".as_slice(), b"".as_slice()] {
        let mut terminal = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
        terminal.ingest(prefix);
        terminal.ingest(marker);
        if prefix.is_empty() {
            terminal.ingest(b"-wrong");
        }
        terminal.ingest(b"\r\n");
        assert!(!proof_received(&terminal, marker));
    }
    let mut wrapped = Terminal::new(Dimensions::new(20, 30).unwrap()).unwrap();
    wrapped.ingest(marker);
    wrapped.ingest(b"\r\n");
    assert!(!proof_received(&wrapped, marker));
}

#[test]
fn roundtrip_proof_refuses_wrapped_prefix_and_combining_suffix() {
    let marker = b"FESTERM_LAB_0123456789abcdef0123456789abcdef";
    let mut wrapped_prefix = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
    wrapped_prefix.ingest(&[b'x'; 120]);
    wrapped_prefix.ingest(marker);
    wrapped_prefix.ingest(b"\r\n");
    assert!(!proof_received(&wrapped_prefix, marker));

    let mut decorated = Terminal::new(Dimensions::new(120, 30).unwrap()).unwrap();
    decorated.ingest(marker);
    decorated.ingest("\u{0301}\r\n".as_bytes());
    assert!(!proof_received(&decorated, marker));
}

#[track_caller]
fn blocked() -> ! {
    panic!("FESTERM_DEV_TUNNEL_LAB_BLOCKED");
}

#[track_caller]
fn require<T, E>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        // Callsite only: never format the error or its potentially secret data.
        Err(_) => blocked(),
    }
}

fn private_bytes(path: &Path) -> Vec<u8> {
    assert!(path.is_absolute(), "absolute lab fixture path required");
    let parent = path.parent().unwrap_or_else(|| blocked());
    let name = Path::new(path.file_name().unwrap_or_else(|| blocked()));
    #[cfg(unix)]
    let mut file = {
        use festerm_unix_security as security;
        use std::fs::File;
        let mut directory = require(File::open("/"));
        for component in parent.components() {
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

        assert!(
            require(security::is_current_user_only(&directory, true)),
            "private lab fixture directory required"
        );
        let file = require(security::open_private_file(&directory, name, false));
        assert!(
            require(security::is_current_user_only(&file, false)),
            "private lab fixture file required"
        );
        file
    };
    #[cfg(windows)]
    let mut file = {
        use festerm_windows_security as security;
        let directory = require(security::lock_directory_path_without_delete_sharing(parent));
        assert!(
            require(security::is_current_user_only(&directory)),
            "private lab fixture directory required"
        );
        let file = require(security::open_file_no_reparse(&directory, name));
        assert!(
            require(security::is_current_user_only(&file)),
            "private lab fixture file required"
        );
        file
    };
    let mut bytes = Vec::new();
    require(file.by_ref().take(1024 * 1024 + 1).read_to_end(&mut bytes));
    assert!(bytes.len() <= 1024 * 1024, "bounded lab metadata required");
    bytes
}

struct Peer {
    context: OrdinaryContext,
    budgets: host_client::Budgets,
    deadline: Instant,
    original_root: PathBuf,
    shell: Shell,
}

impl Peer {
    fn context(&self) -> ClientContext {
        self.context.clone().into()
    }

    fn scope(&self, context: &ClientContext) -> Scope {
        Scope {
            request: OperationId::new(),
            generation: 1,
            binding: context.binding().clone(),
        }
    }

    fn operation(&self, operation: Operation) -> Completion {
        let context = self.context();
        let scope = self.scope(&context);
        let worker = require(Worker::start(
            context,
            scope.clone(),
            operation,
            &self.budgets,
            Arc::new(|| {}),
        ));
        loop {
            assert!(
                Instant::now() < self.deadline,
                "bounded HOST campaign deadline"
            );
            if let Some(completion) = worker.try_receive(&scope) {
                return completion;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn inventory(&self) -> Inventory {
        match self.operation(Operation::Inventory).outcome {
            Outcome::Inventory(inventory) => inventory,
            _ => panic!("authorized HOST inventory failed"),
        }
    }

    fn attach_roundtrip(&self, attachment: ApprovedAttachment) {
        let session = require(attachment.start(&self.budgets, noop_session_event_notifier()));
        let result = || {
            let mut terminal = require(Terminal::new(require(Dimensions::new(120, 30))));
            while session.lifecycle() != SessionLifecycle::Running {
                assert!(
                    Instant::now() < self.deadline,
                    "bounded HOST attachment deadline"
                );
                require(session.adopt_into(&mut terminal));
                let _ = session.try_recv_event();
                assert!(!session.lifecycle().is_terminal(), "HOST attachment failed");
                std::thread::sleep(Duration::from_millis(10));
            }
            let (input, marker) = probe(self.shell, &uuid::Uuid::new_v4().simple().to_string());
            require(session.try_send_input(&input));
            let mut output_bytes = 0usize;
            while !proof_received(&terminal, &marker) {
                assert!(
                    Instant::now() < self.deadline,
                    "bounded shell roundtrip deadline"
                );
                if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                    output_bytes = output_bytes.saturating_add(bytes.len());
                    assert!(output_bytes <= 1024 * 1024, "bounded lab shell output");
                    terminal.ingest(&bytes);
                }
                assert_eq!(
                    session.lifecycle(),
                    SessionLifecycle::Running,
                    "shell stopped during roundtrip"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        // Always attempt exact attachment cleanup, even after a failed assertion.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(result));
        let detached = session.shutdown(Duration::from_secs(5));
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
        assert!(
            matches!(detached, Ok(ShutdownResult::Stopped)),
            "exact HOST detach unconfirmed; fixture retained for reconciliation"
        );
    }

    fn check_survives(&self, target: &Target) {
        let inventory = self.inventory();
        assert!(
            inventory
                .sessions
                .iter()
                .any(|session| &session.target == target && !session.attached),
            "durable fixture did not survive exact detach"
        );
    }

    fn ensure_generic(
        &self,
        logical: LogicalSessionId,
    ) -> (Completion, Arc<OriginalStore>, AttachmentId) {
        let context = self.context();
        let (originals, recovered) = require(OriginalStore::recover(&self.original_root, &context));
        if !recovered.is_empty() {
            // An earlier uncertain request requires explicit operator reconciliation.
            // Never replace its original attachment/request in an unattended rerun.
            blocked();
        }
        let attachment = AttachmentId::new();
        let intent = Arc::new(require(CreationIntent::new(
            &context,
            CreationMode::Ensure,
            host_client::Creation {
                logical,
                attachment: attachment.clone(),
                name: Some("fesTerm VM lab generic".into()),
                recipe: Recipe::default(),
                lifetime: Lifetime::Persistent {},
            },
        )));
        require(intent.protect_original(
            originals.clone(),
            "Explicit controlled-account VM lab generic ensure".into(),
        ));
        (
            self.operation(Operation::Create(intent)),
            originals,
            attachment,
        )
    }

    fn forget_confirmed_original(
        &self,
        originals: &Arc<OriginalStore>,
        attachment: AttachmentId,
        logical: &LogicalSessionId,
        target: Option<&Target>,
    ) {
        let context = self.context();
        let cancel = CancellationToken::new();
        let review =
            require(originals.review_forget(&context, OriginalId::Creation(attachment), &cancel));
        assert_eq!(
            review.logical(),
            Some(logical),
            "only this campaign's exact original can be retired"
        );
        assert_eq!(
            review.target(),
            target,
            "only a confirmed response can retire the owned original"
        );
        require(review.forget(&context, &cancel));
    }
}

#[test]
#[ignore = "real GitHub/native-vault/HOST fixture; scripts/check_dev_tunnel_workflow.py --run"]
fn controlled_github_host_service_client_workflow() {
    if std::env::var("FESTERM_RUN_DEV_TUNNEL_LAB").as_deref() != Ok("1") {
        blocked();
    }
    let record = PathBuf::from(
        std::env::var_os("FESTERM_DEV_TUNNEL_LAB_RECORD").unwrap_or_else(|| blocked()),
    );
    let fixture: Fixture = require(serde_json::from_slice(&private_bytes(&record)));
    assert!(
        fixture.schema_version == 2
            && fixture.account == "fes-festerm1"
            && fixture.allow_shell_input,
        "controlled-account opt-in required"
    );
    assert_eq!(
        std::env::var_os("FESTERM_SESSIOND_RUNTIME_ROOT").map(PathBuf::from),
        Some(fixture.runtime_root.clone())
    );
    assert_ne!(
        fixture.generic_logical, fixture.denied_logical,
        "distinct stable lab logical identities required"
    );
    assert_ne!(fixture.private_logical, fixture.generic_logical);
    assert_ne!(fixture.private_logical, fixture.denied_logical);
    let configuration_bytes = private_bytes(&fixture.configuration);
    let configuration = require(Configuration::parse(require(std::str::from_utf8(
        &configuration_bytes,
    ))));
    let account = configuration
        .accounts()
        .iter()
        .find(|account| account.login() == fixture.account)
        .unwrap_or_else(|| blocked());
    let account_id = account.account_id();
    let store = require(native_store());
    let secret = require(store.get(account.credential_reference()));
    let identity = require(secret.with_bytes(|bytes| {
        Identity::decode_private(
            bytes,
            account_id,
            identity::registered_client_id().unwrap_or_else(|| blocked()),
        )
    }));
    assert_eq!(
        identity.account.login, "fes-festerm1",
        "native identity must match the controlled account"
    );
    let cancel = CancellationToken::new();
    let runtime = require(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
    );
    let budgets = host_client::Budgets::default();
    let index = require(OrdinaryKeyIndex::native());
    let configuration_path = fixture.configuration.clone();
    let guard = OrdinaryAccountGuard::new(
        CancellationToken::new(),
        Arc::new(move || {
            if private_bytes(&configuration_path) == configuration_bytes {
                Ok(())
            } else {
                Err(ContractError::Stale)
            }
        }),
    );
    let (context, discovered_inventory) = runtime.block_on(async {
        require(
            tokio::time::timeout(Duration::from_secs(30), async {
                require(
                    identity
                        .confirm_account(&require(Https::new()), &cancel)
                        .await,
                );
                let hosts = require(service::list_hosts(&identity, &cancel).await);
                assert!(
                    hosts.iter().any(|host| host.locator == fixture.locator),
                    "fixture host not found in controlled account"
                );
                require(
                    ordinary::discover(
                        &identity,
                        &fixture.locator,
                        index,
                        &budgets,
                        guard,
                        &cancel,
                    )
                    .await,
                )
            })
            .await,
        )
    });
    let peer = Peer {
        context,
        budgets,
        deadline: Instant::now() + Duration::from_secs(120),
        shell: fixture.shell,
        original_root: fixture
            .runtime_root
            .parent()
            .unwrap_or_else(|| blocked())
            .to_owned(),
    };
    let inventory = discovered_inventory;
    assert_eq!(
        inventory.capabilities.creation_allowed, fixture.creation_allowed,
        "host creation policy differs from selected campaign phase"
    );
    assert!(
        inventory.capabilities.profiles
            && inventory.capabilities.ensure_profile
            && inventory.capabilities.attach,
        "shared-profile HOST capabilities required"
    );
    assert!(
        !inventory
            .profiles
            .iter()
            .any(|profile| profile.name == fixture.private_profile_name),
        "private profile leaked into authorized inventory"
    );
    assert!(
        not_started(&inventory, &fixture.private_logical),
        "private logical identity leaked into sessions or uncertain inventory"
    );
    assert!(matches!(
        peer.operation(Operation::EnsureProfile {
            logical: fixture.private_logical.clone(),
            revision: fixture.private_revision,
            attachment: AttachmentId::new(),
        })
        .outcome,
        Outcome::Failed(host_client::Failure::Control(Error::Refused(
            Refusal::Denied | Refusal::Stale
        )))
    ));
    let stopped = stopped_profile(&fixture, &inventory)
        .unwrap_or_else(|()| blocked())
        .cloned();
    if let Some(profile) = &stopped {
        assert!(
            matches!(
                peer.operation(Operation::EnsureProfile {
                    logical: profile.logical.clone(),
                    revision: profile.revision,
                    attachment: AttachmentId::new(),
                })
                .outcome,
                Outcome::Failed(host_client::Failure::Control(Error::Refused(
                    Refusal::Denied
                )))
            ),
            "stopped shared profile must receive exact server denial while creation is off"
        );
        let after = peer.inventory();
        assert!(!after.capabilities.creation_allowed);
        assert_eq!(
            stopped_profile(&fixture, &after).unwrap_or_else(|()| blocked()),
            Some(profile),
            "denied stopped profile must retain exact catalog revision and NotStarted state"
        );
    }
    let profiles: Vec<_> = inventory
        .profiles
        .iter()
        .filter(|profile| profile.name == fixture.shared_profile_name)
        .collect();
    if profiles.len() != 1 {
        blocked();
    }
    let profile = profiles[0];
    assert_ne!(profile.logical, fixture.generic_logical);
    assert_ne!(profile.logical, fixture.denied_logical);
    let mut previous_shared = None;
    for _ in 0..2 {
        let completion = peer.operation(Operation::EnsureProfile {
            logical: profile.logical.clone(),
            revision: profile.revision,
            attachment: AttachmentId::new(),
        });
        let (session, disposition, attachment) = match completion.outcome {
            Outcome::ProfileEnsured {
                session,
                disposition,
                attachment,
            } => (session, disposition, attachment),
            _ => panic!("authorized shared-profile start/resume failed"),
        };
        assert!(matches!(session.lifetime, Lifetime::Persistent {}));
        let target = session.target.clone();
        if let Some(previous) = &previous_shared {
            assert_eq!(
                &target, previous,
                "shared profile ensure duplicated its durable fixture"
            );
        }
        previous_shared = Some(target.clone());
        let context = peer.context();
        peer.attach_roundtrip(require(ApprovedAttachment::from_profile_ensured(
            context,
            completion.scope,
            session,
            disposition,
            attachment,
        )));
        peer.check_survives(&target);
    }
    if fixture.creation_allowed {
        let mut previous = None;
        for _ in 0..2 {
            let (completion, originals, original_id) =
                peer.ensure_generic(fixture.generic_logical.clone());
            let created = match completion.outcome {
                Outcome::Created(created) => created,
                _ => panic!("permitted generic durable creation/resume failed"),
            };
            let target = created.session().target.clone();
            if let Some(previous) = &previous {
                assert_eq!(
                    &target, previous,
                    "ensure duplicated the durable logical fixture"
                );
            }
            previous = Some(target.clone());
            let context = peer.context();
            peer.attach_roundtrip(require(ApprovedAttachment::from_created(
                context,
                completion.scope,
                *created,
            )));
            peer.check_survives(&target);
            peer.forget_confirmed_original(
                &originals,
                original_id,
                &fixture.generic_logical,
                Some(&target),
            );
        }
    } else {
        assert!(
            not_started(&inventory, &fixture.denied_logical),
            "denied-create fixture logical ID must not already exist"
        );
        let (completion, originals, original_id) =
            peer.ensure_generic(fixture.denied_logical.clone());
        match completion.outcome {
            Outcome::Failed(host_client::Failure::Control(Error::Refused(Refusal::Denied))) => {
                peer.forget_confirmed_original(
                    &originals,
                    original_id,
                    &fixture.denied_logical,
                    None,
                );
            }
            _ => panic!("host did not explicitly deny generic creation while creation was off"),
        }
    }
    if !fixture.creation_allowed {
        let after = peer.inventory();
        assert!(
            !after.capabilities.creation_allowed && not_started(&after, &fixture.denied_logical),
            "denied generic logical must remain absent without uncertainty"
        );
    }
    if stopped.is_some() {
        println!("FESTERM_DEV_TUNNEL_LAB_STOPPED_PROFILE_DENIED_PASS");
    }
    println!("FESTERM_DEV_TUNNEL_LAB_ORDINARY_CLIENT_PASS");
}

#[cfg(test)]
mod stopped_policy_tests {
    use super::*;
    use festerm_host_protocol::{Capabilities, GenerationId, ProfileSummary, SessionSummary};

    #[test]
    fn blocked_diagnostic_never_formats_sensitive_errors() {
        struct Sensitive;
        impl std::fmt::Debug for Sensitive {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("sensitive error must never be formatted")
            }
        }
        assert_eq!(require::<_, Sensitive>(Ok(7)), 7);
        let payload = std::panic::catch_unwind(|| require::<(), _>(Err(Sensitive)))
            .expect_err("refusal must remain a failure");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"FESTERM_DEV_TUNNEL_LAB_BLOCKED")
        );
    }

    fn fixture_inventory() -> (Fixture, Inventory) {
        let fixture = Fixture {
            schema_version: 2,
            account: "fes-festerm1".into(),
            configuration: "/fixture/config".into(),
            runtime_root: "/fixture/runtime".into(),
            locator: "fixture.region".into(),
            shared_profile_name: "Shared".into(),
            private_profile_name: "Private".into(),
            private_logical: LogicalSessionId::new(),
            private_revision: Epoch::new(1).unwrap(),
            generic_logical: LogicalSessionId::new(),
            denied_logical: LogicalSessionId::new(),
            creation_allowed: false,
            allow_shell_input: true,
            shell: Shell::Posix,
            stopped_shared_profile_name: Some("Stopped".into()),
        };
        let inventory = Inventory {
            capabilities: Capabilities {
                issuance_epoch: Some(Epoch::new(1).unwrap()),
                inventory: true,
                profiles: true,
                ensure: true,
                ensure_profile: true,
                create: true,
                attach: true,
                detach: true,
                explicit_end: true,
                temporary: false,
                creation_allowed: false,
            },
            sessions: vec![],
            unknown: vec![],
            profiles: vec![ProfileSummary {
                logical: LogicalSessionId::new(),
                revision: Epoch::new(7).unwrap(),
                name: "Stopped".into(),
            }],
        };
        (fixture, inventory)
    }

    #[test]
    fn stopped_profile_requires_unique_opt_in_off_catalog_identity() {
        let (mut fixture, mut inventory) = fixture_inventory();
        let profile = inventory.profiles[0].clone();
        assert_eq!(stopped_profile(&fixture, &inventory), Ok(Some(&profile)));
        fixture.creation_allowed = true;
        assert!(stopped_profile(&fixture, &inventory).is_err());
        fixture.creation_allowed = false;
        inventory.profiles.push(profile.clone());
        assert!(stopped_profile(&fixture, &inventory).is_err());
        inventory.profiles.pop();
        inventory.profiles.push(ProfileSummary {
            name: "Alias".into(),
            ..profile.clone()
        });
        assert!(stopped_profile(&fixture, &inventory).is_err());
        inventory.profiles.pop();
        fixture.denied_logical = profile.logical;
        assert!(stopped_profile(&fixture, &inventory).is_err());
        fixture.stopped_shared_profile_name = None;
        assert_eq!(stopped_profile(&fixture, &inventory), Ok(None));
    }

    #[test]
    fn stopped_profile_unknown_or_running_never_means_not_started() {
        let (fixture, mut inventory) = fixture_inventory();
        let logical = inventory.profiles[0].logical.clone();
        let target = Target {
            logical: logical.clone(),
            generation: GenerationId::new(3).unwrap(),
        };
        inventory.unknown.push(target.clone());
        assert!(!not_started(&inventory, &logical));
        assert!(stopped_profile(&fixture, &inventory).is_err());
        inventory.unknown.clear();
        inventory.sessions.push(SessionSummary {
            target,
            name: "Existing".into(),
            lifetime: Lifetime::Persistent {},
            attached: false,
        });
        assert!(stopped_profile(&fixture, &inventory).is_err());
        inventory.sessions.clear();
        assert!(not_started(&inventory, &logical));
        assert!(stopped_profile(&fixture, &inventory).is_ok());
        inventory.unknown.push(Target {
            logical: fixture.private_logical.clone(),
            generation: GenerationId::new(4).unwrap(),
        });
        assert!(!not_started(&inventory, &fixture.private_logical));
        assert!(not_started(&inventory, &logical));
        assert!(stopped_profile(&fixture, &inventory).is_ok());
    }

    #[test]
    fn stopped_profile_revision_change_cannot_satisfy_same_catalog_postcondition() {
        let (fixture, mut inventory) = fixture_inventory();
        let original = stopped_profile(&fixture, &inventory).unwrap().cloned();
        inventory.profiles[0].revision = Epoch::new(8).unwrap();
        assert_ne!(
            stopped_profile(&fixture, &inventory).unwrap(),
            original.as_ref()
        );
        inventory.profiles.clear();
        assert!(stopped_profile(&fixture, &inventory).is_err());
    }
}
