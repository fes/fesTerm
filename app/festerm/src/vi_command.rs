//! Parsing for the editor's single-line command area (ADR 0034 §10a).
//!
//! This module is deliberately free of `egui` and of any application state so
//! that the whole of the `:` surface can be tested as data. It answers three
//! questions: what a typed line means, what a prefix could still become, and
//! what to say when the line means nothing we support.
//!
//! The ADR's central rule shapes everything here: anything outside the
//! supported set is refused with no side effect, because half-executing a
//! command the user recognises from Vim is worse than declining it.

use eframe::egui::{self, Align, FontId};
use festerm_ui_egui::theme;

use crate::tabs::TabId;
use festerm_document::{SubstituteCommand, SubstituteError};

/// A refusal or a complaint about a typed command line.
///
/// The two halves exist because the command area is one line: the headline has
/// to fit beside the input, while the detail is what the user needs in order to
/// fix the command, and is shown on hover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    headline: String,
    detail: String,
}

impl CommandError {
    pub fn new(headline: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            headline: headline.into(),
            detail: detail.into(),
        }
    }

    pub fn headline(&self) -> &str {
        &self.headline
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl From<SubstituteError> for CommandError {
    fn from(error: SubstituteError) -> Self {
        Self::new(error.headline(), error.detail())
    }
}

/// What the editor should do about a command line that parsed.
///
/// Every variant maps onto an application command that already exists and
/// already has a safety policy. That is the point of the `:` commands: they are
/// a familiar way to reach the ordinary Save, Close and Refresh, not a second
/// implementation of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViCommand {
    /// `:w`, `:write` — the ordinary Save, reporting only once the durable
    /// replacement completes.
    Write,
    /// `:w {path}`, `:saveas {path}`, `:w` with no destination yet — the
    /// reviewed Save As picker, never a silent overwrite.
    WriteAs { path: Option<String> },
    /// `:wq`, `:x`, `ZZ` — save, then close only if the save succeeded.
    WriteQuit,
    /// `:q`, `:quit` — the ordinary close, with its ordinary dirty prompt.
    Quit,
    /// `:q!`, `ZQ` — close discarding changes, which still raises the normal
    /// final-view confirmation rather than discarding state other views can
    /// see without asking.
    QuitDiscarding,
    /// `:e`, `:e!`, `:edit` — Refresh, under its ordinary conflict rules.
    Refresh { force: bool },
    /// `:s`, `:%s`, `:'<,'>s` — parsed by the shared substitution engine so the
    /// command area and the Find bar cannot drift apart.
    Substitute(Box<SubstituteCommand>),
}

impl ViCommand {
    /// What this command will do, in the words the banner uses. The banner
    /// explains the command that has actually been typed, so a reader learns
    /// what Enter is about to do before pressing it.
    pub fn explanation(&self) -> &'static str {
        match self {
            Self::Write => {
                "saves through the ordinary Save command and reports only once the write lands."
            }
            Self::WriteAs { .. } => {
                "opens the reviewed Save As sheet rather than overwriting a path typed on one line."
            }
            Self::WriteQuit => {
                "saves through the ordinary Save command, then closes only after the save succeeds."
            }
            Self::Quit => {
                "closes this view, with the ordinary prompt if there is anything unsaved."
            }
            Self::QuitDiscarding => {
                "closes this view discarding changes, still asking before the last view goes."
            }
            Self::Refresh { .. } => "refreshes from disk under the ordinary conflict rules.",
            Self::Substitute(_) => {
                "replaces through the same engine and the same regex dialect the Find bar uses."
            }
        }
    }
}

/// Every command name the area will accept, for completion.
///
/// Completion offers only these, so it cannot advertise something that will
/// then fail — which is the ADR's rule for the command area.
const COMMAND_NAMES: &[&str] = &[
    "e", "edit", "q", "quit", "s", "saveas", "w", "wq", "write", "x",
];

