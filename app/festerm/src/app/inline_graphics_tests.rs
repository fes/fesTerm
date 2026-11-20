//! ADR 0089 cross-layer evidence: live session output travels through the
//! app pump, core, reply forwarding, and the egui renderer exactly as a
//! local PTY's would.

use super::*;
use crate::session_controller::fake::FakeSshSession;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use festerm_session::{SessionEvent, TerminalSize};

const DA1: &[u8] = b"\x1b[?6c";
const RED: &str = "/wAA/w==";
const GREEN: &str = "AP8A/w==";

fn run_frame(ui: &mut egui::Ui, app: &mut FesTermApp) {
    app.frame_logic(ui.ctx());
    app.ui_content(ui);
}

fn opted_in() -> Configuration {
    Configuration::empty()
        .with_interface_settings(InterfaceSettings::DEFAULT.with_inline_graphics(true))
        .unwrap()
}

fn apc(command: &str) -> Vec<u8> {
    format!("\x1b_G{command}\x1b\\").into_bytes()
}

struct Live {
    harness: Harness<'static, FesTermApp>,
    transport: FakeSshSession,
    tab: TabId,
}

impl Live {
    fn local(configuration: Configuration) -> Self {
        Self::place(configuration, AppState::place_test_local_session_with)
    }

    fn ssh(configuration: Configuration) -> Self {
        Self::place(configuration, AppState::place_test_ssh_session)
    }

    fn place(
        configuration: Configuration,
        place: impl FnOnce(&mut AppState, FakeSshSession) -> TabId,
    ) -> Self {
        Self::place_in(
            FesTermApp::for_test_with_configuration(configuration),
            place,
        )
    }

    fn place_in(
        mut app: FesTermApp,
        place: impl FnOnce(&mut AppState, FakeSshSession) -> TabId,
    ) -> Self {
        let transport = FakeSshSession::new([]);
        let tab = place(&mut app.state, transport.clone());
        let harness = Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .with_pixels_per_point(1.0)
            .build_ui_state(run_frame, app);
        Self {
            harness,
            transport,
            tab,
        }
    }

    /// An eligible local session whose PTY has applied the published pixels.
    fn graphics_ready() -> Self {
        Self::graphics_ready_in(FesTermApp::for_test_with_configuration(opted_in()))
    }

    fn graphics_ready_in(app: FesTermApp) -> Self {
        let mut live = Self::place_in(app, AppState::place_test_local_session_with);
        let size = live.requested();
        live.apply(size);
        live
    }

    fn session(&self) -> &crate::tabs::SessionTab {
        self.harness.state().state.session_tab(self.tab).unwrap()
    }

    /// The newest PTY size requested once the resize debounce has settled.
    fn requested(&mut self) -> TerminalSize {
        self.harness.run_steps(4);
        thread::sleep(festerm_ui_egui::TERMINAL_RESIZE_DEBOUNCE);
        self.harness.run_steps(2);
        self.session()
            .controller
            .resize_probe()
            .generations()
            .last()
            .unwrap()
            .dimensions
    }

    /// What a real PTY reports once `TIOCSWINSZ` has taken effect.
    fn apply(&mut self, size: TerminalSize) {
        let event = match self
            .transport
            .resize_requests()
            .into_iter()
            .rev()
            .find(|(requested, _)| *requested == size)
        {
            Some((size, Some(generation))) => {
                SessionEvent::ResizeAppliedGeneration { size, generation }
            }
            _ => SessionEvent::ResizeApplied(size),
        };
        self.transport.push_event(event);
        self.harness.run_steps(2);
    }

    fn sent(&self) -> Vec<u8> {
        self.transport.sent().concat()
    }

    /// Delivers one PTY read and returns everything the app wrote back once
    /// any off-thread PNG validation it started has been committed.
    fn output(&mut self, bytes: &[u8]) -> Vec<u8> {
        let before = self.sent().len();
        self.transport
            .push_event(SessionEvent::Output(bytes.to_vec()));
        self.harness.run_steps(4);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while self.session().terminal.kitty_validation_pending() {
            assert!(std::time::Instant::now() < deadline, "validation stalled");
            thread::sleep(Duration::from_millis(1));
            self.harness.run_steps(1);
        }
        self.harness.run_steps(2);
        self.sent()[before..].to_vec()
    }

