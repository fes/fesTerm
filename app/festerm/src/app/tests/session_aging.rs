use super::*;
use crate::direct2d::profile::aging::{resource_snapshot, Frame, Renderer, PHYSICAL_SIZE};
use crate::session_controller::fake::FakeSshSession;
use eframe::egui_wgpu::wgpu;
use festerm_session::SessionEvent;
use festerm_test_support::tui_workload::Workload;
use festerm_windows_direct2d::process_cpu_time;
use serde::Serialize;
use std::{
    io::Write,
    sync::{atomic::AtomicU64, Condvar, Mutex},
    time::Instant,
};

const SESSION_COUNT: usize = 6;
const CADENCE: Duration = Duration::from_millis(100);
const MAX_IDLE_FRAMES: usize = 10_000;
const REGISTRY_INTERVAL: usize = 20;
const LIFECYCLE_CHURN_CYCLES: usize = 2;

fn bounded_setting(value: Option<&str>, default: usize, maximum: usize) -> Result<usize, String> {
    match value {
        None => Ok(default),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or_else(|| format!("expected an integer in 1..={maximum}, got {value:?}")),
    }
}

#[derive(Default)]
struct Requests {
    immediate: AtomicU64,
    delayed: AtomicU64,
    deadline: Mutex<Option<Instant>>,
    changed: Condvar,
}

impl Requests {
    fn schedule(&self, delay: Duration) {
        let Some(deadline) = Instant::now().checked_add(delay) else {
            return;
        };
        let mut next = self.deadline.lock().unwrap();
        if next.is_none_or(|next| deadline < next) {
            *next = Some(deadline);
            self.changed.notify_one();
        }
    }

    fn counts(&self) -> [u64; 2] {
        [
            self.immediate.load(Ordering::Relaxed),
            self.delayed.load(Ordering::Relaxed),
        ]
    }

    fn wait_due(&self, end: Instant) -> bool {
        let mut next = self.deadline.lock().unwrap();
        loop {
            let now = Instant::now();
            if now >= end {
                return false;
            }
            if next.is_some_and(|next| next <= now) {
                *next = None;
                return true;
            }
            let until = next.map_or(end, |next| next.min(end));
            let (guard, _) = self.changed.wait_timeout(next, until - now).unwrap();
            next = guard;
        }
    }
}

struct Fixture {
    app: FesTermApp,
    sessions: Vec<(TabId, FakeSshSession)>,
    context: egui::Context,
    requests: Arc<Requests>,
    started: Instant,
}

impl Fixture {
    fn new() -> Self {
        let context = egui::Context::default();
        context.set_theme(egui::ThemePreference::Dark);
        context.set_visuals(festerm_ui_egui::theme::default_visuals());
        let (app, sessions) = background_efficiency_fixture(SESSION_COUNT, &context);
        let requests = Arc::new(Requests::default());
        let observed = requests.clone();
        context.set_request_repaint_callback(move |info| {
            if info.viewport_id != egui::ViewportId::ROOT {
                return;
            }
            if info.delay.is_zero() {
                observed.immediate.fetch_add(1, Ordering::Relaxed);
            } else {
                observed.delayed.fetch_add(1, Ordering::Relaxed);
            }
            observed.schedule(info.delay);
        });
        for (_, transport) in &sessions {
            transport.set_notifier_for_test(crate::tabs::session_notifier_for_test(&context));
        }
        Self {
            app,
            sessions,
            context,
            requests,
            started: Instant::now(),
        }
    }

