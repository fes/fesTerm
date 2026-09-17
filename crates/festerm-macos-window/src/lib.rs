//! AppKit integration kept outside the cross-platform application crate.

/// Semantic application command emitted by the native macOS menu. The app
/// translates these through the same command paths used by chrome, shortcuts,
/// and the command palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMenuCommand {
    Paste,
    NewSession,
    NewWindow,
    StartLocalShell,
    OpenSettings,
    CloseActiveSurface,
    ToggleCommandPalette,
    ToggleSessionInspector,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
}

/// Accelerator metadata only; application policy remains in the composition root.
pub struct NativeShortcut {
    pub command: NativeMenuCommand,
    pub key: String,
    pub control: bool,
    pub command_modifier: bool,
    pub option: bool,
    pub shift: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeMenuAction {
    Paste,
    NewSession,
    NewWindow,
    StartLocalShell,
    OpenSettings,
    CloseActiveSurface,
    ToggleCommandPalette,
    ToggleSessionInspector,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
}

#[cfg(any(target_os = "macos", test))]
impl NativeMenuAction {
    const fn command(self) -> NativeMenuCommand {
        match self {
            Self::Paste => NativeMenuCommand::Paste,
            Self::NewSession => NativeMenuCommand::NewSession,
            Self::NewWindow => NativeMenuCommand::NewWindow,
            Self::StartLocalShell => NativeMenuCommand::StartLocalShell,
            Self::OpenSettings => NativeMenuCommand::OpenSettings,
            Self::CloseActiveSurface => NativeMenuCommand::CloseActiveSurface,
            Self::ToggleCommandPalette => NativeMenuCommand::ToggleCommandPalette,
            Self::ToggleSessionInspector => NativeMenuCommand::ToggleSessionInspector,
            Self::ClearTerminal => NativeMenuCommand::ClearTerminal,
            Self::ResetTerminal => NativeMenuCommand::ResetTerminal,
            Self::ToggleFocusMode => NativeMenuCommand::ToggleFocusMode,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Eq, PartialEq)]
struct NativeMenuState<'a> {
    close_label: &'a str,
    inspector_enabled: bool,
    inspector_label: &'static str,
}

#[cfg(any(target_os = "macos", test))]
const fn native_menu_state(
    close_label: &str,
    inspector_enabled: bool,
    inspector_open: bool,
) -> NativeMenuState<'_> {
    NativeMenuState {
        close_label,
        inspector_enabled,
        inspector_label: if inspector_open {
            "Hide Session Inspector"
        } else {
            "Show Session Inspector"
        },
    }
}

/// Observes macOS resume-from-sleep (ADR 0018: "resume from system sleep" is
/// the primary wake/network-change trigger for an on-demand SSH liveness
/// probe). Network-interface/route-change detection is deliberately out of
/// scope here; see issue #48 for that follow-up.
#[cfg(target_os = "macos")]
mod wake {
    use std::sync::Arc;