    fn image_nodes(&self) -> usize {
        self.harness
            .query_all_by_role(egui::accesskit::Role::Image)
            .filter(|node| node.accesskit_node().label().as_deref() == Some("image"))
            .count()
    }

    fn kitty_textures(&self) -> usize {
        self.kitty_texture_ids().len()
    }

    fn kitty_texture_ids(&self) -> Vec<egui::TextureId> {
        self.harness
            .ctx
            .tex_manager()
            .read()
            .allocated()
            .filter(|(_, meta)| meta.name.starts_with("kitty-image-"))
            .map(|(id, _)| *id)
            .collect()
    }

    fn view_mut(&mut self) -> &mut festerm_ui_egui::TerminalView {
        let tab = self.tab;
        &mut self
            .harness
            .state_mut()
            .state
            .session_tab_mut(tab)
            .unwrap()
            .view
    }

    fn decodes(&self) -> usize {
        self.session().view.kitty_decode_count()
    }

    /// The point at `column` cells across the first grid row.
    fn at(&self, column: f32) -> egui::Pos2 {
        let diagnostics = self.session().view.diagnostics();
        let (grid, cell) = (
            diagnostics.grid_rect.unwrap(),
            diagnostics.cell_metrics.unwrap(),
        );
        grid.left_top() + egui::vec2(column * cell.width, cell.height / 2.0)
    }

    /// Hovers every cell of the first row, as link and image hit tests do.
    fn sweep_pointer(&mut self, columns: usize) {
        for column in 0..columns {
            let position = self.at(column as f32 + 0.5);
            self.harness.event(egui::Event::PointerMoved(position));
            self.harness.step();
        }
    }

    /// Drags a local selection across the first row and runs Copy.
    fn copy_columns(&mut self, from: f32, to: f32) -> Option<String> {
        let (start, end) = (self.at(from), self.at(to));
        self.harness.event(egui::Event::PointerMoved(start));
        self.harness.step();
        self.harness.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        self.harness.step();
        self.harness.event(egui::Event::PointerMoved(end));
        self.harness.step();
        self.harness.event(egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        self.harness.step();
        let (modifiers, key) = crate::keyboard::chord(
            festerm_config::KeyboardAction::Copy.default_chord(cfg!(target_os = "macos")),
        )
        .unwrap();
        self.harness.input_mut().events.push(egui::Event::Key {
            key,
            modifiers,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
        });
        self.harness.input_mut().events.push(egui::Event::Copy);
        self.harness.step();
        let copied = self
            .harness
            .output()
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            });
        self.harness.input_mut().events.push(egui::Event::Key {
            key,
            modifiers,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
        });
        self.harness.step();
        copied
    }
}

/// `CSI 16 t` then `CSI 14 t` replies for the pixels a PTY size carries.
fn geometry_reply(size: TerminalSize) -> Vec<u8> {
    let (width, height) = (size.pixel_width().unwrap(), size.pixel_height().unwrap());
    format!(
        "\x1b[6;{};{}t\x1b[4;{height};{width}t",
        height / size.rows(),
        width / size.columns(),
    )
    .into_bytes()
}

fn only_repeats(bytes: &[u8], unit: &[u8]) -> bool {
    bytes.len().is_multiple_of(unit.len()) && bytes.chunks(unit.len()).all(|chunk| chunk == unit)
}