    fn input(&self, scale: f32) -> egui::RawInput {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(
                    PHYSICAL_SIZE[0] as f32 / scale,
                    PHYSICAL_SIZE[1] as f32 / scale,
                ),
            )),
            time: Some(self.started.elapsed().as_secs_f64()),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        input
    }

    fn draw(&mut self, renderer: &mut Renderer, scale: f32) -> (Frame, usize) {
        *self.requests.deadline.lock().unwrap() = None;
        let calls = renderer.calls();
        let output = self.context.run_ui(self.input(scale), |ui| {
            self.app.frame_logic(ui.ctx());
            self.app.ui_content(ui);
        });
        self.requests
            .schedule(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay);
        let dirty = self
            .app
            .state
            .session_tab(self.app.state.active())
            .unwrap()
            .view
            .diagnostics()
            .dirty_rows;
        let frame = renderer.draw(&self.context, output, calls);
        assert!(self
            .sessions
            .iter()
            .all(|(_, transport)| transport.pending_events_for_test() == 0));
        (frame, dirty)
    }

    fn normalize(&mut self, renderer: &mut Renderer) {
        for _ in 0..8 {
            self.draw(renderer, 2.0);
        }
        for (tab, transport) in self.sessions.clone() {
            self.app
                .state
                .dispatch(AppCommand::ActivateTab(tab), &self.context);
            self.app
                .zoom_active_session(ZoomCommand::Reset, &self.context);
            self.draw(renderer, 2.0);
            transport.push_event(SessionEvent::Output(Workload::Localized.setup()));
            self.draw(renderer, 2.0);
        }
        self.wait_for_transient_notice();
        for _ in 0..8 {
            self.draw(renderer, 2.0);
        }
        assert!(self.app.overlays.transient_notice.is_none());
        assert_eq!(
            self.app.active_terminal_dimensions_for_test(),
            festerm_core::Dimensions::new(120, 40).unwrap()
        );
    }

    fn wait_for_transient_notice(&self) {
        if let Some((_, deadline)) = self.app.overlays.transient_notice.as_ref() {
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }

    fn feed(&self, mode: &str, step: usize) -> usize {
        let mut count = 0;
        for (tab, transport) in &self.sessions {
            let active = *tab == self.app.state.active();
            if mode == "all" || (mode == "active" && active) || (mode == "background" && !active) {
                transport.push_event(SessionEvent::Output(Workload::Localized.update(step)));
                count += 1;
            }
        }
        count
    }
}

#[derive(Serialize)]
struct Sample {
    index: usize,
    elapsed_ms: f64,
    work_ms: f64,
    cpu_ms: f64,
    supplied_events: usize,
    dirty_rows: usize,
    rendering: Frame,
}

fn phase_marker(directory: &std::path::Path, phase: &str) {
    let record = serde_json::json!({
        "phase": phase, "pid": std::process::id(),
        "unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis(),
    });
    // Replace a complete marker; the sampler opens it with delete sharing.
    std::fs::write(
        directory.join("phase-next.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    std::fs::rename(
        directory.join("phase-next.json"),
        directory.join("phase.json"),
    )
    .unwrap();
}

fn observe_registry(
    observations: &mut Vec<serde_json::Value>,
    instance: &wgpu::Instance,
    name: &str,
) {
    observations.push(serde_json::json!({
        "name": name,
        "registries": resource_snapshot(instance),
    }));
}

fn observe_teardown(
    observations: &mut Vec<serde_json::Value>,
    instance: &wgpu::Instance,
    directory: &std::path::Path,
    name: &str,
    seconds: usize,
) {
    assert!(instance.poll_all(true), "teardown work must complete");
    let registries = resource_snapshot(instance);
    phase_marker(directory, name);
    let window = held_window(seconds);
    observations.push(serde_json::json!({
        "name": name,
        "registries": registries,
        "window": window,
    }));
}

fn held_window(seconds: usize) -> serde_json::Value {
    let unix_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    };
    let started_unix_ms = unix_ms();
    let started = Instant::now();
    thread::sleep(Duration::from_secs(seconds as u64));
    serde_json::json!({
        "started_unix_ms": started_unix_ms,
        "completed_unix_ms": unix_ms(),
        "wall_seconds": started.elapsed().as_secs_f64(),
    })
}

fn churn_cycle(fixture: &mut Fixture, renderer: &mut Renderer, cycle: usize) {
    let enlarged = cycle.is_multiple_of(2);
    for (tab, _) in fixture.sessions.clone() {
        fixture
            .app
            .state
            .dispatch(AppCommand::ActivateTab(tab), &fixture.context);
        fixture.app.zoom_active_session(
            if enlarged {
                ZoomCommand::In
            } else {
                ZoomCommand::Reset
            },
            &fixture.context,
        );
        fixture.feed("all", cycle);
        fixture.draw(renderer, if enlarged { 1.25 } else { 2.0 });
    }
}