/// Parse a command line, without its leading `:`.
pub fn parse(line: &str) -> Result<ViCommand, CommandError> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(CommandError::new(
            "Empty command",
            "Type a command name after the colon, or press Esc to cancel.",
        ));
    }

    // A substitution carries its own range prefix and its own delimiter rules,
    // so it is recognised before the line is split on whitespace: a replacement
    // may legitimately contain spaces.
    if is_substitution(trimmed) {
        // The shared engine owns the `:` prefix as part of its own grammar, so
        // it is put back here rather than teaching two parsers the same rule.
        let command = SubstituteCommand::parse(&format!(":{trimmed}"))?;
        return Ok(ViCommand::Substitute(Box::new(command)));
    }

    if trimmed.starts_with('!') {
        return Err(CommandError::new(
            "Not supported",
            "Shell commands are not run from the editor. Use a terminal tab.",
        ));
    }

    let (name, argument) = split_name(trimmed);
    let argument = argument.trim();

    match name {
        "w" | "write" => {
            if argument.is_empty() {
                Ok(ViCommand::Write)
            } else {
                Ok(ViCommand::WriteAs {
                    path: Some(argument.to_string()),
                })
            }
        }
        "saveas" => Ok(ViCommand::WriteAs {
            path: (!argument.is_empty()).then(|| argument.to_string()),
        }),
        "wq" | "x" | "xit" => {
            reject_argument(name, argument)?;
            Ok(ViCommand::WriteQuit)
        }
        "q" | "quit" => {
            reject_argument(name, argument)?;
            Ok(ViCommand::Quit)
        }
        "q!" | "quit!" => {
            reject_argument(name, argument)?;
            Ok(ViCommand::QuitDiscarding)
        }
        "e" | "edit" => {
            reject_argument(name, argument)?;
            Ok(ViCommand::Refresh { force: false })
        }
        "e!" | "edit!" => {
            reject_argument(name, argument)?;
            Ok(ViCommand::Refresh { force: true })
        }
        "set" | "map" | "nmap" | "imap" | "source" | "so" => Err(CommandError::new(
            "Not supported",
            format!(
                "`:{name}` belongs to Vim's configuration environment, which fesTerm does not \
                 implement. Use Editor options for per-view settings."
            ),
        )),
        _ => Err(CommandError::new(
            "Unknown command",
            format!("`:{name}` is not one of {}.", command_list()),
        )),
    }
}

/// Command names that could still complete the given prefix.
pub fn completions(prefix: &str) -> Vec<&'static str> {
    let prefix = prefix.trim_start_matches(':');
    if prefix.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMAND_NAMES
        .iter()
        .copied()
        .filter(|name| name.starts_with(prefix))
        .collect()
}

/// `ZZ` and `ZQ` are Normal-mode keys rather than command lines, but they mean
/// exactly what `:x` and `:q!` mean, so they are resolved here to keep one
/// definition of what those words do.
pub fn normal_mode_command(keys: &str) -> Option<ViCommand> {
    match keys {
        "ZZ" => Some(ViCommand::WriteQuit),
        "ZQ" => Some(ViCommand::QuitDiscarding),
        _ => None,
    }
}

fn is_substitution(line: &str) -> bool {
    let rest = line
        .strip_prefix("'<,'>")
        .or_else(|| line.strip_prefix('%'))
        .unwrap_or(line);
    rest.starts_with('s')
        && rest
            .strip_prefix('s')
            .and_then(|rest| rest.chars().next())
            .is_some_and(|delimiter| delimiter == '/')
}

/// Split a command word from its argument, keeping a trailing `!` attached to
/// the word: `:q!` is a different command from `:q`, not `:q` with an argument.
fn split_name(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(index) => (&line[..index], &line[index..]),
        None => (line, ""),
    }
}

fn reject_argument(name: &str, argument: &str) -> Result<(), CommandError> {
    if argument.is_empty() {
        return Ok(());
    }
    Err(CommandError::new(
        "Unexpected argument",
        format!("`:{name}` does not take an argument."),
    ))
}