#[test]
fn graphics_pixel_geometry_reaches_pty_core_and_replies() {
    let mut live = Live::local(opted_in());
    let standard = live.requested();
    if !crate::inline_graphics::BUILD.is_available() {
        assert_eq!(standard.pixel_width(), None);
        assert!(live.output(b"\x1b[16t\x1b[14t").is_empty());
        return;
    }
    assert!(
        live.output(b"\x1b[16t\x1b[14t").is_empty(),
        "core must not report pixels the PTY has not applied: {standard:?}"
    );
    live.apply(standard);
    let reply = geometry_reply(standard);
    let applied = live.output(b"\x1b[16t\x1b[14t");
    assert!(
        !applied.is_empty() && only_repeats(&applied, &reply),
        "{:?}",
        String::from_utf8_lossy(&applied)
    );

    live.harness.set_pixels_per_point(2.0);
    let retina = live.requested();
    assert_ne!(retina, standard);
    let retina_reply = geometry_reply(retina);
    let pending = live.output(b"\x1b[16t\x1b[14t");
    assert!(
        only_repeats(&pending, &reply),
        "until the PTY applies {retina:?}, only the applied generation may be reported: {:?}",
        String::from_utf8_lossy(&pending)
    );
    live.apply(retina);
    let republished = live.output(b"\x1b[16t\x1b[14t");
    assert!(
        republished.ends_with(&retina_reply),
        "{:?}",
        String::from_utf8_lossy(&republished)
    );
}

#[test]
fn a_stale_pty_ack_for_a_superseded_grid_never_restores_kitty_geometry() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::local(opted_in());
    let old = live.requested();
    live.apply(old);
    assert_eq!(live.output(b"\x1b[16t\x1b[14t"), geometry_reply(old));

    live.harness.set_size(egui::vec2(640.0, 480.0));
    let new = live.requested();
    assert_ne!((new.columns(), new.rows()), (old.columns(), old.rows()));
    // The frontend grid has moved on; a late ack still describes the old one.
    live.apply(old);
    let mut probe = apc(&format!("a=T,i=7,f=32,s=1,v=1,c=1,r=1;{RED}"));
    probe.extend_from_slice(b"\x1b[16t\x1b[14t\x1b[c");
    let stale = live.output(&probe);
    assert_eq!(
        stale,
        DA1,
        "a stale ack must withhold every graphics reply: {:?}",
        String::from_utf8_lossy(&stale)
    );
    assert!(live.session().terminal.kitty_placements().is_empty());

    live.apply(new);
    assert_eq!(live.output(b"\x1b[16t\x1b[14t"), geometry_reply(new));
}

#[test]
fn a_delayed_ack_for_an_earlier_identical_size_waits_for_the_newest_generation() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::local(opted_in());
    let a = live.requested();
    live.harness.set_size(egui::vec2(640.0, 480.0));
    let b = live.requested();
    live.harness.set_size(egui::vec2(800.0, 600.0));
    assert_eq!(live.requested(), a);
    let tagged = live
        .transport
        .resize_requests()
        .into_iter()
        .filter_map(|(size, generation)| Some((size, generation?)))
        .collect::<Vec<_>>();
    let (newest, earlier) = tagged.split_last().unwrap();
    assert_eq!(
        tagged[tagged.len() - 3..]
            .iter()
            .map(|(size, _)| *size)
            .collect::<Vec<_>>(),
        [a, b, a]
    );
    let mut probe = apc(&format!("a=T,i=7,f=32,s=1,v=1,c=1,r=1;{RED}"));
    probe.extend_from_slice(b"\x1b[16t\x1b[14t\x1b[c");
    // A₁ arrives after A₂ was requested: same grid and pixels, older generation.
    for &(size, generation) in earlier {
        live.transport
            .push_event(SessionEvent::ResizeAppliedGeneration { size, generation });
        live.harness.run_steps(2);
        let reply = live.output(&probe);
        assert_eq!(
            reply,
            DA1,
            "{size:?} generation {generation} must withhold graphics: {:?}",
            String::from_utf8_lossy(&reply)
        );
        assert!(live.session().terminal.kitty_placements().is_empty());
    }

    let &(size, generation) = newest;
    live.transport
        .push_event(SessionEvent::ResizeAppliedGeneration { size, generation });
    live.harness.run_steps(2);
    let mut expected = b"\x1b_Gi=7;OK\x1b\\".to_vec();
    expected.extend(geometry_reply(a));
    expected.extend_from_slice(DA1);
    assert_eq!(
        String::from_utf8(live.output(&probe)).unwrap(),
        String::from_utf8(expected).unwrap()
    );
    assert_eq!(live.session().terminal.kitty_placements().len(), 1);
}

