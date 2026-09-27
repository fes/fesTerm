use std::net::IpAddr;

use eframe::egui::{self, Ui};
use festerm_config::LocalBindPolicy;
use festerm_ssh::SshConnectionProfile;
use festerm_ui_egui::theme;

const MAX_DISCOVERED_LOCAL_ADDRESSES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedLocalBind {
    Automatic,
    Address(IpAddr),
}

impl ResolvedLocalBind {
    pub(crate) const fn from_address(address: Option<IpAddr>) -> Self {
        match address {
            Some(address) => Self::Address(address),
            None => Self::Automatic,
        }
    }

    pub(crate) const fn address(self) -> Option<IpAddr> {
        match self {
            Self::Automatic => None,
            Self::Address(address) => Some(address),
        }
    }

    pub(crate) const fn policy(self) -> LocalBindPolicy {
        match self {
            Self::Automatic => LocalBindPolicy::Automatic,
            Self::Address(address) => LocalBindPolicy::Address(address),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalBindDraft {
    mode: LocalBindMode,
    address: String,
    discovered: Vec<DiscoveredLocalAddress>,
    discovery_error: Option<String>,
    refreshed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalBindMode {
    Automatic,
    Fixed,
    Ask,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveredLocalAddress {
    name: String,
    address: IpAddr,
}

impl Default for LocalBindDraft {
    fn default() -> Self {
        Self::from_policy(LocalBindPolicy::Automatic)
    }
}

impl LocalBindDraft {
    pub(crate) fn from_policy(policy: LocalBindPolicy) -> Self {
        let address = policy
            .fixed_address()
            .map(|address| address.to_string())
            .unwrap_or_default();
        Self {
            mode: match policy {
                LocalBindPolicy::Automatic => LocalBindMode::Automatic,
                LocalBindPolicy::Address(_) => LocalBindMode::Fixed,
                LocalBindPolicy::Ask => LocalBindMode::Ask,
            },
            address,
            discovered: Vec::new(),
            discovery_error: None,
            refreshed: false,
        }
    }

    pub(crate) fn resolved(&self) -> Result<ResolvedLocalBind, String> {
        match self.mode {
            LocalBindMode::Automatic => Ok(ResolvedLocalBind::Automatic),
            LocalBindMode::Fixed => {
                parse_local_bind_address(&self.address).map(ResolvedLocalBind::Address)
            }
            LocalBindMode::Ask => {
                Err("Choose Automatic or a fixed source address before connecting.".to_owned())
            }
        }
    }

    pub(crate) fn saved_policy(&self) -> Result<LocalBindPolicy, String> {
        match self.mode {
            LocalBindMode::Automatic => Ok(LocalBindPolicy::Automatic),
            LocalBindMode::Fixed => {
                parse_local_bind_address(&self.address).map(LocalBindPolicy::Address)
            }
            LocalBindMode::Ask => Ok(LocalBindPolicy::Ask),
        }
    }

    pub(crate) fn apply_to_profile(
        &self,
        profile: SshConnectionProfile,
    ) -> Result<SshConnectionProfile, String> {
        profile
            .with_local_bind_address(self.resolved()?.address())
            .map_err(|error| error.to_string())
    }
}

pub(crate) fn parse_local_bind_address(input: &str) -> Result<IpAddr, String> {
    let address: IpAddr = input
        .trim()
        .parse()
        .map_err(|_| "Enter a numeric IPv4 or IPv6 source address.".to_owned())?;
    validate_local_bind_address(address)?;
    Ok(address)
}

fn validate_local_bind_address(address: IpAddr) -> Result<(), String> {
    festerm_ssh::validate_local_bind_address(address).map_err(|error| error.to_string())
}

pub(crate) fn show_local_bind_draft(
    ui: &mut Ui,
    draft: &mut LocalBindDraft,
    allow_ask: bool,
    id_salt: impl std::hash::Hash + std::fmt::Debug + Copy,
) {
    show_local_bind_draft_with_discovery(ui, draft, allow_ask, id_salt, discover_local_addresses);
}

fn show_local_bind_draft_with_discovery(
    ui: &mut Ui,
    draft: &mut LocalBindDraft,
    allow_ask: bool,
    id_salt: impl std::hash::Hash + std::fmt::Debug + Copy,
    mut discover: impl FnMut() -> Result<Vec<DiscoveredLocalAddress>, String>,
) {
    ui.label(egui::RichText::new("Source address").color(theme::TEXT_PRIMARY));
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(&mut draft.mode, LocalBindMode::Automatic, "Automatic");
        ui.radio_value(&mut draft.mode, LocalBindMode::Fixed, "Fixed address");
        if allow_ask {
            ui.radio_value(
                &mut draft.mode,
                LocalBindMode::Ask,
                "Ask on first connection",
            );
        }
    });
    if draft.mode == LocalBindMode::Ask && !allow_ask {
        ui.label("Choose a source address or Automatic for this connection.");
    }
    ui.label(
        egui::RichText::new(
            "Binds only the socket source IP. It does not pin a network interface, VPN, route, or DNS path.",
        )
        .size(11.0)
        .color(theme::TEXT_SECONDARY),
    );
    if draft.mode == LocalBindMode::Fixed {
        ui.horizontal_wrapped(|ui| {
            let label = ui.label("Local source IP");
            ui.add(
                egui::TextEdit::singleline(&mut draft.address)
                    .id(ui.make_persistent_id((id_salt, "address")))
                    .desired_width(180.0),
            )
            .labelled_by(label.id);
            if ui.button("Refresh local addresses").clicked() || !draft.refreshed {
                match discover() {
                    Ok(discovered) => {
                        draft.discovery_error = None;
                        draft.discovered = discovered;
                    }
                    Err(error) => {
                        draft.discovery_error = Some(error);
                        draft.discovered.clear();
                    }
                }
                draft.refreshed = true;
            }
        });
        if let Some(error) = &draft.discovery_error {
            ui.colored_label(theme::STATUS_ERROR, error);
        } else if draft.refreshed && draft.discovered.is_empty() {
            ui.label(
                egui::RichText::new(
                    "No eligible local addresses found. You can still enter a numeric address manually.",
                )
                .size(11.0)
                .color(theme::TEXT_SECONDARY),
            );
        }
        if !draft.discovered.is_empty() {
            egui::ComboBox::from_id_salt((id_salt, "addresses"))
                .selected_text("Use discovered address…")
                .show_ui(ui, |ui| {
                    for item in &draft.discovered {
                        if ui
                            .button(format!("{} — {}", item.address, item.name))
                            .clicked()
                        {
                            draft.address = item.address.to_string();
                            draft.mode = LocalBindMode::Fixed;
                        }
                    }
                });
        }
        if let Err(error) = parse_local_bind_address(&draft.address) {
            ui.colored_label(theme::STATUS_ERROR, error);
        }
    }
}

fn discover_local_addresses() -> Result<Vec<DiscoveredLocalAddress>, String> {
    let interfaces = if_addrs::get_if_addrs()
        .map_err(|error| format!("Could not enumerate local addresses: {error}"))?;
    normalize_discovered_addresses(interfaces.into_iter().map(|interface| {
        let address = interface.ip();
        (interface.name, address)
    }))
}

fn normalize_discovered_addresses(
    interfaces: impl IntoIterator<Item = (String, IpAddr)>,
) -> Result<Vec<DiscoveredLocalAddress>, String> {
    let mut addresses = Vec::new();
    for (name, address) in interfaces {
        if validate_local_bind_address(address).is_err() {
            continue;
        }
        let entry = DiscoveredLocalAddress { name, address };
        if addresses.contains(&entry) {
            continue;
        }
        if addresses.len() == MAX_DISCOVERED_LOCAL_ADDRESSES {
            return Err("Too many local addresses to list. Enter the desired numeric source address manually.".to_owned());
        }
        addresses.push(entry);
    }
    addresses.sort_by(|left, right| {
        left.address
            .to_string()
            .cmp(&right.address.to_string())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use festerm_ssh::HostIdentity;

    #[test]
    fn malformed_and_scoped_source_addresses_are_rejected_before_connecting() {
        assert!(parse_local_bind_address("not an address").is_err());
        assert!(parse_local_bind_address("0.0.0.0").is_err());
        assert!(parse_local_bind_address("224.0.0.1").is_err());
        assert!(parse_local_bind_address("fe80::1").is_err());
        assert!(parse_local_bind_address("::ffff:0.0.0.0").is_err());
        assert!(parse_local_bind_address("::ffff:192.0.2.1").is_err());
    }

    #[test]
    fn explicit_automatic_choice_applies_no_backend_bind_address() {
        let draft = LocalBindDraft::from_policy(LocalBindPolicy::Automatic);
        let profile = SshConnectionProfile::new(
            HostIdentity::new("ssh.example.test", 22).unwrap(),
            "deploy",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            festerm_session::TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap();

        let profile = draft.apply_to_profile(profile).unwrap();

        assert_eq!(profile.local_bind_address(), None);
    }

    #[test]
    fn discovery_filters_unsupported_addresses_and_bounds_results() {
        let mut interfaces = vec![
            ("wildcard".to_owned(), "0.0.0.0".parse().unwrap()),
            ("multicast".to_owned(), "ff02::1".parse().unwrap()),
            ("link-local".to_owned(), "fe80::1".parse().unwrap()),
            ("mapped".to_owned(), "::ffff:192.0.2.1".parse().unwrap()),
        ];
        for index in 0..(MAX_DISCOVERED_LOCAL_ADDRESSES + 5) {
            interfaces.push((
                format!("lo{index}"),
                IpAddr::from([127, 0, 0, (index % 250 + 1) as u8]),
            ));
        }

        assert!(normalize_discovered_addresses(interfaces.clone()).is_err());
        interfaces.truncate(MAX_DISCOVERED_LOCAL_ADDRESSES + 4);
        let discovered = normalize_discovered_addresses(interfaces).unwrap();
        assert_eq!(discovered.len(), MAX_DISCOVERED_LOCAL_ADDRESSES);
        assert!(discovered
            .iter()
            .all(|item| validate_local_bind_address(item.address).is_ok()));
    }

    #[test]
    fn unresolved_ask_cancels_before_backend_profile_is_built() {
        let draft = LocalBindDraft::from_policy(LocalBindPolicy::Ask);

        assert!(draft.resolved().is_err());
    }

    #[test]
    fn source_chooser_requires_choice_and_surfaces_discovery_failure_without_repainting_io() {
        let mut harness = Harness::builder().build_ui_state(
            |ui, state: &mut (LocalBindDraft, usize)| {
                show_local_bind_draft_with_discovery(ui, &mut state.0, false, "fixture", || {
                    state.1 += 1;
                    Err("Fixture enumeration failure; enter an address manually.".to_owned())
                });
            },
            (LocalBindDraft::from_policy(LocalBindPolicy::Ask), 0),
        );
        harness.run();
        assert!(harness.state().0.resolved().is_err());
        assert_eq!(harness.state().1, 0);
        harness.get_by_label("Fixed address").click();
        harness.run();
        assert!(
            harness.state().0.resolved().is_err(),
            "no fabricated default IP"
        );
        assert_eq!(harness.state().1, 1);
        harness.get_by_label("Fixture enumeration failure; enter an address manually.");
        harness.run();
        assert_eq!(harness.state().1, 1);
        harness.get_by_label("Local source IP").click();
        harness.run();
        harness
            .get_by_label("Local source IP")
            .type_text("127.0.0.1");
        harness.run();
        assert_eq!(
            harness.state().0.resolved().unwrap(),
            ResolvedLocalBind::Address(IpAddr::from([127, 0, 0, 1]))
        );
        harness.get_by_label("Refresh local addresses").click();
        harness.run();
        assert_eq!(harness.state().1, 2);
        harness.get_by_label("Automatic").click();
        harness.run();
        assert_eq!(
            harness.state().0.resolved().unwrap(),
            ResolvedLocalBind::Automatic
        );
    }
}
