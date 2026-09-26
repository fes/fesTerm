//! Bounded, coalesced Launcher inventory work. No provider I/O runs in a frame.

use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::multiplexer_sessions::{self, MultiplexerSession};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub native: Vec<festerm_sessiond::UnattachedSession>,
    pub tmux: Vec<MultiplexerSession>,
    pub screen: Vec<MultiplexerSession>,
    pub errors: Vec<String>,
}

impl Inventory {
    fn discover() -> Self {
        let mut result = Self::default();
        match festerm_sessiond::list_unattached_local_sessions() {
            Ok(sessions) => result.native = sessions,
            Err(error) => result.errors.push(format!("fesTerm Native: {error}")),
        }
        match multiplexer_sessions::list_tmux_sessions() {
            Ok(sessions) => result.tmux = sessions,
            Err(error) => result.errors.push(format!("tmux: {error}")),
        }
        match multiplexer_sessions::list_screen_sessions() {
            Ok(sessions) => result.screen = sessions,
            Err(error) => result.errors.push(format!("screen: {error}")),
        }
        result
    }
}

#[derive(Default)]
pub struct Discovery {
    pub inventory: Inventory,
    generation: u64,
    requested: bool,
    enabled: bool,
    worker: Option<Worker>,
}

struct Worker {
    generation: u64,
    receiver: Receiver<Inventory>,
    cancel: SyncSender<()>,
    handle: JoinHandle<()>,
    result: Option<Inventory>,
}

impl Worker {
    fn cancel(&self) {
        let _ = self.cancel.try_send(());
    }
}

struct RepaintOnFinish(eframe::egui::Context);

impl Drop for RepaintOnFinish {
    fn drop(&mut self) {
        // Also wake the UI if a provider panics, so its failure is collected.
        self.0.request_repaint();
    }
}

