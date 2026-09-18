//! A pure, deterministic vi modal-editing engine (ADR 0034 §10).
//!
//! The engine deliberately owns none of the text. The document is shared by
//! every view (ADR 0034 §1), so a caret and a copy of the buffer are
//! view-scoped and would go stale the moment a sibling view typed. Every call
//! is therefore handed the document's current normalised text and the current
//! caret, and hands back a *described* action — edits to commit, or an intent
//! to route — rather than mutating anything itself.
//!
//! The organising rule is ADR 0034 §10's honesty clause: anything outside the
//! fidelity matrix returns a concise refusal and has **no side effect**.
//! Half-executing an unsupported command is worse than refusing it, because
//! the user cannot tell what happened to their text. Every path that cannot
//! be honoured returns [`ViAction::Refused`] carrying an empty edit list.
//!
//! All offsets the engine speaks in are byte offsets into that normalised
//! text, and every one is computed in char-index space and mapped back, so a
//! motion, text object, or operator range can never land inside a multi-byte
//! character — the class of bug that once panicked a sibling module on
//! `find_all("é")`.

use crate::search::SearchError;
use crate::text::TextEdit;

/// The mode the editor is in, shown verbatim in the status bar (ADR 0034 §10).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViMode {
    Normal,
    Insert,
    Replace,
    Visual,
    VisualLine,
}

impl ViMode {
    /// The exact uppercase word the status bar must show. `VISUAL` covers both
    /// characterwise and linewise visual because the ADR's status vocabulary
    /// has one word for them; the distinction is the selection shape, not a
    /// second mode name a screen reader would have to learn.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Replace => "REPLACE",
            Self::Visual | Self::VisualLine => "VISUAL",
        }
    }
}

/// One keystroke, described independently of any UI toolkit so the engine can
/// be driven entirely from a unit test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViKey {
    Char(char),
    Ctrl(char),
    Escape,
    Enter,
    Backspace,
}

/// A search or navigation keystroke the engine recognises but does not run,
/// because `/ ? n N * #` reuse the shared `search.rs` machinery and the
/// command area (ADR 0034 §10a) that lives in the UI layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchIntent {
    /// `/` — open the command area to type a forward pattern.
    PromptForward,
    /// `?` — open the command area to type a backward pattern.
    PromptBackward,
    /// `n` — repeat the last search in its own direction.
    Next,
    /// `N` — repeat the last search reversed.
    Previous,
    /// `*` — search forward for the word under the caret, taken literally.
    WordForward,
    /// `#` — search backward for the word under the caret, taken literally.
    WordBackward,
}

/// What the caller should do in response to a keystroke.
///
/// Undo and redo are *intents*, never edits: the history they walk belongs to
/// the shared [`crate::TextDocument`] and is already implemented, so the engine
/// only names the operation. Committed edits are ordinary [`TextEdit`]s the
/// caller applies through [`crate::TextDocument::apply_edits`], which is what
/// makes a multi-line operator one undo transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ViAction {
    /// Nothing to commit: a motion, a mode switch, or a deliberately ignored
    /// key.
    None,
    /// Commit these edits as one transaction, then place the caret at the
    /// returned offset.
    Edit(Vec<TextEdit>),
    /// Undo one transaction of the shared document.
    Undo,
    /// Redo one transaction of the shared document.
    Redo,
    /// Route this search intent to the shared search machinery.
    Search(SearchIntent),
    /// The keystroke is outside the matrix. Show this and change nothing; the
    /// edit list a caller would apply is, by construction, empty.
    Refused(SearchError),
}

/// The full result of feeding one keystroke: what to do, where the caret ends
/// up, the mode to show, and whether the engine is mid-sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViResponse {
    pub action: ViAction,
    pub caret: usize,
    pub mode: ViMode,
    /// True while a multi-key command is only half-typed (`2d`, `g`, `r`), so
    /// the caller knows the next key belongs to the engine and must not be
    /// treated as a global shortcut.
    pub pending: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

/// Where a partly-typed command is waiting for its next key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Ready,
    /// An operator is pending; the next key is its motion or text object.
    AfterOperator(Operator),
    /// A bare `g` was seen (`gg`), or an operator's `g` (`dgg`).
    AfterG(Option<Operator>),
    /// A text-object introducer `i`/`a` was seen after an operator or in
    /// visual mode; `true` means inner (`i`), `false` means around (`a`).
    AfterTextObject(Operator, bool),
    /// `r` was seen; the next character is the replacement.
    AwaitReplace,
}

/// The one register vi mode supports: the unnamed register (ADR 0034 §10).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Register {
    text: String,
    linewise: bool,
}

/// The view-scoped state machine. One per editor view; it never outlives a
/// keystroke's borrow of the document text.
#[derive(Clone, Debug)]
pub struct ViEngine {
    mode: ViMode,
    stage: Stage,
    count: Option<usize>,
    /// The count typed before an operator, kept apart so `2d3w` can multiply
    /// the two the way Vim does.
    operator_count: Option<usize>,
    register: Register,
    /// The selection's fixed end, as a byte offset, while a visual mode is
    /// active.
    visual_anchor: usize,
    /// Keys of the change in progress, recorded so `.` can replay it.
    record: Vec<ViKey>,
    last_change: Option<Vec<ViKey>>,
    change_in_progress: bool,
    /// True while replaying a recorded change for `.`, so the replay is not
    /// itself recorded and cannot recurse.
    replaying: bool,
}

impl Default for ViEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ViEngine {
    pub fn new() -> Self {
        Self {
            mode: ViMode::Normal,
            stage: Stage::Ready,
            count: None,
            operator_count: None,
            register: Register::default(),
            visual_anchor: 0,
            record: Vec::new(),
            last_change: None,
            change_in_progress: false,
            replaying: false,
        }
    }

    pub const fn mode(&self) -> ViMode {
        self.mode
    }