    use objc2::rc::Retained;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol};

    struct WakeObserverIvars {
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    define_class!(
        // SAFETY: NSObject imposes no additional subclassing invariants. The
        // observer is main-thread-only and its Rust ivars are dropped
        // normally.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = WakeObserverIvars]
        struct WakeObserver;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for WakeObserver {}

        impl WakeObserver {
            #[unsafe(method(didWake:))]
            fn did_wake(&self, _notification: Option<&NSNotification>) {
                (self.ivars().wake)();
            }
        }
    );

    impl WakeObserver {
        fn new(wake: Arc<dyn Fn() + Send + Sync>, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(WakeObserverIvars { wake });
            // SAFETY: NSObject's init signature is correct.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Registers `wake` to run once per resume-from-sleep for as long as the
    /// returned `WakeMonitor` stays alive; dropping it unregisters the
    /// observer.
    pub struct WakeMonitor {
        // The observer is retained for its NSNotificationCenter registration;
        // dropping it removes that registration in `Drop` below.
        observer: Option<Retained<WakeObserver>>,
    }

    impl WakeMonitor {
        pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
            let mtm =
                MainThreadMarker::new().expect("wake-monitor installation requires main thread");
            let observer = WakeObserver::new(wake, mtm);
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            // SAFETY: `observer` is a valid, retained NSObject subclass
            // implementing `didWake:`, and it outlives this registration
            // (removed in `Drop` before the observer is deallocated).
            unsafe {
                center.addObserver_selector_name_object(
                    &observer,
                    sel!(didWake:),
                    Some(objc2_app_kit::NSWorkspaceDidWakeNotification),
                    None,
                );
            }
            Self {
                observer: Some(observer),
            }
        }
    }

    impl Drop for WakeMonitor {
        fn drop(&mut self) {
            if let Some(observer) = self.observer.take() {
                let center = NSWorkspace::sharedWorkspace().notificationCenter();
                // SAFETY: `observer` was registered above and is still a
                // valid NSObject at this point.
                unsafe {
                    center.removeObserver(&observer);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use wake::WakeMonitor;

#[cfg(not(target_os = "macos"))]
pub struct WakeMonitor;

#[cfg(not(target_os = "macos"))]
impl WakeMonitor {
    pub fn install(_wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
mod menu {
    use std::sync::{mpsc, Arc};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

    use super::{native_menu_state, NativeMenuAction, NativeMenuCommand};

    struct MenuTargetIvars {
        sender: mpsc::Sender<NativeMenuCommand>,
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    define_class!(
        // SAFETY: NSObject imposes no additional subclassing invariants. The
        // target is main-thread-only and its Rust ivars are dropped normally.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = MenuTargetIvars]
        struct MenuTarget;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for MenuTarget {}

        impl MenuTarget {
            #[unsafe(method(pasteFromClipboard:))]
            fn paste_from_clipboard(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::Paste);
            }
            #[unsafe(method(newSession:))]
            fn new_session(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::NewSession);
            }

            #[unsafe(method(newWindow:))]
            fn new_window(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::NewWindow);
            }

            #[unsafe(method(startLocalShell:))]
            fn start_local_shell(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::StartLocalShell);
            }

            #[unsafe(method(openSettings:))]
            fn open_settings(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::OpenSettings);
            }

            #[unsafe(method(closeActiveSurface:))]
            fn close_active_surface(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::CloseActiveSurface);
            }

            #[unsafe(method(toggleCommandPalette:))]
            fn toggle_command_palette(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleCommandPalette);
            }

            #[unsafe(method(toggleSessionInspector:))]
            fn toggle_session_inspector(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleSessionInspector);
            }

            #[unsafe(method(clearTerminal:))]
            fn clear_terminal(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ClearTerminal);
            }

            #[unsafe(method(resetTerminal:))]
            fn reset_terminal(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ResetTerminal);
            }

            #[unsafe(method(toggleFocusMode:))]
            fn toggle_focus_mode(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleFocusMode);
            }
        }
    );

    impl MenuTarget {
        fn new(
            sender: mpsc::Sender<NativeMenuCommand>,
            wake: Arc<dyn Fn() + Send + Sync>,
            mtm: MainThreadMarker,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { sender, wake });
            // SAFETY: NSObject's init signature is correct.
            unsafe { msg_send![super(this), init] }
        }

        fn emit(&self, action: NativeMenuAction) {
            let mtm = MainThreadMarker::new().expect("native menu callback is main-thread-only");
            if NSApplication::sharedApplication(mtm)
                .currentEvent()
                .is_some_and(|event| {
                    event.r#type() == objc2_app_kit::NSEventType::KeyDown && event.isARepeat()
                })
            {
                return;
            }
            let _ = self.ivars().sender.send(action.command());
            (self.ivars().wake)();
        }
    }

    pub struct NativeMenu {
        main: Option<Retained<NSMenu>>,
        receiver: Option<mpsc::Receiver<NativeMenuCommand>>,
        // NSMenuItem targets are weak; retain the target for the menu lifetime.
        _target: Option<Retained<MenuTarget>>,
        close_item: Option<Retained<NSMenuItem>>,
        inspector_item: Option<Retained<NSMenuItem>>,
    }

    impl NativeMenu {
        pub fn unavailable() -> Self {
            Self {
                main: None,
                receiver: None,
                _target: None,
                close_item: None,
                inspector_item: None,
            }
        }

        pub fn try_recv(&self) -> Option<NativeMenuCommand> {
            self.receiver
                .as_ref()
                .and_then(|receiver| receiver.try_recv().ok())
        }

        pub fn update(&self, close_label: &str, inspector_enabled: bool, inspector_open: bool) {
            let state = native_menu_state(close_label, inspector_enabled, inspector_open);
            if let Some(close_item) = &self.close_item {
                close_item.setTitle(&NSString::from_str(state.close_label));
            }
            if let Some(inspector_item) = &self.inspector_item {
                inspector_item.setEnabled(state.inspector_enabled);
                inspector_item.setTitle(&NSString::from_str(state.inspector_label));
            }
        }

        pub fn update_shortcuts(&self, shortcuts: &[super::NativeShortcut]) {
            let Some(main) = &self.main else { return };
            for root in main.itemArray() {
                let Some(menu) = root.submenu() else { continue };
                for item in menu.itemArray() {
                    let Some(selector) = item.action() else {
                        continue;
                    };
                    let command = if selector == sel!(newSession:) {
                        Some(NativeMenuCommand::NewSession)
                    } else if selector == sel!(newWindow:) {
                        Some(NativeMenuCommand::NewWindow)
                    } else if selector == sel!(startLocalShell:) {
                        Some(NativeMenuCommand::StartLocalShell)
                    } else if selector == sel!(openSettings:) {
                        Some(NativeMenuCommand::OpenSettings)
                    } else if selector == sel!(closeActiveSurface:) {
                        Some(NativeMenuCommand::CloseActiveSurface)
                    } else if selector == sel!(toggleCommandPalette:) {
                        Some(NativeMenuCommand::ToggleCommandPalette)
                    } else if selector == sel!(clearTerminal:) {
                        Some(NativeMenuCommand::ClearTerminal)
                    } else if selector == sel!(resetTerminal:) {
                        Some(NativeMenuCommand::ResetTerminal)
                    } else if selector == sel!(toggleFocusMode:) {
                        Some(NativeMenuCommand::ToggleFocusMode)
                    } else {
                        None
                    };
                    if let Some(command) = command {
                        let binding = shortcuts.iter().find(|binding| binding.command == command);
                        item.setKeyEquivalent(&NSString::from_str(
                            binding.map_or("", |binding| binding.key.as_str()),
                        ));
                        let mut flags = NSEventModifierFlags::empty();
                        if let Some(binding) = binding {
                            if binding.control {
                                flags |= NSEventModifierFlags::Control;
                            }
                            if binding.command_modifier {
                                flags |= NSEventModifierFlags::Command;
                            }
                            if binding.option {
                                flags |= NSEventModifierFlags::Option;
                            }
                            if binding.shift {
                                flags |= NSEventModifierFlags::Shift;
                            }
                        }
                        item.setKeyEquivalentModifierMask(flags);
                    }
                    // Keyboard clipboard is routed by the application with
                    // native-event provenance. Menu clicks retain responder intent.
                    if selector == sel!(copy:) || selector == sel!(paste:) {
                        item.setKeyEquivalent(&NSString::from_str(""));
                    }
                }
            }
        }
    }

    pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> NativeMenu {
        let mtm = MainThreadMarker::new().expect("AppKit menu installation requires main thread");
        let (sender, receiver) = mpsc::channel();
        let target = MenuTarget::new(sender, wake, mtm);
        let app = NSApplication::sharedApplication(mtm);

        let main = menu(mtm, "Main");
        let app_menu = menu(mtm, "fesTerm");
        let app_root = submenu_root(mtm, "fesTerm", &app_menu);
        main.addItem(&app_root);

        app_menu.addItem(&custom_item(
            mtm,
            "Settings…",
            ",",
            NSEventModifierFlags::Command,
            sel!(openSettings:),
            &target,
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        let services = menu(mtm, "Services");
        let services_item = submenu_root(mtm, "Services", &services);
        app_menu.addItem(&services_item);
        app.setServicesMenu(Some(&services));
        app_menu.addItem(&responder_item(mtm, "Hide fesTerm", "h", sel!(hide:)));
        let hide_others = responder_item(mtm, "Hide Others", "h", sel!(hideOtherApplications:));
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        app_menu.addItem(&hide_others);
        app_menu.addItem(&responder_item(
            mtm,
            "Show All",
            "",
            sel!(unhideAllApplications:),
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&responder_item(mtm, "Quit fesTerm", "q", sel!(terminate:)));

        let file = menu(mtm, "File");
        main.addItem(&submenu_root(mtm, "File", &file));
        file.addItem(&custom_item(
            mtm,
            "New Session…",
            "t",
            NSEventModifierFlags::Command,
            sel!(newSession:),
            &target,
        ));
        file.addItem(&custom_item(
            mtm,
            "New Window",
            "n",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(newWindow:),
            &target,
        ));
        file.addItem(&custom_item(
            mtm,
            "Start Local Shell",
            "n",
            NSEventModifierFlags::Command,
            sel!(startLocalShell:),
            &target,
        ));
        file.addItem(&NSMenuItem::separatorItem(mtm));
        let close_item = custom_item(
            mtm,
            "Close Session",
            "w",
            NSEventModifierFlags::Command,
            sel!(closeActiveSurface:),
            &target,
        );
        file.addItem(&close_item);
        let close_window = responder_item(mtm, "Close Window", "w", sel!(performClose:));
        close_window.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        );
        file.addItem(&close_window);

        let edit = menu(mtm, "Edit");
        main.addItem(&submenu_root(mtm, "Edit", &edit));
        edit.addItem(&responder_item(mtm, "Copy", "c", sel!(copy:)));
        edit.addItem(&custom_item(
            mtm,
            "Paste",
            "",
            NSEventModifierFlags::empty(),
            sel!(pasteFromClipboard:),
            &target,
        ));
        edit.addItem(&NSMenuItem::separatorItem(mtm));
        edit.addItem(&custom_item(
            mtm,
            "Clear Terminal",
            "k",
            NSEventModifierFlags::Command,
            sel!(clearTerminal:),
            &target,
        ));

        let shell = menu(mtm, "Shell");
        main.addItem(&submenu_root(mtm, "Shell", &shell));
        shell.addItem(&custom_item(
            mtm,
            "Reset Terminal",
            "r",
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
            sel!(resetTerminal:),
            &target,
        ));

        let view = menu(mtm, "View");
        main.addItem(&submenu_root(mtm, "View", &view));
        let palette = custom_item(
            mtm,
            "Command Palette…",
            "p",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(toggleCommandPalette:),
            &target,
        );
        view.addItem(&palette);
        view.addItem(&custom_item(
            mtm,
            "Toggle Focus Mode",
            "f",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(toggleFocusMode:),
            &target,
        ));
        let inspector_item = custom_item(
            mtm,
            "Show Session Inspector",
            "",
            NSEventModifierFlags::empty(),
            sel!(toggleSessionInspector:),
            &target,
        );
        view.addItem(&inspector_item);

        let window = menu(mtm, "Window");
        main.addItem(&submenu_root(mtm, "Window", &window));
        window.addItem(&responder_item(
            mtm,
            "Minimize",
            "m",
            sel!(performMiniaturize:),
        ));
        window.addItem(&responder_item(mtm, "Zoom", "", sel!(performZoom:)));
        app.setWindowsMenu(Some(&window));

        app.setMainMenu(Some(&main));
        NativeMenu {
            main: Some(main),
            receiver: Some(receiver),
            _target: Some(target),
            close_item: Some(close_item),
            inspector_item: Some(inspector_item),
        }
    }

    fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
        NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title))
    }

    fn submenu_root(mtm: MainThreadMarker, title: &str, submenu: &NSMenu) -> Retained<NSMenuItem> {
        let item = responder_item(mtm, title, "", sel!(noop:));
        item.setSubmenu(Some(submenu));
        item
    }

    fn responder_item(
        mtm: MainThreadMarker,
        title: &str,
        key: &str,
        selector: objc2::runtime::Sel,
    ) -> Retained<NSMenuItem> {
        // SAFETY: selectors are compile-time AppKit responder selectors.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(selector),
                &NSString::from_str(key),
            )
        }
    }

    fn custom_item(
        mtm: MainThreadMarker,
        title: &str,
        key: &str,
        modifiers: NSEventModifierFlags,
        selector: objc2::runtime::Sel,
        target: &MenuTarget,
    ) -> Retained<NSMenuItem> {
        let item = responder_item(mtm, title, key, selector);
        item.setKeyEquivalentModifierMask(modifiers);
        // SAFETY: target implements every selector supplied to this helper and
        // is retained by NativeMenu for at least as long as the item.
        unsafe { item.setTarget(Some(target)) };
        item
    }
}