/// Runs one frame only when the app scheduled it itself, as an idle window
/// does, and requires that a graphics retry is still armed.
fn scheduled_frame(live: &mut Live, waiting: &str) {
    let retry = live
        .session()
        .controller
        .next_output_backlog_repaint_delay();
    let delay = live.harness.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(
        retry.is_some_and(|retry| delay <= retry),
        "{waiting}: the app idled for {delay:?} with retry {retry:?}"
    );
    thread::sleep(delay);
    live.harness.step();
}

#[test]
fn final_graphics_replies_held_by_write_backpressure_are_sent_by_self_scheduled_frames() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let deliver = |live: &mut Live, output: Vec<u8>, expected: &[u8]| {
        let before = live.sent().len();
        live.transport.set_input_full(true);
        live.transport.push_event(SessionEvent::Output(output));
        live.harness.run_steps(4);
        for _ in 0..8 {
            scheduled_frame(live, "transport full");
        }
        assert_eq!(live.sent().len(), before);
        // The PTY drains without notifying anyone.
        live.transport.set_input_full(false);
        let deadline = Instant::now() + Duration::from_secs(10);
        while live.sent().len() - before < expected.len() {
            assert!(Instant::now() < deadline, "replies never drained");
            scheduled_frame(live, "replies pending");
        }
        live.harness.run_steps(2);
        assert!(live.sent()[before..] == *expected);
        assert_eq!(
            live.session()
                .controller
                .next_output_backlog_repaint_delay(),
            None,
            "the retry disarms once every reply is delivered"
        );
    };

    // Output and replies both back up behind the full transport.
    let queries = festerm_core::TRANSPORT_QUEUE_HIGH_WATERMARK / DA1.len();
    let mut output = b"\x1b[c".repeat(queries);
    output.extend(apc(&format!("a=T,i=5,f=32,s=1,v=1,c=1,r=1;{RED}")));
    let mut expected = DA1.repeat(queries);
    expected.extend_from_slice(b"\x1b_Gi=5;OK\x1b\\");
    deliver(&mut live, output, &expected);

    // The stream's final command is fully ingested; only its reply waits.
    deliver(
        &mut live,
        apc(&format!("a=T,i=6,f=32,s=1,v=1,c=1,r=1;{GREEN}")),
        b"\x1b_Gi=6;OK\x1b\\",
    );
    assert_eq!(live.session().terminal.kitty_placements().len(), 2);
}

#[test]
fn ineligible_sessions_publish_no_pixels_and_answer_only_da1() {
    for mut live in [Live::local(Configuration::empty()), Live::ssh(opted_in())] {
        let requested = live.requested();
        assert!(!live.session().inline_graphics().is_eligible());
        assert_eq!(
            (requested.pixel_width(), requested.pixel_height()),
            (None, None)
        );
        live.apply(requested);
        let mut output = apc("a=q,i=31,s=1,v=1,f=24;AQID");
        output.extend_from_slice(b"\x1b[14t\x1b[16t\x1b[c");
        assert_eq!(live.output(&output), DA1);
        assert!(live.session().terminal.kitty_images().is_empty());
    }
}

#[test]
fn kitty_query_reply_precedes_following_da1_and_never_echoes_payload() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    assert!(live.output(b"\x1b_Ga=q,i=31,s=1,v=1,f=24;AQ").is_empty());
    assert_eq!(
        String::from_utf8(live.output(b"ID\x1b\\\x1b[c")).unwrap(),
        "\x1b_Gi=31;OK\x1b\\\x1b[?6c"
    );
    assert!(live.session().terminal.kitty_images().is_empty());
}