    pub const fn mode_label(&self) -> &'static str {
        self.mode.label()
    }

    /// Whether a multi-key command is only half-typed. The caller needs this
    /// so a pending `d` does not let the following key escape to a global
    /// binding.
    pub const fn is_pending(&self) -> bool {
        !matches!(self.stage, Stage::Ready) || self.count.is_some() || self.operator_count.is_some()
    }

    /// Feeds one keystroke against the document's current text and caret.
    pub fn on_key(&mut self, key: ViKey, text: &str, caret: usize) -> ViResponse {
        // `.`, `u`, and Ctrl-r are handled before any recording: repeat must
        // not record itself, and undo/redo are not "changes" that `.` should
        // ever reproduce.
        if !self.replaying && self.mode == ViMode::Normal {
            match key {
                ViKey::Char('.') if matches!(self.stage, Stage::Ready) => {
                    return self.repeat_last_change(text, caret);
                }
                ViKey::Char('u') if matches!(self.stage, Stage::Ready) && self.count.is_none() => {
                    self.reset_pending();
                    return self.respond(ViAction::Undo, caret);
                }
                ViKey::Ctrl('r') if matches!(self.stage, Stage::Ready) => {
                    self.reset_pending();
                    return self.respond(ViAction::Redo, caret);
                }
                _ => {}
            }
        }

        if !self.replaying {
            self.record.push(key);
        }

        let buf = Buffer::new(text);
        let caret = clamp_caret(&buf, caret);
        let (action, new_caret) = self.step(&buf, key, caret);

        if !self.replaying {
            self.update_recording(&action);
        }

        self.respond(action, new_caret)
    }

    fn respond(&self, action: ViAction, caret: usize) -> ViResponse {
        ViResponse {
            action,
            caret,
            mode: self.mode,
            pending: self.is_pending(),
        }
    }

    /// Records the keystrokes of a change so `.` can reproduce it. A key that
    /// leaves the engine cleanly in Normal mode having produced an edit — or a
    /// completed insert session — is the boundary of one repeatable change;
    /// anything that resolves to a pure motion is discarded so `.` keeps the
    /// *previous* change.
    fn update_recording(&mut self, action: &ViAction) {
        let produced_edit = matches!(action, ViAction::Edit(_));
        if produced_edit {
            self.change_in_progress = true;
        }
        let settled = self.mode == ViMode::Normal && !self.is_pending();
        if !settled {
            return;
        }
        if self.change_in_progress {
            self.last_change = Some(std::mem::take(&mut self.record));
        }
        self.record.clear();
        self.change_in_progress = false;
    }

    /// Replays the recorded change against the *current* text and caret and
    /// returns the single edit that reproduces it.
    ///
    /// This diverges from the reported-only treatment of undo and redo on
    /// purpose: `.` reproduces the engine's own last keystrokes, which is
    /// state the engine fully owns, so it can and must compute the result
    /// deterministically rather than ask the caller to. Collapsing the replay
    /// to one prefix/suffix diff also means a repeated insert lands as a
    /// single undo transaction, matching Vim.
    fn repeat_last_change(&mut self, text: &str, caret: usize) -> ViResponse {
        let Some(keys) = self.last_change.clone() else {
            return self.respond(ViAction::None, caret);
        };

        let mut sub = ViEngine::new();
        sub.register = self.register.clone();
        sub.replaying = true;

        let mut current = text.to_owned();
        let mut car = caret;
        for key in keys {
            let response = sub.on_key(key, &current, car);
            car = response.caret;
            if let ViAction::Edit(edits) = response.action {
                current = apply_edits_to_string(&current, &edits);
            }
        }
        // The replay may have refilled the register (a repeated `dd` yanks
        // afresh); keep the visible register in step with what happened.
        self.register = sub.register;

        match single_edit_diff(text, &current) {
            Some(edit) => self.respond(ViAction::Edit(vec![edit]), car),
            None => self.respond(ViAction::None, car),
        }
    }

    fn reset_pending(&mut self) {
        self.stage = Stage::Ready;
        self.count = None;
        self.operator_count = None;
    }

    /// The heart of the machine: one keystroke, dispatched on the current mode
    /// and stage. Returns the described action and the caret after it.
    fn step(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        match self.mode {
            ViMode::Insert => self.step_insert(buf, key, caret),
            ViMode::Replace => self.step_replace(buf, key, caret),
            ViMode::Visual | ViMode::VisualLine => self.step_visual(buf, key, caret),
            ViMode::Normal => self.step_normal(buf, key, caret),
        }
    }

    // --- Insert mode -------------------------------------------------------

    fn step_insert(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        match key {
            ViKey::Escape => {
                // Vim leaves the caret one column left of where insertion
                // stopped, but never before the line's first column.
                let ci = buf.ci_of_byte(caret);
                let start = buf.line_start(ci);
                let new = if ci > start { buf.byte(ci - 1) } else { caret };
                self.mode = ViMode::Normal;
                (ViAction::None, new)
            }
            ViKey::Char(c) => {
                let edit = insert_at(caret, &c.to_string());
                (ViAction::Edit(vec![edit]), caret + c.len_utf8())
            }
            ViKey::Enter => (ViAction::Edit(vec![insert_at(caret, "\n")]), caret + 1),
            ViKey::Backspace => {
                if caret == 0 {
                    return (ViAction::None, caret);
                }
                let ci = buf.ci_of_byte(caret);
                let start = buf.byte(ci - 1);
                let removed = buf.slice(ci - 1, ci);
                (
                    ViAction::Edit(vec![TextEdit {
                        start,
                        removed,
                        inserted: String::new(),
                    }]),
                    start,
                )
            }
            // Other control keys in insert do nothing rather than erroring:
            // a stray modifier chord should not blank a person's document.
            ViKey::Ctrl(_) => (ViAction::None, caret),
        }
    }

    fn step_replace(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        match key {
            ViKey::Escape => {
                let ci = buf.ci_of_byte(caret);
                let start = buf.line_start(ci);
                let new = if ci > start { buf.byte(ci - 1) } else { caret };
                self.mode = ViMode::Normal;
                (ViAction::None, new)
            }
            ViKey::Char(c) => {
                let ci = buf.ci_of_byte(caret);
                let end = buf.line_end(ci);
                if ci < end {
                    // Overwrite the character under the caret.
                    let removed = buf.slice(ci, ci + 1);
                    (
                        ViAction::Edit(vec![TextEdit {
                            start: caret,
                            removed,
                            inserted: c.to_string(),
                        }]),
                        caret + c.len_utf8(),
                    )
                } else {
                    // Past the last character Replace appends, like insert.
                    (
                        ViAction::Edit(vec![insert_at(caret, &c.to_string())]),
                        caret + c.len_utf8(),
                    )
                }
            }
            ViKey::Enter => (ViAction::Edit(vec![insert_at(caret, "\n")]), caret + 1),
            ViKey::Backspace => {
                // Vim restores the overwritten byte here; without tracking the
                // pre-overwrite text the honest thing is to only step left,
                // which is documented as the Partial edge of Replace.
                if caret == 0 {
                    return (ViAction::None, caret);
                }
                let ci = buf.ci_of_byte(caret);
                (ViAction::None, buf.byte(ci - 1))
            }
            ViKey::Ctrl(_) => (ViAction::None, caret),
        }
    }

    // --- Normal mode -------------------------------------------------------

    fn step_normal(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        match self.stage {
            Stage::AwaitReplace => self.apply_replace_char(buf, key, caret),
            Stage::AfterG(op) => self.after_g(buf, op, key, caret),
            Stage::AfterOperator(op) => self.after_operator(buf, op, key, caret),
            Stage::AfterTextObject(op, inner) => self.after_text_object(buf, op, inner, key, caret),
            Stage::Ready => self.normal_ready(buf, key, caret),
        }
    }

    fn normal_ready(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        let ch = match key {
            ViKey::Char(c) => c,
            ViKey::Escape => {
                self.reset_pending();
                return (ViAction::None, caret);
            }
            ViKey::Ctrl('r') => return (ViAction::Redo, caret),
            ViKey::Ctrl('v') => {
                return self.refuse(
                    caret,
                    "Blockwise visual is not supported",
                    "vi mode offers characterwise (v) and linewise (V) visual only.",
                );
            }
            ViKey::Ctrl(_) | ViKey::Enter | ViKey::Backspace => {
                return (ViAction::None, caret);
            }
        };

        // A count builds up until a command consumes it; `0` is the
        // start-of-line motion only when no count is being typed.
        if ch.is_ascii_digit() && !(ch == '0' && self.count.is_none()) {
            let digit = ch as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0) * 10 + digit);
            return (ViAction::None, caret);
        }

        let count = self.count.take();

        match ch {
            'i' => self.enter_insert(caret),
            'I' => {
                let target = buf.byte(buf.first_non_blank(buf.line_start(buf.ci_of_byte(caret))));
                self.enter_insert(target)
            }
            'a' => {
                let ci = buf.ci_of_byte(caret);
                let target = buf.byte((ci + 1).min(buf.line_end(ci)));
                self.enter_insert(target)
            }
            'A' => {
                let target = buf.byte(buf.line_end(buf.ci_of_byte(caret)));
                self.enter_insert(target)
            }
            'o' => self.open_line(buf, caret, true),
            'O' => self.open_line(buf, caret, false),
            'R' => {
                self.mode = ViMode::Replace;
                (ViAction::None, caret)
            }
            'v' => {
                self.mode = ViMode::Visual;
                self.visual_anchor = caret;
                (ViAction::None, caret)
            }
            'V' => {
                self.mode = ViMode::VisualLine;
                self.visual_anchor = caret;
                (ViAction::None, caret)
            }
            'd' => {
                self.operator_count = count;
                self.stage = Stage::AfterOperator(Operator::Delete);
                (ViAction::None, caret)
            }
            'c' => {
                self.operator_count = count;
                self.stage = Stage::AfterOperator(Operator::Change);
                (ViAction::None, caret)
            }
            'y' => {
                self.operator_count = count;
                self.stage = Stage::AfterOperator(Operator::Yank);
                (ViAction::None, caret)
            }
            'g' => {
                self.count = count; // preserve any count for `{count}gg`
                self.stage = Stage::AfterG(None);
                (ViAction::None, caret)
            }
            'x' => self.delete_chars_forward(buf, caret, count.unwrap_or(1)),
            'X' => self.delete_chars_back(buf, caret, count.unwrap_or(1)),
            'D' => self.delete_to_line_end(buf, caret, false),
            'C' => self.delete_to_line_end(buf, caret, true),
            's' => {
                let (action, new) = self.delete_chars_forward(buf, caret, count.unwrap_or(1));
                if matches!(action, ViAction::Edit(_)) {
                    self.mode = ViMode::Insert;
                }
                (action, new)
            }
            'r' => {
                self.count = count;
                self.stage = Stage::AwaitReplace;
                (ViAction::None, caret)
            }
            'J' => self.join_lines(buf, caret, count),
            'p' => self.paste(buf, caret, true),
            'P' => self.paste(buf, caret, false),
            'u' => (ViAction::Undo, caret),
            '/' => (ViAction::Search(SearchIntent::PromptForward), caret),
            '?' => (ViAction::Search(SearchIntent::PromptBackward), caret),
            'n' => (ViAction::Search(SearchIntent::Next), caret),
            'N' => (ViAction::Search(SearchIntent::Previous), caret),
            '*' => (ViAction::Search(SearchIntent::WordForward), caret),
            '#' => (ViAction::Search(SearchIntent::WordBackward), caret),
            '"' => self.refuse(
                caret,
                "Named registers are not supported",
                "vi mode uses only the unnamed register; \"x selections are unavailable.",
            ),
            'm' => self.refuse(
                caret,
                "Marks are not supported",
                "Setting a mark with m is outside the supported vi subset.",
            ),
            '`' | '\'' => self.refuse(
                caret,
                "Marks are not supported",
                "Jumping to a mark is outside the supported vi subset.",
            ),
            'q' => self.refuse(
                caret,
                "Macros are not supported",
                "Recording a macro with q is outside the supported vi subset.",
            ),
            '@' => self.refuse(
                caret,
                "Macros are not supported",
                "Replaying a macro with @ is outside the supported vi subset.",
            ),
            // Pure motions in Normal mode simply move the caret.
            'h' | 'l' | 'w' | 'W' | 'b' | 'B' | 'e' | 'E' | '0' | '^' | '$' | 'j' | 'k' | 'G' => {
                match resolve_motion(buf, caret, ch, count, None) {
                    Some(motion) => (ViAction::None, move_cursor(buf, caret, &motion)),
                    None => self.refuse_unsupported(caret, ch),
                }
            }
            other => self.refuse_unsupported(caret, other),
        }
    }

    fn after_g(
        &mut self,
        buf: &Buffer,
        op: Option<Operator>,
        key: ViKey,
        caret: usize,
    ) -> (ViAction, usize) {
        let count = self.count.take();
        self.stage = Stage::Ready;
        match key {
            ViKey::Char('g') => {
                // `gg` goes to a line (default the first); as an operator
                // motion it is linewise over the span between here and there.
                let target_line = count.map_or(0, |n| n.saturating_sub(1));
                match op {
                    None => {
                        let start = buf.first_non_blank(buf.nth_line_start(target_line));
                        (ViAction::None, buf.byte(start))
                    }
                    Some(op) => {
                        let here = buf.line_index(buf.ci_of_byte(caret));
                        let (first, last) = ordered(here, target_line.min(buf.line_count() - 1));
                        self.apply_linewise(buf, op, first, last)
                    }
                }
            }
            _ => {
                self.operator_count = None;
                self.refuse(
                    caret,
                    "Unsupported g command",
                    "Only gg is supported after g in vi mode.",
                )
            }
        }
    }

    fn after_operator(
        &mut self,
        buf: &Buffer,
        op: Operator,
        key: ViKey,
        caret: usize,
    ) -> (ViAction, usize) {
        let ch = match key {
            ViKey::Char(c) => c,
            ViKey::Escape => {
                self.reset_pending();
                return (ViAction::None, caret);
            }
            _ => {
                self.reset_pending();
                return self.refuse(
                    caret,
                    "Incomplete operator",
                    "That key cannot follow an operator in vi mode.",
                );
            }
        };

        // A count may follow the operator too (`d3w`); fold digits in.
        if ch.is_ascii_digit() && !(ch == '0' && self.count.is_none()) {
            let digit = ch as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0) * 10 + digit);
            return (ViAction::None, caret);
        }

        // Vim multiplies a count before the operator by a count after it. The
        // `Option` is preserved so a countless `dG` still means "to the last
        // line" rather than "to line 1".
        let oc = self.operator_count.take();
        let mc = self.count.take();
        let effective = match (oc, mc) {
            (None, None) => None,
            _ => Some(oc.unwrap_or(1) * mc.unwrap_or(1)),
        };
        let count = effective.unwrap_or(1);

        // A doubled operator (`dd`, `cc`, `yy`) is linewise over `count` lines.
        let doubled = matches!(
            (op, ch),
            (Operator::Delete, 'd') | (Operator::Change, 'c') | (Operator::Yank, 'y')
        );
        if doubled {
            self.stage = Stage::Ready;
            let here = buf.line_index(buf.ci_of_byte(caret));
            let last = (here + count - 1).min(buf.line_count() - 1);
            return self.apply_linewise(buf, op, here, last);
        }

        match ch {
            'i' => {
                self.stage = Stage::AfterTextObject(op, true);
                self.count = Some(count);
                (ViAction::None, caret)
            }
            'a' => {
                self.stage = Stage::AfterTextObject(op, false);
                self.count = Some(count);
                (ViAction::None, caret)
            }
            'g' => {
                self.stage = Stage::AfterG(Some(op));
                self.count = effective;
                (ViAction::None, caret)
            }
            _ => {
                self.stage = Stage::Ready;
                match resolve_motion(buf, caret, ch, effective, Some(op)) {
                    Some(motion) => self.apply_operator(buf, op, motion),
                    None => self.refuse_unsupported(caret, ch),
                }
            }
        }
    }

    fn after_text_object(
        &mut self,
        buf: &Buffer,
        op: Operator,
        inner: bool,
        key: ViKey,
        caret: usize,
    ) -> (ViAction, usize) {
        self.stage = Stage::Ready;
        self.count.take();
        let big = match key {
            ViKey::Char('w') => false,
            ViKey::Char('W') => true,
            _ => {
                return self.refuse(
                    caret,
                    "Unsupported text object",
                    "Only iw, aw, iW, and aW are supported in vi mode.",
                );
            }
        };
        let (start, end) = word_object(buf, buf.ci_of_byte(caret), inner, big);
        let motion = Motion::Chars {
            start,
            end,
            inclusive: false,
        };
        self.apply_operator(buf, op, motion)
    }

    fn apply_replace_char(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        self.stage = Stage::Ready;
        let count = self.count.take().unwrap_or(1);
        let c = match key {
            ViKey::Char(c) => c,
            ViKey::Enter => '\n',
            ViKey::Escape => return (ViAction::None, caret),
            _ => {
                return self.refuse(
                    caret,
                    "Unsupported replacement",
                    "r must be followed by a single character.",
                );
            }
        };
        let ci = buf.ci_of_byte(caret);
        let end = buf.line_end(ci);
        // Vim refuses `r` when the count exceeds the characters left on the
        // line, changing nothing rather than spilling onto the next line.
        if ci + count > end {
            return self.refuse(
                caret,
                "Cannot replace past end of line",
                "r would need more characters than remain on this line.",
            );
        }
        let removed = buf.slice(ci, ci + count);
        let inserted: String = std::iter::repeat_n(c, count).collect();
        let new_caret = buf.byte(ci + count - 1);
        (
            ViAction::Edit(vec![TextEdit {
                start: caret,
                removed,
                inserted,
            }]),
            new_caret,
        )
    }

    // --- Visual mode -------------------------------------------------------

    fn step_visual(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        // A text-object introducer seen mid-selection resolves the selection
        // to that object rather than moving the caret.
        if let Stage::AfterTextObject(_, inner) = self.stage {
            self.stage = Stage::Ready;
            let big = match key {
                ViKey::Char('w') => false,
                ViKey::Char('W') => true,
                _ => {
                    return self.refuse(
                        caret,
                        "Unsupported text object",
                        "Only iw, aw, iW, and aW are supported in vi mode.",
                    );
                }
            };
            let (start, end) = word_object(buf, buf.ci_of_byte(caret), inner, big);
            self.visual_anchor = buf.byte(start);
            let last = buf.byte(end.saturating_sub(1).max(start));
            return (ViAction::None, last);
        }
        if let Stage::AwaitReplace = self.stage {
            return self.visual_replace(buf, key, caret);
        }
        if let Stage::AfterG(_) = self.stage {
            self.stage = Stage::Ready;
            if let ViKey::Char('g') = key {
                let count = self.count.take();
                let target_line = count.map_or(0, |n| n.saturating_sub(1));
                return (
                    ViAction::None,
                    buf.byte(buf.first_non_blank(buf.nth_line_start(target_line))),
                );
            }
            return self.refuse(
                caret,
                "Unsupported g command",
                "Only gg is supported after g.",
            );
        }

        let ch = match key {
            ViKey::Escape => {
                self.mode = ViMode::Normal;
                self.reset_pending();
                return (ViAction::None, caret);
            }
            ViKey::Char(c) => c,
            ViKey::Ctrl('v') => {
                return self.refuse(
                    caret,
                    "Blockwise visual is not supported",
                    "vi mode offers characterwise (v) and linewise (V) visual only.",
                );
            }
            _ => return (ViAction::None, caret),
        };

        if ch.is_ascii_digit() && !(ch == '0' && self.count.is_none()) {
            let digit = ch as usize - '0' as usize;
            self.count = Some(self.count.unwrap_or(0) * 10 + digit);
            return (ViAction::None, caret);
        }
        let count = self.count.take();

        match ch {
            'v' => {
                // Toggling to characterwise, or off if already there.
                self.mode = ViMode::Visual;
                (ViAction::None, caret)
            }
            'V' => {
                self.mode = ViMode::VisualLine;
                (ViAction::None, caret)
            }
            'i' => {
                self.stage = Stage::AfterTextObject(Operator::Delete, true);
                (ViAction::None, caret)
            }
            'a' => {
                self.stage = Stage::AfterTextObject(Operator::Delete, false);
                (ViAction::None, caret)
            }
            'g' => {
                self.count = count;
                self.stage = Stage::AfterG(None);
                (ViAction::None, caret)
            }
            'd' | 'x' => self.visual_operate(buf, Operator::Delete, caret),
            'c' | 's' => self.visual_operate(buf, Operator::Change, caret),
            'y' => self.visual_operate(buf, Operator::Yank, caret),
            'r' => {
                self.stage = Stage::AwaitReplace;
                (ViAction::None, caret)
            }
            '"' => self.refuse(
                caret,
                "Named registers are not supported",
                "vi mode uses only the unnamed register.",
            ),
            'h' | 'l' | 'w' | 'W' | 'b' | 'B' | 'e' | 'E' | '0' | '^' | '$' | 'j' | 'k' | 'G' => {
                match resolve_motion(buf, caret, ch, count, None) {
                    Some(motion) => (ViAction::None, move_cursor(buf, caret, &motion)),
                    None => self.refuse_unsupported(caret, ch),
                }
            }
            other => self.refuse_unsupported(caret, other),
        }
    }

    fn visual_bounds(&self, buf: &Buffer, caret: usize) -> (usize, usize) {
        let anchor_ci = buf.ci_of_byte(self.visual_anchor);
        let caret_ci = buf.ci_of_byte(caret);
        ordered(anchor_ci, caret_ci)
    }

    fn visual_operate(&mut self, buf: &Buffer, op: Operator, caret: usize) -> (ViAction, usize) {
        let linewise = self.mode == ViMode::VisualLine;
        let (lo, hi) = self.visual_bounds(buf, caret);
        self.mode = ViMode::Normal;
        self.reset_pending();
        if linewise {
            let first = buf.line_index(lo);
            let last = buf.line_index(hi);
            self.apply_linewise(buf, op, first, last)
        } else {
            // Visual characterwise selection is inclusive of the caret char.
            let motion = Motion::Chars {
                start: lo,
                end: (hi + 1).min(buf.nchars()),
                inclusive: false,
            };
            self.apply_operator(buf, op, motion)
        }
    }

    fn visual_replace(&mut self, buf: &Buffer, key: ViKey, caret: usize) -> (ViAction, usize) {
        self.stage = Stage::Ready;
        let c = match key {
            ViKey::Char(c) => c,
            _ => {
                self.mode = ViMode::Normal;
                return self.refuse(
                    caret,
                    "Unsupported replacement",
                    "r must be followed by a single character.",
                );
            }
        };
        let linewise = self.mode == ViMode::VisualLine;
        let (lo, hi) = self.visual_bounds(buf, caret);
        self.mode = ViMode::Normal;
        let (start_ci, end_ci) = if linewise {
            (buf.nth_line_start(buf.line_index(lo)), buf.line_end(hi))
        } else {
            (lo, (hi + 1).min(buf.nchars()))
        };
        // Replace every non-newline character in the selection; newlines are
        // preserved so `r` cannot silently reshape the line structure.
        let mut inserted = String::new();
        for ci in start_ci..end_ci {
            if buf.ch(ci) == Some('\n') {
                inserted.push('\n');
            } else {
                inserted.push(c);
            }
        }
        let removed = buf.slice(start_ci, end_ci);
        (
            ViAction::Edit(vec![TextEdit {
                start: buf.byte(start_ci),
                removed,
                inserted,
            }]),
            buf.byte(start_ci),
        )
    }

    // --- Shared edit builders ---------------------------------------------

    fn enter_insert(&mut self, caret: usize) -> (ViAction, usize) {
        self.mode = ViMode::Insert;
        (ViAction::None, caret)
    }

    fn open_line(&mut self, buf: &Buffer, caret: usize, below: bool) -> (ViAction, usize) {
        let ci = buf.ci_of_byte(caret);
        if below {
            let at = buf.byte(buf.line_end(ci));
            self.mode = ViMode::Insert;
            (ViAction::Edit(vec![insert_at(at, "\n")]), at + 1)
        } else {
            let at = buf.byte(buf.line_start(ci));
            self.mode = ViMode::Insert;
            (ViAction::Edit(vec![insert_at(at, "\n")]), at)
        }
    }

    fn delete_chars_forward(
        &mut self,
        buf: &Buffer,
        caret: usize,
        count: usize,
    ) -> (ViAction, usize) {
        let ci = buf.ci_of_byte(caret);
        let end = buf.line_end(ci);
        if ci >= end {
            return (ViAction::None, caret);
        }
        let stop = (ci + count).min(end);
        let removed = buf.slice(ci, stop);
        self.register = Register {
            text: removed.clone(),
            linewise: false,
        };
        let new_caret = caret_after_delete(buf, ci, stop);
        (
            ViAction::Edit(vec![TextEdit {
                start: caret,
                removed,
                inserted: String::new(),
            }]),
            new_caret,
        )
    }

    fn delete_chars_back(&mut self, buf: &Buffer, caret: usize, count: usize) -> (ViAction, usize) {
        let ci = buf.ci_of_byte(caret);
        let start_line = buf.line_start(ci);
        if ci == start_line {
            return (ViAction::None, caret);
        }
        let start = ci.saturating_sub(count).max(start_line);
        let removed = buf.slice(start, ci);
        self.register = Register {
            text: removed.clone(),
            linewise: false,
        };
        (
            ViAction::Edit(vec![TextEdit {
                start: buf.byte(start),
                removed,
                inserted: String::new(),
            }]),
            buf.byte(start),
        )
    }

    fn delete_to_line_end(
        &mut self,
        buf: &Buffer,
        caret: usize,
        change: bool,
    ) -> (ViAction, usize) {
        let ci = buf.ci_of_byte(caret);
        let end = buf.line_end(ci);
        let removed = buf.slice(ci, end);
        self.register = Register {
            text: removed.clone(),
            linewise: false,
        };
        if change {
            self.mode = ViMode::Insert;
        }
        let new_caret = if change {
            caret
        } else {
            // After D the caret rests on the new last character, if any.
            let start = buf.line_start(ci);
            if ci > start {
                buf.byte(ci - 1)
            } else {
                caret
            }
        };
        (
            ViAction::Edit(vec![TextEdit {
                start: caret,
                removed,
                inserted: String::new(),
            }]),
            new_caret,
        )
    }

    fn join_lines(
        &mut self,
        buf: &Buffer,
        caret: usize,
        count: Option<usize>,
    ) -> (ViAction, usize) {
        // `J` joins this line with the next; `{count}J` joins `count` lines,
        // so `3J` collapses three lines into one with two joins. Each seam is
        // computed against the original buffer, which keeps the edits ascending
        // and non-overlapping — one undo transaction.
        let joins = count.map_or(1, |n| n.saturating_sub(1).max(1));
        let base_line = buf.line_index(buf.ci_of_byte(caret));
        let mut edits = Vec::new();
        let mut join_caret = caret;
        for k in 0..joins {
            let line = base_line + k;
            if line + 1 >= buf.line_count() {
                break;
            }
            let this_end = buf.line_end_by_line(line); // index of this line's '\n'
            let next_start = this_end + 1;
            // Vim discards the second line's leading whitespace at the seam.
            let next_first = buf.first_non_blank(next_start);
            // A stray CR is never left behind where the lines meet.
            let mut end_of_first = this_end;
            while end_of_first > buf.nth_line_start(line) && buf.ch(end_of_first - 1) == Some('\r')
            {
                end_of_first -= 1;
            }
            let separator = join_separator(buf, end_of_first, next_first);
            if k == 0 {
                // Vim leaves the caret on the join column: the separating
                // space, or the last non-blank of the first line when its own
                // whitespace is reused as the separator.
                let mut caret_col = end_of_first;
                while caret_col > buf.nth_line_start(line)
                    && matches!(buf.ch(caret_col - 1), Some(' ') | Some('\t'))
                {
                    caret_col -= 1;
                }
                join_caret = buf.byte(caret_col);
            }
            edits.push(TextEdit {
                start: buf.byte(end_of_first),
                removed: buf.slice(end_of_first, next_first),
                inserted: separator.to_owned(),
            });
        }
        if edits.is_empty() {
            return (ViAction::None, caret);
        }
        (ViAction::Edit(edits), join_caret)
    }

    fn paste(&mut self, buf: &Buffer, caret: usize, after: bool) -> (ViAction, usize) {
        if self.register.text.is_empty() {
            return (ViAction::None, caret);
        }
        let ci = buf.ci_of_byte(caret);
        if self.register.linewise {
            let text = &self.register.text;
            let line = buf.line_index(ci);
            if after {
                let end = buf.line_end_by_line(line);
                if end < buf.nchars() {
                    // There is a following line: insert before it.
                    let at = buf.byte(end + 1);
                    let caret = buf.byte(buf.first_non_blank(end + 1));
                    return (ViAction::Edit(vec![insert_at(at, text)]), caret);
                }
                // Last line without a trailing newline: open a line below and
                // drop the register's own trailing newline so no blank line is
                // manufactured at the end of the file.
                let at = buf.byte(end);
                let body = text.strip_suffix('\n').unwrap_or(text);
                let inserted = format!("\n{body}");
                let caret = at + 1;
                return (ViAction::Edit(vec![insert_at(at, &inserted)]), caret);
            }
            let start = buf.nth_line_start(line);
            let at = buf.byte(start);
            let caret = at + leading_blank_bytes(text);
            (ViAction::Edit(vec![insert_at(at, text)]), caret)
        } else {
            let text = self.register.text.clone();
            let insert_ci = if after {
                (ci + 1).min(buf.line_end(ci))
            } else {
                ci
            };
            let at = buf.byte(insert_ci);
            // Characterwise paste leaves the caret on the last inserted char.
            let last = at + text.len() - last_char_len(&text);
            (ViAction::Edit(vec![insert_at(at, &text)]), last)
        }
    }

    fn apply_operator(&mut self, buf: &Buffer, op: Operator, motion: Motion) -> (ViAction, usize) {
        let (start_ci, end_ci) = match motion {
            Motion::Lines { first, last } => {
                return self.apply_linewise(buf, op, first, last);
            }
            Motion::Chars {
                start,
                end,
                inclusive,
            } => {
                let end = if inclusive {
                    (end + 1).min(buf.nchars())
                } else {
                    end
                };
                // Vim's exclusive-motion special cases (`:help exclusive`):
                // an exclusive motion that ends in column 1 is pulled back to
                // the end of the previous line, and if it started at or before
                // the first non-blank it becomes linewise. This is what makes
                // `dw` on the last word of a line stop at the line's end
                // instead of eating the newline.
                if !inclusive && end > start {
                    match promote_exclusive(buf, start, end) {
                        Some(new_end) => (start, new_end),
                        None => (start, end),
                    }
                } else {
                    (start, end)
                }
            }
        };

        let removed = buf.slice(start_ci, end_ci);
        let start = buf.byte(start_ci);
        match op {
            Operator::Yank => {
                self.register = Register {
                    text: removed,
                    linewise: false,
                };
                (ViAction::None, start)
            }
            Operator::Delete => {
                self.register = Register {
                    text: removed.clone(),
                    linewise: false,
                };
                let new_caret = caret_after_delete(buf, start_ci, end_ci);
                (
                    ViAction::Edit(vec![TextEdit {
                        start,
                        removed,
                        inserted: String::new(),
                    }]),
                    new_caret,
                )
            }
            Operator::Change => {
                self.register = Register {
                    text: removed.clone(),
                    linewise: false,
                };
                self.mode = ViMode::Insert;
                (
                    ViAction::Edit(vec![TextEdit {
                        start,
                        removed,
                        inserted: String::new(),
                    }]),
                    start,
                )
            }
        }
    }

    fn apply_linewise(
        &mut self,
        buf: &Buffer,
        op: Operator,
        first: usize,
        last: usize,
    ) -> (ViAction, usize) {
        let (first, last) = ordered(first, last);
        let register_text = buf.linewise_register_text(first, last);
        match op {
            Operator::Yank => {
                self.register = Register {
                    text: register_text,
                    linewise: true,
                };
                let start = buf.first_non_blank(buf.nth_line_start(first));
                (ViAction::None, buf.byte(start))
            }
            Operator::Delete => {
                self.register = Register {
                    text: register_text,
                    linewise: true,
                };
                let (start, end) = buf.linewise_delete_range(first, last);
                let removed = buf.slice(start, end);
                // Vim drops the caret to the first non-blank of the line that
                // now occupies the deleted span: the line after the block if
                // there is one, otherwise the line before it, else the start.
                let removed_bytes = buf.byte(end) - buf.byte(start);
                let new_caret = if last + 1 < buf.line_count() {
                    let target = buf.first_non_blank(buf.nth_line_start(last + 1));
                    buf.byte(target) - removed_bytes
                } else if first > 0 {
                    buf.byte(buf.first_non_blank(buf.nth_line_start(first - 1)))
                } else {
                    0
                };
                (
                    ViAction::Edit(vec![TextEdit {
                        start: buf.byte(start),
                        removed,
                        inserted: String::new(),
                    }]),
                    new_caret,
                )
            }
            Operator::Change => {
                // `cc`/`cj` remove the lines' content but keep one empty line
                // to insert on, collapsing any joined lines' newlines too.
                self.register = Register {
                    text: register_text,
                    linewise: true,
                };
                let start = buf.nth_line_start(first);
                let end = buf.line_end_by_line(last);
                let removed = buf.slice(start, end);
                self.mode = ViMode::Insert;
                (
                    ViAction::Edit(vec![TextEdit {
                        start: buf.byte(start),
                        removed,
                        inserted: String::new(),
                    }]),
                    buf.byte(start),
                )
            }
        }
    }

    fn refuse(&mut self, caret: usize, headline: &str, detail: &str) -> (ViAction, usize) {
        // A refusal is total: pending state is discarded and no edit is
        // produced, so the user's text is exactly where they left it and the
        // engine is not stranded mid-sequence.
        self.reset_pending();
        (
            ViAction::Refused(SearchError::new(headline.to_owned(), detail.to_owned())),
            caret,
        )
    }

    fn refuse_unsupported(&mut self, caret: usize, ch: char) -> (ViAction, usize) {
        let detail = format!("The key '{ch}' is not part of the supported vi subset.");
        self.refuse(caret, "Unsupported command", &detail)
    }
}