#[cfg(target_os = "macos")]
pub use menu::{install as install_application_menu, NativeMenu};

#[cfg(not(target_os = "macos"))]
pub struct NativeMenu;

#[cfg(not(target_os = "macos"))]
pub fn install_application_menu(_: std::sync::Arc<dyn Fn() + Send + Sync>) -> NativeMenu {
    NativeMenu
}

#[cfg(not(target_os = "macos"))]
impl NativeMenu {
    pub const fn unavailable() -> Self {
        Self
    }

    pub const fn try_recv(&self) -> Option<NativeMenuCommand> {
        None
    }

    pub fn update(&self, _: &str, _: bool, _: bool) {}
    pub fn update_shortcuts(&self, _: &[NativeShortcut]) {}
}

#[cfg(target_os = "macos")]
use std::ptr::NonNull;

#[cfg(any(target_os = "macos", test))]
fn traffic_light_origin_y(
    superview_height: f64,
    band_center_from_top: f64,
    button_height: f64,
) -> f64 {
    superview_height - band_center_from_top - button_height / 2.0
}

/// Applies fesTerm's native window chrome to *every* window the process
/// owns: the traffic lights are aligned with the chip band, and AppKit's
/// own window dragging is disabled.
///
/// Every window is swept rather than one addressed by handle because only
/// eframe's root viewport exposes a `raw-window-handle`; the additional
/// windows fesTerm opens (ADR 0033) are egui viewports with no handle of
/// their own, and leaving them out left AppKit dragging those windows
/// whenever a tab chip in them was dragged - which made moving tabs out of
/// any window but the first impossible. Windows without traffic lights
/// (the tab-drag ghost) simply have nothing to align.
///
/// Callers are expected to call this every frame: it is idempotent (it
/// only ever assigns the exact target position, and only disables movement
/// that is still enabled) and windows opened later need the same treatment
/// as soon as they exist.
#[cfg(target_os = "macos")]
pub fn sync_window_chrome(band_center_from_top: f64) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(main_thread);
    for window in application.windows() {
        offset_traffic_lights(&window, band_center_from_top);
        disable_native_window_movement(&window);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn sync_window_chrome(_: f64) {}

/// Vertically places macOS's standard traffic lights so their center sits
/// `band_center_from_top` points below the window's top edge, matching
/// fesTerm's integrated chrome band (`festerm_ui_egui::chrome::
/// chrome_band_center_from_top`).
///
/// This computes an absolute position from the window's own current height
/// rather than nudging AppKit's default placement by a fixed empirical
/// delta: an assumed default titlebar height can drift across macOS
/// versions, and a fixed one-time delta would go stale the moment the chip
/// row's height itself becomes runtime-configurable.
#[cfg(target_os = "macos")]
fn offset_traffic_lights(ns_window: &objc2_app_kit::NSWindow, band_center_from_top: f64) {
    use objc2_app_kit::NSWindowButton;
    use objc2_foundation::NSPoint;

    for button_kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let Some(button) = ns_window.standardWindowButton(button_kind) else {
            continue;
        };
        // The button's frame is relative to its immediate superview (a
        // small titlebar-container view living in the top-right corner of
        // the window chrome), not the window's own frame, so the "distance
        // from the top edge" must be computed against that superview's
        // height, not `ns_window.frame().size.height`.
        let Some(superview) = (unsafe { button.superview() }) else {
            continue;
        };
        let superview_height = superview.bounds().size.height;
        let frame = button.frame();
        // AppKit's window coordinate space has a bottom-left origin;
        // convert the desired distance from the top edge into that space.
        let target_origin_y =
            traffic_light_origin_y(superview_height, band_center_from_top, frame.size.height);
        if (frame.origin.y - target_origin_y).abs() > f64::EPSILON {
            button.setFrameOrigin(NSPoint::new(frame.origin.x, target_origin_y));
        }
    }
}

