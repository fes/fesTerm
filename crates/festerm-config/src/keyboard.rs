//! Stable application bindings, independent of the GUI and terminal encoder.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyboardAction {
    CommandPalette,
    NewSession,
    StartLocalShell,
    CloseActiveSurface,
    NextSession,
    PreviousSession,
    Settings,
    SettingsHotkey,
    ZoomIn,
    ZoomInAlternate,
    ZoomOut,
    ZoomReset,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
    PortForwardManager,
    MarkdownFind,
    MarkdownReload,
    MarkdownPreviewSource,
    MarkdownOutline,
    OpenMarkdownFile,
    Find,
    Copy,
    Paste,
    CopyAlternate,
    PasteAlternate,
    Quick1,
    Quick2,
    Quick3,
    Quick4,
    Quick5,
    Quick6,
    Quick7,
    Quick8,
    Quick9,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardScope {
    Global,
    Terminal,
    Markdown,
    Document,
}

impl KeyboardScope {
    pub const ALL: [Self; 4] = [Self::Global, Self::Terminal, Self::Markdown, Self::Document];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Terminal => "Terminal",
            Self::Markdown => "Markdown",
            Self::Document => "Document",
        }
    }

    /// Scope follows from what the action does, so it is presented rather
    /// than offered as a choice.
    pub const fn help(self) -> &'static str {
        match self {
            Self::Global => "Applies anywhere in fesTerm.",
            Self::Terminal => "Applies only while a terminal surface has input.",
            Self::Markdown => "Applies only in the Markdown viewer.",
            Self::Document => "Applies only while a document can be opened.",
        }
    }
}

