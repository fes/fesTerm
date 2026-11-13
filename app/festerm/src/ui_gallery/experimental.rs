//! Finite offline catalog of the implemented remote and experimental surfaces.
//! Fixtures invoke production renderers; no returned UI action is dispatched.

use super::*;
#[cfg(feature = "local-host-settings")]
use crate::app::desktop_remote_access as remote;
use egui_kittest::kittest::NodeT;

pub(super) const ROUTES: &str = include_str!("experimental-routes.json");

fn finish_experimental<State>(harness: &mut Harness<'_, State>) -> image::RgbaImage {
    // Busy surfaces intentionally repaint forever. Advance a fixed number of
    // frames instead of asking the harness to wait for an idle spinner.
    harness.remove_cursor();
    harness.run_steps(4);
    let image = harness.render().expect("experimental surface render");
    trim_to_content(&image, GALLERY_MARGIN)
}

fn render(mut show: impl FnMut(&mut egui::Ui)) -> image::RgbaImage {
    let mut harness = gallery_harness(egui::vec2(1100.0, 1100.0), move |ui, ()| show(ui), ());
    finish_experimental(&mut harness)
}

#[cfg(feature = "experimental-dev-tunnels")]
fn app_surface(mut app: crate::app::FesTermApp, surface: &'static str) -> image::RgbaImage {
    render(move |ui| {
        crate::app::experimental_gallery::show(&mut app, ui.ctx(), surface);
    })
}

#[cfg(feature = "local-host-settings")]
fn remote_view(scene: &str) -> remote::View {
    use remote::{BundleStatus, Failure, Operation, Scope};
    let mut view = remote::View::default();
    view.draft.friendly_name = "Synthetic review workstation".into();
    view.draft.account = "42".into();
    if scene == "advanced-selected" {
        view.draft.manual_bundle = true;
        view.draft.manifest = "/review-fixtures/host-bundle/manifest.json".into();
        view.draft.signature = "/review-fixtures/host-bundle/manifest.sig".into();
        view.draft.archive = "/review-fixtures/host-bundle/native-host.zip".into();
    }
    let phase = match scene {
        "disabled" => Some("disabled"),
        "enable-requested" => Some("enable-requested"),
        "provisioned" => Some("provisioned"),
        "reconciling" => Some("reconciling"),
        "stop-requested" => Some("stop-requested"),
        "cleanup-missing" | "cleanup-unverified" | "cleanup-guidance" => Some("cleanup-pending"),
        "removed" => Some("removed"),
        _ => None,
    };
    if let Some(phase) = phase {
        view.scope = Scope::Present(std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({
                "version": 1,
                "installation": "11111111-1111-4111-8111-111111111111",
                "origin": "desktop",
                "settings": {
                    "friendly_name": "Synthetic review workstation",
                    "allow_creation": false,
                    "startup": "manual"
                },
                "epoch": 7, "phase": phase, "binding": null, "pending": null
            }))
            .expect("synthetic lifecycle snapshot"),
        ));
    }
    if scene == "absent" {
        view.scope = Scope::Absent;
    }
    view.review = match scene {
        "review-enable" => Some(Operation::Enable),
        "review-owner" => Some(Operation::ApproveOwner),
        "review-start" => Some(Operation::Start),
        "review-observe" => Some(Operation::ObserveNative),
        "review-inspect" => Some(Operation::InspectOwner),
        "review-recover" => Some(Operation::Reconcile),
        "review-disable" => Some(Operation::Disable),
        "review-uninstall" => Some(Operation::Uninstall),
        _ => None,
    };
    view.failure = match scene {
        "error-busy" => Some(Failure::Busy),
        "error-cancelled" => Some(Failure::Cancelled),
        "error-refresh" => Some(Failure::RefreshFirst),
        "error-changed" => Some(Failure::ScopeChanged),
        "error-private" => Some(Failure::PrivateStorage),
        "error-signature" => Some(Failure::SignedBundle),
        "error-unpublished" => Some(Failure::BundleNotPublished),
        "error-download" => Some(Failure::AcquisitionUnavailable),
        "error-vault" => Some(Failure::NativeVault),
        "error-account" => Some(Failure::SignIn),
        "error-owner" => Some(Failure::OwnerApproval),
        "error-platform" => Some(Failure::PlatformPrerequisite),
        "error-recovery" => Some(Failure::Reconciliation),
        "error-share" => Some(Failure::ShareCapability),
        "error-session" => Some(Failure::SessionChanged),
        "error-native" => Some(Failure::NoNativeObservation),
        "error-draft" => Some(Failure::InvalidDraft),
        "error-cloud" => Some(Failure::CloudUncertain),
        _ => None,
    };
    view.bundle_status = match scene {
        "acquiring" => BundleStatus::Acquiring,
        "authenticated" => BundleStatus::Authenticated,
        "qualifying" => BundleStatus::Qualifying,
        "complete" => BundleStatus::Complete,
        "error-unpublished" => BundleStatus::Blocked,
        "cancelling" => BundleStatus::Cancelled,
        _ => BundleStatus::Idle,
    };
    view.busy = matches!(
        scene,
        "acquiring" | "authenticated" | "qualifying" | "device" | "cancelling"
    );
    view.cancelling = scene == "cancelling";
    if scene == "device" {
        view.device_code = Some(remote::gallery_device_code());
    }
    if scene == "owner-context" {
        view.owner_binding = Some("Synthetic public owner binding · installation 11111111-1111-4111-8111-111111111111 · github:42 · policy revision 7".into());
        view.owner_revision = Some(7);
    }
    if scene == "cleanup-unverified" || scene == "cleanup-guidance" {
        view.cleanup = Some(festerm_host_setup::cloud::CleanupContext {
            installation: "11111111-1111-4111-8111-111111111111".into(),
            epoch: 7,
            account_id: 42,
            cluster: "use".into(),
            tunnel_id: "synthetic-review".into(),
            ownership_label: "synthetic-owned-installation".into(),
            incarnation: "synthetic-incarnation-7".into(),
        });
        view.cleanup_verified_for_manual_review = scene == "cleanup-guidance";
    }
    if scene.starts_with("share-") {
        let target = remote::ShareTarget {
            tab: AppState::for_test().active(),
            profile: Some("Synthetic persistent build shell".into()),
            lifecycle_generation: 7,
            identity: festerm_sessiond::NativeSessionIdentity {
                name: "synthetic-build".into(),
                pid: 4200,
                created_at_unix_ms: 1_700_000_000_000,
                endpoint: "synthetic-connected-local-endpoint".into(),
            },
            source: None,
        };
        view.share_review = Some(target.clone());
        if scene == "share-recovery" {
            view.share_operation_input = "22222222-2222-4222-8222-222222222222".into();
        }
        let phase = match scene {
            "share-prepared" => Some(remote::SharePhase::Prepared),
            "share-active" => Some(remote::SharePhase::Active),
            "share-revoked" => Some(remote::SharePhase::Revoked),
            "share-released" => Some(remote::SharePhase::Released),
            "share-uncertain" => Some(remote::SharePhase::Uncertain),
            _ => None,
        };
        if let Some(phase) = phase {
            view.sharing = Some(remote::ShareStatus {
                target,
                operation: "22222222-2222-4222-8222-222222222222".into(),
                phase,
            });
        }
    }
    view
}