fn command_list() -> String {
    COMMAND_NAMES
        .iter()
        .map(|name| format!(":{name}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Which prompt the command area is showing, which is also which grammar the
/// input will be read with when it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPrompt {
    /// `:` — an Ex command.
    Ex,
    /// `/` — search forward.
    SearchForward,
    /// `?` — search backward.
    SearchBackward,
}

impl CommandPrompt {
    pub fn glyph(self) -> &'static str {
        match self {
            CommandPrompt::Ex => ":",
            CommandPrompt::SearchForward => "/",
            CommandPrompt::SearchBackward => "?",
        }
    }

    /// What an empty line is waiting for, which is also the field's name.
    fn hint(self) -> &'static str {
        match self {
            CommandPrompt::Ex => "Command",
            CommandPrompt::SearchForward => "Search forward",
            CommandPrompt::SearchBackward => "Search backward",
        }
    }

    /// Search accepts a match rather than running a command, and the mockups
    /// say so in the hint rather than leaving the user to infer it.
    fn verb(self) -> &'static str {
        match self {
            CommandPrompt::Ex => "Enter to run",
            _ => "Enter to accept",
        }
    }
}

/// What the view should do about a frame of the command area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAreaEvent {
    /// The input changed; a search prompt should re-run its incremental match.
    Changed,
    /// Enter was pressed with this line.
    Run { prompt: CommandPrompt, line: String },
    /// Esc was pressed, or focus was lost.
    Cancelled,
}

/// The single-line area that sits immediately above the persistent status bar
/// (ADR 0034 §10a).
///
/// It never replaces the status bar, because the document's mode, format,
/// position and save state have to stay readable while a command is being
/// typed — which is exactly when a user is most likely to want them.
#[derive(Debug, Default)]
pub struct CommandArea {
    prompt: Option<CommandPrompt>,
    input: String,
    /// A bounded history, most recent last. Bounded because this is a
    /// convenience, not a record: an unbounded one would grow for the life of
    /// the view without anybody ever asking it to.
    history: Vec<String>,
    /// Where Up/Down has walked to, as an index from the end.
    recall: Option<usize>,
    /// What the last command said, shown in place of the hint until the next
    /// one opens.
    result: Option<CommandOutcome>,
    /// Set for the frame after opening so the field takes focus without
    /// stealing it on every subsequent frame.
    focus_pending: bool,
    /// Whether the field held focus last frame, so that losing it can be told
    /// apart from never having had it.
    was_focused: bool,
}

/// What to show where the hint was, once a command has run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    Message(String),
    Failed(CommandError),
}

const HISTORY_LIMIT: usize = 50;

impl CommandArea {
    pub fn is_open(&self) -> bool {
        self.prompt.is_some()
    }

