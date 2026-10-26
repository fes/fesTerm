use raw_window_handle::RawDisplayHandle;

/// Handles interfacing with the OS clipboard.
///
/// If the "clipboard" feature is off, or we cannot connect to the OS clipboard,
/// then a fallback clipboard that just works within the same app is used instead.
pub struct Clipboard {
    #[cfg(all(
        not(any(target_os = "android", target_os = "ios")),
        feature = "arboard",
    ))]
    arboard: Option<arboard::Clipboard>,

    #[cfg(all(
        any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ),
        feature = "smithay-clipboard"
    ))]
    smithay: Option<smithay_clipboard::Clipboard>,

    /// Fallback manual clipboard.
    clipboard: String,

    /// Never treat an X11 clipboard connection as native Wayland support.
    is_wayland: bool,
}

impl Clipboard {
    /// Construct a new instance
    pub fn new(_raw_display_handle: Option<RawDisplayHandle>) -> Self {
        Self {
            #[cfg(all(
                not(any(target_os = "android", target_os = "ios")),
                feature = "arboard",
            ))]
            arboard: init_arboard(),

            #[cfg(all(
                any(
                    target_os = "linux",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd"
                ),
                feature = "smithay-clipboard"
            ))]
            smithay: init_smithay_clipboard(_raw_display_handle),

            clipboard: Default::default(),
            is_wayland: matches!(_raw_display_handle, Some(RawDisplayHandle::Wayland(_))),
        }
    }

    pub fn get(&mut self) -> Option<String> {
        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ),
            feature = "smithay-clipboard"
        ))]
        if let Some(clipboard) = &mut self.smithay {
            match clipboard.load() {
                Ok(text) => return Some(text),
                Err(err) => {
                    log::error!("smithay paste error: {err}");
                }
            }
        }

        #[cfg(all(
            not(any(target_os = "android", target_os = "ios")),
            feature = "arboard",
        ))]
        if let Some(clipboard) = &mut self.arboard {
            return match clipboard.get_text() {
                Ok(text) => Some(text),
                Err(err) => {
                    log::error!("arboard paste error: {err}");
                    None
                }
            };
        }

        Some(self.clipboard.clone())
    }

    pub fn set_text(&mut self, text: String) {
        #[cfg(target_os = "windows")]
        let text = normalize_windows_clipboard_text(text);

        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ),
            feature = "smithay-clipboard"
        ))]
        if let Some(clipboard) = &mut self.smithay {
            clipboard.store(text);
            return;
        }

        #[cfg(all(
            not(any(target_os = "android", target_os = "ios")),
            feature = "arboard",
        ))]
        if let Some(clipboard) = &mut self.arboard {
            if let Err(err) = clipboard.set_text(text) {
                log::error!("arboard copy/cut error: {err}");
            }
            return;
        }

        self.clipboard = text;
    }

    /// Copy HTML with a plain-text alternative, degrading to text when unsupported.
    pub fn set_html(&mut self, html: String, text: String) {
        #[cfg(target_os = "windows")]
        let text = normalize_windows_clipboard_text(text);

        #[cfg(all(
            any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            ),
            feature = "smithay-clipboard"
        ))]
        if let Some(clipboard) = &mut self.smithay {
            log::warn!(
                "HTML clipboard format is unsupported by smithay-clipboard; publishing plain text only"
            );
            // Smithay exposes an asynchronous text-only store with no error result.
            clipboard.store(text);
            return;
        }

        if self.is_wayland {
            log::error!(
                "No native Wayland clipboard backend is available; HTML and plain text were not published to the system clipboard"
            );
            self.clipboard = text;
            return;
        }

        #[cfg(all(
            not(any(target_os = "android", target_os = "ios")),
            feature = "arboard",
        ))]
        if let Some(clipboard) = &mut self.arboard {
            if let Err(err) =
                write_html_with_plain_fallback(&html, &text, |html, text| match html {
                    Some(html) => clipboard.set_html(html, Some(text)),
                    None => clipboard.set_text(text),
                })
            {
                log::error!("arboard plain-text fallback copy failed: {err}");
            }
            return;
        }

        log::warn!(
            "HTML clipboard format is unsupported without a native clipboard backend; retaining plain text in the application clipboard only"
        );
        self.clipboard = text;
        _ = html;
    }

    pub fn set_image(&mut self, image: &egui::ColorImage) {
        #[cfg(all(
            not(any(target_os = "android", target_os = "ios")),
            feature = "arboard",
        ))]
        if let Some(clipboard) = &mut self.arboard {
            if let Err(err) = clipboard.set_image(arboard::ImageData {
                width: image.width(),
                height: image.height(),
                bytes: std::borrow::Cow::Borrowed(bytemuck::cast_slice(&image.pixels)),
            }) {
                log::error!("arboard copy/cut error: {err}");
            }
            log::debug!("Copied image to clipboard");
            return;
        }

        log::error!(
            "Copying images is not supported. Enable the 'clipboard' feature of `egui-winit` to enable it."
        );
        _ = image;
    }
}