#[test]
fn kitty_feature_query_replies_before_da1_and_leaves_uploads_and_placements_untouched() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let placed = apc(&format!("a=T,i=5,f=32,s=1,v=1,c=1,r=1,q=2;{RED}"));
    assert!(live.output(&placed).is_empty());
    assert!(live
        .output(&apc("a=t,i=9,f=32,s=1,v=1,m=1;/wAA"))
        .is_empty());
    let ids = |live: &Live| {
        let terminal = &live.session().terminal;
        let images: Vec<u32> = terminal
            .kitty_images()
            .iter()
            .map(|image| image.id)
            .collect();
        (images, terminal.kitty_placements().len())
    };
    let before = ids(&live);
    assert_eq!(before, (vec![5], 1));

    let mut query = apc("a=q,i=42,e=T,f=24,U=1,t=f;ignored");
    query.extend_from_slice(b"\x1b[c");
    assert_eq!(
        String::from_utf8(live.output(&query)).unwrap(),
        "\x1b_Gi=42,e=T,f=24;OK\x1b\\\x1b[?6c",
        "only supported keys are echoed and the payload is never interpreted"
    );
    assert_eq!(ids(&live), before);

    // The interleaved query left the pending chunked upload resumable.
    assert_eq!(
        String::from_utf8(live.output(&apc("m=0;/w=="))).unwrap(),
        "\x1b_Gi=9;OK\x1b\\"
    );
    assert_eq!(ids(&live), (vec![5, 9], 1));
}

#[test]
fn kitty_reply_pressure_commits_upload_and_keeps_da1_order_over_live_output() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    // Leaves room for one more DA1 but not for the upload's reply.
    let queries = (festerm_core::TRANSPORT_QUEUE_HIGH_WATERMARK - 6) / DA1.len();
    let mut output = b"\x1b[c".repeat(queries);
    output.extend(apc(&format!("a=T,i=5,f=32,s=1,v=1,c=1,r=1;{RED}")));
    output.extend_from_slice(b"\x1b[c");
    let sent = live.output(&output);
    let mut expected = DA1.repeat(queries);
    expected.extend_from_slice(b"\x1b_Gi=5;OK\x1b\\\x1b[?6c");
    assert!(
        sent == expected,
        "sent {} of {} bytes, ending {:?}",
        sent.len(),
        expected.len(),
        String::from_utf8_lossy(&sent[sent.len().saturating_sub(24)..])
    );
    let terminal = &live.session().terminal;
    assert_eq!(terminal.kitty_images().len(), 1);
    assert_eq!(terminal.kitty_placements().len(), 1);
}

/// Filter byte 0, then one opaque red RGBA pixel.
const RED_SCANLINE: [u8; 5] = [0, 255, 0, 0, 255];

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |crc, &byte| {
        (0..8).fold(crc ^ u32::from(byte), |crc, _| {
            if crc & 1 == 1 {
                crc >> 1 ^ 0xedb8_8320
            } else {
                crc >> 1
            }
        })
    })
}

fn adler32(bytes: &[u8]) -> u32 {
    let (a, b) = bytes.iter().fold((1u32, 0u32), |(a, b), &byte| {
        let a = (a + u32::from(byte)) % 65_521;
        (a, (b + a) % 65_521)
    });
    b << 16 | a
}

/// One stored deflate block holding `scanlines`, then `adler`.
fn stored_zlib(scanlines: &[u8], adler: u32) -> Vec<u8> {
    let length = u16::try_from(scanlines.len()).unwrap();
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&length.to_le_bytes());
    zlib.extend_from_slice(&(!length).to_le_bytes());
    zlib.extend_from_slice(scanlines);
    zlib.extend_from_slice(&adler.to_be_bytes());
    zlib
}

/// A structurally valid 1x1 RGBA PNG whose single IDAT is `idat`.
fn png(idat: &[u8]) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0][..]),
        (b"IDAT", idat),
        (b"IEND", &[][..]),
    ] {
        png.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let crc = crc32(&png[start..]);
        png.extend_from_slice(&crc.to_be_bytes());
    }
    png
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, &byte)| {
            word | u32::from(byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            text.push(if index <= chunk.len() {
                char::from(ALPHABET[(word >> (18 - 6 * index) & 63) as usize])
            } else {
                '='
            });
        }
    }
    text
}