fn wait_for_changed_inventory(
    baseline: &Inventory,
    cancel: &Receiver<()>,
    mut discover: impl FnMut() -> Inventory,
    interval: Duration,
    mut immediate: bool,
) -> Option<Inventory> {
    loop {
        if !immediate {
            match cancel.recv_timeout(interval) {
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        immediate = false;
        if cancel.try_recv() != Err(mpsc::TryRecvError::Empty) {
            return None;
        }
        let inventory = discover();
        if cancel.try_recv() != Err(mpsc::TryRecvError::Empty) {
            return None;
        }
        if inventory != *baseline {
            return Some(inventory);
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.cancel();
            let _ = worker.handle.join();
        }
    }
}

impl Discovery {
    pub fn refresh(&mut self) {
        // One pending refresh, not one queued request per gesture.
        if !self.requested {
            self.generation = self.generation.wrapping_add(1);
            self.requested = true;
            if let Some(worker) = &self.worker {
                worker.cancel();
            }
        }
    }

    /// Whether discovery considers itself live, which is what decides if a
    /// probe is spawned on this frame.
    #[cfg(test)]
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn update(&mut self, enabled: bool, context: &eframe::egui::Context) {
        self.update_with(enabled, context, Inventory::discover);
    }

    fn update_with(
        &mut self,
        enabled: bool,
        context: &eframe::egui::Context,
        discover: impl FnMut() -> Inventory + Send + 'static,
    ) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.inventory = Inventory::default();
            self.refresh();
        }
        if let Some(worker) = &mut self.worker {
            let ready = worker.result.is_some()
                || match worker.receiver.try_recv() {
                    Ok(inventory) => {
                        worker.result = Some(inventory);
                        true
                    }
                    Err(mpsc::TryRecvError::Disconnected) => true,
                    Err(mpsc::TryRecvError::Empty) => false,
                };
            if worker.handle.is_finished() {
                let worker = self.worker.take().unwrap();
                let joined = worker.handle.join();
                if enabled && worker.generation == self.generation {
                    let result = worker.result.or_else(|| worker.receiver.try_recv().ok());
                    match (joined, result) {
                        (Ok(()), Some(inventory)) => self.inventory = inventory,
                        _ => {
                            self.inventory = Inventory::default();
                            self.inventory.errors.push(
                                "Running Sessions discovery failed; Refresh to retry.".into(),
                            );
                        }
                    }
                }
            } else if ready {
                // Publication can wake the event loop just before the thread
                // finishes. Retire it without joining provider work in a frame.
                let predicted_frame = Duration::from_secs_f32(context.input(|i| i.predicted_dt));
                context.request_repaint_after(Duration::from_millis(10) + predicted_frame);
            }
        }
        if !enabled {
            return;
        }
        if self.worker.is_none() {
            let immediate = self.requested;
            self.requested = false;
            let (sender, receiver) = mpsc::sync_channel(1);
            let (cancel, cancellation) = mpsc::sync_channel(1);
            let baseline = self.inventory.clone();
            let context = context.clone();
            let handle = thread::Builder::new()
                .name("running-session-discovery".into())
                .spawn(move || {
                    let _repaint = RepaintOnFinish(context);
                    // Close the result channel before notifying, including on panic.
                    let result_sender = sender;
                    if let Some(inventory) = wait_for_changed_inventory(
                        &baseline,
                        &cancellation,
                        discover,
                        REFRESH_INTERVAL,
                        immediate,
                    ) {
                        let _ = result_sender.send(inventory);
                    }
                });
            match handle {
                Ok(handle) => {
                    self.worker = Some(Worker {
                        generation: self.generation,
                        receiver,
                        cancel,
                        handle,
                        result: None,
                    });
                }
                Err(error) => {
                    self.inventory = Inventory::default();
                    self.inventory.errors.push(format!(
                        "Could not start Running Sessions discovery: {error}. Refresh to retry."
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn unchanged_inventory_keeps_polling_until_a_visible_change() {
        let (_sender, cancellation) = mpsc::sync_channel(1);
        let mut polls = 0;
        let changed = wait_for_changed_inventory(
            &Inventory::default(),
            &cancellation,
            || {
                polls += 1;
                if polls < 4 {
                    Inventory::default()
                } else {
                    Inventory {
                        errors: vec!["provider unavailable".into()],
                        ..Default::default()
                    }
                }
            },
            Duration::ZERO,
            true,
        )
        .unwrap();
        assert_eq!(polls, 4);
        assert_eq!(changed.errors, ["provider unavailable"]);
    }

    #[test]
    fn unchanged_discovery_does_not_schedule_a_gui_repaint() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let context = eframe::egui::Context::default();
        let repaints = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&repaints);
        context.set_request_repaint_callback(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
        let (sender, receiver) = mpsc::channel();
        let mut discovery = Discovery::default();
        discovery.update_with(true, &context, move || {
            sender.send(()).unwrap();
            Inventory::default()
        });
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(repaints.load(Ordering::SeqCst), 0);
        assert!(!discovery.worker.as_ref().unwrap().handle.is_finished());
    }

    #[test]
    fn cancellation_discards_inflight_inventory_and_stops_further_probes() {
        let (sender, cancellation) = mpsc::sync_channel(1);
        let result = wait_for_changed_inventory(
            &Inventory::default(),
            &cancellation,
            || {
                sender.send(()).unwrap();
                Inventory {
                    errors: vec!["obsolete result".into()],
                    ..Default::default()
                }
            },
            Duration::ZERO,
            true,
        );
        assert!(result.is_none());

        drop(sender);
        assert!(wait_for_changed_inventory(
            &Inventory::default(),
            &cancellation,
            || panic!("cancelled discovery must not probe"),
            REFRESH_INTERVAL,
            false,
        )
        .is_none());
    }

    #[test]
    fn dropping_discovery_interrupts_the_background_refresh_wait() {
        let (sender, receiver) = mpsc::channel();
        let mut discovery = Discovery::default();
        discovery.update_with(true, &eframe::egui::Context::default(), move || {
            sender.send(()).unwrap();
            Inventory::default()
        });
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        let (done, completed) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(discovery);
            done.send(()).unwrap();
        });
        completed.recv_timeout(Duration::from_secs(3)).unwrap();
        dropper.join().unwrap();
    }

    #[test]
    fn provider_panics_wake_the_ui_and_leave_an_actionable_error() {
        let context = eframe::egui::Context::default();
        let (sender, receiver) = mpsc::channel();
        context.set_request_repaint_callback(move |_| {
            let _ = sender.send(());
        });
        let mut discovery = Discovery::default();
        discovery.update_with(true, &context, || panic!("controlled provider failure"));
        receiver.recv_timeout(Duration::from_secs(3)).unwrap();
        finish(&mut discovery, &context);
        assert_eq!(
            discovery.inventory.errors,
            ["Running Sessions discovery failed; Refresh to retry."]
        );
    }

    #[test]
    fn changed_inventory_is_published_and_periodic_discovery_continues() {
        let context = eframe::egui::Context::default();
        let mut discovery = Discovery::default();
        discovery.update_with(true, &context, || Inventory {
            tmux: vec![MultiplexerSession {
                name: "same-name".into(),
                match_key: "generation-one".into(),
                attached: false,
                started_at_unix_seconds: Some(1),
            }],
            ..Default::default()
        });
        finish(&mut discovery, &context);
        assert_eq!(discovery.inventory.tmux.len(), 1);
        assert!(discovery.worker.is_some());

        let mut replaced = discovery.inventory.clone();
        replaced.tmux[0].match_key = "generation-two".into();
        assert_ne!(discovery.inventory, replaced);
        replaced = discovery.inventory.clone();
        replaced.tmux[0].attached = true;
        assert_ne!(discovery.inventory, replaced);
    }

    #[test]
    fn publication_before_worker_exit_schedules_nonblocking_retirement() {
        let context = eframe::egui::Context::default();
        let (repaint, repaint_requested) = mpsc::channel();
        context.set_request_repaint_callback(move |info| {
            let _ = repaint.send(info.delay);
        });
        let (sender, receiver) = mpsc::sync_channel(1);
        let (published, publication) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let handle = thread::spawn(move || {
            sender
                .send(Inventory {
                    errors: vec!["published result".into()],
                    ..Default::default()
                })
                .unwrap();
            published.send(()).unwrap();
            released.recv().unwrap();
        });
        publication.recv_timeout(Duration::from_secs(3)).unwrap();
        let (cancel, _cancellation) = mpsc::sync_channel(1);
        let mut discovery = Discovery {
            inventory: Inventory::default(),
            generation: 0,
            requested: false,
            enabled: true,
            worker: Some(Worker {
                generation: 0,
                receiver,
                cancel,
                handle,
                result: None,
            }),
        };
        discovery.update_with(true, &context, || panic!("worker still running"));
        // Release before assertions so a failure cannot block Discovery::drop.
        release.send(()).unwrap();
        assert_eq!(
            repaint_requested
                .recv_timeout(Duration::from_secs(3))
                .unwrap(),
            Duration::from_millis(10)
        );
        finish(&mut discovery, &context);
        assert_eq!(discovery.inventory.errors, ["published result"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_provider_timeout_is_visible_and_explicit_refresh_recovers() {
        let context = eframe::egui::Context::default();
        let mut discovery = Discovery::default();
        discovery.update_with(true, &context, || {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", "exec /bin/sleep 5"]);
            let error =
                crate::local_command::output(command, Duration::from_millis(40)).unwrap_err();
            Inventory {
                errors: vec![format!("screen: {error}")],
                ..Default::default()
            }
        });
        finish(&mut discovery, &context);
        assert_eq!(discovery.inventory.errors.len(), 1);
        let error = &discovery.inventory.errors[0];
        assert!(error.starts_with("screen:"));
        assert!(error.contains("timed out"));
        assert!(error.contains("stdout_eof=false"));
        assert!(error.contains("waiting_for_exit=false"));
        assert!(error.contains("Refresh to retry"));
        assert!(discovery.inventory.screen.is_empty());

        let generation = discovery.generation;
        for _ in 0..10_000 {
            discovery.refresh();
        }
        assert_eq!(discovery.generation, generation + 1);
        let recovered_inventory = || Inventory {
            screen: vec![MultiplexerSession {
                name: "same-owned-shell".into(),
                match_key: "123.same-owned-shell|1700000000".into(),
                attached: false,
                started_at_unix_seconds: Some(1700000000),
            }],
            ..Default::default()
        };
        discovery.update_with(true, &context, recovered_inventory);
        finish_with(&mut discovery, &context, recovered_inventory);
        assert!(discovery.inventory.errors.is_empty());
        assert_eq!(discovery.inventory.screen.len(), 1);
        assert_eq!(
            discovery.inventory.screen[0].match_key,
            "123.same-owned-shell|1700000000"
        );
        assert!(discovery.worker.is_some());
        assert!(!discovery.requested);
    }

    fn finish(discovery: &mut Discovery, context: &eframe::egui::Context) {
        finish_with(discovery, context, || {
            panic!("unexpected extra discovery worker")
        });
    }

    fn finish_with(
        discovery: &mut Discovery,
        context: &eframe::egui::Context,
        discover: fn() -> Inventory,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let worker = discovery.worker.as_ref().unwrap();
            let requested_generation_finished =
                worker.generation == discovery.generation && worker.handle.is_finished();
            discovery.update_with(true, context, discover);
            if requested_generation_finished {
                return;
            }
            assert!(Instant::now() < deadline, "discovery worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn explicit_refresh_waits_for_cancelled_worker_before_collecting_replacement() {
        let context = eframe::egui::Context::default();
        let (cancel, cancellation) = mpsc::sync_channel(1);
        let (_sender, receiver) = mpsc::sync_channel(1);
        let (cancelled, cancellation_observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let handle = thread::spawn(move || {
            cancellation.recv().unwrap();
            cancelled.send(()).unwrap();
            released.recv().unwrap();
        });
        let mut discovery = Discovery {
            enabled: true,
            inventory: Inventory::default(),
            generation: 0,
            requested: false,
            worker: Some(Worker {
                generation: 0,
                receiver,
                cancel,
                handle,
                result: None,
            }),
        };
        let replacement = || Inventory {
            errors: vec!["replacement generation".into()],
            ..Default::default()
        };
        discovery.refresh();
        let cancellation_observed = cancellation_observed.recv_timeout(Duration::from_secs(3));
        discovery.update_with(true, &context, replacement);
        let old_generation_retained = discovery.worker.as_ref().unwrap().generation == 0;
        let refresh_pending = discovery.requested;
        release.send(()).unwrap();
        cancellation_observed.unwrap();
        assert!(old_generation_retained);
        assert!(refresh_pending);
        finish_with(&mut discovery, &context, replacement);
        assert_eq!(discovery.inventory.errors, ["replacement generation"]);
        assert_eq!(
            discovery.worker.as_ref().unwrap().generation,
            discovery.generation
        );
        assert!(!discovery.requested);
    }

    #[test]
    fn rapid_refresh_is_coalesced_and_stale_results_are_discarded() {
        let mut discovery = Discovery {
            enabled: true,
            inventory: Inventory::default(),
            generation: 0,
            requested: false,
            worker: None,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let handle = thread::spawn(move || {
            sender
                .send(Inventory {
                    errors: vec!["obsolete".into()],
                    ..Default::default()
                })
                .unwrap();
        });
        while !handle.is_finished() {
            thread::yield_now();
        }
        let (cancel, _cancellation) = mpsc::sync_channel(1);
        discovery.worker = Some(Worker {
            generation: 0,
            receiver,
            cancel,
            handle,
            result: None,
        });
        for _ in 0..10_000 {
            discovery.refresh();
        }
        assert_eq!(discovery.generation, 1);
        // Disabling must clear data and retire, rather than spawn, work.
        discovery.update(false, &eframe::egui::Context::default());
        assert!(discovery.worker.is_none());
        assert!(discovery.inventory.errors.is_empty());
    }
}