fn lifecycle_round(
    directory: &std::path::Path,
    index: usize,
    idle_seconds: usize,
) -> serde_json::Value {
    phase_marker(directory, &format!("lifecycle-{index}-create"));
    let mut fixture = Fixture::new();
    let repaint_owner = Arc::downgrade(&fixture.requests);
    let mut renderer = Renderer::new(&fixture.context);
    for cycle in 0..LIFECYCLE_CHURN_CYCLES {
        churn_cycle(&mut fixture, &mut renderer, cycle);
    }
    fixture.normalize(&mut renderer);
    let temporary_oracle_rgba_bytes = {
        let actual = renderer.image();
        let reference = image::open(directory.join("fresh-normalized.png"))
            .unwrap()
            .into_rgba8();
        assert!(
            actual == reference,
            "whole-owner rebuild must preserve exact pixels"
        );
        actual
            .save(directory.join(format!("lifecycle-{index}-normalized.png")))
            .unwrap();
        actual.as_raw().len() + reference.as_raw().len()
    };
    let instance = renderer.instance();
    let live_registries = resource_snapshot(&instance);
    drop(renderer);
    drop(fixture);
    assert!(repaint_owner.upgrade().is_none(), "repaint owner leaked");
    assert!(instance.poll_all(true), "retired owner work must complete");
    let drained_registries = resource_snapshot(&instance);
    drop(instance);
    phase_marker(directory, &format!("lifecycle-{index}-dropped"));
    let window = held_window(idle_seconds);
    serde_json::json!({
        "index": index, "churn_cycles": LIFECYCLE_CHURN_CYCLES,
        "churn_submitted_frames": LIFECYCLE_CHURN_CYCLES * SESSION_COUNT,
        "live_registries": live_registries, "drained_registries": drained_registries,
        "reporting_instance_dropped": true, "repaint_owner_released": true,
        "wgpu_submission_completed": true,
        "temporary_oracle_rgba_bytes": temporary_oracle_rgba_bytes, "window": window,
    })
}

fn complete_sample_receipt(record: &serde_json::Value, pid: u32) -> bool {
    record["pid"].as_u64() == Some(u64::from(pid)) && record["phase"].as_str() == Some("complete")
}