#[test]
fn invalid_png_streams_are_refused_through_the_wired_validator() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let row = RED_SCANLINE;
    let good = stored_zlib(&row, adler32(&row));
    let two_rows = [row, row].concat();
    let mut trailing = good.clone();
    trailing.push(0);
    let mut live = Live::graphics_ready();
    let mut accepted = Vec::new();
    for (case, idat) in [
        ("checksum", stored_zlib(&row, adler32(&row) ^ 1)),
        ("trailing", trailing),
        ("truncated", good[..good.len() - 4].to_vec()),
        ("excess row", stored_zlib(&two_rows, adler32(&two_rows))),
    ] {
        let reply = live.output(&apc(&format!("a=t,i=7,f=100;{}", base64(&png(&idat)))));
        if reply != b"\x1b_Gi=7;EINVAL:invalid graphics command\x1b\\"
            || !live.session().terminal.kitty_images().is_empty()
        {
            accepted.push((case, String::from_utf8_lossy(&reply).into_owned()));
            live.output(&apc("a=d,d=I,i=7,q=2"));
        }
    }
    assert!(
        accepted.is_empty(),
        "invalid PNG streams accepted: {accepted:?}"
    );
    let reply = live.output(&apc(&format!("a=t,i=7,f=100;{}", base64(&png(&good)))));
    assert_eq!(String::from_utf8(reply).unwrap(), "\x1b_Gi=7;OK\x1b\\");
    assert_eq!(live.session().terminal.kitty_images().len(), 1);
}

#[test]
fn pending_png_validation_pauses_only_its_stream_while_frames_and_other_sessions_advance() {
    use std::sync::{Condvar, Mutex};
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    // (released, validations started); the worker blocks until released.
    let gate = Arc::new((Mutex::new((false, 0_usize)), Condvar::new()));
    let validator = {
        let gate = Arc::clone(&gate);
        festerm_core::kitty::KittyPngValidator::new(move |data, width, height| {
            let (lock, changed) = &*gate;
            let mut state = lock.lock().unwrap();
            state.1 += 1;
            while !state.0 {
                state = changed.wait(state).unwrap();
            }
            drop(state);
            festerm_ui_egui::validate_kitty_png(data, width, height)
        })
    };
    let _validator = crate::inline_graphics::override_png_validator_for_test(validator);
    let mut app = FesTermApp::for_test_with_configuration(opted_in());
    let other = FakeSshSession::new([]);
    let other_tab = app.state.place_test_local_session_with(other.clone());
    let mut live = Live::graphics_ready_in(app);
    let wait = |live: &mut Live, done: &dyn Fn(&Live) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(live) {
            assert!(Instant::now() < deadline, "the worker never progressed");
            thread::sleep(Duration::from_millis(1));
            live.harness.run_steps(1);
        }
    };

    let before = live.sent().len();
    let image = base64(&png(&stored_zlib(&RED_SCANLINE, adler32(&RED_SCANLINE))));
    let mut stream = apc(&format!("a=T,i=3,f=100,c=1,r=1;{image}"));
    stream.extend_from_slice(b"Z\x1b[c");
    live.transport.push_event(SessionEvent::Output(stream));
    wait(&mut live, &|_| gate.0.lock().unwrap().1 == 1);

    other.push_event(SessionEvent::Output(b"other\x1b[c".to_vec()));
    live.harness.run_steps(8);
    let other_terminal = &live
        .harness
        .state()
        .state
        .session_tab(other_tab)
        .unwrap()
        .terminal;
    assert!(other_terminal.row_text(0).unwrap().starts_with("other"));
    assert_eq!(other.sent().concat(), DA1);
    let terminal = &live.session().terminal;
    assert!(terminal.kitty_validation_pending());
    assert!(terminal.kitty_images().is_empty());
    assert!(!terminal.row_text(0).unwrap().contains('Z'));
    assert_eq!(
        live.sent().len(),
        before,
        "nothing commits past the command"
    );

    let (lock, changed) = &*gate;
    lock.lock().unwrap().0 = true;
    changed.notify_all();
    wait(&mut live, &|live| {
        !live.session().terminal.kitty_validation_pending() && live.sent().len() > before
    });
    live.harness.run_steps(2);
    assert_eq!(
        String::from_utf8(live.sent()[before..].to_vec()).unwrap(),
        "\x1b_Gi=3;OK\x1b\\\x1b[?6c"
    );
    let terminal = &live.session().terminal;
    assert_eq!(terminal.kitty_images().len(), 1);
    assert_eq!(terminal.kitty_placements().len(), 1);
    assert!(terminal.row_text(0).unwrap().contains('Z'));
    assert_eq!(lock.lock().unwrap().1, 1);
}