// --- Motions ---------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Motion {
    Chars {
        start: usize,
        end: usize,
        inclusive: bool,
    },
    Lines {
        first: usize,
        last: usize,
    },
}

/// Applies Vim's exclusive-motion adjustment for an operator whose motion ends
/// in column 1: the end is pulled back to the end of the previous line.
/// Returns `None` when the motion does not end at a line start.
///
/// The generic "becomes linewise" promotion (`:help exclusive` rule 2) is
/// deliberately not applied here: the only exclusive word motions in scope are
/// `w`/`W`, and Vim's own `w` special case keeps `dw` on a line's last word
/// characterwise — it deletes to the end of the line and leaves the newline,
/// rather than swallowing the whole line.
fn promote_exclusive(buf: &Buffer, start: usize, end: usize) -> Option<usize> {
    let end_line = buf.line_index(end.min(buf.nchars()));
    if end_line == 0 || end != buf.nth_line_start(end_line) {
        return None;
    }
    let start_line = buf.line_index(start);
    if end_line <= start_line {
        return None;
    }
    Some(buf.line_end_by_line(end_line - 1))
}

/// Resolves a single-character motion into a range. `op` is `Some` when the
/// motion is the object of an operator, which changes `j`/`k`/`G` into linewise
/// spans and enables `cw`'s special case. `count` is `None` when the user typed
/// no count, which `G` needs to tell "go to the last line" from "go to line 1".
fn resolve_motion(
    buf: &Buffer,
    caret: usize,
    key: char,
    count: Option<usize>,
    op: Option<Operator>,
) -> Option<Motion> {
    let ci = buf.ci_of_byte(caret);
    let for_op = op.is_some();
    let n = count.unwrap_or(1);
    match key {
        'h' => {
            let start = buf.line_start(ci);
            let target = ci.saturating_sub(n).max(start);
            Some(Motion::Chars {
                start: target,
                end: ci,
                inclusive: false,
            })
        }
        'l' => {
            let end = buf.line_end(ci);
            let target = (ci + n).min(end);
            Some(Motion::Chars {
                start: ci,
                end: target,
                inclusive: false,
            })
        }
        'w' | 'W' => {
            let big = key == 'W';
            // `cw` on a non-blank behaves like `ce`: Vim does not swallow the
            // whitespace after the word when changing it.
            if op == Some(Operator::Change) && buf.ch(ci).map(class) != Some(Class::Blank) {
                let target = word_end(buf, ci, n, big);
                return Some(Motion::Chars {
                    start: ci,
                    end: target,
                    inclusive: true,
                });
            }
            let target = word_forward(buf, ci, n, big);
            Some(Motion::Chars {
                start: ci,
                end: target,
                inclusive: false,
            })
        }
        'b' | 'B' => {
            let target = word_back(buf, ci, n, key == 'B');
            Some(Motion::Chars {
                start: target,
                end: ci,
                inclusive: false,
            })
        }
        'e' | 'E' => {
            let target = word_end(buf, ci, n, key == 'E');
            Some(Motion::Chars {
                start: ci,
                end: target,
                inclusive: true,
            })
        }
        '0' => Some(Motion::Chars {
            start: buf.line_start(ci),
            end: ci,
            inclusive: false,
        }),
        '^' => {
            let target = buf.first_non_blank(buf.line_start(ci));
            let (start, end) = ordered(target, ci);
            Some(Motion::Chars {
                start,
                end,
                inclusive: false,
            })
        }
        '$' => {
            let line = (buf.line_index(ci) + n.saturating_sub(1)).min(buf.line_count() - 1);
            let end = buf.line_end_by_line(line);
            // `$` is inclusive: the last character is part of the range.
            let last = if end > buf.nth_line_start(line) {
                end - 1
            } else {
                end
            };
            Some(Motion::Chars {
                start: ci,
                end: last,
                inclusive: true,
            })
        }
        'j' => {
            if for_op {
                let line = buf.line_index(ci);
                let last = (line + n).min(buf.line_count() - 1);
                Some(Motion::Lines { first: line, last })
            } else {
                Some(Motion::Chars {
                    start: ci,
                    end: vertical(buf, ci, n as isize),
                    inclusive: false,
                })
            }
        }
        'k' => {
            if for_op {
                let line = buf.line_index(ci);
                let first = line.saturating_sub(n);
                Some(Motion::Lines { first, last: line })
            } else {
                Some(Motion::Chars {
                    start: vertical(buf, ci, -(n as isize)),
                    end: ci,
                    inclusive: false,
                })
            }
        }
        'G' => {
            // A bare `G` goes to the last line; `{count}G` to that line. The
            // `Option` is what tells the two apart, since a default count of 1
            // and an explicit `1G` are otherwise identical.
            let line = match count {
                Some(c) => c.saturating_sub(1).min(buf.line_count() - 1),
                None => buf.line_count() - 1,
            };
            if for_op {
                let here = buf.line_index(ci);
                let (first, last) = ordered(here, line);
                Some(Motion::Lines { first, last })
            } else {
                let target = buf.first_non_blank(buf.nth_line_start(line));
                Some(Motion::Chars {
                    start: target,
                    end: target,
                    inclusive: false,
                })
            }
        }
        _ => None,
    }
}

