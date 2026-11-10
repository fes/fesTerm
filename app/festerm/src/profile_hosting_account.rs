use super::*;
use fs2::FileExt;
use serde::Deserialize;
use std::collections::BTreeSet;

const SELECTION_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Eq, PartialEq)]
struct GitHubAccount {
    id: String,
    username: Option<String>,
}

fn parse_account(bytes: &[u8]) -> Result<GitHubAccount, &'static str> {
    #[derive(Deserialize)]
    struct Account {
        status: String,
        provider: String,
        #[serde(rename = "objectId")]
        id: String,
        username: Option<String>,
    }
    let account: Account =
        serde_json::from_slice(bytes).map_err(|_| "Account verification failed.")?;
    if account.status != "Logged in"
        || account.provider != "github"
        || account.id.is_empty()
        || account.id.len() > 256
        || !account.id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("A verified GitHub sign-in is required.");
    }
    Ok(GitHubAccount {
        id: account.id,
        username: account.username.filter(|name| {
            !name.is_empty()
                && name.len() <= 39
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        }),
    })
}

struct Inspection {
    current: Option<GitHubAccount>,
    accounts: BTreeSet<String>,
    has_bindings: bool,
    verified: Instant,
}

enum InspectionMode {
    Check,
    Connect,
    Recover(String),
}

fn authorize_sign_in(
    mode: &InspectionMode,
    accounts: &BTreeSet<String>,
    has_bindings: bool,
) -> Result<(), &'static str> {
    match mode {
        InspectionMode::Check => Ok(()),
        InspectionMode::Connect if has_bindings => Err(
            "Existing tunnel exposures still require their recorded account. Use that account or finish their cleanup before switching.",
        ),
        InspectionMode::Recover(expected) if accounts.iter().any(|account| account != expected) => {
            Err("Other known tunnel exposures require a different account. No sign-in was started.")
        }
        InspectionMode::Connect | InspectionMode::Recover(_) => Ok(()),
    }
}

struct Task {
    receiver: mpsc::Receiver<Result<Inspection, String>>,
    cancel: CancellationToken,
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Prompt {
    tab: TabId,
    request: egui::Id,
    original_id: Option<String>,
    task: Option<Task>,
    inspection: Option<Inspection>,
    error: Option<String>,
    focus_cancel: bool,
}

struct Receipt {
    tab: TabId,
    request: egui::Id,
    original_id: Option<String>,
    account: String,
    verified: Instant,
}

impl Receipt {
    fn valid_for(
        &self,
        tab: TabId,
        request: Option<egui::Id>,
        original_id: Option<&str>,
        editor: Option<Option<String>>,
        now: Instant,
    ) -> bool {
        self.tab == tab
            && Some(self.request) == request
            && self.original_id.as_deref() == original_id
            && now.saturating_duration_since(self.verified) < SELECTION_LIFETIME
            && editor == Some(self.original_id.clone())
    }
}

#[derive(Default)]
pub(super) struct Chooser {
    prompt: Option<Prompt>,
    receipt: Option<Receipt>,
}

impl Chooser {
    pub(super) fn begin(&mut self, context: &egui::Context, tab: TabId, request: egui::Id) {
        let Some(original_id) = screens::local_host_account_request(context, tab, request) else {
            return;
        };
        if self.prompt.is_some() {
            screens::report_local_host_account(
                context,
                tab,
                request,
                Err("Account selection is busy. Try again.".into()),
            );
            return;
        }
        self.receipt = None;
        self.prompt = Some(Prompt {
            tab,
            request,
            original_id,
            task: Some(start_task(context, false)),
            inspection: None,
            error: None,
            focus_cancel: true,
        });
    }

    pub(super) fn take(
        &mut self,
        context: &egui::Context,
        tab: TabId,
        request: Option<egui::Id>,
        original_id: Option<&str>,
    ) -> Result<String, &'static str> {
        let receipt = self
            .receipt
            .as_ref()
            .ok_or("Choose a GitHub account before enabling hosting.")?;
        if !receipt.valid_for(
            tab,
            request,
            original_id,
            screens::local_host_account_request(context, tab, receipt.request),
            Instant::now(),
        ) {
            return Err(
                "The account selection expired or the editor changed. Choose the account again.",
            );
        }
        Ok(self
            .receipt
            .take()
            .expect("verified account receipt")
            .account)
    }