    pub fn prompt(&self) -> Option<CommandPrompt> {
        self.prompt
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    #[cfg(test)]
    pub fn outcome(&self) -> Option<&CommandOutcome> {
        self.result.as_ref()
    }

    /// Opens the area with an empty line, discarding whatever the last command
    /// reported: a stale `3 matches` beside a half-typed new command would be
    /// read as belonging to the new one.
    pub fn open(&mut self, prompt: CommandPrompt) {
        self.prompt = Some(prompt);
        self.input.clear();
        self.recall = None;
        self.result = None;
        self.focus_pending = true;
    }

    /// Opens with a line already in it, for the state gallery.
    #[cfg(test)]
    pub fn open_for_gallery(&mut self, prompt: CommandPrompt, line: &str) {
        self.open(prompt);
        self.input = line.to_owned();
    }

    pub fn close(&mut self) {
        self.prompt = None;
        self.input.clear();
        self.recall = None;
        self.focus_pending = false;
        self.was_focused = false;
    }

    /// Says something in the area without opening it, for keystrokes that
    /// report a result without ever having had a command line — `n`, `*`, and
    /// anything outside the matrix.
    pub fn report(&mut self, outcome: CommandOutcome) {
        self.result = Some(outcome);
    }

    /// Closes the area and leaves a message where the hint was.
    pub fn finish(&mut self, outcome: CommandOutcome) {
        let line = self.input.clone();
        self.close();
        self.remember(line);
        self.result = Some(outcome);
    }

    fn remember(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        // A command repeated immediately is not two pieces of history, and
        // holding it twice would mean Up had to be pressed twice to get past
        // the thing just typed.
        if self.history.last().is_some_and(|last| *last == line) {
            return;
        }
        self.history.push(line);
        if self.history.len() > HISTORY_LIMIT {
            self.history.remove(0);
        }
    }

    /// Walks back through history; returns whether anything changed.
    pub fn recall_previous(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let next = match self.recall {
            None => 0,
            Some(index) if index + 1 < self.history.len() => index + 1,
            Some(index) => index,
        };
        self.recall = Some(next);
        self.input = self.history[self.history.len() - 1 - next].clone();
        true
    }

    /// Walks forward again, ending on the empty line the user started from.
    pub fn recall_next(&mut self) -> bool {
        match self.recall {
            None => false,
            Some(0) => {
                self.recall = None;
                self.input.clear();
                true
            }
            Some(index) => {
                self.recall = Some(index - 1);
                self.input = self.history[self.history.len() - index].clone();
                true
            }
        }
    }

    /// The only completion offered is a command name that will actually run.
    pub fn complete(&mut self) -> bool {
        if self.prompt != Some(CommandPrompt::Ex) {
            return false;
        }
        let candidates = completions(&self.input);
        let [only] = candidates.as_slice() else {
            return false;
        };
        if *only == self.input {
            return false;
        }
        self.input = (*only).to_string();
        true
    }
}

const AREA_TEXT_SIZE: f32 = 13.0;
const AREA_PADDING_X: i8 = 12;
const AREA_PADDING_Y: i8 = 7;
const PROMPT_GAP: f32 = 8.0;

impl CommandArea {
    /// The height the area needs, so the body can give it up rather than push
    /// it off the bottom of the view.
    pub fn reserved_height(&self) -> f32 {
        if !self.is_open() && self.result.is_none() {
            return 0.0;
        }
        f32::from(AREA_PADDING_Y) * 2.0 + AREA_TEXT_SIZE + 6.0
    }