/// The byte offset a pure cursor motion lands on, given where the caret was.
/// The destination is whichever endpoint of the range is not the caret's
/// origin, and a caret never rests on a `\n` unless the line is empty.
fn move_cursor(buf: &Buffer, caret: usize, motion: &Motion) -> usize {
    let ci = buf.ci_of_byte(caret);
    let (start, end) = match *motion {
        Motion::Chars { start, end, .. } => (start, end),
        Motion::Lines { last, .. } => {
            return buf.byte(buf.first_non_blank(buf.nth_line_start(last)));
        }
    };
    let dest = if start == ci {
        end
    } else if end == ci {
        start
    } else {
        end
    };
    clamp_cursor(buf, dest.min(buf.nchars()))
}

fn clamp_cursor(buf: &Buffer, ci: usize) -> usize {
    // A Normal/Visual caret rests on a character, never on the newline or the
    // empty slot past the last character, unless the line itself is empty.
    let start = buf.line_start(ci);
    let end = buf.line_end(ci);
    if ci >= end && end > start {
        return buf.byte(end - 1);
    }
    buf.byte(ci)
}

/// Where the caret lands after deleting the char range `start..end`. Computed
/// from the *pre-edit* buffer but describing the *post-edit* text: if the
/// character that will survive at `start` is a newline or the end of the
/// buffer, the caret steps back onto the line's new last character, exactly as
/// Vim leaves it after `x` at end of line or `dw` on a line's last word.
fn caret_after_delete(buf: &Buffer, start: usize, end: usize) -> usize {
    let line_start = buf.line_start(start);
    let at_line_end = end >= buf.nchars() || buf.ch(end) == Some('\n');
    if at_line_end && start > line_start {
        buf.byte(start - 1)
    } else {
        buf.byte(start)
    }
}

