//! Embeds fesTerm's icon into the Windows PE resources so Explorer,
//! the taskbar, and Alt-Tab show the real app icon for `festerm.exe`
//! instead of a generic executable icon. `eframe`'s runtime
//! `ViewportBuilder::with_icon` only sets the in-process window/taskbar
//! icon while the app is running; it has no effect on how Explorer
//! renders the file itself, which requires an icon resource compiled
//! into the binary.
fn main() {
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/app-icon/festerm.ico");
        if let Err(err) = resource.compile() {
            println!("cargo:warning=failed to embed Windows icon resource: {err}");
        }
    }
}