fn wait_for_complete_sample(directory: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(bytes) = std::fs::read(directory.join("sampled.json")) {
            if let Ok(record) = serde_json::from_slice(&bytes) {
                if complete_sample_receipt(&record, std::process::id()) {
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "supervisor did not capture final process commitment"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn measured_phase(
    fixture: &mut Fixture,
    renderer: &mut Renderer,
    directory: &std::path::Path,
    name: &str,
    mode: &str,
    frames: usize,
    idle_seconds: usize,
) {
    phase_marker(directory, name);
    let mut log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(format!("{name}.jsonl")))
        .unwrap();
    let before = fixture.requests.counts();
    let cpu = process_cpu_time().unwrap();
    let started = Instant::now();
    let end = started + Duration::from_secs(idle_seconds as u64);
    let mut index = 0;
    let mut events = 0;
    loop {
        if mode == "idle" {
            assert!(
                index < MAX_IDLE_FRAMES,
                "idle repaint storm exceeded diagnostic cap"
            );
            if !fixture.requests.wait_due(end) {
                break;
            }
        } else {
            if index == frames {
                break;
            }
            if let Some(delay) =
                (started + CADENCE.mul_f64(index as f64)).checked_duration_since(Instant::now())
            {
                thread::sleep(delay);
            }
        }
        let frame_cpu = process_cpu_time().unwrap();
        let work = Instant::now();
        let supplied_events = fixture.feed(mode, index);
        let (rendering, dirty_rows) = fixture.draw(renderer, 2.0);
        assert_eq!(
            rendering.native_calls, 1,
            "measured frame must use native painting"
        );
        assert!(
            rendering.host_copy,
            "measured frame must retain automatic copy eligibility"
        );
        let sample = Sample {
            index,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            work_ms: work.elapsed().as_secs_f64() * 1000.0,
            cpu_ms: (process_cpu_time().unwrap() - frame_cpu).as_secs_f64() * 1000.0,
            supplied_events,
            dirty_rows,
            rendering,
        };
        serde_json::to_writer(&mut log, &sample).unwrap();
        writeln!(log).unwrap();
        events += supplied_events;
        index += 1;
    }
    if mode != "idle" {
        if let Some(delay) =
            (started + CADENCE.mul_f64(frames as f64)).checked_duration_since(Instant::now())
        {
            thread::sleep(delay);
        }
    }
    log.flush().unwrap();
    let elapsed = started.elapsed().as_secs_f64();
    let cpu_ms = (process_cpu_time().unwrap() - cpu).as_secs_f64() * 1000.0;
    let after = fixture.requests.counts();
    let summary = serde_json::json!({
        "schema": 1, "phase": name, "mode": mode,
        "completed_frames": index, "supplied_events": events,
        "wall_seconds": elapsed, "process_cpu_ms": cpu_ms,
        "completed_hz": index as f64 / elapsed,
        "cpu_ms_per_completed_frame": (index > 0).then(|| cpu_ms / index as f64),
        "immediate_repaint_callbacks": after[0] - before[0],
        "delayed_repaint_callbacks": after[1] - before[1],
        "requested_cadence_ms": (mode != "idle").then_some(100),
        "event_driven": mode == "idle",
        "pending_events": fixture.sessions.iter()
            .map(|(_, transport)| transport.pending_events_for_test()).collect::<Vec<_>>(),
    });
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
}

fn state_phases(
    fixture: &mut Fixture,
    renderer: &mut Renderer,
    directory: &std::path::Path,
    state: &str,
    frames: usize,
    idle_seconds: usize,
) {
    fixture.normalize(renderer);
    let image = renderer.image();
    image
        .save(directory.join(format!("{state}-normalized.png")))
        .unwrap();
    drop(image);
    for mode in ["frozen", "active", "background", "idle"] {
        for _ in 0..4 {
            fixture.draw(renderer, 2.0);
        }
        measured_phase(
            fixture,
            renderer,
            directory,
            &format!("{state}-{mode}"),
            mode,
            frames,
            idle_seconds,
        );
    }
}

#[test]
fn aging_settings_reject_zero_overflow_and_malformed_values() {
    assert_eq!(bounded_setting(None, 120, 2000).unwrap(), 120);
    assert_eq!(bounded_setting(Some("2000"), 120, 2000).unwrap(), 2000);
    for value in ["", "0", "2001", "-1", "2.0", " 20", "18446744073709551616"] {
        assert!(bounded_setting(Some(value), 120, 2000).is_err(), "{value}");
    }
}

#[test]
fn aging_fixture_notifies_and_pumps_all_six_owned_sessions() {
    let mut fixture = Fixture::new();
    for _ in 0..4 {
        let mut output = fixture.context.run_ui(Default::default(), |_| {});
        output.textures_delta.clear();
    }
    let before = fixture.requests.counts();
    assert_eq!(fixture.feed("all", 7), SESSION_COUNT);
    assert!(fixture.requests.counts()[0] > before[0]);
    assert!(fixture
        .sessions
        .iter()
        .all(|(_, transport)| transport.pending_events_for_test() == 1));
    let mut output = fixture.context.run_ui(Default::default(), |ui| {
        fixture.app.pump_all_sessions(ui.ctx());
    });
    output.textures_delta.clear();
    for (tab, transport) in &fixture.sessions {
        assert_eq!(transport.pending_events_for_test(), 0);
        assert!(transport.sent().is_empty());
        assert_eq!(
            fixture
                .app
                .state
                .session_tab(*tab)
                .unwrap()
                .has_new_output_since_active,
            *tab != fixture.app.state.active()
        );
    }
}

#[test]
fn aging_idle_scheduler_waits_for_demand_and_honors_deadline() {
    let requests = Requests::default();
    assert!(!requests.wait_due(Instant::now()));
    requests.schedule(Duration::ZERO);
    assert!(requests.wait_due(Instant::now() + Duration::from_secs(1)));
    requests.schedule(Duration::from_secs(5));
    requests.schedule(Duration::ZERO);
    assert!(requests.wait_due(Instant::now() + Duration::from_secs(1)));
}

#[test]
fn aging_normalization_waits_for_real_notice_expiry_without_dismissing_it() {
    let mut fixture = Fixture::new();
    let deadline = Instant::now() + Duration::from_millis(10);
    fixture.app.overlays.transient_notice = Some(("controlled zoom notice".to_owned(), deadline));
    fixture.wait_for_transient_notice();
    assert!(Instant::now() >= deadline);
    assert!(fixture.app.overlays.transient_notice.is_some());
    let mut output = fixture.context.run_ui(Default::default(), |ui| {
        fixture.app.show_transient_notice(ui.ctx());
    });
    output.textures_delta.clear();
    assert!(fixture.app.overlays.transient_notice.is_none());
}

#[test]
fn aging_fake_transports_do_not_retain_resize_history() {
    use festerm_session::Session;
    let fixture = Fixture::new();
    for (_, transport) in &fixture.sessions {
        for columns in 80..120 {
            transport
                .try_resize(festerm_session::TerminalSize::new(columns, 40).unwrap())
                .unwrap();
        }
        assert!(transport.operations().is_empty());
        assert!(transport.sent().is_empty());
        assert_eq!(transport.pending_events_for_test(), 0);
    }
}

#[test]
fn aging_fixture_drop_releases_repaint_owner_with_six_backlogged_sessions() {
    for _ in 0..4 {
        let mut fixture = Fixture::new();
        let owner = Arc::downgrade(&fixture.requests);
        for step in 0..=crate::session_controller::MAX_SESSION_EVENTS_PER_FRAME {
            fixture.feed("all", step);
        }
        fixture.app.pump_all_sessions(&fixture.context);
        assert!(fixture
            .sessions
            .iter()
            .all(|(_, transport)| transport.pending_events_for_test() == 1));
        drop(fixture);
        assert!(
            owner.upgrade().is_none(),
            "queued output must not pin the retired egui owner"
        );
    }
}

#[test]
fn aging_six_session_generation_retirement_refuses_stale_clipboard_completion() {
    use festerm_ui_egui::EncodedInputSink;
    let mut fixture = Fixture::new();
    for (tab, transport) in &fixture.sessions {
        let controller = &mut fixture.app.state.session_tab_mut(*tab).unwrap().controller;
        assert!(controller.begin_clipboard_input(71));
        controller.record_encoded_input(b"retired-input");
        assert!(transport.sent().is_empty());
        controller.advance_lifecycle_generation();
        assert!(!controller.prepare_clipboard_input(71));
        assert_eq!(
            controller.take_clipboard_discarded_bytes(),
            b"retired-input".len()
        );
        assert!(controller.begin_clipboard_input(72));
        controller.record_encoded_input(b"cancelled-input");
        controller.cancel_clipboard_input(71, "stale-cancel");
        assert!(controller.clipboard_input_pending(72));
        controller.flush_pending_writes();
        assert!(transport.sent().is_empty());
        controller.cancel_clipboard_input(72, "current-cancel");
        assert_eq!(
            controller.take_clipboard_discarded_bytes(),
            b"cancelled-input".len()
        );
        controller.record_encoded_input(b"recovered-input");
        controller.flush_pending_writes();
        assert_eq!(transport.sent(), [b"recovered-input".to_vec()]);
    }
}

#[test]
fn aging_complete_sample_receipt_rejects_other_processes_and_early_phases() {
    assert!(complete_sample_receipt(
        &serde_json::json!({"pid": 7, "phase": "complete"}),
        7
    ));
    for record in [
        serde_json::json!({"pid": 8, "phase": "complete"}),
        serde_json::json!({"pid": 7, "phase": "rebuilt-fixture-dropped"}),
        serde_json::json!({"pid": "7", "phase": "complete"}),
        serde_json::json!({}),
    ] {
        assert!(!complete_sample_receipt(&record, 7));
    }
}

#[test]
#[ignore = "optional bounded six-session aging discriminator; not multi-day/native presentation evidence"]
fn profile_six_session_aging() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let setting = |name, default, maximum| {
        let value = match std::env::var(name) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => panic!("{name}: {error}"),
        };
        bounded_setting(value.as_deref(), default, maximum)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let cycles = setting("FESTERM_AGING_CYCLES", 120, 2000);
    let frames = setting("FESTERM_AGING_FRAMES", 100, 1000);
    let idle_seconds = setting("FESTERM_AGING_IDLE_SECONDS", 10, 300);
    let lifecycle_repeats = setting("FESTERM_AGING_LIFECYCLE_REPEATS", 3, 8);
    let directory =
        PathBuf::from(std::env::var_os("FESTERM_AGING_OUT").expect("set FESTERM_AGING_OUT"));
    assert!(!directory.exists(), "use a fresh evidence directory");
    std::fs::create_dir_all(&directory).unwrap();
    let mut fixture = Fixture::new();
    let mut renderer = Renderer::new(&fixture.context);
    let mut registries = Vec::new();
    state_phases(
        &mut fixture,
        &mut renderer,
        &directory,
        "fresh",
        frames,
        idle_seconds,
    );
    observe_registry(&mut registries, &renderer.instance(), "fresh");
    phase_marker(&directory, "churn");
    for cycle in 0..cycles {
        churn_cycle(&mut fixture, &mut renderer, cycle);
        if (cycle + 1) % REGISTRY_INTERVAL == 0 || cycle + 1 == cycles {
            observe_registry(
                &mut registries,
                &renderer.instance(),
                &format!("churn-{}", cycle + 1),
            );
        }
    }
    state_phases(
        &mut fixture,
        &mut renderer,
        &directory,
        "churned",
        frames,
        idle_seconds,
    );
    let old_instance = renderer.instance();
    observe_registry(&mut registries, &old_instance, "churned");
    phase_marker(&directory, "rebuilding");
    drop(renderer);
    observe_teardown(
        &mut registries,
        &old_instance,
        &directory,
        "renderer-dropped",
        idle_seconds,
    );
    phase_marker(&directory, "dropping-fixture");
    drop(fixture);
    observe_teardown(
        &mut registries,
        &old_instance,
        &directory,
        "fixture-dropped",
        idle_seconds,
    );
    phase_marker(&directory, "creating-rebuilt");
    drop(old_instance);
    let mut fixture = Fixture::new();
    let mut renderer = Renderer::new(&fixture.context);
    state_phases(
        &mut fixture,
        &mut renderer,
        &directory,
        "rebuilt",
        frames,
        idle_seconds,
    );
    let rebuilt_instance = renderer.instance();
    observe_registry(&mut registries, &rebuilt_instance, "rebuilt");
    drop(renderer);
    observe_teardown(
        &mut registries,
        &rebuilt_instance,
        &directory,
        "rebuilt-renderer-dropped",
        idle_seconds,
    );
    phase_marker(&directory, "dropping-rebuilt-fixture");
    drop(fixture);
    observe_teardown(
        &mut registries,
        &rebuilt_instance,
        &directory,
        "rebuilt-fixture-dropped",
        idle_seconds,
    );
    drop(rebuilt_instance);
    let lifecycles = (0..lifecycle_repeats)
        .map(|index| lifecycle_round(&directory, index, idle_seconds))
        .collect::<Vec<_>>();
    std::fs::write(
        directory.join("lifecycles.json"),
        serde_json::to_vec_pretty(&lifecycles).unwrap(),
    )
    .unwrap();
    phase_marker(&directory, "validating-pixels");
    std::fs::write(
        directory.join("registries.json"),
        serde_json::to_vec_pretty(&registries).unwrap(),
    )
    .unwrap();
    let reference = image::open(directory.join("fresh-normalized.png"))
        .unwrap()
        .into_rgba8();
    for state in ["churned", "rebuilt"] {
        let actual = image::open(directory.join(format!("{state}-normalized.png")))
            .unwrap()
            .into_rgba8();
        assert!(
            reference == actual,
            "{state} normalization must preserve every pixel"
        );
    }
    drop(reference);
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": 1, "session_count": SESSION_COUNT, "churn_cycles": cycles,
            "churn_submitted_frames": cycles * SESSION_COUNT, "frames_per_paced_phase": frames,
            "idle_seconds": idle_seconds, "physical_size": PHYSICAL_SIZE,
            "measurement_scale": 2.0, "churn_scales": [1.25, 2.0],
            "registry_schema": 1, "registry_interval": REGISTRY_INTERVAL,
            "frame_resource_schema": 1, "process_memory_schema": 1,
            "lifecycle_schema": 1, "lifecycle_repeats": lifecycle_repeats,
            "lifecycle_churn_cycles": LIFECYCLE_CHURN_CYCLES,
            "states": ["fresh", "churned", "rebuilt"],
            "modes": ["frozen", "active", "background", "idle"],
            "normalized_pixels_equal": true, "installed_sessions_accessed": false,
            "production_cadence_changed": false,
            "limitations": "Completed offscreen work, forced paced non-idle frames, event-driven idle demand; not native presentation or a multi-day reproducer. Rebuild discards synthetic GUI and renderer state, not user shells."
        })).unwrap(),
    ).unwrap();
    phase_marker(&directory, "complete");
    wait_for_complete_sample(&directory);
}