#[test]
fn kitty_placement_reaches_the_renderer_and_reset_releases_it() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let images = live.image_nodes();
    assert_eq!(live.kitty_textures(), 0);
    live.output(&apc(&format!("a=T,f=32,s=1,v=1,i=1,c=2,r=1,q=2;{RED}")));
    assert_eq!(live.session().terminal.kitty_placements().len(), 1);
    assert_eq!(live.image_nodes(), images + 1);
    assert_eq!(live.kitty_textures(), 1);

    live.output(b"\x1bc");
    let terminal = &live.session().terminal;
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(live.image_nodes(), images);
    assert_eq!(live.kitty_textures(), 0);
}

#[test]
fn app_copy_omits_text_under_above_text_images_and_keeps_text_over_lower_images() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let mut output = b"\x1b[2J\x1b[Habcdef\x1b[1;3H".to_vec();
    output.extend(apc(&format!("a=T,f=32,s=1,v=1,i=1,c=2,r=1,C=1,q=2;{RED}")));
    output.extend_from_slice(b"\x1b[1;5H");
    output.extend(apc(&format!(
        "a=T,f=32,s=1,v=1,i=2,z=-1,c=2,r=1,C=1,q=2;{GREEN}"
    )));
    live.output(&output);
    assert_eq!(live.session().terminal.kitty_placements().len(), 2);
    assert_eq!(live.copy_columns(0.25, 5.75).as_deref(), Some("abef"));
}

fn png_pixel(rgba: [u8; 4]) -> String {
    let scanline = [0, rgba[0], rgba[1], rgba[2], rgba[3]];
    base64(&png(&stored_zlib(&scanline, adler32(&scanline))))
}

#[test]
fn copy_and_pointer_stay_on_the_plain_path_without_drawn_above_text_pixels() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let mut output = b"\x1b[2J\x1b[Habcdef".to_vec();
    output.extend(apc(&format!(
        "a=t,f=100,i=3,q=2;{}",
        png_pixel([255, 0, 0, 255])
    )));
    live.output(&output);
    live.sweep_pointer(6);
    assert_eq!(live.copy_columns(0.25, 5.75).as_deref(), Some("abcdef"));
    assert_eq!(
        live.decodes(),
        0,
        "stored, unplaced images are never decoded"
    );
    assert_eq!(live.kitty_textures(), 0);

    let mut output = b"\x1b[1;1H".to_vec();
    output.extend(apc(&format!(
        "a=T,f=100,i=4,c=2,r=1,C=1,q=2;{}",
        png_pixel([255, 0, 0, 0])
    )));
    output.extend_from_slice(b"\x1b[1;4H");
    output.extend(apc("a=p,i=3,z=-1,c=2,r=1,C=1,q=2"));
    live.output(&output);
    assert_eq!(live.session().terminal.kitty_placements().len(), 2);
    live.sweep_pointer(6);
    assert_eq!(
        live.copy_columns(0.25, 5.75).as_deref(),
        Some("abcdef"),
        "transparent above-text and lower-layer pixels never occlude"
    );
    assert_eq!(
        live.decodes(),
        2,
        "each placed image is decoded exactly once"
    );
}

