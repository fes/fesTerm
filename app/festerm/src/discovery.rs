//! Bounded, coalesced Launcher inventory work. No provider I/O runs in a frame.

use std::{
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::multiplexer_sessions::{self, MultiplexerSession};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
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
    next_refresh: Option<Instant>,
    worker: Option<(u64, Receiver<Inventory>, JoinHandle<()>)>,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some((_, _, handle)) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Discovery {
    pub fn refresh(&mut self) {
        // One pending refresh, not one queued request per gesture.
        if !self.requested {
            self.generation = self.generation.wrapping_add(1);
            self.requested = true;
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
        discover: impl FnOnce() -> Inventory + Send + 'static,
    ) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.inventory = Inventory::default();
            self.refresh();
        }
        if let Some((_, _, handle)) = &self.worker {
            if handle.is_finished() {
                let (generation, receiver, handle) = self.worker.take().unwrap();
                let joined = handle.join();
                if enabled && generation == self.generation {
                    match (joined, receiver.try_recv()) {
                        (Ok(()), Ok(inventory)) => self.inventory = inventory,
                        _ => {
                            self.inventory = Inventory::default();
                            self.inventory.errors.push(
                                "Running Sessions discovery failed; Refresh to retry.".into(),
                            );
                        }
                    }
                }
            }
        }
        if !enabled {
            return;
        }
        let now = Instant::now();
        if self.worker.is_none()
            && (self.requested || self.next_refresh.is_none_or(|deadline| now >= deadline))
        {
            self.requested = false;
            let (sender, receiver) = mpsc::sync_channel(1);
            let context = context.clone();
            let handle = thread::Builder::new()
                .name("running-session-discovery".into())
                .spawn(move || {
                    let _ = sender.send(discover());
                    context.request_repaint();
                });
            match handle {
                Ok(handle) => self.worker = Some((self.generation, receiver, handle)),
                Err(error) => {
                    self.inventory = Inventory::default();
                    self.inventory.errors.push(format!(
                        "Could not start Running Sessions discovery: {error}. Refresh to retry."
                    ));
                }
            }
            self.next_refresh = Some(now + REFRESH_INTERVAL);
        }
        context.request_repaint_after(REFRESH_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        discovery.update_with(true, &context, || Inventory {
            screen: vec![MultiplexerSession {
                name: "same-owned-shell".into(),
                match_key: "123.same-owned-shell|1700000000".into(),
                attached: false,
                started_at_unix_seconds: Some(1700000000),
            }],
            ..Default::default()
        });
        finish(&mut discovery, &context);
        assert!(discovery.inventory.errors.is_empty());
        assert_eq!(discovery.inventory.screen.len(), 1);
        assert_eq!(
            discovery.inventory.screen[0].match_key,
            "123.same-owned-shell|1700000000"
        );
        assert!(discovery.worker.is_none());
        assert!(!discovery.requested);
    }

    #[cfg(unix)]
    fn finish(discovery: &mut Discovery, context: &eframe::egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !discovery.worker.as_ref().unwrap().2.is_finished() {
            assert!(Instant::now() < deadline, "discovery worker did not finish");
            thread::sleep(Duration::from_millis(1));
        }
        discovery.next_refresh = Some(Instant::now() + Duration::from_secs(60));
        discovery.update_with(true, context, || {
            panic!("unexpected extra discovery worker")
        });
    }

    #[test]
    fn rapid_refresh_is_coalesced_and_stale_results_are_discarded() {
        let mut discovery = Discovery {
            enabled: true,
            inventory: Inventory::default(),
            generation: 0,
            requested: false,
            next_refresh: None,
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
        discovery.worker = Some((0, receiver, handle));
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
