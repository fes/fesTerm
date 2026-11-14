//! Real GitHub/Dev Tunnels/native-vault integration; opt-in on a dedicated lab user.
//! No password login, owner enrollment, token export, global cleanup or GUI claim.
#![cfg(feature = "experimental-dev-tunnels")]

use festerm_config::{host_recipes::PublicRecipe, Configuration};
use festerm_core::{Dimensions, Terminal};
use festerm_host_protocol::{
    AttachmentId, Lifetime, LogicalSessionId, OperationId, Recipe, Refusal,
};
use festerm_secret_store::native_store;
use festerm_session::{
    noop_session_event_notifier, Session, SessionEvent, SessionLifecycle, ShutdownResult,
};
use festerm_tunnel_bridge::host_client::{
    self,
    approvals::{Receipt, ReceiptIndex},
    originals::{OriginalId, OriginalStore},
    ApprovedAttachment, ApprovedContext, Completion, CreationIntent, CreationMode, Inventory,
    Operation, Outcome, Scope, Target, Worker,
};
use festerm_tunnel_client::{
    host_control::{Binding, Error},
    identity::{self, Identity},
    service::{self, ResolvedTunnel},
    Https,
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
    recipe_name: String,
    shared_profile_name: String,
    private_profile_name: String,
    generic_logical: LogicalSessionId,
    denied_logical: LogicalSessionId,
    creation_allowed: bool,
    allow_shell_input: bool,
    shell: Shell,
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
        Shell::Cmd => format!("set \"FESTERM_VM_LAB_A=FESTERM_LAB_{first}\"\rset \"FESTERM_VM_LAB_B={second}\"\recho %FESTERM_VM_LAB_A%%FESTERM_VM_LAB_B%\rset FESTERM_VM_LAB_A=\rset FESTERM_VM_LAB_B=\r"),
    };
    assert!(
        !command.contains(&marker),
        "proof must not be present in echoed input"
    );
    (command.into_bytes(), marker.into_bytes())
}

fn proof_received(output: &[u8], marker: &[u8]) -> bool {
    // Require a terminated complete output line, never a wrapped input prefix.
    output.split_inclusive(|byte| *byte == b'\n').any(|line| {
        line.strip_suffix(b"\n")
            .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
            == Some(marker)
    })
}

#[test]
fn roundtrip_proof_is_absent_from_every_supported_input_and_requires_complete_line() {
    for shell in [Shell::Posix, Shell::Powershell, Shell::Cmd] {
        let (input, marker) = probe(shell, "0123456789abcdef0123456789abcdef");
        assert!(!input.windows(marker.len()).any(|bytes| bytes == marker));
        assert!(!proof_received(&input, &marker));
        let mut output = b"\r\n".to_vec();
        output.extend(&marker);
        assert!(!proof_received(&output, &marker));
        output.extend(b"-wrong\r\n");
        assert!(!proof_received(&output, &marker));
        output.extend(&marker);
        output.extend(b"\r\n");
        assert!(proof_received(&output, &marker));
    }
}

fn blocked() -> ! {
    panic!("FESTERM_DEV_TUNNEL_LAB_BLOCKED");
}

fn require<T, E>(result: Result<T, E>) -> T {
    result.unwrap_or_else(|_| blocked())
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
    receipt: Receipt,
    resolved: ResolvedTunnel,
    budgets: host_client::Budgets,
    deadline: Instant,
    original_root: PathBuf,
    shell: Shell,
}

impl Peer {
    fn context(&self) -> ApprovedContext {
        let binding = require(Binding::new(
            self.receipt.binding().host.host.clone(),
            self.receipt.binding().account.clone(),
            format!(
                "{}.{}/{}/{}",
                self.resolved.binding.tunnel_id,
                self.resolved.binding.cluster,
                self.resolved.binding.host_id,
                self.resolved.binding.port
            ),
        ));
        require(ApprovedContext::from_protected_approval(
            self.receipt.binding().clone(),
            self.receipt.reference(),
            self.resolved.endpoint.clone(),
            self.resolved.binding.port,
            self.resolved.connect_token.clone(),
            binding,
        ))
    }

    fn scope(&self, context: &ApprovedContext) -> Scope {
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
            let mut output = Vec::new();
            while !proof_received(&output, &marker) {
                assert!(
                    Instant::now() < self.deadline,
                    "bounded shell roundtrip deadline"
                );
                if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                    output.extend(bytes);
                    assert!(output.len() <= 1024 * 1024, "bounded lab shell output");
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
        fixture.schema_version == 1
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
    let configuration = require(Configuration::parse(require(std::str::from_utf8(
        &private_bytes(&fixture.configuration),
    ))));
    let account = configuration
        .accounts()
        .iter()
        .find(|account| account.login() == fixture.account)
        .unwrap_or_else(|| blocked());
    let recipes: Vec<&PublicRecipe> = configuration
        .host_recipes()
        .recipes()
        .iter()
        .filter(|recipe| recipe.name == fixture.recipe_name)
        .collect();
    if recipes.len() != 1 {
        blocked();
    }
    let recipe = recipes[0];
    let festerm_config::host_recipes::AccountReference::Github { account_id } = recipe.host.account;
    assert_eq!(
        account_id,
        account.account_id(),
        "fixture recipe must use the controlled account"
    );
    let hint = recipe.host_hint.as_ref().unwrap_or_else(|| blocked());
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
    let resolved = runtime.block_on(async {
        require(
            tokio::time::timeout(Duration::from_secs(30), async {
                require(
                    identity
                        .confirm_account(&require(Https::new()), &cancel)
                        .await,
                );
                let hosts = require(service::list_hosts(&identity, &cancel).await);
                assert!(
                    hosts.iter().any(|host| host.locator == hint.locator),
                    "fixture host not found in controlled account"
                );
                require(service::resolve_host(&identity, &hint.locator, &cancel).await)
            })
            .await,
        )
    });
    let index = require(ReceiptIndex::native());
    let receipt = require(index.load(&recipe.host.id, &format!("github:{account_id}")));
    let peer = Peer {
        receipt,
        resolved,
        budgets: host_client::Budgets::default(),
        deadline: Instant::now() + Duration::from_secs(120),
        shell: fixture.shell,
        original_root: fixture
            .runtime_root
            .parent()
            .unwrap_or_else(|| blocked())
            .to_owned(),
    };
    let inventory = peer.inventory();
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
            !inventory
                .sessions
                .iter()
                .any(|session| session.target.logical == fixture.denied_logical),
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
    println!("FESTERM_DEV_TUNNEL_LAB_SERVICE_CLIENT_PASS");
}