#[test]
fn png_textures_decode_once_and_are_reused_across_frames_pointer_and_copy() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    let mut output = b"\x1b[2J\x1b[Habcdef\x1b[1;3H".to_vec();
    output.extend(apc(&format!(
        "a=T,f=100,i=3,c=2,r=1,C=1,q=2;{}",
        png_pixel([255, 0, 0, 255])
    )));
    live.output(&output);
    let textures = live.kitty_texture_ids();
    assert_eq!((live.decodes(), textures.len()), (1, 1));
    for _ in 0..3 {
        live.sweep_pointer(6);
        assert_eq!(live.copy_columns(0.25, 5.75).as_deref(), Some("abef"));
        live.harness.run_steps(4);
    }
    assert_eq!(live.decodes(), 1);
    assert_eq!(live.kitty_texture_ids(), textures);
}

#[test]
fn a_render_budget_for_one_png_never_thrashes_and_the_undrawn_png_never_occludes() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut live = Live::graphics_ready();
    // A 1x1 image is charged 4 RGBA bytes and 1 alpha byte.
    live.view_mut().set_graphics_texture_budget_bytes(5);
    let red = png_pixel([255, 0, 0, 255]);
    let mut output = b"\x1b[2J\x1b[Habcdef\x1b[1;3H".to_vec();
    output.extend(apc(&format!("a=T,f=100,i=3,c=2,r=1,C=1,q=2;{red}")));
    output.extend_from_slice(b"\x1b[1;5H");
    output.extend(apc(&format!("a=T,f=100,i=4,c=2,r=1,C=1,q=2;{red}")));
    live.output(&output);
    assert_eq!(live.session().terminal.kitty_placements().len(), 2);
    let textures = live.kitty_texture_ids();
    for _ in 0..3 {
        live.sweep_pointer(6);
        assert_eq!(
            live.copy_columns(0.25, 5.75).as_deref(),
            Some("abef"),
            "only the drawn image occludes"
        );
        live.harness.run_steps(4);
    }
    assert_eq!(live.decodes(), 1, "the undrawn image is never decoded");
    assert_eq!(textures.len(), 1);
    assert_eq!(live.kitty_texture_ids(), textures);

    live.output(&apc("a=d,d=I,i=3,q=2"));
    live.sweep_pointer(6);
    assert_eq!(live.copy_columns(0.25, 5.75).as_deref(), Some("abcd"));
    let textures = live.kitty_texture_ids();
    live.harness.run_steps(8);
    assert_eq!(live.decodes(), 2);
    assert_eq!((textures.len(), live.kitty_texture_ids()), (1, textures));
}

#[test]
fn a_later_recording_never_offers_or_attaches_an_eligible_graphics_session() {
    if !crate::inline_graphics::BUILD.is_available() {
        return;
    }
    let mut app = FesTermApp::for_test_with_configuration(opted_in());
    let graphics = app
        .state
        .place_test_local_session_with(FakeSshSession::new([]));
    let plain = app.state.place_test_ssh_session(FakeSshSession::new([]));
    let choices = app
        .recording_choices()
        .into_iter()
        .map(|(id, ..)| id)
        .collect::<Vec<_>>();
    assert_eq!(choices, [plain]);
    assert!(app.recording_session(graphics).is_none());

    let directory = crate::recording_storage::test_private_parent();
    let path = crate::recording_storage::test_directory_path(directory.path()).join("recordings");
    let (storage, _) = crate::recording_storage::RecordingStorage::open_for_test(&path).unwrap();
    let terminal =
        festerm_core::Terminal::new(festerm_core::Dimensions::new(20, 2).unwrap()).unwrap();
    let (recording, mut streams) =
        festerm_recording::SharedRecording::start(storage.path(), &[&terminal], || {}).unwrap();
    assert!(!app.attach_recording(graphics, streams.pop().unwrap()));
    assert_eq!(
        app.state
            .session_tab(graphics)
            .unwrap()
            .controller
            .recording_id(),
        None
    );
    recording
        .stop(festerm_recording::FinishReason::User)
        .unwrap();
}