fn vertical(buf: &Buffer, ci: usize, delta: isize) -> usize {
    let line = buf.line_index(ci) as isize;
    let target_line = (line + delta).clamp(0, buf.line_count() as isize - 1) as usize;
    let column = ci - buf.line_start(ci);
    let start = buf.nth_line_start(target_line);
    let end = buf.line_end_by_line(target_line);
    let last = if end > start { end - 1 } else { start };
    (start + column).min(last)
}

// --- Word scanning ---------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        // `is_alphanumeric` keeps accented Latin, CJK, and other letters in the
        // "word" class, so a motion over `café` or `日本語` treats them as one
        // word rather than splitting mid-character.
        Class::Word
    } else {
        Class::Punct
    }
}

fn word_forward(buf: &Buffer, mut ci: usize, count: usize, big: bool) -> usize {
    let n = buf.nchars();
    for _ in 0..count {
        if ci >= n {
            return n;
        }
        let start_class = class(buf.chars[ci]);
        if start_class != Class::Blank {
            if big {
                while ci < n && class(buf.chars[ci]) != Class::Blank {
                    ci += 1;
                }
            } else {
                while ci < n && class(buf.chars[ci]) == start_class {
                    ci += 1;
                }
            }
        }
        while ci < n && class(buf.chars[ci]) == Class::Blank {
            // An empty line counts as a word: `w` stops on it rather than
            // skipping through the blank run.
            if buf.chars[ci] == '\n' && (ci == 0 || buf.chars[ci - 1] == '\n') {
                break;
            }
            ci += 1;
        }
    }
    ci.min(n)
}

