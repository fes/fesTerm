//! AppKit integration kept outside the cross-platform application crate.

/// Semantic application command emitted by the native macOS menu. The app
/// translates these through the same command paths used by chrome, shortcuts,
/// and the command palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMenuCommand {
    NewSession,
    StartLocalShell,
    OpenSettings,
    CloseActiveSurface,
    ToggleCommandPalette,
    ToggleSessionInspector,
}

#[cfg(target_os = "macos")]
mod menu {
    use std::sync::{mpsc, Arc};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

    use super::NativeMenuCommand;

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
            #[unsafe(method(newSession:))]
            fn new_session(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::NewSession);
            }

            #[unsafe(method(startLocalShell:))]
            fn start_local_shell(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::StartLocalShell);
            }

            #[unsafe(method(openSettings:))]
            fn open_settings(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::OpenSettings);
            }

            #[unsafe(method(closeActiveSurface:))]
            fn close_active_surface(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::CloseActiveSurface);
            }

            #[unsafe(method(toggleCommandPalette:))]
            fn toggle_command_palette(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::ToggleCommandPalette);
            }

            #[unsafe(method(toggleSessionInspector:))]
            fn toggle_session_inspector(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuCommand::ToggleSessionInspector);
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

        fn emit(&self, command: NativeMenuCommand) {
            let _ = self.ivars().sender.send(command);
            (self.ivars().wake)();
        }
    }

    pub struct NativeMenu {
        receiver: Option<mpsc::Receiver<NativeMenuCommand>>,
        // NSMenuItem targets are weak; retain the target for the menu lifetime.
        _target: Option<Retained<MenuTarget>>,
        close_item: Option<Retained<NSMenuItem>>,
        inspector_item: Option<Retained<NSMenuItem>>,
    }

    impl NativeMenu {
        pub fn unavailable() -> Self {
            Self {
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
            if let Some(close_item) = &self.close_item {
                close_item.setTitle(&NSString::from_str(close_label));
            }
            if let Some(inspector_item) = &self.inspector_item {
                inspector_item.setEnabled(inspector_enabled);
                inspector_item.setTitle(&NSString::from_str(if inspector_open {
                    "Hide Session Inspector"
                } else {
                    "Show Session Inspector"
                }));
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
            "Start Local Shell",
            "",
            NSEventModifierFlags::empty(),
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
        edit.addItem(&responder_item(mtm, "Paste", "v", sel!(paste:)));

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
}

#[cfg(target_os = "macos")]
use std::ptr::NonNull;

/// Vertically places macOS's standard traffic lights so their center sits
/// `band_center_from_top` points below the window's top edge, matching
/// fesTerm's integrated chrome band (`festerm_ui_egui::chrome::
/// chrome_band_center_from_top`). The view pointer originates from winit's
/// AppKit window handle.
///
/// This computes an absolute position from the window's own current height
/// rather than nudging AppKit's default placement by a fixed empirical
/// delta: an assumed default titlebar height can drift across macOS
/// versions, and a fixed one-time delta would go stale the moment the chip
/// row's height itself becomes runtime-configurable. Callers are expected
/// to call this every frame (safe/idempotent; it only assigns the exact
/// target position) so a future runtime chip-height change is picked up
/// automatically with no further wiring.
#[cfg(target_os = "macos")]
pub fn offset_traffic_lights(ns_view: NonNull<std::ffi::c_void>, band_center_from_top: f64) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use objc2_foundation::NSPoint;

    // SAFETY: winit supplies a live NSView pointer for the root window
    // handle; this function runs on the main thread while that window is
    // alive.
    let ns_view = unsafe { ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = ns_view.window() else {
        return;
    };

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
        let target_origin_y = superview_height - band_center_from_top - frame.size.height / 2.0;
        if (frame.origin.y - target_origin_y).abs() > f64::EPSILON {
            button.setFrameOrigin(NSPoint::new(frame.origin.x, target_origin_y));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn offset_traffic_lights(_: (), _: f64) {}