/// Disables AppKit's own default window-dragging behavior entirely (both
/// from the native title bar and from clicking the window background),
/// leaving 100% of window movement under fesTerm's own explicit
/// `egui::ViewportCommand::StartDrag` calls
/// (`festerm_ui_egui::chrome::show`'s row-level drag region).
///
/// This matters because `with_decorations(true)` (kept so the native
/// traffic lights keep working) plus `with_fullsize_content_view(true)`
/// still leaves a real, if visually blank, native title-bar strip across
/// the *entire* top of the window - AppKit drags the window from a
/// press-drag anywhere in that strip by default, before the event ever
/// reaches egui's own hit-testing. Since fesTerm's chip row paints its
/// title text inside that same strip, a drag started on a chip's title
/// used to move the whole window instead of reordering the chip - or
/// dragging it to another window - regardless of how egui's own widgets
/// resolved the same gesture. Disabling native movement removes that
/// OS-level shortcut entirely, so only the explicit drag regions fesTerm
/// itself defines can ever move the window.
#[cfg(target_os = "macos")]
fn disable_native_window_movement(ns_window: &objc2_app_kit::NSWindow) {
    if ns_window.isMovable() {
        ns_window.setMovable(false);
    }
}

/// Forces winit's own content `NSView` back to first responder so key events
/// resume being delivered after the OS window regains key/main status.
///
/// This works around a real AppKit gap (winit itself hits the same issue
/// internally - see its `set_style_mask`'s "If we don't do this, key
/// handling will break" comment): `windowDidBecomeKey`/egui's own
/// `WindowFocused(true)` fire reliably whenever the *window* becomes key
/// again, but AppKit does not always restore first-responder status to the
/// content view as part of that - most notably when the activating click
/// lands in the blank native title-bar strip above fesTerm's own chip row
/// (`disable_native_window_movement`'s doc comment explains why that strip
/// exists) rather than on any interactive view. In that case the window is
/// key, egui's own focus bookkeeping still thinks the terminal is focused,
/// but no keyDown ever reaches the app until the user separately clicks
/// inside real view content. Explicitly reclaiming first-responder status on
/// every regained-focus event closes that gap regardless of where the
/// activating click landed.
#[cfg(target_os = "macos")]
pub fn reclaim_first_responder(ns_view: NonNull<std::ffi::c_void>) {
    use objc2_app_kit::NSView;

    // SAFETY: winit supplies a live NSView pointer for the root window
    // handle; this function runs on the main thread while that window is
    // alive.
    let ns_view = unsafe { ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = ns_view.window() else {
        return;
    };
    let _ = ns_window.makeFirstResponder(Some(ns_view));
}

#[cfg(not(target_os = "macos"))]
pub fn reclaim_first_responder(_: ()) {}

#[cfg(test)]
mod tests {
    use super::{
        native_menu_state, traffic_light_origin_y, NativeMenuAction, NativeMenuCommand,
        NativeMenuState,
    };

    #[test]
    fn native_menu_actions_map_to_shared_commands() {
        let cases = [
            (NativeMenuAction::Paste, NativeMenuCommand::Paste),
            (NativeMenuAction::NewSession, NativeMenuCommand::NewSession),
            (
                NativeMenuAction::StartLocalShell,
                NativeMenuCommand::StartLocalShell,
            ),
            (
                NativeMenuAction::OpenSettings,
                NativeMenuCommand::OpenSettings,
            ),
            (
                NativeMenuAction::CloseActiveSurface,
                NativeMenuCommand::CloseActiveSurface,
            ),
            (
                NativeMenuAction::ToggleCommandPalette,
                NativeMenuCommand::ToggleCommandPalette,
            ),
            (
                NativeMenuAction::ToggleSessionInspector,
                NativeMenuCommand::ToggleSessionInspector,
            ),
            (
                NativeMenuAction::ClearTerminal,
                NativeMenuCommand::ClearTerminal,
            ),
            (
                NativeMenuAction::ResetTerminal,
                NativeMenuCommand::ResetTerminal,
            ),
            (
                NativeMenuAction::ToggleFocusMode,
                NativeMenuCommand::ToggleFocusMode,
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(action.command(), expected);
        }
    }

    #[test]
    fn native_menu_state_tracks_surface_and_inspector_context() {
        assert_eq!(
            native_menu_state("Close Settings", false, false),
            NativeMenuState {
                close_label: "Close Settings",
                inspector_enabled: false,
                inspector_label: "Show Session Inspector",
            }
        );
        assert_eq!(
            native_menu_state("Close Session", true, true),
            NativeMenuState {
                close_label: "Close Session",
                inspector_enabled: true,
                inspector_label: "Hide Session Inspector",
            }
        );
    }

    #[test]
    fn traffic_light_origin_centers_button_in_chrome_band() {
        assert_eq!(traffic_light_origin_y(40.0, 14.0, 12.0), 20.0);
        assert_eq!(traffic_light_origin_y(20.0, 24.0, 12.0), -10.0);
    }
}