fn word_end(buf: &Buffer, mut ci: usize, count: usize, big: bool) -> usize {
    let n = buf.nchars();
    for _ in 0..count {
        if ci + 1 >= n {
            return n.saturating_sub(1);
        }
        ci += 1;
        while ci < n && class(buf.chars[ci]) == Class::Blank {
            ci += 1;
        }
        if ci >= n {
            return n - 1;
        }
        let c = class(buf.chars[ci]);
        if big {
            while ci + 1 < n && class(buf.chars[ci + 1]) != Class::Blank {
                ci += 1;
            }
        } else {
            while ci + 1 < n && class(buf.chars[ci + 1]) == c {
                ci += 1;
            }
        }
    }
    ci.min(n.saturating_sub(1))
}

fn word_back(buf: &Buffer, mut ci: usize, count: usize, big: bool) -> usize {
    for _ in 0..count {
        if ci == 0 {
            return 0;
        }
        ci -= 1;
        while ci > 0 && class(buf.chars[ci]) == Class::Blank {
            ci -= 1;
        }
        if class(buf.chars[ci]) == Class::Blank {
            return 0;
        }
        let c = class(buf.chars[ci]);
        if big {
            while ci > 0 && class(buf.chars[ci - 1]) != Class::Blank {
                ci -= 1;
            }
        } else {
            while ci > 0 && class(buf.chars[ci - 1]) == c {
                ci -= 1;
            }
        }
    }
    ci
}

/// The char range of an `iw`/`aw`/`iW`/`aW` text object around `ci`.
fn word_object(buf: &Buffer, ci: usize, inner: bool, big: bool) -> (usize, usize) {
    let n = buf.nchars();
    if n == 0 {
        return (0, 0);
    }
    let ci = ci.min(n - 1);
    let here = class(buf.chars[ci]);
    let same = |a: char| -> bool {
        if big {
            class(a) != Class::Blank
        } else {
            class(a) == here
        }
    };
    // Expand across the run the caret is inside (a blank run for `iw` on a
    // space, a word run otherwise).
    let mut start = ci;
    while start > 0
        && ((here == Class::Blank && class(buf.chars[start - 1]) == Class::Blank)
            || (here != Class::Blank && same(buf.chars[start - 1])))
    {
        start -= 1;
    }
    let mut end = ci + 1;
    while end < n
        && ((here == Class::Blank && class(buf.chars[end]) == Class::Blank)
            || (here != Class::Blank && same(buf.chars[end])))
    {
        end += 1;
    }
    if inner {
        return (start, end);
    }
    // `aw` includes trailing whitespace, or leading whitespace when there is
    // no trailing whitespace on the line.
    let mut aend = end;
    while aend < n && class(buf.chars[aend]) == Class::Blank && buf.chars[aend] != '\n' {
        aend += 1;
    }
    if aend > end {
        return (start, aend);
    }
    let mut astart = start;
    while astart > 0
        && class(buf.chars[astart - 1]) == Class::Blank
        && buf.chars[astart - 1] != '\n'
    {
        astart -= 1;
    }
    (astart, end)
}

// --- Buffer view -----------------------------------------------------------

/// A read-only, char-indexed view over the document text for the duration of
/// one keystroke. Holding it no longer than that is what keeps the engine from
/// ever operating on a stale copy of a shared document.
struct Buffer {
    chars: Vec<char>,
    byte_starts: Vec<usize>,
}

impl Buffer {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut byte_starts = Vec::with_capacity(chars.len() + 1);
        let mut byte = 0;
        for &c in &chars {
            byte_starts.push(byte);
            byte += c.len_utf8();
        }
        byte_starts.push(byte);
        Self { chars, byte_starts }
    }

    fn nchars(&self) -> usize {
        self.chars.len()
    }

    fn byte(&self, ci: usize) -> usize {
        self.byte_starts[ci.min(self.chars.len())]
    }

    fn ci_of_byte(&self, byte: usize) -> usize {
        match self.byte_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    fn ch(&self, ci: usize) -> Option<char> {
        self.chars.get(ci).copied()
    }

    fn slice(&self, start: usize, end: usize) -> String {
        let start = start.min(self.chars.len());
        let end = end.min(self.chars.len());
        self.chars[start..end].iter().collect()
    }

    fn line_start(&self, ci: usize) -> usize {
        let mut i = ci.min(self.chars.len());
        while i > 0 && self.chars[i - 1] != '\n' {
            i -= 1;
        }
        i
    }

    fn line_end(&self, ci: usize) -> usize {
        let mut i = ci.min(self.chars.len());
        while i < self.chars.len() && self.chars[i] != '\n' {
            i += 1;
        }
        i
    }

    fn first_non_blank(&self, line_start: usize) -> usize {
        let end = self.line_end(line_start);
        let mut i = line_start;
        while i < end && (self.chars[i] == ' ' || self.chars[i] == '\t') {
            i += 1;
        }
        i
    }

    fn line_index(&self, ci: usize) -> usize {
        self.chars[..ci.min(self.chars.len())]
            .iter()
            .filter(|&&c| c == '\n')
            .count()
    }

    fn line_count(&self) -> usize {
        self.chars.iter().filter(|&&c| c == '\n').count() + 1
    }

    fn nth_line_start(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut seen = 0;
        for (i, &c) in self.chars.iter().enumerate() {
            if c == '\n' {
                seen += 1;
                if seen == line {
                    return i + 1;
                }
            }
        }
        self.chars.len()
    }

    fn line_end_by_line(&self, line: usize) -> usize {
        self.line_end(self.nth_line_start(line))
    }

    /// The linewise register text for lines `first..=last`, always terminated
    /// with a newline so a later `p` puts it on its own line.
    fn linewise_register_text(&self, first: usize, last: usize) -> String {
        let start = self.nth_line_start(first);
        let end = self.line_end_by_line(last);
        let mut text = self.slice(start, end);
        text.push('\n');
        text
    }

    /// The char range a linewise delete of lines `first..=last` removes,
    /// taking the trailing newline when there is one and otherwise the leading
    /// newline so the last line does not leave a blank behind (`dd` on the
    /// final line).
    fn linewise_delete_range(&self, first: usize, last: usize) -> (usize, usize) {
        let start = self.nth_line_start(first);
        let end = self.line_end_by_line(last);
        if end < self.chars.len() {
            // A newline follows the last line: take it so the lines vanish.
            (start, end + 1)
        } else if start > 0 {
            // Last line, no trailing newline: consume the preceding newline.
            (start - 1, end)
        } else {
            // The whole buffer.
            (start, end)
        }
    }
}