#[cfg(any(
    test,
    all(
        not(any(target_os = "android", target_os = "ios")),
        feature = "arboard",
    )
))]
fn write_html_with_plain_fallback<E>(
    html: &str,
    text: &str,
    mut write: impl FnMut(Option<&str>, &str) -> Result<(), E>,
) -> Result<(), E> {
    if write(Some(html), text).is_ok() {
        return Ok(());
    }
    log::warn!("HTML clipboard copy failed; attempting plain-text fallback");
    write(None, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disconnected_clipboard(is_wayland: bool) -> Clipboard {
        Clipboard {
            #[cfg(all(
                not(any(target_os = "android", target_os = "ios")),
                feature = "arboard",
            ))]
            arboard: None,
            #[cfg(all(
                any(
                    target_os = "linux",
                    target_os = "dragonfly",
                    target_os = "freebsd",
                    target_os = "netbsd",
                    target_os = "openbsd"
                ),
                feature = "smithay-clipboard"
            ))]
            smithay: None,
            clipboard: String::new(),
            is_wayland,
        }
    }

    #[test]
    fn html_copy_without_native_backend_retains_plain_text() {
        let mut clipboard = disconnected_clipboard(false);
        clipboard.set_html("<b>hello</b>".to_owned(), "hello".to_owned());
        assert_eq!(clipboard.get().as_deref(), Some("hello"));
        clipboard.set_text("existing text behavior".to_owned());
        assert_eq!(clipboard.get().as_deref(), Some("existing text behavior"));
    }

    #[test]
    fn wayland_html_copy_without_smithay_retains_plain_text() {
        let mut clipboard = disconnected_clipboard(true);
        clipboard.set_html("<b>hello</b>".to_owned(), "hello".to_owned());
        assert_eq!(clipboard.get().as_deref(), Some("hello"));
    }

    #[test]
    fn html_copy_publishes_both_formats_in_one_write() {
        let mut writes = Vec::new();
        let result = write_html_with_plain_fallback("<b>hello</b>", "hello", |html, text| {
            writes.push((html.map(str::to_owned), text.to_owned()));
            Ok::<_, ()>(())
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            writes,
            vec![(Some("<b>hello</b>".to_owned()), "hello".to_owned())]
        );
    }

    #[test]
    fn failed_html_copy_retries_plain_text() {
        let mut writes = Vec::new();
        let result = write_html_with_plain_fallback("<b>hello</b>", "hello", |html, text| {
            writes.push((html.map(str::to_owned), text.to_owned()));
            if html.is_some() { Err(()) } else { Ok(()) }
        });
        assert_eq!(result, Ok(()));
        assert_eq!(
            writes,
            vec![
                (Some("<b>hello</b>".to_owned()), "hello".to_owned()),
                (None, "hello".to_owned())
            ]
        );
    }

    #[test]
    fn failed_plain_text_fallback_returns_error() {
        let mut writes = 0;
        let result = write_html_with_plain_fallback("<b>hello</b>", "hello", |_, _| {
            writes += 1;
            Err(writes)
        });
        assert_eq!(result, Err(2));
        assert_eq!(writes, 2);
    }
}

/// Normalizes text for Windows `CF_UNICODETEXT`, which requires CRLF line endings.
#[doc(hidden)]
pub fn normalize_windows_clipboard_text(text: String) -> String {
    if !text.contains('\r') && !text.contains('\n') {
        return text;
    }

    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push_str("\r\n");
            }
            '\n' => normalized.push_str("\r\n"),
            _ => normalized.push(character),
        }
    }
    normalized
}

#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    feature = "arboard",
))]
fn init_arboard() -> Option<arboard::Clipboard> {
    profiling::function_scope!();

    log::trace!("Initializing arboard clipboard…");
    match arboard::Clipboard::new() {
        Ok(clipboard) => Some(clipboard),
        Err(err) => {
            log::warn!("Failed to initialize arboard clipboard: {err}");
            None
        }
    }
}

#[cfg(all(
    any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ),
    feature = "smithay-clipboard"
))]
fn init_smithay_clipboard(
    raw_display_handle: Option<RawDisplayHandle>,
) -> Option<smithay_clipboard::Clipboard> {
    #![expect(clippy::undocumented_unsafe_blocks)]

    profiling::function_scope!();

    if let Some(RawDisplayHandle::Wayland(display)) = raw_display_handle {
        log::trace!("Initializing smithay clipboard…");
        #[expect(unsafe_code)]
        Some(unsafe { smithay_clipboard::Clipboard::new(display.display.as_ptr()) })
    } else {
        #[cfg(feature = "wayland")]
        log::debug!("Cannot init smithay clipboard without a Wayland display handle");
        #[cfg(not(feature = "wayland"))]
        log::debug!(
            "Cannot init smithay clipboard: the 'wayland' feature of 'egui-winit' is not enabled"
        );
        None
    }
}