#[cfg(feature = "local-host-settings")]
fn capture_remote(scene: &'static str) -> image::RgbaImage {
    let mut view = remote_view(scene);
    let targets: Vec<_> = if scene == "share-eligible" {
        view.share_review.take().into_iter().collect()
    } else {
        Vec::new()
    };
    let mut harness = gallery_harness(
        egui::vec2(1100.0, 1100.0),
        move |ui, ()| {
            ui.heading("Remote access · this device");
            let _ = remote::show(ui, &view, &targets);
        },
        (),
    );
    harness.run_steps(4);
    let expand = match scene {
        "owner-context" => Some("Public owner binding for independent client review"),
        "review-owner" => Some("Explicit owner-key approval · native vault only"),
        "advanced" | "advanced-selected" => Some("Advanced · explicit signed local bundle"),
        _ => None,
    };
    if let Some(label) = expand {
        // These disclosures are nested inside the Advanced / Recovery section.
        for label in ["Advanced / Recovery", label] {
            harness.get_by_label(label).click();
            harness.run_steps(4);
        }
    }
    finish_experimental(&mut harness)
}

fn capture_automation(scene: &'static str) -> image::RgbaImage {
    let mut harness = gallery_harness(
        egui::vec2(1100.0, 1500.0),
        move |ui, ()| {
            crate::automation::SettingsSnapshot::gallery_show(ui, scene);
        },
        (),
    );
    harness.run_steps(4);
    if matches!(
        scene,
        "workflow-source"
            | "workflow-source-modules"
            | "workflow-source-page2"
            | "workflow-source-last"
    ) {
        harness
            .get_by_label("Review captured source (not approval)")
            .click();
        harness.run_steps(4);
    }
    if scene == "workflow-source-modules" {
        harness
            .get_by_role_and_label(accesskit::Role::ComboBox, "Source module")
            .click();
        harness.run_steps(4);
    }
    if scene == "workflow-source-page2" || scene == "workflow-source-last" {
        harness.get_by_label("Next source page").click();
        harness.run_steps(4);
        if scene == "workflow-source-last" {
            harness.get_by_label("Next source page").click();
            harness.run_steps(4);
        }
    }
    if scene == "workflow-capabilities" {
        harness
            .get_by_label("Requested capabilities (not granted)")
            .click();
        harness.run_steps(4);
    }
    if scene == "workflow-parameters" {
        harness.get_by_label("Parameters and modules").click();
        harness.run_steps(4);
    }
    finish_experimental(&mut harness)
}

fn capture_powershell_details() -> image::RgbaImage {
    let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
        crate::powershell::PowerShellState::Ready,
    );
    let mut harness = gallery_harness(
        egui::vec2(1100.0, 1100.0),
        move |ui, ()| {
            let _ = tab.show(ui);
        },
        (),
    );
    harness.run_steps(4);
    harness.get_by_label("Structured value").click();
    finish_experimental(&mut harness)
}

fn capture_recording(scene: &'static str) -> image::RgbaImage {
    let base = match scene {
        "font-menu" | "theme-menu" | "speed-menu" => "settings",
        "start-options" => "start",
        _ => scene,
    };
    let mut harness = gallery_harness(
        egui::vec2(1100.0, 1400.0),
        move |ui, ()| {
            crate::application::recording::gallery_show(ui.ctx(), base);
        },
        (),
    );
    harness.run_steps(4);
    match scene {
        "font-menu" => {
            harness
                .get_by_role_and_label(accesskit::Role::ComboBox, "Font")
                .scroll_to_me();
            harness.run_steps(16);
            harness
                .get_by_role_and_label(accesskit::Role::ComboBox, "Font")
                .click();
            harness.run_steps(4);
            harness.get_by_label("JuliaMono");
        }
        "theme-menu" => {
            harness
                .get_by_role_and_label(accesskit::Role::ComboBox, "Theme")
                .scroll_to_me();
            harness.run_steps(16);
            harness
                .get_by_role_and_label(accesskit::Role::ComboBox, "Theme")
                .click();
            harness.run_steps(4);
            harness.get_by_label("fesTerm light");
        }
        "speed-menu" => {
            let selectors: Vec<_> = harness
                .query_all_by_role(accesskit::Role::ComboBox)
                .filter(|node| node.value().as_deref() == Some("1x"))
                .collect();
            assert_eq!(selectors.len(), 1, "unique playback-speed selector");
            selectors[0].click();
            harness.run_steps(4);
            harness.get_by_label("8x");
        }
        "start-options" => {
            harness.get_by_label("One-recording export options").click();
            harness.run_steps(4);
            harness
                .get_by_label("Recording folder template")
                .scroll_to_me();
            harness.run_steps(16);
        }
        "settings-advanced" => {
            harness.get_by_label("Output reveal").scroll_to_me();
            harness.run_steps(16);
        }
        "export-calculating" | "export-ready" => {
            harness.get_by_label("Keep source for later").scroll_to_me();
            harness.run_steps(16);
            if scene == "export-ready" {
                harness.get_by_label("Estimated duration: 42.0 seconds");
                assert!(!harness
                    .get_by_role_and_label(accesskit::Role::Button, "Export recording")
                    .accesskit_node()
                    .is_disabled());
            } else {
                harness.get_by_label("Calculating the synchronized duration...");
                assert!(harness
                    .get_by_role_and_label(accesskit::Role::Button, "Export recording")
                    .accesskit_node()
                    .is_disabled());
            }
        }
        _ => {}
    }
    finish_experimental(&mut harness)
}

fn capture_remote_profile(scene: &'static str) -> image::RgbaImage {
    let tab = AppState::for_test().active();
    let mut configuration = None;
    let mut harness = gallery_harness(
        egui::vec2(1100.0, 1500.0),
        move |ui, ()| {
            let configuration = configuration.get_or_insert_with(|| {
                AppState::gallery_install_remote_profile(ui.ctx(), tab, scene)
            });
            AppState::gallery_show_remote_profile(ui, tab, configuration);
        },
        (),
    );
    harness.run_steps(4);
    if matches!(
        scene,
        "profile-dev-tunnel-advanced" | "profile-dev-tunnel-legacy"
    ) {
        harness.get_by_label("Advanced").click();
        harness.run_steps(4);
    }
    finish_experimental(&mut harness)
}