// --- Small helpers ---------------------------------------------------------

fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn clamp_caret(buf: &Buffer, caret: usize) -> usize {
    let caret = caret.min(buf.byte(buf.nchars()));
    let ci = buf.ci_of_byte(caret);
    buf.byte(ci)
}

fn insert_at(at: usize, text: &str) -> TextEdit {
    TextEdit {
        start: at,
        removed: String::new(),
        inserted: text.to_owned(),
    }
}

fn join_separator(buf: &Buffer, end_of_first: usize, next_first: usize) -> &'static str {
    // Vim's join rule: normally one space between the joined halves, but no
    // space when the first line ends in whitespace, when the second line is
    // empty, or when the next character is a close paren.
    if next_first >= buf.nchars() || buf.ch(next_first) == Some('\n') {
        return "";
    }
    if end_of_first > 0 {
        let prev = buf.ch(end_of_first - 1);
        if prev == Some(' ') || prev == Some('\t') {
            return "";
        }
    }
    if buf.ch(next_first) == Some(')') {
        return "";
    }
    " "
}

fn leading_blank_bytes(text: &str) -> usize {
    text.bytes()
        .take_while(|&b| b == b' ' || b == b'\t')
        .count()
}

fn last_char_len(text: &str) -> usize {
    text.chars().next_back().map_or(0, char::len_utf8)
}

fn apply_edits_to_string(text: &str, edits: &[TextEdit]) -> String {
    let mut out = text.to_owned();
    for edit in edits.iter().rev() {
        out.replace_range(edit.start..edit.start + edit.removed.len(), &edit.inserted);
    }
    out
}

