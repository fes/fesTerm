/// Visual cursor shape requested by DECSCUSR.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorStyle {
    #[default]
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

/// Mouse tracking mode requested by the terminal application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseTrackingMode {
    #[default]
    None,
    /// DECSET `?9`: button presses only.
    X10,
    /// DECSET `?1000`: button presses and releases.
    ButtonEvent,
    /// DECSET `?1002`: button events and motion while a button is held.
    ButtonMotion,
    /// DECSET `?1003`: all pointer motion.
    AnyMotion,
}

/// Terminal modes implemented through Milestone 3.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalModes {
    pub(crate) auto_wrap: bool,
    pub(crate) origin_mode: bool,
    pub(crate) alternate_screen: bool,
    pub(crate) cursor_visible: bool,
    pub(crate) application_cursor: bool,
    pub(crate) application_keypad: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) focus_reporting: bool,
    pub(crate) mouse_tracking: MouseTrackingMode,
    pub(crate) sgr_mouse: bool,
}

impl TerminalModes {
    pub const fn auto_wrap(self) -> bool {
        self.auto_wrap
    }

    pub const fn origin_mode(self) -> bool {
        self.origin_mode
    }

    pub const fn alternate_screen(self) -> bool {
        self.alternate_screen
    }

    pub const fn cursor_visible(self) -> bool {
        self.cursor_visible
    }

    pub const fn application_cursor(self) -> bool {
        self.application_cursor
    }

    pub const fn application_keypad(self) -> bool {
        self.application_keypad
    }

    pub const fn bracketed_paste(self) -> bool {
        self.bracketed_paste
    }

    pub const fn focus_reporting(self) -> bool {
        self.focus_reporting
    }

    pub const fn mouse_tracking(self) -> MouseTrackingMode {
        self.mouse_tracking
    }

    pub const fn sgr_mouse(self) -> bool {
        self.sgr_mouse
    }
}

impl Default for TerminalModes {
    fn default() -> Self {
        Self {
            auto_wrap: true,
            origin_mode: false,
            alternate_screen: false,
            cursor_visible: true,
            application_cursor: false,
            application_keypad: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_tracking: MouseTrackingMode::None,
            sgr_mouse: false,
        }
    }
}