#[cfg(feature = "experimental-dev-tunnels")]
fn capture_host_picker(scene: &'static str) -> image::RgbaImage {
    let tab = AppState::for_test().active();
    let mut harness = gallery_harness(
        egui::vec2(760.0, 900.0),
        move |ui, configuration: &mut Option<festerm_config::Configuration>| {
            let configuration = configuration
                .get_or_insert_with(|| AppState::gallery_host_picker(ui.ctx(), tab, scene));
            let _ = crate::screens::show_profiles(
                ui,
                tab,
                configuration,
                None,
                None,
                festerm_config::PersistenceProviderKind::FestermSessiond,
            );
        },
        None,
    );
    harness.run_steps(4);
    let open = match scene {
        "picker-open" => Some("Host"),
        "session-open" => Some("Existing sessions"),
        _ => None,
    };
    if let Some(label) = open {
        harness.get_by_label(label).click();
        harness.run_steps(4);
    }
    finish_experimental(&mut harness)
}

pub(super) fn scenarios() -> Vec<Scenario> {
    macro_rules! scene {
        ($id:literal, $title:literal, $capture:expr) => {
            Scenario {
                id: $id, section: "experimental-surfaces", title: $title,
                caption: "Production UI with synthetic, inert fixture state. No network, native vault, service, tunnel, remote execution or recording is started. See the route catalog for reachability and native-only limitations.",
                capture: $capture,
            }
        };
    }
    #[cfg(feature = "local-host-settings")]
    macro_rules! remote {
        ($id:literal, $title:literal, $state:literal) => {
            scene!($id, $title, || capture_remote($state))
        };
    }
    macro_rules! automation {
        ($id:literal, $title:literal, $state:literal) => {
            scene!($id, $title, || capture_automation($state))
        };
    }
    macro_rules! recording {
        ($id:literal, $title:literal, $state:literal) => {
            scene!($id, $title, || capture_recording($state))
        };
    }
    let mut catalog = Vec::new();
    #[cfg(feature = "local-host-settings")]
    catalog.extend([
        remote!(
            "remote-unobserved",
            "Remote access · not observed",
            "unobserved"
        ),
        remote!(
            "remote-absent",
            "Remote access · no local installation",
            "absent"
        ),
        remote!("remote-disabled", "Remote access · disabled", "disabled"),
        remote!(
            "remote-enable-requested",
            "Remote access · enable requested",
            "enable-requested"
        ),
        remote!(
            "remote-provisioned",
            "Remote access · provisioned is not ready",
            "provisioned"
        ),
        remote!(
            "remote-reconciling",
            "Remote access · reconciling",
            "reconciling"
        ),
        remote!(
            "remote-stop-requested",
            "Remote access · stopping",
            "stop-requested"
        ),
        remote!(
            "remote-removed",
            "Remote access · recorded removal",
            "removed"
        ),
        remote!(
            "remote-review-enable",
            "Remote access · explicit enable consent",
            "review-enable"
        ),
        remote!(
            "remote-review-owner",
            "Remote access · exact owner-key approval",
            "review-owner"
        ),
        remote!(
            "remote-review-start",
            "Remote access · start confirmation",
            "review-start"
        ),
        remote!(
            "remote-review-observe",
            "Remote access · native observation confirmation",
            "review-observe"
        ),
        remote!(
            "remote-review-inspect",
            "Remote access · owner-context review",
            "review-inspect"
        ),
        remote!(
            "remote-review-recover",
            "Remote access · interrupted setup recovery",
            "review-recover"
        ),
        remote!(
            "remote-review-disable",
            "Remote access · revoke and drain",
            "review-disable"
        ),
        remote!(
            "remote-review-uninstall",
            "Remote access · uninstall confirmation",
            "review-uninstall"
        ),
        remote!(
            "remote-owner-context",
            "Remote access · public owner context",
            "owner-context"
        ),
        remote!(
            "remote-advanced-bundle",
            "Remote access · explicit advanced signed local bundle",
            "advanced"
        ),
        remote!(
            "remote-advanced-selected",
            "Remote access · reviewed manual signed file selection",
            "advanced-selected"
        ),
        remote!(
            "remote-acquiring",
            "Remote access · authenticating exact bundle",
            "acquiring"
        ),
        remote!(
            "remote-authenticated",
            "Remote access · authenticated bytes",
            "authenticated"
        ),
        remote!(
            "remote-qualifying",
            "Remote access · native qualification",
            "qualifying"
        ),
        remote!(
            "remote-complete",
            "Remote access · setup returned, not ready",
            "complete"
        ),
        remote!(
            "remote-device-signin",
            "Remote access · foreground device sign-in",
            "device"
        ),
        remote!(
            "remote-cancelling",
            "Remote access · cancelling and fencing",
            "cancelling"
        ),
        remote!(
            "remote-cleanup-missing",
            "Remote access · missing cleanup receipt",
            "cleanup-missing"
        ),
        remote!(
            "remote-cleanup-unverified",
            "Remote access · unverified stored cleanup context",
            "cleanup-unverified"
        ),
        remote!(
            "remote-cleanup-guidance",
            "Remote access · authenticated manual provider guidance",
            "cleanup-guidance"
        ),
        remote!(
            "remote-share-unsupported",
            "Share exact local shell · unsupported source",
            "share-unsupported"
        ),
        remote!(
            "remote-share-recovery",
            "Share exact local shell · original-operation recovery",
            "share-recovery"
        ),
        remote!(
            "remote-share-prepared",
            "Share exact local shell · private prepared status",
            "share-prepared"
        ),
        remote!(
            "remote-share-active",
            "Share exact local shell · active response",
            "share-active"
        ),
        remote!(
            "remote-share-revoked",
            "Share exact local shell · revoked response",
            "share-revoked"
        ),
        remote!(
            "remote-share-released",
            "Share exact local shell · released response",
            "share-released"
        ),
        remote!(
            "remote-share-uncertain",
            "Share exact local shell · uncertain original operation",
            "share-uncertain"
        ),
        remote!(
            "remote-error-busy",
            "Remote access · admission draining",
            "error-busy"
        ),
        remote!(
            "remote-error-cancelled",
            "Remote access · cancelled operation",
            "error-cancelled"
        ),
        remote!(
            "remote-error-refresh",
            "Remote access · refresh required",
            "error-refresh"
        ),
        remote!(
            "remote-error-changed",
            "Remote access · stale scope refusal",
            "error-changed"
        ),
        remote!(
            "remote-error-private",
            "Remote access · protected storage refusal",
            "error-private"
        ),
        remote!(
            "remote-error-signature",
            "Remote access · signed bundle refused",
            "error-signature"
        ),
        remote!(
            "remote-error-unpublished",
            "Remote access · exact release not published",
            "error-unpublished"
        ),
        remote!(
            "remote-error-download",
            "Remote access · bounded acquisition failed",
            "error-download"
        ),
        remote!(
            "remote-error-vault",
            "Remote access · native vault unavailable",
            "error-vault"
        ),
        remote!(
            "remote-error-account",
            "Remote access · account verification required",
            "error-account"
        ),
        remote!(
            "remote-error-owner",
            "Remote access · owner approval refused",
            "error-owner"
        ),
        remote!(
            "remote-error-platform",
            "Remote access · native policy unqualified",
            "error-platform"
        ),
        remote!(
            "remote-error-recovery",
            "Remote access · incomplete reconciliation",
            "error-recovery"
        ),
        remote!(
            "remote-error-share",
            "Remote access · source capability refused",
            "error-share"
        ),
        remote!(
            "remote-error-session",
            "Remote access · session generation changed",
            "error-session"
        ),
        remote!(
            "remote-error-native",
            "Remote access · no native observation",
            "error-native"
        ),
        remote!(
            "remote-error-draft",
            "Remote access · bounded draft validation",
            "error-draft"
        ),
        remote!(
            "remote-error-cloud",
            "Remote access · cloud outcome uncertain",
            "error-cloud"
        ),
    ]);
    catalog.extend([
        scene!("enterprise-setup", "Enterprise discovery · setup", || {
            let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                crate::enterprise::EnterpriseState::Setup,
            );
            render(move |ui| {
                let _ = tab.show(ui);
            })
        }),
        scene!(
            "enterprise-signing-in",
            "Enterprise discovery · signing in",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::SigningIn,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-discovering",
            "Enterprise discovery · discovering",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::Discovering,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-inventory",
            "Enterprise discovery · read-only inventory",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::Ready,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-failed",
            "Enterprise discovery · refused request",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::Failed,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-closing",
            "Enterprise discovery · closing",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::Closing,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-closed",
            "Enterprise discovery · closed",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_fixture(
                    crate::enterprise::EnterpriseState::Closed,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-setup",
            "PowerShell · credential and transport setup",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Setup,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-ssh-setup",
            "PowerShell · SSH transport and pinned identity",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_ssh_fixture();
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!("powershell-connecting", "PowerShell · connecting", || {
            let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                crate::powershell::PowerShellState::Connecting,
            );
            render(move |ui| {
                let _ = tab.show(ui);
            })
        }),
        scene!(
            "powershell-ready",
            "PowerShell · structured result details",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Ready,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-structured-value",
            "PowerShell · expanded typed object",
            capture_powershell_details
        ),
        scene!(
            "powershell-running",
            "PowerShell · running pipeline",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Running,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-cancelling",
            "PowerShell · cancellation pending",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Cancelling,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-failed",
            "PowerShell · connection refusal",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Failed,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "powershell-closing",
            "PowerShell · runspace cleanup",
            || {
                let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                    crate::powershell::PowerShellState::Closing,
                );
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!("powershell-closed", "PowerShell · runspace closed", || {
            let mut tab = crate::powershell::PowerShellTab::gallery_fixture(
                crate::powershell::PowerShellState::Closed,
            );
            render(move |ui| {
                let _ = tab.show(ui);
            })
        }),
        recording!(
            "recording-settings",
            "Recording · device-local defaults",
            "settings"
        ),
        recording!(
            "recording-start",
            "Recording · explicit session consent",
            "start"
        ),
        recording!(
            "recording-privacy-blocked",
            "Recording · native privacy monitoring unavailable",
            "privacy-blocked"
        ),
        recording!(
            "recording-include",
            "Recording · include additional sessions",
            "include"
        ),
        recording!(
            "recording-export",
            "Recording · calculate and export",
            "export"
        ),
        recording!(
            "recording-library",
            "Recording · private source library",
            "library"
        ),
        recording!(
            "recording-library-empty",
            "Recording · inspected empty private library",
            "library-empty"
        ),
        scene!(
            "recording-status-active",
            "Recording · active capture status and stop",
            || render(|ui| crate::app::experimental_gallery::recording_status(ui, "active"))
        ),
        scene!(
            "recording-status-preparing",
            "Recording · preparation status and cancel",
            || render(|ui| crate::app::experimental_gallery::recording_status(ui, "preparing"))
        ),
        scene!(
            "recording-status-exporting",
            "Recording · export status and cancel",
            || render(|ui| crate::app::experimental_gallery::recording_status(ui, "exporting"))
        ),
        scene!(
            "recording-status-failed",
            "Recording · incomplete retained source status",
            || render(|ui| crate::app::experimental_gallery::recording_status(ui, "failed"))
        ),
        scene!(
            "recording-status-complete",
            "Recording · ready for export status",
            || render(|ui| crate::app::experimental_gallery::recording_status(ui, "complete"))
        ),
        recording!(
            "recording-retry",
            "Recording · retry retained private source",
            "retry"
        ),
        recording!(
            "recording-recovered",
            "Recording · recover validated prefix",
            "recovered"
        ),
        recording!(
            "recording-delete-source",
            "Recording · delete private source confirmation",
            "delete-source"
        ),
        recording!(
            "recording-delete-manifest",
            "Recording · delete unfinished options confirmation",
            "delete-manifest"
        ),
        recording!(
            "recording-delete-staging",
            "Recording · delete staged video confirmation",
            "delete-staging"
        ),
        automation!("mcp-disabled", "MCP · disabled", "disabled"),
        automation!(
            "mcp-observation-review",
            "MCP · exact attachment observation consent",
            "observation-review"
        ),
        automation!(
            "mcp-observation-granted",
            "MCP · observation grant and revocation",
            "observation-granted"
        ),
        automation!(
            "mcp-direct-verify",
            "MCP · independent direct-channel fingerprint",
            "direct-verify"
        ),
        automation!(
            "mcp-controller-idle",
            "MCP controller · registration grants no authority",
            "controller-idle"
        ),
        automation!(
            "mcp-controller-choose",
            "MCP controller · choose this window's exact terminal",
            "controller-choose"
        ),
        automation!(
            "mcp-controller-associate",
            "MCP controller · association confirmation",
            "controller-associate"
        ),
        automation!(
            "mcp-controller-metadata",
            "MCP controller · metadata consent entry",
            "controller-metadata"
        ),
        automation!(
            "mcp-controller-metadata-review",
            "MCP controller · separate metadata review",
            "controller-metadata-review"
        ),
        automation!(
            "mcp-controller-metadata-granted",
            "MCP controller · granted metadata revocation",
            "controller-metadata-granted"
        ),
        automation!(
            "mcp-controller-input-request",
            "MCP controller · requested input review entry",
            "controller-input-request"
        ),
        automation!(
            "mcp-controller-input-review",
            "MCP controller · exact text confirmation",
            "controller-input-review"
        ),
        automation!(
            "mcp-controller-keys-review",
            "MCP controller · exact key confirmation",
            "controller-keys-review"
        ),
        automation!(
            "mcp-controller-submitting",
            "MCP controller · submission may be admitted",
            "controller-submitting"
        ),
        automation!(
            "mcp-controller-preparing",
            "MCP controller · preparing exact input",
            "controller-preparing"
        ),
        automation!(
            "mcp-controller-cancelling",
            "MCP controller · cancelling requested input",
            "controller-cancelling"
        ),
        automation!(
            "mcp-controller-metadata-cancelling",
            "MCP controller · metadata consent cancelling",
            "controller-metadata-cancelling"
        ),
        automation!(
            "mcp-controller-metadata-revoking",
            "MCP controller · metadata grant revoking",
            "controller-metadata-revoking"
        ),
        automation!(
            "mcp-controller-armed",
            "MCP controller · armed control and disarm",
            "controller-armed"
        ),
        automation!(
            "workflow-empty",
            "JavaScript workflow · offline selection entry",
            "workflow-empty"
        ),
        automation!(
            "workflow-capturing",
            "JavaScript workflow · bounded source capture",
            "workflow-capturing"
        ),
        automation!(
            "workflow-waiting",
            "JavaScript workflow · newer source selection waiting",
            "workflow-waiting"
        ),
        automation!(
            "workflow-failed",
            "JavaScript workflow · validation refusal",
            "workflow-failed"
        ),
        automation!(
            "workflow-cancelling",
            "JavaScript workflow · cancelling source read",
            "workflow-cancelling"
        ),
        automation!(
            "workflow-preview",
            "JavaScript workflow · frozen snapshot and privacy gate",
            "workflow-preview"
        ),
        automation!(
            "workflow-source",
            "JavaScript workflow · captured source review",
            "workflow-source"
        ),
        automation!(
            "workflow-monitoring",
            "JavaScript workflow · privacy monitoring starting",
            "workflow-monitoring"
        ),
        automation!(
            "workflow-ready",
            "JavaScript workflow · explicit execution consent",
            "workflow-ready"
        ),
        automation!(
            "workflow-running",
            "JavaScript workflow · active run management",
            "workflow-running"
        ),
        automation!(
            "workflow-input",
            "JavaScript workflow · exact input consent",
            "workflow-input"
        ),
        automation!(
            "workflow-sending",
            "JavaScript workflow · approved input waits for idle",
            "workflow-sending"
        ),
    ]);
    #[cfg(feature = "experimental-dev-tunnels")]
    {
        macro_rules! browser {
            ($id:literal, $title:literal, $kind:literal, $fixture:ident, $state:literal) => {
                scene!($id, $title, || app_surface(
                    crate::app::FesTermApp::$fixture($state),
                    $kind
                ))
            };
        }
        macro_rules! recipe {
            ($id:literal, $title:literal, $state:literal) => {
                scene!($id, $title, || render(|ui| AppState::gallery_host_recipes(
                    ui, $state
                )))
            };
        }
        macro_rules! account {
            ($id:literal, $title:literal, $state:literal) => {
                scene!($id, $title, || render(|ui| {
                    crate::app::FesTermApp::gallery_hosting_account(ui.ctx(), $state)
                }))
            };
        }
        macro_rules! picker {
            ($id:literal, $title:literal, $state:literal) => {
                scene!($id, $title, || capture_host_picker($state))
            };
        }
        catalog.extend([
            picker!(
                "host-profile-picker-open",
                "New Dev Tunnel profile · hosts grouped by account",
                "picker-open"
            ),
            picker!(
                "host-profile-picker-refreshing",
                "New Dev Tunnel profile · refreshing every account",
                "picker-refreshing"
            ),
            picker!(
                "host-profile-picker-stale",
                "New Dev Tunnel profile · refresh failed, cached hosts kept",
                "picker-stale"
            ),
            picker!(
                "host-profile-session-open",
                "New Dev Tunnel profile · existing sessions dropdown",
                "session-open"
            ),
            picker!(
                "host-profile-session-existing",
                "New Dev Tunnel profile · existing session selected",
                "session-existing"
            ),
            picker!(
                "host-profile-session-new",
                "New Dev Tunnel profile · new session named",
                "session-new"
            ),
            picker!(
                "host-profile-session-locked",
                "Edit Dev Tunnel profile · saved session kept",
                "session-locked"
            ),
        ]);
        catalog.extend([
            browser!(
                "host-review",
                "Selected HOST · first-connect binding consent",
                "host",
                gallery_host_browser,
                "review"
            ),
            browser!(
                "host-review-working",
                "Selected HOST · bounded owner worker",
                "host",
                gallery_host_browser,
                "working"
            ),
            browser!(
                "host-review-error",
                "Selected HOST · protected-store refusal",
                "host",
                gallery_host_browser,
                "error"
            ),
            browser!(
                "host-owner-key",
                "Selected HOST · public owner key and independent host approval",
                "host",
                gallery_host_browser,
                "owner"
            ),
            browser!(
                "host-inventory-empty",
                "Selected HOST · empty inventory grants no creation authority",
                "host",
                gallery_host_browser,
                "inventory"
            ),
            browser!(
                "host-inventory-unknown",
                "Selected HOST · unknown is never absent",
                "host",
                gallery_host_browser,
                "unknown"
            ),
            browser!(
                "host-inventory-sessions",
                "Selected HOST · authorized available and occupied sessions",
                "host",
                gallery_host_browser,
                "sessions"
            ),
            browser!(
                "host-create-review",
                "Selected HOST · explicit exact launch confirmation",
                "host",
                gallery_host_browser,
                "create"
            ),
            browser!(
                "host-temporary-review",
                "Selected HOST · temporary lifetime creation consent",
                "host",
                gallery_host_browser,
                "temporary"
            ),
            browser!(
                "dev-tunnels-working",
                "Dev Tunnels · discovery in progress",
                "tunnels",
                gallery_tunnel_browser,
                "working"
            ),
            browser!(
                "dev-tunnels-signin",
                "Dev Tunnels · device sign-in",
                "tunnels",
                gallery_tunnel_browser,
                "device"
            ),
            browser!(
                "dev-tunnels-list",
                "Dev Tunnels · account browser",
                "tunnels",
                gallery_tunnel_browser,
                "list"
            ),
            browser!(
                "dev-tunnels-empty",
                "Dev Tunnels · empty account",
                "tunnels",
                gallery_tunnel_browser,
                "empty"
            ),
            browser!(
                "dev-tunnels-rename",
                "Dev Tunnels · rename public friendly name",
                "tunnels",
                gallery_tunnel_browser,
                "rename"
            ),
            browser!(
                "dev-tunnels-delete",
                "Dev Tunnels · destructive deletion confirmation",
                "tunnels",
                gallery_tunnel_browser,
                "delete"
            ),
            browser!(
                "dev-tunnels-error",
                "Dev Tunnels · secure storage unavailable",
                "tunnels",
                gallery_tunnel_browser,
                "error"
            ),
            browser!(
                "dev-tunnel-connect",
                "Dev Tunnel connection · working",
                "connect",
                gallery_tunnel_connect,
                "working"
            ),
            browser!(
                "dev-tunnel-device",
                "Dev Tunnel connection · explicit device sign-in",
                "connect",
                gallery_tunnel_connect,
                "device"
            ),
            browser!(
                "dev-tunnel-trust",
                "Dev Tunnel connection · host fingerprint trust",
                "connect",
                gallery_tunnel_connect,
                "host"
            ),
            browser!(
                "dev-tunnel-connect-error",
                "Dev Tunnel connection · native storage refusal",
                "connect",
                gallery_tunnel_connect,
                "error"
            ),
            browser!(
                "hosting-new-shell",
                "Profile hosting · new persistent shell confirmation",
                "hosting",
                gallery_hosting_confirmation,
                "new-shell"
            ),
            browser!(
                "hosting-recreate",
                "Profile hosting · recreate missing exposure confirmation",
                "hosting",
                gallery_hosting_confirmation,
                "recreate"
            ),
            browser!(
                "hosting-cleanup",
                "Profile hosting · unreferenced exposure cleanup",
                "hosting",
                gallery_hosting_confirmation,
                "cleanup"
            ),
            account!(
                "hosting-account",
                "Profile hosting · explicit GitHub account choice",
                "ready"
            ),
            account!(
                "hosting-account-working",
                "Profile hosting · checking or waiting for browser",
                "working"
            ),
            account!(
                "hosting-account-signed-out",
                "Profile hosting · no verified account",
                "signed-out"
            ),
            account!(
                "hosting-account-conflict",
                "Profile hosting · incompatible recorded accounts",
                "conflict"
            ),
            account!(
                "hosting-account-error",
                "Profile hosting · verified helper unavailable",
                "error"
            ),
            recipe!(
                "host-recipes-list",
                "HOST recipes · offline metadata list",
                "list"
            ),
            recipe!(
                "host-recipes-add",
                "HOST recipes · add public launch metadata",
                "add"
            ),
            recipe!(
                "host-recipes-edit",
                "HOST recipes · edit future launch metadata",
                "edit"
            ),
            recipe!(
                "host-recipes-copy",
                "HOST recipes · duplicate with fresh identity",
                "duplicate"
            ),
            recipe!(
                "host-recipes-delete",
                "HOST recipes · delete metadata, keep shell",
                "delete"
            ),
            recipe!(
                "host-recipes-import",
                "HOST recipes · paste portable JSON",
                "import"
            ),
            recipe!(
                "host-recipes-import-review",
                "HOST recipes · review unverified import",
                "import-review"
            ),
            recipe!(
                "host-recipes-export",
                "HOST recipes · portable public metadata export",
                "export"
            ),
            recipe!(
                "host-recipes-error",
                "HOST recipes · failed metadata persistence",
                "error"
            ),
            browser!(
                "dev-tunnel-changed-host",
                "Dev Tunnel · previous and proposed host identity",
                "connect",
                gallery_tunnel_connect,
                "changed-host"
            ),
            browser!(
                "dev-tunnel-save-account",
                "Dev Tunnel · account-save-only host trust",
                "connect",
                gallery_tunnel_connect,
                "save-account"
            ),
            browser!(
                "host-retry-original",
                "Selected HOST · retained original-action metadata",
                "host",
                gallery_host_browser,
                "retry-original"
            ),
            browser!(
                "host-resolved-unpresented",
                "Selected HOST · host-confirmed original not admitted for presentation",
                "host",
                gallery_host_browser,
                "resolved-original"
            ),
            browser!(
                "host-always-create",
                "Selected HOST · explicitly fresh logical session",
                "host",
                gallery_host_browser,
                "always-create"
            ),
            browser!(
                "host-attach-exact-generation",
                "Selected HOST · exact-generation attach only",
                "host",
                gallery_host_browser,
                "attach-only"
            ),
            recipe!(
                "host-recipes-import-invalid",
                "HOST recipes · invalid portable JSON refusal",
                "import-invalid"
            ),
            recipe!(
                "host-recipes-import-reviewed",
                "HOST recipes · explicitly reviewed unverified import",
                "import-reviewed"
            ),
            recipe!(
                "host-recipes-edit-temporary",
                "HOST recipes · bounded temporary lifetime",
                "edit-temporary"
            ),
            recipe!(
                "host-recipes-edit-attach-only",
                "HOST recipes · exact running generation",
                "edit-attach-only"
            ),
            recipe!(
                "host-recipes-edit-always-create",
                "HOST recipes · always create behavior",
                "edit-always-create"
            ),
            recipe!(
                "host-recipes-edit-resume",
                "HOST recipes · resume-or-create (persistent default; same editor)",
                "edit-resume"
            ),
        ]);
    }
    #[cfg(feature = "local-host-settings")]
    catalog.push(remote!(
        "remote-share-eligible",
        "Share exact local shell · eligible-session selection",
        "share-eligible"
    ));
    catalog.extend([
        scene!(
            "enterprise-empty-inventory",
            "Enterprise discovery · no owned inventory",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_inventory("empty");
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-unauthorized-project",
            "Enterprise discovery · project resource authorization refused",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_inventory("unauthorized");
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        scene!(
            "enterprise-truncated-inventory",
            "Enterprise discovery · bounded omitted and unfetched results",
            || {
                let mut tab = crate::enterprise::EnterpriseTab::gallery_inventory("truncated");
                render(move |ui| {
                    let _ = tab.show(ui);
                })
            }
        ),
        recording!(
            "recording-library-populated",
            "Recording · complete, recoverable, corrupt, empty, orphan and staged rows",
            "library-populated"
        ),
        recording!(
            "recording-export-calculating",
            "Recording · synchronized duration calculation",
            "export-calculating"
        ),
        recording!(
            "recording-export-ready",
            "Recording · calculated duration and export confirmation",
            "export-ready"
        ),
        automation!(
            "mcp-controller-choose-empty",
            "MCP controller · no local target in this window",
            "controller-choose-empty"
        ),
        automation!(
            "mcp-controller-choose-incomplete",
            "MCP controller · bounded incomplete target inventory",
            "controller-choose-incomplete"
        ),
        automation!(
            "mcp-controller-choose-foreign",
            "MCP controller · choice belongs to another window",
            "controller-choose-foreign"
        ),
        automation!(
            "mcp-controller-associate-missing",
            "MCP controller · selected target disappeared",
            "controller-associate-missing"
        ),
        automation!(
            "mcp-controller-associate-foreign",
            "MCP controller · confirm in originating window",
            "controller-associate-foreign"
        ),
        automation!(
            "mcp-controller-metadata-stale",
            "MCP controller · cancelled metadata review",
            "controller-metadata-stale"
        ),
        automation!(
            "mcp-controller-metadata-missing",
            "MCP controller · missing metadata target",
            "controller-metadata-missing"
        ),
        automation!(
            "mcp-controller-metadata-foreign",
            "MCP controller · metadata belongs to another window",
            "controller-metadata-foreign"
        ),
        automation!(
            "mcp-controller-input-stale",
            "MCP controller · stale input review refused",
            "controller-input-stale"
        ),
        automation!(
            "mcp-controller-input-missing",
            "MCP controller · input target disappeared",
            "controller-input-missing"
        ),
        automation!(
            "mcp-controller-input-foreign",
            "MCP controller · review input in associated window",
            "controller-input-foreign"
        ),
        automation!(
            "mcp-controller-input-submit",
            "MCP controller · exact text with explicit Enter",
            "controller-input-submit"
        ),
        automation!(
            "mcp-controller-submitting-not-admitted",
            "MCP controller · submitting approved input",
            "controller-submitting-not-admitted"
        ),
        automation!(
            "workflow-capabilities",
            "JavaScript workflow · expanded requested capability disclosure",
            "workflow-capabilities"
        ),
        automation!(
            "workflow-parameters",
            "JavaScript workflow · expanded typed parameters and module list",
            "workflow-parameters"
        ),
        automation!(
            "workflow-source-modules",
            "JavaScript workflow · captured module selector",
            "workflow-source-modules"
        ),
        automation!(
            "workflow-source-page2",
            "JavaScript workflow · second bounded source page",
            "workflow-source-page2"
        ),
        automation!(
            "workflow-parameters-invalid",
            "JavaScript workflow · integer range and text bound refusal",
            "workflow-parameters-invalid"
        ),
        automation!(
            "workflow-parameters-reviewed",
            "JavaScript workflow · reviewed parameter-bearing run",
            "workflow-parameters-reviewed"
        ),
        automation!(
            "workflow-ready-no-target",
            "JavaScript workflow · no eligible local terminal",
            "workflow-ready-no-target"
        ),
    ]);
    catalog.extend([
        recording!(
            "recording-settings-advanced",
            "Recording · typing, burst and progressive-reveal controls",
            "settings-advanced"
        ),
        recording!(
            "recording-settings-invalid",
            "Recording · invalid naming template preview",
            "settings-invalid"
        ),
        recording!(
            "recording-start-empty",
            "Recording · no running terminal available",
            "start-empty"
        ),
        recording!(
            "recording-start-include-new",
            "Recording · consent to future sessions and integrity warning",
            "start-include-new"
        ),
        recording!(
            "recording-include-empty",
            "Recording · no additional eligible sessions",
            "include-empty"
        ),
        recording!(
            "recording-start-options",
            "Recording · expanded one-recording options",
            "start-options"
        ),
        recording!(
            "recording-font-menu",
            "Recording · first-party export font selector",
            "font-menu"
        ),
        recording!(
            "recording-theme-menu",
            "Recording · export theme selector",
            "theme-menu"
        ),
        recording!(
            "recording-speed-menu",
            "Recording · playback-speed selector",
            "speed-menu"
        ),
        automation!(
            "mcp-controller-optin",
            "MCP · explicit new-connection controller registration opt-in",
            "controller-optin"
        ),
        automation!(
            "workflow-starting",
            "JavaScript workflow · isolated worker starting",
            "workflow-starting"
        ),
        automation!(
            "workflow-input-queued",
            "JavaScript workflow · queued exact inputs and refusal notice",
            "workflow-input-queued"
        ),
        automation!(
            "workflow-source-last",
            "JavaScript workflow · final source page boundary",
            "workflow-source-last"
        ),
    ]);
    macro_rules! profile_form {
        ($id:literal, $title:literal) => {
            scene!($id, $title, || capture_remote_profile($id))
        };
    }
    catalog.extend([
        profile_form!(
            "host-setup-windows",
            "Host setup · Windows pinned installer command"
        ),
        profile_form!(
            "host-setup-macos",
            "Host setup · macOS pinned installer command"
        ),
        profile_form!(
            "host-setup-linux",
            "Host setup · Linux dependencies and pinned installer"
        ),
        profile_form!(
            "host-setup-upgrade",
            "Host setup · upgrade disables friendly name and refuses takeover"
        ),
        profile_form!(
            "profile-dev-tunnel-new",
            "Dev Tunnel profile · basic new form"
        ),
        profile_form!(
            "profile-dev-tunnel-edit",
            "Dev Tunnel profile · saved public endpoint editing"
        ),
        profile_form!(
            "profile-dev-tunnel-advanced",
            "Dev Tunnel profile · expanded Advanced controls"
        ),
        profile_form!(
            "profile-dev-tunnel-legacy",
            "Dev Tunnel profile · legacy destination file and scoped token controls"
        ),
        profile_form!(
            "profile-dev-tunnel-validation",
            "Dev Tunnel profile · invalid locator refusal"
        ),
        profile_form!(
            "profile-dev-tunnel-saving",
            "Dev Tunnel profile · pending transactional save"
        ),
        profile_form!(
            "profile-dev-tunnel-save-error",
            "Dev Tunnel profile · failed save retains draft"
        ),
        profile_form!(
            "profile-powershell-new",
            "PowerShell profile · basic HTTPS/NTLM form"
        ),
        profile_form!(
            "profile-powershell-https-edit",
            "PowerShell profile · named HTTPS configuration, CA and Ask source"
        ),
        profile_form!(
            "profile-powershell-ssh-edit",
            "PowerShell profile · pinned SSH subsystem editing"
        ),
        profile_form!(
            "profile-powershell-validation",
            "PowerShell profile · invalid port refusal"
        ),
        profile_form!(
            "profile-powershell-ssh-validation",
            "PowerShell profile · missing SSH host pin refusal"
        ),
        profile_form!(
            "profile-powershell-endpoint-changed",
            "PowerShell profile · endpoint change clears password"
        ),
    ]);
    catalog
}

#[test]
fn experimental_catalog_is_unique_and_routes_are_valid_json() {
    let scenes = scenarios();
    let ids: std::collections::BTreeSet<_> = scenes.iter().map(|scene| scene.id).collect();
    assert_eq!(ids.len(), scenes.len());
    let routes: serde_json::Value = serde_json::from_str(ROUTES).unwrap();
    assert!(routes["groups"].is_array());
    let mut declared = std::collections::BTreeSet::new();
    for group in routes["groups"].as_array().unwrap() {
        for field in ["route", "exit"] {
            assert!(
                group[field]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
                "missing {field}"
            );
        }
        for field in ["production_symbols", "gui_ids"] {
            assert!(
                group[field]
                    .as_array()
                    .is_some_and(|items| !items.is_empty()),
                "missing {field}"
            );
        }
        if let Some(prefix) = group["id_prefix"].as_str() {
            for state in group["states"].as_array().unwrap() {
                assert!(declared.insert(format!("{prefix}{}", state.as_str().unwrap())));
            }
        } else {
            for id in group["ids"].as_array().unwrap() {
                assert!(declared.insert(id.as_str().unwrap().to_owned()));
            }
        }
    }
    for id in &ids {
        assert!(declared.contains(*id), "missing route for {id}");
    }
    #[cfg(all(feature = "experimental-dev-tunnels", feature = "local-host-settings"))]
    assert_eq!(declared.len(), ids.len(), "orphan route entry");
    assert!(scenes
        .iter()
        .all(|scene| scene.section == "experimental-surfaces"));
}

#[test]
fn experimental_catalog_fixture_models_render_without_workers() {
    fn frame(mut show: impl FnMut(&mut egui::Ui)) {
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| show(ui));
        output.textures_delta.clear();
    }
    #[cfg(feature = "local-host-settings")]
    for scene in [
        "unobserved",
        "disabled",
        "enable-requested",
        "provisioned",
        "reconciling",
        "stop-requested",
        "cleanup-missing",
        "cleanup-unverified",
        "cleanup-guidance",
        "removed",
        "device-signin",
        "share-eligible",
        "share-prepared",
        "share-uncertain",
        "review-owner",
    ] {
        let view = remote_view(scene);
        frame(|ui| {
            let _ = remote::show(ui, &view, &[]);
        });
    }
    #[cfg(feature = "experimental-dev-tunnels")]
    {
        for scene in [
            "review",
            "working",
            "error",
            "owner",
            "inventory",
            "unknown",
            "sessions",
            "create",
            "temporary",
            "retry-original",
            "resolved-original",
            "always-create",
            "attach-only",
        ] {
            let mut app = crate::app::FesTermApp::gallery_host_browser(scene);
            frame(|ui| crate::app::experimental_gallery::show(&mut app, ui.ctx(), "host"));
        }
        for scene in [
            "working", "device", "error", "list", "empty", "rename", "delete",
        ] {
            let mut app = crate::app::FesTermApp::gallery_tunnel_browser(scene);
            frame(|ui| crate::app::experimental_gallery::show(&mut app, ui.ctx(), "tunnels"));
        }
        for scene in [
            "working",
            "device",
            "host",
            "error",
            "changed-host",
            "save-account",
        ] {
            let mut app = crate::app::FesTermApp::gallery_tunnel_connect(scene);
            frame(|ui| crate::app::experimental_gallery::show(&mut app, ui.ctx(), "connect"));
        }
        for scene in ["new-shell", "recreate", "cleanup"] {
            let mut app = crate::app::FesTermApp::gallery_hosting_confirmation(scene);
            frame(|ui| crate::app::experimental_gallery::show(&mut app, ui.ctx(), "hosting"));
        }
        for scene in ["ready", "working", "signed-out", "conflict", "error"] {
            frame(|ui| crate::app::FesTermApp::gallery_hosting_account(ui.ctx(), scene));
        }
        for scene in [
            "list",
            "add",
            "edit",
            "duplicate",
            "delete",
            "import",
            "import-review",
            "export",
            "error",
            "import-invalid",
            "import-reviewed",
            "edit-temporary",
            "edit-attach-only",
            "edit-always-create",
            "edit-resume",
        ] {
            frame(|ui| AppState::gallery_host_recipes(ui, scene));
        }
    }
    for scene in [
        "disabled",
        "observation-review",
        "observation-granted",
        "direct-verify",
        "controller-idle",
        "controller-optin",
        "controller-choose",
        "controller-associate",
        "controller-metadata",
        "controller-metadata-granted",
        "controller-metadata-review",
        "controller-input-request",
        "controller-input-review",
        "controller-keys-review",
        "controller-preparing",
        "controller-submitting",
        "controller-cancelling",
        "controller-metadata-cancelling",
        "controller-metadata-revoking",
        "controller-armed",
        "controller-choose-empty",
        "controller-choose-incomplete",
        "controller-choose-foreign",
        "controller-associate-missing",
        "controller-associate-foreign",
        "controller-metadata-stale",
        "controller-metadata-missing",
        "controller-metadata-foreign",
        "controller-input-stale",
        "controller-input-missing",
        "controller-input-foreign",
        "controller-input-submit",
        "controller-submitting-not-admitted",
        "workflow-empty",
        "workflow-capturing",
        "workflow-waiting",
        "workflow-failed",
        "workflow-cancelling",
        "workflow-preview",
        "workflow-source",
        "workflow-monitoring",
        "workflow-ready",
        "workflow-running",
        "workflow-input",
        "workflow-sending",
        "workflow-starting",
        "workflow-input-queued",
        "workflow-capabilities",
        "workflow-parameters",
        "workflow-source-modules",
        "workflow-source-page2",
        "workflow-source-last",
        "workflow-parameters-invalid",
        "workflow-parameters-reviewed",
        "workflow-ready-no-target",
    ] {
        frame(|ui| crate::automation::SettingsSnapshot::gallery_show(ui, scene));
    }
    for scene in [
        "settings",
        "settings-advanced",
        "settings-invalid",
        "start",
        "start-empty",
        "start-include-new",
        "privacy-blocked",
        "include",
        "include-empty",
        "export",
        "library",
        "library-empty",
        "retry",
        "recovered",
        "delete-source",
        "delete-manifest",
        "delete-staging",
        "library-populated",
        "export-calculating",
        "export-ready",
    ] {
        frame(|ui| crate::application::recording::gallery_show(ui.ctx(), scene));
    }
    for scene in ["empty", "unauthorized", "truncated"] {
        let mut tab = crate::enterprise::EnterpriseTab::gallery_inventory(scene);
        frame(|ui| {
            let _ = tab.show(ui);
        });
    }
    for scene in [
        "host-setup-windows",
        "host-setup-macos",
        "host-setup-linux",
        "host-setup-upgrade",
        "profile-dev-tunnel-new",
        "profile-dev-tunnel-edit",
        "profile-dev-tunnel-advanced",
        "profile-dev-tunnel-legacy",
        "profile-dev-tunnel-validation",
        "profile-dev-tunnel-saving",
        "profile-dev-tunnel-save-error",
        "profile-powershell-new",
        "profile-powershell-https-edit",
        "profile-powershell-ssh-edit",
        "profile-powershell-validation",
        "profile-powershell-ssh-validation",
        "profile-powershell-endpoint-changed",
    ] {
        let tab = AppState::for_test().active();
        frame(|ui| {
            let configuration = AppState::gallery_install_remote_profile(ui.ctx(), tab, scene);
            AppState::gallery_show_remote_profile(ui, tab, &configuration);
        });
    }
}