/// The single prefix/suffix edit that turns `old` into `new`, computed on char
/// boundaries so the resulting `TextEdit` can never split a multi-byte
/// character.
fn single_edit_diff(old: &str, new: &str) -> Option<TextEdit> {
    if old == new {
        return None;
    }
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let start_byte: usize = old_chars[..prefix].iter().map(|c| c.len_utf8()).sum();
    let removed: String = old_chars[prefix..old_chars.len() - suffix].iter().collect();
    let inserted: String = new_chars[prefix..new_chars.len() - suffix].iter().collect();
    Some(TextEdit {
        start: start_byte,
        removed,
        inserted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A caret is written into fixtures as a bare `|`; none of the fixtures
    // contain a literal pipe, so it is an unambiguous marker.
    fn unmark(marked: &str) -> (String, usize) {
        let caret = marked.find('|').expect("fixture needs a | caret");
        (marked.replacen('|', "", 1), caret)
    }

    fn mark(text: &str, caret: usize) -> String {
        let mut out = text.to_owned();
        out.insert(caret, '|');
        out
    }

    fn chars(s: &str) -> Vec<ViKey> {
        s.chars().map(ViKey::Char).collect()
    }

    /// Drives the engine through a sequence of keys, committing every returned
    /// edit exactly as the real caller would through `apply_edits`, and reports
    /// the final marked text, the engine, and the last action.
    fn drive(fixture: &str, keys: &[ViKey]) -> (ViEngine, String, ViAction) {
        let (mut text, mut caret) = unmark(fixture);
        let mut engine = ViEngine::new();
        let mut last = ViAction::None;
        for &key in keys {
            let response = engine.on_key(key, &text, caret);
            if let ViAction::Edit(edits) = &response.action {
                text = apply_edits_to_string(&text, edits);
            }
            caret = response.caret;
            last = response.action.clone();
        }
        (engine, mark(&text, caret), last)
    }

    fn run(fixture: &str, keys: &str) -> String {
        drive(fixture, &chars(keys)).1
    }

    #[track_caller]
    fn expect(fixture: &str, keys: &str, after: &str) {
        assert_eq!(run(fixture, keys), after, "keys `{keys}` on `{fixture}`");
    }

    // --- Motions -----------------------------------------------------------

    #[test]
    fn horizontal_and_word_motions() {
        expect("|hello world", "l", "h|ello world");
        expect("|hello world", "w", "hello |world");
        expect("|hello world", "e", "hell|o world");
        expect("|hello world", "$", "hello worl|d");
        expect("hello worl|d", "0", "|hello world");
        expect("hello worl|d", "b", "hello |world");
        expect("hello |world", "b", "|hello world");
        expect("|hello world", "^", "|hello world");
        expect("  |hello", "0", "|  hello");
        expect("|  hello", "^", "  |hello");
    }

    #[test]
    fn word_motions_stop_at_punctuation() {
        // A run of punctuation is its own word, distinct from the word chars
        // beside it.
        expect("|foo.bar", "w", "foo|.bar");
        expect("foo|.bar", "w", "foo.|bar");
        expect("|foo.bar", "W", "foo.ba|r"); // no next WORD: land on last char
        expect("|a b c", "W", "a |b c");
    }

    #[test]
    fn motions_clamp_at_buffer_edges() {
        expect("|abc", "h", "|abc");
        expect("ab|c", "l", "ab|c"); // cannot pass the last character
        expect("|abc", "b", "|abc");
        expect("ab|c", "w", "ab|c");
        expect("|abc", "$", "ab|c");
    }

    #[test]
    fn vertical_and_goto_motions() {
        expect("|one\ntwo\nthree", "j", "one\n|two\nthree");
        expect("one\n|two\nthree", "k", "|one\ntwo\nthree");
        expect("|one\ntwo\nthree", "G", "one\ntwo\n|three");
        expect("one\ntwo\n|three", "gg", "|one\ntwo\nthree");
        expect("|one\ntwo\nthree", "2G", "one\n|two\nthree");
        // Column is preserved across a vertical move, clamped to the line.
        expect("ab|c\nxy", "j", "abc\nx|y");
    }

    #[test]
    fn motions_on_empty_and_short_lines() {
        expect("|a\n\nb", "j", "a\n|\nb"); // caret rests on the empty line
        expect("a\n|\nb", "j", "a\n\n|b");
        expect("a\n|\nb", "$", "a\n|\nb"); // $ on an empty line stays put
        expect("|a\n\nb", "w", "a\n|\nb"); // w stops on the empty line
    }

    // --- Character edits ---------------------------------------------------

    #[test]
    fn delete_and_replace_single_characters() {
        expect("|hello", "x", "|ello");
        expect("|hello", "3x", "|lo");
        expect("hell|o", "x", "hel|l");
        expect("he|llo", "X", "h|llo");
        expect("|hello", "X", "|hello"); // nothing before the caret
        expect("|hello", "rx", "|xello");
        expect("|hello", "3rx", "xx|xlo");
    }

    #[test]
    fn replace_refuses_past_end_of_line_without_editing() {
        let (_engine, text, action) = drive("hel|lo", &chars("3rx"));
        assert!(matches!(action, ViAction::Refused(_)));
        assert_eq!(text, "hel|lo");
    }

    #[test]
    fn delete_to_end_of_line() {
        expect("hello |world", "D", "hello| ");
        expect("|hello", "D", "|");
    }

    // --- Operator + motion -------------------------------------------------

    #[test]
    fn delete_with_motions() {
        expect("|hello world", "dw", "|world");
        expect("|hello world", "de", "| world");
        expect("hello |world", "db", "|world");
        expect("|hello world", "d$", "|");
        expect("|hello world", "dl", "|ello world");
    }

    #[test]
    fn delete_word_counts_agree_with_vim() {
        expect("|one two three", "2dw", "|three");
        expect("|one two three", "d2w", "|three");
        expect("|one two three", "3x", "| two three");
    }

    #[test]
    fn dw_on_last_word_of_a_line_keeps_the_newline() {
        // Vim's `w` special case: `dw` on a line's final word deletes to the
        // end of the line and leaves the line break intact.
        expect("|foo\nbar", "dw", "|\nbar");
        expect("ab |cd\nef", "dw", "ab| \nef");
    }

    #[test]
    fn change_word_behaves_like_change_to_end() {
        // `cw` on a non-blank does not eat the trailing space, unlike `dw`.
        let (engine, text, _) = drive("|one two", &[ViKey::Char('c'), ViKey::Char('w')]);
        assert_eq!(engine.mode(), ViMode::Insert);
        assert_eq!(text, "| two");
    }

    #[test]
    fn change_then_type_and_leave_insert() {
        let (engine, text, _) = drive(
            "|one two",
            &[
                ViKey::Char('c'),
                ViKey::Char('w'),
                ViKey::Char('X'),
                ViKey::Escape,
            ],
        );
        assert_eq!(engine.mode(), ViMode::Normal);
        assert_eq!(text, "|X two");
    }

    #[test]
    fn yank_then_paste_characterwise() {
        // `yl` leaves the caret at the yank start and `p` inserts after it.
        expect("|hello world", "ylp", "h|hello world");
        expect("|hello world", "ylP", "|hhello world");
    }

    // --- Linewise operators ------------------------------------------------

    #[test]
    fn linewise_delete_and_yank() {
        expect("|one\ntwo\nthree", "dd", "|two\nthree");
        expect("one\n|two\nthree", "dd", "one\n|three");
        expect("one\ntwo\n|three", "dd", "one\n|two");
        expect("|only", "dd", "|");
        expect("|one\ntwo\nthree", "2dd", "|three");
        expect("|one\ntwo\nthree", "dj", "|three");
    }

    #[test]
    fn change_line_keeps_an_empty_line() {
        let (engine, text, _) = drive("  |hello\nworld", &chars("cc"));
        assert_eq!(engine.mode(), ViMode::Insert);
        assert_eq!(text, "|\nworld");
    }

    #[test]
    fn linewise_paste_lands_on_its_own_line() {
        // The classic bug: a linewise register must open a new line, never
        // splice into the current one.
        expect("|one\ntwo", "yyp", "one\n|one\ntwo");
        expect("|one\ntwo", "yyP", "|one\none\ntwo");
        // On the last line with no trailing newline, `p` still opens below.
        expect("one\n|two", "yyp", "one\ntwo\n|two");
    }

    #[test]
    fn characterwise_paste_does_not_open_a_line() {
        expect("|abc", "ylp", "a|abc");
    }

    // --- Join --------------------------------------------------------------

    #[test]
    fn join_lines_with_the_single_space_rule() {
        expect("|one\ntwo", "J", "one| two");
        expect("|a\nb\nc", "3J", "a| b c");
        // A trailing space on the first line is not doubled.
        expect("|one \ntwo", "J", "one| two");
        // No space is inserted before a close paren.
        expect("|foo\n)", "J", "foo|)");
        // Joining an empty next line adds no space.
        expect("|foo\n\nbar", "J", "foo|\nbar");
    }

    #[test]
    fn join_never_leaves_a_stray_carriage_return() {
        // The document is normalised to `\n`, but even handed a CRLF seam the
        // join must not leave a `\r` behind.
        expect("|a\r\nb", "J", "a| b");
    }

    // --- Visual mode -------------------------------------------------------

    #[test]
    fn characterwise_visual_delete_and_yank() {
        expect("|hello world", "vlld", "|lo world");
        // Selecting backwards covers the same span.
        expect("hell|o", "vhhd", "h|e");
        expect("|hello", "vlyp", "hh|eello");
    }

    #[test]
    fn linewise_visual_delete() {
        expect("|one\ntwo\nthree", "Vjd", "|three");
        expect("one\n|two\nthree", "Vd", "one\n|three");
    }

    #[test]
    fn visual_reports_its_mode_word() {
        let mut engine = ViEngine::new();
        engine.on_key(ViKey::Char('v'), "abc", 0);
        assert_eq!(engine.mode_label(), "VISUAL");
        engine.on_key(ViKey::Char('V'), "abc", 0);
        assert_eq!(engine.mode_label(), "VISUAL");
    }

    // --- Text objects ------------------------------------------------------

    #[test]
    fn word_text_objects() {
        expect("hel|lo world", "diw", "| world");
        expect("hel|lo world", "daw", "|world");
        expect("foo |bar baz", "diw", "foo | baz");
        expect("a.|b.c", "diW", "|"); // WORD spans punctuation-joined token
    }

    #[test]
    fn visual_text_object_selection() {
        expect("hel|lo world", "viwd", "| world");
    }

    // --- Multi-byte safety -------------------------------------------------

    #[test]
    fn multibyte_word_deletion_lands_on_boundaries() {
        expect("|café world", "dw", "|world");
        expect("|日本語 x", "dw", "|x");
    }

    #[test]
    fn multibyte_character_edits() {
        expect("|café", "x", "|afé");
        expect("caf|é", "x", "ca|f");
        // Delete an emoji (a 4-byte character) under the caret.
        expect("a|😀b", "x", "a|b");
        expect("|a😀b", "lx", "a|b");
    }

    #[test]
    fn multibyte_motions_are_boundary_safe() {
        expect("|café", "e", "caf|é");
        expect("caf|é", "b", "|café");
        expect("|a😀b", "$", "a😀|b");
    }

    // --- Mode entry / exit -------------------------------------------------

    #[test]
    fn mode_entry_and_exit_words() {
        let mut engine = ViEngine::new();
        assert_eq!(engine.mode_label(), "NORMAL");
        engine.on_key(ViKey::Char('i'), "abc", 0);
        assert_eq!(engine.mode_label(), "INSERT");
        engine.on_key(ViKey::Escape, "abc", 1);
        assert_eq!(engine.mode_label(), "NORMAL");
        engine.on_key(ViKey::Char('R'), "abc", 0);
        assert_eq!(engine.mode_label(), "REPLACE");
    }

    #[test]
    fn append_and_open_line() {
        expect("|abc", "aX", "aX|bc");
        expect("ab|c", "AX", "abcX|");
        // `o` opens below and inserts.
        expect("|ab\ncd", "oX", "ab\nX|\ncd");
        // `O` opens above.
        expect("ab\n|cd", "OX", "ab\nX|\ncd");
    }

    #[test]
    fn insert_typing_and_backspace() {
        expect("|abc", "iXY", "XY|abc");
        let (_e, text, _) = drive(
            "|abc",
            &[ViKey::Char('i'), ViKey::Char('X'), ViKey::Backspace],
        );
        assert_eq!(text, "|abc");
    }

    #[test]
    fn substitute_char_enters_insert() {
        let (engine, text, _) = drive("|hello", &[ViKey::Char('s')]);
        assert_eq!(engine.mode(), ViMode::Insert);
        assert_eq!(text, "|ello");
    }

    // --- Pending state -----------------------------------------------------

    #[test]
    fn pending_state_is_visible_mid_sequence() {
        let mut engine = ViEngine::new();
        engine.on_key(ViKey::Char('2'), "hello", 0);
        assert!(engine.is_pending());
        engine.on_key(ViKey::Char('d'), "hello", 0);
        assert!(engine.is_pending());
        engine.on_key(ViKey::Char('w'), "hello world", 0);
        assert!(!engine.is_pending());
    }

    // --- Undo / redo intents ----------------------------------------------

    #[test]
    fn undo_and_redo_are_reported_not_performed() {
        let (_e, text, action) = drive("|hello", &chars("u"));
        assert_eq!(action, ViAction::Undo);
        assert_eq!(text, "|hello"); // the engine changed nothing itself
        let (_e2, _t2, redo) = drive("|hello", &[ViKey::Ctrl('r')]);
        assert_eq!(redo, ViAction::Redo);
    }

    #[test]
    fn search_keys_are_reported_as_intents() {
        for (key, intent) in [
            ('n', SearchIntent::Next),
            ('N', SearchIntent::Previous),
            ('*', SearchIntent::WordForward),
            ('#', SearchIntent::WordBackward),
            ('/', SearchIntent::PromptForward),
            ('?', SearchIntent::PromptBackward),
        ] {
            let (_e, _t, action) = drive("|hello", &[ViKey::Char(key)]);
            assert_eq!(action, ViAction::Search(intent), "key {key}");
        }
    }

    // --- Refusals ----------------------------------------------------------

    #[track_caller]
    fn assert_refused(keys: &[ViKey]) {
        let (_engine, text, action) = drive("|hello world", keys);
        match action {
            ViAction::Refused(_) => {}
            other => panic!("expected refusal, got {other:?}"),
        }
        // The refusal must have committed nothing: the text is untouched.
        assert_eq!(text, "|hello world");
    }

    #[test]
    fn unsupported_commands_refuse_with_no_edit() {
        assert_refused(&[ViKey::Char('"')]); // named register
        assert_refused(&[ViKey::Ctrl('v')]); // blockwise visual
        assert_refused(&[ViKey::Char('m')]); // set mark
        assert_refused(&[ViKey::Char('`')]); // jump to mark
        assert_refused(&[ViKey::Char('\'')]); // jump to mark line
        assert_refused(&[ViKey::Char('q')]); // record macro
        assert_refused(&[ViKey::Char('@')]); // replay macro
    }

    #[test]
    fn refusal_message_is_user_readable() {
        let (_e, _t, action) = drive("|x", &[ViKey::Char('"')]);
        if let ViAction::Refused(error) = action {
            assert!(!error.headline().is_empty());
            assert!(!error.detail().is_empty());
        } else {
            panic!("expected refusal");
        }
    }

    // --- Repeat ------------------------------------------------------------

    #[test]
    fn dot_repeats_a_simple_delete() {
        expect("|abcdef", "x.", "|cdef");
        expect("|one two three", "dw.", "|three");
    }

    #[test]
    fn dot_repeats_a_change_with_its_count() {
        expect("|abcdefgh", "3x.", "|gh");
    }

    #[test]
    fn dot_repeats_an_insert() {
        // `iX<Esc>` inserts an X; `.` inserts another at the new caret.
        let keys = [
            ViKey::Char('i'),
            ViKey::Char('X'),
            ViKey::Escape,
            ViKey::Char('.'),
        ];
        let (_e, text, _) = drive("|abc", &keys);
        assert_eq!(text, "|XXabc");
    }

    #[test]
    fn dot_without_a_prior_change_does_nothing() {
        let (_e, text, action) = drive("|abc", &[ViKey::Char('.')]);
        assert_eq!(action, ViAction::None);
        assert_eq!(text, "|abc");
    }
}