    /// Renders the area, or the result of the last command when it is closed.
    ///
    /// `summary` is the search progress the view already knows (`1 of 3`); it
    /// is passed in rather than computed here because match state is per-view
    /// and belongs to the editor, not to this widget.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        tab: TabId,
        summary: Option<&str>,
    ) -> Option<CommandAreaEvent> {
        if !self.is_open() && self.result.is_none() {
            return None;
        }
        let mut event = None;
        egui::Frame::new()
            .fill(theme::SURFACE_CHROME)
            .inner_margin(egui::Margin::symmetric(AREA_PADDING_X, AREA_PADDING_Y))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    event = self.show_row(ui, tab, summary);
                });
            });
        event
    }

    fn show_row(
        &mut self,
        ui: &mut egui::Ui,
        tab: TabId,
        summary: Option<&str>,
    ) -> Option<CommandAreaEvent> {
        let Some(prompt) = self.prompt else {
            self.show_outcome(ui);
            return None;
        };

        // The keys this area owns are taken before the field is built. A
        // single-line `TextEdit` surrenders focus on Enter and Escape and
        // swallows both, so a field that has already been drawn will never
        // report them; taking them first is also what ADR 0034 §10a means by
        // swallowing them rather than leaking them to global shortcuts.
        let focused = ui.memory(|memory| memory.has_focus(field_id(tab)));
        let held = focused || self.focus_pending;
        let mut event = held.then(|| self.route_keys(ui, prompt)).flatten();
        if !self.is_open() {
            return event;
        }

        // egui surrenders focus on Escape before a frame begins, so the key
        // itself never reaches the area. Losing focus while open therefore
        // means the same thing as Escape, which is also the right answer when
        // the user clicks somewhere else: an abandoned command runs nothing.
        if self.was_focused && !focused && !self.focus_pending {
            self.close();
            self.was_focused = false;
            return Some(CommandAreaEvent::Cancelled);
        }
        self.was_focused = focused;

        // The glyph is the field's label as well as its prompt, so it is kept
        // as a real label widget and the field is tied to it: an unlabelled
        // single-line field in a bar of its own is unreadable to a screen
        // reader.
        let prompt_label = ui.label(
            egui::RichText::new(prompt.glyph())
                .font(FontId::monospace(AREA_TEXT_SIZE))
                .color(theme::TEXT_MUTED),
        );
        ui.add_space(PROMPT_GAP);

        let hint = match summary {
            Some(summary) => format!("{summary} · {} · Esc to cancel", prompt.verb()),
            None => format!("{} · Esc to cancel", prompt.verb()),
        };
        let hint_width = ui
            .painter()
            .layout_no_wrap(
                hint.clone(),
                FontId::proportional(AREA_TEXT_SIZE - 1.0),
                theme::TEXT_MUTED,
            )
            .size()
            .x;

        // The field claims the row minus the hint, so the hint stays on the
        // right edge where the mockups put it instead of being pushed off by a
        // long command.
        let field_width = (ui.available_width() - hint_width - PROMPT_GAP * 2.0).max(80.0);
        let response = ui.add_sized(
            egui::vec2(field_width, AREA_TEXT_SIZE + 4.0),
            egui::TextEdit::singleline(&mut self.input)
                .id(field_id(tab))
                .frame(egui::Frame::NONE)
                .desired_width(field_width)
                .font(FontId::monospace(AREA_TEXT_SIZE))
                .hint_text(prompt.hint())
                .text_color(theme::TEXT_PRIMARY),
        );
        let response = response.labelled_by(prompt_label.id);
        if self.focus_pending {
            response.request_focus();
            self.focus_pending = false;
        }

        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                egui::RichText::new(hint)
                    .size(AREA_TEXT_SIZE - 1.0)
                    .color(theme::TEXT_MUTED),
            );
        });

        if response.changed() {
            event = Some(CommandAreaEvent::Changed);
        }
        event
    }

    /// Reads the keys the area owns.
    ///
    /// Enter and Esc are consumed rather than merely observed, because ADR 0034
    /// §10a requires the area to swallow them: leaking Esc to the window would
    /// close a dialog somewhere else while the user was only abandoning a
    /// half-typed command.
    fn route_keys(&mut self, ui: &mut egui::Ui, prompt: CommandPrompt) -> Option<CommandAreaEvent> {
        let (escape, enter, up, down, tab_key) = ui.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
            )
        });
        if escape {
            self.close();
            return Some(CommandAreaEvent::Cancelled);
        }
        if enter {
            return Some(CommandAreaEvent::Run {
                prompt,
                line: self.input.clone(),
            });
        }
        if (up && self.recall_previous())
            || (down && self.recall_next())
            || (tab_key && self.complete())
        {
            return Some(CommandAreaEvent::Changed);
        }
        None
    }

    fn show_outcome(&self, ui: &mut egui::Ui) {
        match &self.result {
            Some(CommandOutcome::Message(message)) => {
                ui.label(
                    egui::RichText::new(message)
                        .size(AREA_TEXT_SIZE - 1.0)
                        .color(theme::TEXT_SECONDARY),
                );
            }
            Some(CommandOutcome::Failed(error)) => {
                ui.label(
                    egui::RichText::new(error.headline())
                        .size(AREA_TEXT_SIZE - 1.0)
                        .color(theme::STATUS_ERROR),
                )
                .on_hover_text(error.detail());
            }
            None => {}
        }
    }
}