impl KeyboardAction {
    pub const ALL: [Self; 35] = [
        Self::CommandPalette,
        Self::NewSession,
        Self::StartLocalShell,
        Self::CloseActiveSurface,
        Self::NextSession,
        Self::PreviousSession,
        Self::Settings,
        Self::SettingsHotkey,
        Self::ZoomIn,
        Self::ZoomInAlternate,
        Self::ZoomOut,
        Self::ZoomReset,
        Self::ClearTerminal,
        Self::ResetTerminal,
        Self::ToggleFocusMode,
        Self::PortForwardManager,
        Self::MarkdownFind,
        Self::MarkdownReload,
        Self::MarkdownPreviewSource,
        Self::MarkdownOutline,
        Self::OpenMarkdownFile,
        Self::Find,
        Self::Copy,
        Self::Paste,
        Self::CopyAlternate,
        Self::PasteAlternate,
        Self::Quick1,
        Self::Quick2,
        Self::Quick3,
        Self::Quick4,
        Self::Quick5,
        Self::Quick6,
        Self::Quick7,
        Self::Quick8,
        Self::Quick9,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::CommandPalette => "Command palette",
            Self::NewSession => "New Session",
            Self::StartLocalShell => "Start Local Shell",
            Self::CloseActiveSurface => "Close active surface",
            Self::NextSession => "Next session",
            Self::PreviousSession => "Previous session",
            Self::Settings => "Settings (macOS convention)",
            Self::SettingsHotkey => "Open Settings",
            Self::ZoomIn => "Zoom in",
            Self::ZoomInAlternate => "Zoom in (alternate)",
            Self::ZoomOut => "Zoom out",
            Self::ZoomReset => "Reset zoom",
            Self::ClearTerminal => "Clear terminal",
            Self::ResetTerminal => "Reset terminal",
            Self::ToggleFocusMode => "Toggle Focus Mode",
            Self::PortForwardManager => "Port forward manager",
            Self::MarkdownFind => "Markdown find",
            Self::MarkdownReload => "Markdown reload",
            Self::MarkdownPreviewSource => "Markdown preview / source",
            Self::MarkdownOutline => "Markdown outline",
            Self::OpenMarkdownFile => "Open Markdown file",
            Self::Find => "Find in terminal",
            Self::Copy => "Copy terminal selection",
            Self::Paste => "Paste into terminal",
            Self::CopyAlternate => "Copy terminal selection (Windows alternate)",
            Self::PasteAlternate => "Paste into terminal (Windows alternate)",
            Self::Quick1 => "Switch to tab 1",
            Self::Quick2 => "Switch to tab 2",
            Self::Quick3 => "Switch to tab 3",
            Self::Quick4 => "Switch to tab 4",
            Self::Quick5 => "Switch to tab 5",
            Self::Quick6 => "Switch to tab 6",
            Self::Quick7 => "Switch to tab 7",
            Self::Quick8 => "Switch to tab 8",
            Self::Quick9 => "Switch to tab 9",
        }
    }

    /// One-line intent, shown beside the binding so a user can tell
    /// similarly named actions apart without triggering them.
    pub const fn description(self) -> &'static str {
        match self {
            Self::CommandPalette => "Open the command palette to find and run commands.",
            Self::NewSession => "Open the New Session launcher.",
            Self::StartLocalShell => "Start a local shell session immediately.",
            Self::CloseActiveSurface => "Close the active tab or surface.",
            Self::NextSession => "Switch to the next session tab.",
            Self::PreviousSession => "Switch to the previous session tab.",
            Self::Settings => "Open Settings using the macOS comma convention.",
            Self::SettingsHotkey => "Open Settings on any platform.",
            Self::ZoomIn => "Increase the terminal font size.",
            Self::ZoomInAlternate => "Increase the terminal font size using the equals key.",
            Self::ZoomOut => "Decrease the terminal font size.",
            Self::ZoomReset => "Restore the default terminal font size.",
            Self::ClearTerminal => "Clear the visible terminal grid.",
            Self::ResetTerminal => "Reset the terminal parser and attributes.",
            Self::ToggleFocusMode => "Hide surrounding chrome to focus on the terminal.",
            Self::PortForwardManager => "Open the SSH port forward manager.",
            Self::MarkdownFind => "Find text in the Markdown viewer.",
            Self::MarkdownReload => "Reload the Markdown document from disk.",
            Self::MarkdownPreviewSource => "Switch between rendered preview and source.",
            Self::MarkdownOutline => "Toggle the Markdown outline panel.",
            Self::OpenMarkdownFile => "Open a Markdown file in a new tab.",
            Self::Find => "Find text in the terminal.",
            Self::Copy => "Copy the terminal selection to the clipboard.",
            Self::Paste => "Paste clipboard contents into the terminal.",
            Self::CopyAlternate => "Copy using the Windows Ctrl+Insert convention.",
            Self::PasteAlternate => "Paste using the Windows Shift+Insert convention.",
            Self::Quick1 => "Switch directly to tab 1.",
            Self::Quick2 => "Switch directly to tab 2.",
            Self::Quick3 => "Switch directly to tab 3.",
            Self::Quick4 => "Switch directly to tab 4.",
            Self::Quick5 => "Switch directly to tab 5.",
            Self::Quick6 => "Switch directly to tab 6.",
            Self::Quick7 => "Switch directly to tab 7.",
            Self::Quick8 => "Switch directly to tab 8.",
            Self::Quick9 => "Switch directly to tab 9.",
        }
    }

    pub const fn scope(self) -> KeyboardScope {
        match self {
            Self::ZoomIn
            | Self::ZoomInAlternate
            | Self::ZoomOut
            | Self::ZoomReset
            | Self::ClearTerminal
            | Self::ResetTerminal
            | Self::ToggleFocusMode
            | Self::PortForwardManager
            | Self::Find
            | Self::Copy
            | Self::Paste => KeyboardScope::Terminal,
            Self::CopyAlternate | Self::PasteAlternate => KeyboardScope::Terminal,
            Self::MarkdownFind
            | Self::MarkdownReload
            | Self::MarkdownPreviewSource
            | Self::MarkdownOutline => KeyboardScope::Markdown,
            Self::OpenMarkdownFile => KeyboardScope::Document,
            _ => KeyboardScope::Global,
        }
    }

    pub const fn default_chord(self, mac: bool) -> &'static str {
        match (self, mac) {
            (Self::CommandPalette, _) => "Primary+Shift+P",
            (Self::NewSession, true) => "Primary+T",
            (Self::NewSession, false) => "Primary+Shift+T",
            (Self::StartLocalShell, true) => "Primary+N",
            (Self::StartLocalShell, false) => "Primary+Shift+N",
            (Self::CloseActiveSurface, true) => "Primary+W",
            (Self::CloseActiveSurface, false) => "Primary+Shift+W",
            (Self::NextSession, _) => "Ctrl+Tab",
            (Self::PreviousSession, _) => "Ctrl+Shift+Tab",
            (Self::Settings, true) => "Primary+Comma",
            (Self::Settings, false) => "",
            (Self::SettingsHotkey, _) => "Primary+Shift+S",
            (Self::ZoomIn, _) => "Primary+Plus",
            (Self::ZoomInAlternate, _) => "Primary+Equals",
            (Self::ZoomOut, _) => "Primary+Minus",
            (Self::ZoomReset, _) => "Primary+0",
            (Self::ClearTerminal, true) => "Primary+K",
            (Self::ClearTerminal, false) => "Primary+Shift+K",
            (Self::ResetTerminal, true) => "Primary+Alt+R",
            (Self::ResetTerminal, false) => "Primary+Shift+R",
            (Self::ToggleFocusMode, true) => "Primary+Shift+F",
            (Self::ToggleFocusMode, false) => "Primary+Shift+F11",
            (Self::PortForwardManager, _) => "Primary+Shift+M",
            (Self::MarkdownFind, _) | (Self::Find, true) => "Primary+F",
            (Self::Find, false) => "Primary+Shift+F",
            (Self::MarkdownReload, _) => "Primary+R",
            (Self::MarkdownPreviewSource, _) => "Primary+Shift+V",
            (Self::MarkdownOutline, _) => "Primary+Shift+O",
            (Self::OpenMarkdownFile, _) => "Primary+O",
            (Self::Copy, true) => "Primary+C",
            (Self::Copy, false) => "Primary+Shift+C",
            (Self::Paste, true) => "Primary+V",
            (Self::Paste, false) => "Primary+Shift+V",
            (Self::CopyAlternate, false) if cfg!(windows) => "Ctrl+Insert",
            (Self::PasteAlternate, false) if cfg!(windows) => "Shift+Insert",
            (Self::CopyAlternate | Self::PasteAlternate, _) => "",
            (Self::Quick1, _) => "Primary+1",
            (Self::Quick2, _) => "Primary+2",
            (Self::Quick3, _) => "Primary+3",
            (Self::Quick4, _) => "Primary+4",
            (Self::Quick5, _) => "Primary+5",
            (Self::Quick6, _) => "Primary+6",
            (Self::Quick7, _) => "Primary+7",
            (Self::Quick8, _) => "Primary+8",
            (Self::Quick9, _) => "Primary+9",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardOverride {
    pub action: KeyboardAction,
    /// Empty explicitly unbinds; an absent override inherits the default.
    pub chord: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyboardBindings(pub Vec<KeyboardOverride>);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chord<'a> {
    pub ctrl: bool,
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: &'a str,
}

impl<'a> Chord<'a> {
    pub fn parse(text: &'a str, mac: bool) -> Result<Option<Self>, &'static str> {
        if text.is_empty() {
            return Ok(None);
        }
        if text.len() > 80 {
            return Err("Chord is too long.");
        }
        let mut chord = Self {
            ctrl: false,
            command: false,
            alt: false,
            shift: false,
            key: "",
        };
        let parts: Vec<_> = text.split('+').collect();
        for modifier in &parts[..parts.len() - 1] {
            let flag = match *modifier {
                "Primary" if mac => &mut chord.command,
                "Primary" | "Ctrl" => &mut chord.ctrl,
                "Command" if mac => &mut chord.command,
                "Command" => return Err("Command is supported only on macOS; use Primary."),
                "Alt" => &mut chord.alt,
                "Shift" => &mut chord.shift,
                _ => return Err("Use Primary, Ctrl, Command (macOS), Alt or Shift modifiers."),
            };
            if *flag {
                return Err("Duplicate modifier.");
            }
            *flag = true;
        }
        chord.key = parts[parts.len() - 1];
        // Logical '+' can come from a shifted layout key or an unshifted
        // keypad key. It is one binding, not two distinguishable labels.
        if chord.key == "Plus" {
            chord.shift = false;
        }
        let key = chord.key;
        let supported = (key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase())
            || (key.len() == 1 && key.as_bytes()[0].is_ascii_digit())
            || matches!(
                key,
                "Tab"
                    | "Comma"
                    | "Period"
                    | "Plus"
                    | "Equals"
                    | "Minus"
                    | "F1"
                    | "F2"
                    | "F3"
                    | "F4"
                    | "F5"
                    | "F6"
                    | "F7"
                    | "F8"
                    | "F9"
                    | "F10"
                    | "F11"
                    | "F12"
                    | "Insert"
            );
        if !supported {
            return Err(
                "Unsupported key; use A–Z, 0–9, F1–F12, Tab, Insert, Comma, Period, Plus, Equals or Minus.",
            );
        }
        if !chord.ctrl && !chord.command && !(chord.shift && !chord.alt && chord.key == "Insert") {
            return Err(
                "Bindings require Ctrl or Primary/Command, except Shift+Insert, to protect typing and widget navigation.",
            );
        }
        if chord.ctrl && chord.alt {
            return Err("Ctrl+Alt overlaps AltGr text input and is not supported.");
        }
        if (mac && chord.command && (key == "Q" || (chord.shift && key == "W")))
            || (mac
                && chord.command
                && (key == "Tab" || (!chord.shift && matches!(key, "Q" | "H" | "M"))))
            || (!mac && chord.alt && key == "F4")
            || (chord.ctrl && chord.shift && key == "F12")
        {
            return Err(
                "Reserved by the OS or the Ctrl+Shift+F12 keyboard-settings recovery route.",
            );
        }
        Ok(Some(chord))
    }
}

impl KeyboardBindings {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn effective(&self, action: KeyboardAction, mac: bool) -> &str {
        self.0
            .iter()
            .find(|entry| entry.action == action)
            .map_or(action.default_chord(mac), |entry| entry.chord.as_str())
    }

    pub fn set(&mut self, action: KeyboardAction, chord: Option<String>) {
        self.0.retain(|entry| entry.action != action);
        if let Some(chord) = chord {
            self.0.push(KeyboardOverride { action, chord });
        }
    }

    pub fn validate(&self, mac: bool) -> Result<(), &'static str> {
        for (index, entry) in self.0.iter().enumerate() {
            if self.0[..index]
                .iter()
                .any(|other| other.action == entry.action)
            {
                return Err("An action has more than one override.");
            }
            Chord::parse(&entry.chord, mac)?;
        }
        for (index, action) in KeyboardAction::ALL.iter().enumerate() {
            let Some(chord) = Chord::parse(self.effective(*action, mac), mac)? else {
                continue;
            };
            for other in &KeyboardAction::ALL[..index] {
                let scopes_overlap = action.scope() == other.scope()
                    || action.scope() == KeyboardScope::Global
                    || other.scope() == KeyboardScope::Global
                    || (mac
                        && matches!(
                            (action.scope(), other.scope()),
                            (KeyboardScope::Document, KeyboardScope::Terminal)
                                | (KeyboardScope::Terminal, KeyboardScope::Document)
                        ))
                    || matches!(
                        (action.scope(), other.scope()),
                        (KeyboardScope::Document, KeyboardScope::Markdown)
                            | (KeyboardScope::Markdown, KeyboardScope::Document)
                    );
                if scopes_overlap && Chord::parse(self.effective(*other, mac), mac)? == Some(chord)
                {
                    return Err("Binding overlaps another action in the same context. Clear or change that action first.");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Configuration, InterfaceSettings, TerminalFontPreference};

    #[test]
    fn keyboard_defaults_are_valid_on_both_platform_families() {
        for mac in [false, true] {
            assert_eq!(KeyboardBindings::default().validate(mac), Ok(()));
        }
    }

    #[test]
    fn keyboard_assignment_unbind_reset_roundtrip_preserves_other_settings() {
        let mut bindings = KeyboardBindings::default();
        bindings.set(KeyboardAction::NewSession, Some("Ctrl+Shift+F8".into()));
        bindings.set(KeyboardAction::Paste, Some(String::new()));
        let settings = InterfaceSettings::DEFAULT
            .with_terminal_typography(TerminalFontPreference::JuliaMono, true)
            .with_keyboard_bindings(bindings.clone());
        let config = Configuration::empty()
            .with_interface_settings(settings.clone())
            .unwrap();
        let reloaded = Configuration::parse(&config.to_toml().unwrap()).unwrap();
        assert_eq!(reloaded.interface_settings(), &settings);
        bindings.set(KeyboardAction::NewSession, None);
        assert_eq!(
            bindings.effective(KeyboardAction::NewSession, false),
            "Primary+Shift+T"
        );
        assert_eq!(bindings.effective(KeyboardAction::Paste, false), "");
        let reset = reloaded
            .with_interface_settings(settings.with_keyboard_bindings(Default::default()))
            .unwrap();
        assert_eq!(
            reset.interface_settings().terminal_font(),
            TerminalFontPreference::JuliaMono
        );
        assert!(!reset.to_toml().unwrap().contains("keyboard_bindings"));
        assert!(Configuration::parse("schema_version = 1")
            .unwrap()
            .interface_settings()
            .keyboard_bindings()
            .is_empty());
    }

    #[test]
    fn keyboard_conflicts_account_for_context_and_primary_aliases() {
        assert_eq!(
            Chord::parse("Primary+Plus", false),
            Chord::parse("Primary+Shift+Plus", false)
        );
        for mac in [false, true] {
            let mut bindings = KeyboardBindings::default();
            bindings.set(KeyboardAction::NewSession, Some("Primary+Shift+S".into()));
            assert!(bindings.validate(mac).is_err());
            bindings.set(KeyboardAction::SettingsHotkey, Some(String::new()));
            assert!(bindings.validate(mac).is_ok());
            bindings.set(KeyboardAction::Copy, Some("Primary+R".into()));
            assert!(
                bindings.validate(mac).is_ok(),
                "Markdown reload is a disjoint context"
            );
            bindings.set(KeyboardAction::NewSession, Some("Primary+R".into()));
            assert!(
                bindings.validate(mac).is_err(),
                "global overlaps both contexts"
            );
        }
        assert_eq!(
            Chord::parse("Ctrl+Shift+S", false),
            Chord::parse("Primary+Shift+S", false)
        );
        assert_ne!(
            Chord::parse("Ctrl+S", true),
            Chord::parse("Primary+S", true)
        );
    }

    #[test]
    fn keyboard_invalid_settings_are_rejected_not_discarded() {
        for chord in [
            "C",
            "Alt+C",
            "Ctrl+Alt+C",
            "Ctrl+Ctrl+C",
            "Ctrl+Shift+F12",
            "Ctrl+Unknown",
        ] {
            assert!(Chord::parse(chord, false).is_err(), "{chord}");
        }
        for body in [
            "action = 'unknown'\nchord = 'Ctrl+K'",
            "action = 'new-session'\nchord = 'Ctrl+Alt+C'",
            "action = 'new-session'\nchord = 'Ctrl+K'\nextra = true",
        ] {
            assert!(Configuration::parse(&format!(
                "schema_version = 1\n[[settings.keyboard_bindings]]\n{body}\n"
            ))
            .is_err());
        }
        let mut bindings = KeyboardBindings::default();
        bindings.0.push(KeyboardOverride {
            action: KeyboardAction::Copy,
            chord: String::new(),
        });
        bindings.0.push(bindings.0[0].clone());
        assert!(bindings.validate(false).is_err());
    }

    #[test]
    fn every_action_documents_what_it_does() {
        let mut descriptions = std::collections::BTreeSet::new();
        for action in KeyboardAction::ALL {
            let description = action.description();
            assert!(
                !description.is_empty(),
                "{} must describe its behaviour",
                action.title()
            );
            assert!(
                description.ends_with('.'),
                "{} description must be a sentence, got {description:?}",
                action.title()
            );
            assert!(
                descriptions.insert(description),
                "{} reuses the description {description:?}",
                action.title()
            );
        }
    }

    #[test]
    fn every_scope_explains_where_its_actions_apply() {
        let mut titles = std::collections::BTreeSet::new();
        for scope in KeyboardScope::ALL {
            assert!(titles.insert(scope.title()), "duplicate scope title");
            assert!(!scope.help().is_empty(), "scope must explain its context");
        }
        // Every action must be reachable from the scope list the editor groups by.
        for action in KeyboardAction::ALL {
            assert!(
                KeyboardScope::ALL.contains(&action.scope()),
                "{} has a scope missing from KeyboardScope::ALL",
                action.title()
            );
        }
    }
}