    pub(super) fn poll(&mut self, context: &egui::Context, active: TabId) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        let current = active == prompt.tab
            && screens::local_host_account_request(context, prompt.tab, prompt.request)
                == Some(prompt.original_id.clone());
        if !current {
            self.receipt = None;
            if let Some(task) = &prompt.task {
                task.cancel.cancel();
            }
        }
        if let Some(task) = &prompt.task {
            match task.receiver.try_recv() {
                Ok(result) => {
                    prompt.task = None;
                    match result {
                        Ok(inspection) => {
                            prompt.inspection = Some(inspection);
                            prompt.error = None;
                        }
                        Err(error) => prompt.error = Some(error),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    prompt.task = None;
                    prompt.error = Some("The account check could not complete. Retry.".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !current && prompt.task.is_none() {
            self.prompt = None;
            return false;
        }
        if !current {
            context.request_repaint_after(Duration::from_millis(100));
            return false;
        }
        let mut selected = None;
        let mut connect = false;
        let mut retry = false;
        let mut cancel = context.input(|input| input.key_pressed(egui::Key::Escape));
        egui::Modal::new(egui::Id::new("local_host_github_account")).show(context, |ui| {
            super::super::confirmations::prepare_confirmation(ui, context, 480.0);
            ui.heading("GitHub account for Dev Tunnel hosting");
            ui.label("Choose the account that will own this tunnel. Nothing is exposed until you Save.");
            ui.label("The Dev Tunnels CLI uses a shared sign-in on this host. Account switching is unavailable while a known desktop or default script host needs its account.");
            ui.label("Older script hosts in custom state directories cannot be discovered and may be interrupted by a different sign-in. Remove or migrate them first.");
            if let Some(inspection) = &prompt.inspection {
                if let Some(account) = &inspection.current {
                    let name = account.username.as_deref().unwrap_or("GitHub account");
                    ui.label(format!("{name} (GitHub ID {})", account.id));
                    let compatible = inspection.accounts.iter().all(|id| id == &account.id)
                        && inspection.verified.elapsed() < SELECTION_LIFETIME;
                    if ui.add_enabled(prompt.task.is_none() && compatible, egui::Button::new("Use this GitHub account")).clicked() {
                        selected = Some((account.id.clone(), inspection.verified));
                    }
                    if !compatible {
                        ui.label("Other local tunnels require a different recorded account. Restore that sign-in before enabling another host.");
                    }
                } else {
                    ui.label("No GitHub sign-in could be verified.");
                }
                if inspection.has_bindings {
                    ui.label("To reauthenticate an existing host, use its Sign in action. To change accounts, first finish removing existing tunnel exposures.");
                }
                connect = ui.add_enabled(prompt.task.is_none() && !inspection.has_bindings,
                    egui::Button::new("Connect a GitHub account in browser")).clicked();
            }
            if prompt.task.is_some() {
                ui.spinner();
                ui.label("Checking the account or waiting for browser sign-in. Cancel stops this request; any sign-in completed in the browser remains with the CLI.");
            }
            if let Some(error) = &prompt.error {
                ui.label(error);
            }
            retry = ui.add_enabled(prompt.task.is_none(), egui::Button::new("Retry account check")).clicked();
            let no = ui.button("Cancel");
            if std::mem::take(&mut prompt.focus_cancel) { no.request_focus(); }
            cancel |= no.clicked();
        });
        if cancel {
            screens::report_local_host_account(context, prompt.tab, prompt.request, Ok(false));
            if let Some(task) = &prompt.task {
                task.cancel.cancel();
            }
            if prompt.task.is_none() {
                self.prompt = None;
            }
        } else if let Some((account, verified)) = selected {
            self.receipt = Some(Receipt {
                tab: prompt.tab,
                request: prompt.request,
                original_id: prompt.original_id.clone(),
                account,
                verified,
            });
            screens::report_local_host_account(context, prompt.tab, prompt.request, Ok(true));
            self.prompt = None;
        } else if connect || retry {
            prompt.error = None;
            prompt.task = Some(start_task(context, connect));
        }
        if self
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.task.is_some())
        {
            context.request_repaint_after(Duration::from_millis(100));
        }
        true
    }
}

fn start_task(context: &egui::Context, login: bool) -> Task {
    let (sender, receiver) = mpsc::sync_channel(1);
    let cancel = CancellationToken::new();
    let cancellation = cancel.clone();
    let context = context.clone();
    let _ = std::thread::Builder::new().name("profile-tunnel-account".into()).spawn(move || {
        let result = (|| {
            let helpers = staging::stage().map_err(|_| "Verified hosting helpers are unavailable. Install a complete fesTerm build and retry.".to_owned())?;
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| "The account worker could not start.".to_owned())?;
            runtime.block_on(async {
                tokio::select! {
                    _ = cancellation.cancelled() => Err("Account selection cancelled.".into()),
                    result = inspect(&helpers, if login { InspectionMode::Connect } else { InspectionMode::Check }) => result,
                }
            })
        })();
        let _ = sender.try_send(result);
        context.request_repaint();
    });
    Task { receiver, cancel }
}

pub(super) async fn recover(helpers: &staging::Helpers, account: String) -> Result<(), String> {
    inspect(helpers, InspectionMode::Recover(account)).await?;
    Ok(())
}

async fn inspect(helpers: &staging::Helpers, mode: InspectionMode) -> Result<Inspection, String> {
    let login = !matches!(mode, InspectionMode::Check);
    let _account = profile_hosting::account_lease(login)
        .map_err(|_| "Another account operation is busy. Retry shortly.")?;
    let _script = if login {
        let root = setup_storage::script_namespace()
            .map_err(|_| "Script-host inventory is unavailable.")?;
        let directory = festerm_sessiond::prepare_native_runtime(&root)
            .map_err(|_| "Script-host storage cannot be verified; sign-in was not changed.")?;
        let file =
            match setup_storage::open_private_file(&directory, Path::new("installer.lock"), true) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    setup_storage::open_private_file(&directory, Path::new("installer.lock"), false)
                        .map_err(|_| "Script-host inventory is busy.")?
                }
                Err(_) => return Err("Script-host inventory cannot be verified.".into()),
            };
        file.try_lock_exclusive()
            .map_err(|_| "A script-host operation is busy. Retry shortly.")?;
        Some(file)
    } else {
        None
    };
    let inventory = local_inventory();
    if inventory.truncated || inventory.entries.iter().any(|(_, grant)| grant.is_none()) {
        return Err(
            "Existing local tunnel accounts could not be verified. No sign-in was changed.".into(),
        );
    }
    let mut accounts = BTreeSet::new();
    let mut has_bindings = false;
    for (_, grant) in inventory.entries {
        let grant = grant.expect("verified inventory");
        if grant.phase != Phase::Revoked {
            has_bindings = true;
            let account = grant
                .selected_account
                .as_ref()
                .or_else(|| {
                    grant
                        .cloud_reservation
                        .as_ref()
                        .map(|reservation| &reservation.account)
                })
                .ok_or(
                    "An existing local tunnel has an unknown account; sign-in was not changed.",
                )?;
            accounts.insert(account.clone());
        }
    }
    #[derive(Deserialize)]
    struct ScriptInventory {
        version: u16,
        scope: String,
        hosts: Vec<ScriptHost>,
    }
    #[derive(Deserialize)]
    struct ScriptHost {
        account: String,
        cleanup: String,
        service: String,
    }
    let mut command = tokio::process::Command::new(&helpers.setup);
    command.args(["--manage", "list", "--json"]);
    let bytes = capture(command, Duration::from_secs(20))
        .await
        .map_err(|_| {
            "Script-created tunnel accounts could not be verified. No sign-in was changed."
        })?;
    let scripts: ScriptInventory = serde_json::from_slice(&bytes)
        .map_err(|_| "Script-created tunnel inventory is invalid.")?;
    if scripts.version != 1
        || scripts.scope != "verified-local-script-installations"
        || scripts.hosts.len() > 1000
    {
        return Err("Script-created tunnel inventory is invalid.".into());
    }
    for host in scripts.hosts {
        if host.cleanup != "complete" || host.service != "stopped" {
            if host.account.is_empty()
                || host.account.len() > 256
                || !host.account.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(
                    "A script-created host has an unknown account; sign-in was not changed.".into(),
                );
            }
            has_bindings = true;
            accounts.insert(host.account);
        }
    }
    authorize_sign_in(&mode, &accounts, has_bindings)?;
    if login {
        let verified = festerm_tunnel_bridge::cli_pins::verify(&helpers.cli)
            .map_err(|_| "The hosting CLI could not be verified; sign-in was not started.")?;
        let mut command = tokio::process::Command::new(verified.path());
        command.args(["user", "login", "--github", "--use-browser-auth"]);
        capture(command, Duration::from_secs(180))
            .await
            .map_err(|_| "GitHub sign-in did not complete. No shell or tunnel was started.")?;
    }
    let mut command = tokio::process::Command::new(&helpers.cli);
    command.args(["user", "show", "--json"]);
    let current = capture(command, Duration::from_secs(15))
        .await
        .ok()
        .and_then(|bytes| parse_account(&bytes).ok());
    if login && current.is_none() {
        return Err("The browser sign-in did not produce a verified GitHub account.".into());
    }
    if let InspectionMode::Recover(expected) = &mode {
        if current
            .as_ref()
            .is_none_or(|account| &account.id != expected)
        {
            return Err("The browser signed in to a different account. Restore the recorded account before retrying; the CLI's shared sign-in may have changed.".into());
        }
    }
    Ok(Inspection {
        current,
        accounts,
        has_bindings,
        verified: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_sign_in_refuses_conflicting_known_accounts() {
        let one = BTreeSet::from(["1234".to_owned()]);
        let two = BTreeSet::from(["1234".to_owned(), "5678".to_owned()]);
        assert!(authorize_sign_in(&InspectionMode::Check, &two, true).is_ok());
        assert!(authorize_sign_in(&InspectionMode::Connect, &one, true).is_err());
        assert!(authorize_sign_in(&InspectionMode::Connect, &BTreeSet::new(), false).is_ok());
        assert!(authorize_sign_in(&InspectionMode::Recover("1234".into()), &one, true).is_ok());
        assert!(authorize_sign_in(&InspectionMode::Recover("1234".into()), &two, true).is_err());
    }

    #[test]
    fn account_receipt_expires_and_cannot_cross_editors_or_documents() {
        let now = Instant::now();
        let tab = TabId::next_for_test();
        let request = egui::Id::new("account-request");
        let receipt = Receipt {
            tab,
            request,
            original_id: Some("local-a".into()),
            account: "1234".into(),
            verified: now,
        };
        let editor = Some(Some("local-a".into()));
        assert!(receipt.valid_for(tab, Some(request), Some("local-a"), editor.clone(), now));
        assert!(!receipt.valid_for(
            tab,
            Some(request),
            Some("local-a"),
            editor.clone(),
            now + SELECTION_LIFETIME
        ));
        assert!(!receipt.valid_for(
            TabId::next_for_test(),
            Some(request),
            Some("local-a"),
            editor.clone(),
            now
        ));
        assert!(!receipt.valid_for(
            tab,
            Some(egui::Id::new("different-request")),
            Some("local-a"),
            editor.clone(),
            now
        ));
        assert!(!receipt.valid_for(tab, Some(request), Some("local-b"), editor, now));
        assert!(!receipt.valid_for(tab, Some(request), Some("local-a"), None, now));
        assert!(!receipt.valid_for(
            tab,
            Some(request),
            Some("local-a"),
            Some(Some("local-b".into())),
            now
        ));
    }

    #[test]
    fn account_selection_requires_logged_in_github_numeric_identity() {
        let valid = br#"{"status":"Logged in","provider":"github","objectId":"1234","username":"example-user"}"#;
        assert_eq!(parse_account(valid).unwrap().id, "1234");
        for invalid in [
            br#"{"status":"Logged out","provider":"github","objectId":"1234"}"#.as_slice(),
            br#"{"status":"Logged in","provider":"microsoft","objectId":"1234"}"#,
            br#"{"status":"Logged in","provider":"github","objectId":"not-an-id"}"#,
            br#"{"status":"Logged in","provider":"github","objectId":""}"#,
            br#"{}"#,
        ] {
            assert!(parse_account(invalid).is_err());
        }
    }
}