/// Anchored to the tab rather than to the `Ui`, because the editor addresses
/// this field from a different scope than the one that draws it.
pub fn field_id(tab: TabId) -> egui::Id {
    egui::Id::new(("text-editor-command-area", tab))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    fn area_harness(prompt: CommandPrompt) -> Harness<'static, CommandArea> {
        let tab = TabId::next_for_test();
        let mut area = CommandArea::default();
        area.open(prompt);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(720.0, 60.0))
            .build_ui_state(
                move |ui, area: &mut CommandArea| {
                    let event = area.show(ui, tab, None);
                    if let Some(CommandAreaEvent::Run { line, .. }) = event {
                        let outcome = match parse(&line) {
                            Ok(_) => CommandOutcome::Message("Ran".to_string()),
                            Err(error) => CommandOutcome::Failed(error),
                        };
                        area.finish(outcome);
                    }
                },
                area,
            );
        harness.run();
        harness
    }

    fn field<'h>(harness: &'h Harness<'static, CommandArea>) -> egui_kittest::Node<'h> {
        harness
            .query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the command area's field")
    }

    #[test]
    fn write_without_a_destination_is_the_ordinary_save() {
        assert_eq!(parse("w").unwrap(), ViCommand::Write);
        assert_eq!(parse("write").unwrap(), ViCommand::Write);
        assert_eq!(parse("  w  ").unwrap(), ViCommand::Write);
    }

    #[test]
    fn write_with_a_destination_goes_through_the_reviewed_picker() {
        assert_eq!(
            parse("w notes/copy.md").unwrap(),
            ViCommand::WriteAs {
                path: Some("notes/copy.md".to_string()),
            }
        );
        assert_eq!(parse("saveas").unwrap(), ViCommand::WriteAs { path: None });
    }

    #[test]
    fn a_bang_makes_a_different_command_rather_than_an_argument() {
        assert_eq!(parse("q").unwrap(), ViCommand::Quit);
        assert_eq!(parse("q!").unwrap(), ViCommand::QuitDiscarding);
        assert_eq!(parse("e").unwrap(), ViCommand::Refresh { force: false });
        assert_eq!(parse("e!").unwrap(), ViCommand::Refresh { force: true });
    }

    #[test]
    fn save_and_close_spellings_all_reach_the_same_command() {
        for line in ["wq", "x"] {
            assert_eq!(parse(line).unwrap(), ViCommand::WriteQuit, "{line}");
        }
        assert_eq!(normal_mode_command("ZZ"), Some(ViCommand::WriteQuit));
        assert_eq!(normal_mode_command("ZQ"), Some(ViCommand::QuitDiscarding));
        assert_eq!(normal_mode_command("Zx"), None);
    }

    #[test]
    fn a_substitution_keeps_its_spaces_and_reaches_the_shared_engine() {
        let command = parse("%s/two words/one word/g").unwrap();
        let ViCommand::Substitute(command) = command else {
            panic!("expected a substitution");
        };
        assert_eq!(command.pattern, "two words");
        assert_eq!(command.replacement, "one word");
    }

    #[test]
    fn every_supported_substitution_range_parses() {
        for line in ["s/a/b/", "%s/a/b/", "'<,'>s/a/b/"] {
            assert!(
                matches!(parse(line), Ok(ViCommand::Substitute(_))),
                "{line} should parse"
            );
        }
    }

    #[test]
    fn a_bad_substitution_is_refused_with_the_engines_own_words() {
        let error = parse("%s/a/b/z").unwrap_err();
        assert!(!error.headline().is_empty());
        assert!(!error.detail().is_empty());
    }

    #[test]
    fn an_unknown_command_names_what_is_available_instead() {
        let error = parse("sort").unwrap_err();
        assert_eq!(error.headline(), "Unknown command");
        assert!(error.detail().contains(":w"));
    }

    #[test]
    fn the_wider_ex_environment_is_refused_by_name() {
        for line in ["set number", "map x y", "!ls", "source ~/.vimrc"] {
            let error = parse(line).unwrap_err();
            assert_eq!(error.headline(), "Not supported", "{line}");
        }
    }

    #[test]
    fn a_command_that_takes_nothing_refuses_an_argument() {
        let error = parse("q now").unwrap_err();
        assert_eq!(error.headline(), "Unexpected argument");
    }

    #[test]
    fn an_empty_line_says_what_to_do_rather_than_failing_silently() {
        let error = parse("   ").unwrap_err();
        assert_eq!(error.headline(), "Empty command");
    }

    #[test]
    fn completion_offers_only_commands_that_will_run() {
        assert_eq!(completions("w"), vec!["w", "wq", "write"]);
        assert_eq!(completions("q"), vec!["q", "quit"]);
        assert!(completions("zzz").is_empty());
        assert!(completions("w ").is_empty());
        for name in completions("") {
            assert!(
                parse(name).is_ok() || name == "s" || name == "saveas",
                "completion offered `:{name}`, which does not parse"
            );
        }
    }

    #[test]
    fn typing_a_command_and_pressing_enter_runs_it_and_leaves_what_it_said() {
        let mut harness = area_harness(CommandPrompt::Ex);
        field(&harness).focus();
        harness.run();
        field(&harness).type_text("w");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();
        assert!(!harness.state().is_open());
        assert_eq!(
            harness.state().outcome(),
            Some(&CommandOutcome::Message("Ran".to_string()))
        );
    }

    #[test]
    fn a_command_that_means_nothing_says_so_and_changes_nothing() {
        let mut harness = area_harness(CommandPrompt::Ex);
        field(&harness).focus();
        harness.run();
        field(&harness).type_text("sort");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.run();
        let Some(CommandOutcome::Failed(error)) = harness.state().outcome() else {
            panic!("expected a refusal, got {:?}", harness.state().outcome());
        };
        assert_eq!(error.headline(), "Unknown command");
    }

    #[test]
    fn escape_closes_the_area_without_running_anything() {
        let mut harness = area_harness(CommandPrompt::Ex);
        field(&harness).focus();
        harness.run();
        field(&harness).type_text("q");
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(!harness.state().is_open());
        assert_eq!(harness.state().outcome(), None);
    }

    #[test]
    fn the_area_remembers_what_was_run_and_walks_back_to_it() {
        let mut area = CommandArea::default();
        area.open(CommandPrompt::Ex);
        area.input = "w".to_string();
        area.finish(CommandOutcome::Message("Saved".to_string()));
        area.open(CommandPrompt::Ex);
        area.input = "q".to_string();
        area.finish(CommandOutcome::Message("Closed".to_string()));

        area.open(CommandPrompt::Ex);
        assert!(area.recall_previous());
        assert_eq!(area.input(), "q");
        assert!(area.recall_previous());
        assert_eq!(area.input(), "w");
        // There is nothing older, so the oldest entry stays put rather than
        // silently wrapping round to the newest.
        assert!(area.recall_previous());
        assert_eq!(area.input(), "w");
        assert!(area.recall_next());
        assert_eq!(area.input(), "q");
        assert!(area.recall_next());
        assert_eq!(area.input(), "");
        assert!(!area.recall_next());
    }

    #[test]
    fn a_command_repeated_is_not_remembered_twice() {
        let mut area = CommandArea::default();
        for _ in 0..3 {
            area.open(CommandPrompt::Ex);
            area.input = "w".to_string();
            area.finish(CommandOutcome::Message("Saved".to_string()));
        }
        assert_eq!(area.history, vec!["w".to_string()]);
    }

    #[test]
    fn history_is_bounded_so_a_long_lived_view_does_not_grow_without_end() {
        let mut area = CommandArea::default();
        for index in 0..HISTORY_LIMIT + 10 {
            area.open(CommandPrompt::Ex);
            area.input = format!("w file{index}");
            area.finish(CommandOutcome::Message("Saved".to_string()));
        }
        assert_eq!(area.history.len(), HISTORY_LIMIT);
        assert_eq!(area.history.last().unwrap(), "w file59");
    }

    #[test]
    fn completion_only_fires_when_one_command_can_follow() {
        let mut area = CommandArea::default();
        area.open(CommandPrompt::Ex);
        // `w` could still become `wq` or `write`, so nothing is chosen for the
        // user.
        area.input = "w".to_string();
        assert!(!area.complete());
        area.input = "wr".to_string();
        assert!(area.complete());
        assert_eq!(area.input(), "write");
        // Completing again would be a no-op, and reporting a change that did
        // not happen would re-run a search for nothing.
        assert!(!area.complete());
    }

    #[test]
    fn a_search_prompt_never_completes_command_names() {
        let mut area = CommandArea::default();
        area.open(CommandPrompt::SearchForward);
        area.input = "wr".to_string();
        assert!(!area.complete());
        assert_eq!(area.input(), "wr");
    }

    #[test]
    fn opening_the_area_clears_what_the_last_command_said() {
        let mut area = CommandArea::default();
        area.open(CommandPrompt::Ex);
        area.input = "w".to_string();
        area.finish(CommandOutcome::Message("Saved".to_string()));
        assert!(area.outcome().is_some());
        area.open(CommandPrompt::Ex);
        assert_eq!(area.outcome(), None);
        assert_eq!(area.input(), "");
    }
}
