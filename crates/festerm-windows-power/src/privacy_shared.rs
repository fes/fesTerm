use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

const MAX_ACTIVE_CLIENTS: usize = 2;
const MAX_SUBSCRIPTIONS: usize = 4096;

type Callback = Arc<dyn Fn() + Send + Sync>;

pub(super) struct SharedSignals {
    lost: AtomicBool,
    next: AtomicUsize,
    // Published flags remain alive until native cleanup has joined the listener.
    flags: Box<[OnceLock<Arc<AtomicBool>>]>,
    active: [AtomicUsize; MAX_ACTIVE_CLIENTS],
}

impl SharedSignals {
    fn new() -> Self {
        Self {
            lost: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            flags: (0..MAX_SUBSCRIPTIONS)
                .map(|_| OnceLock::new())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            active: std::array::from_fn(|_| AtomicUsize::new(0)),
        }
    }

    fn register(&self, slot: usize, loss: Arc<AtomicBool>) -> io::Result<usize> {
        if self.lost.load(Ordering::SeqCst) || loss.load(Ordering::Acquire) {
            loss.store(true, Ordering::Release);
            return Err(unavailable("Windows privacy was already lost"));
        }
        let mut index = self.next.load(Ordering::SeqCst);
        loop {
            if index >= MAX_SUBSCRIPTIONS {
                loss.store(true, Ordering::Release);
                return Err(unavailable(
                    "Windows privacy monitor reached its subscription limit",
                ));
            }
            match self.next.compare_exchange_weak(
                index,
                index + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(current) => index = current,
            }
        }
        if self.flags[index].set(loss.clone()).is_err() {
            loss.store(true, Ordering::Release);
            self.revoke();
            return Err(unavailable(
                "Windows privacy subscription could not be registered",
            ));
        }
        self.active[slot].store(index + 1, Ordering::SeqCst);
        // A loss racing registration either sees this slot or closes it here
        // before registration can return.
        if self.lost.load(Ordering::SeqCst) {
            loss.store(true, Ordering::Release);
            self.detach(slot, index);
            return Err(unavailable("Windows privacy was lost during registration"));
        }
        Ok(index)
    }

    fn detach(&self, slot: usize, index: usize) {
        let _ =
            self.active[slot].compare_exchange(index + 1, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(super) fn revoke(&self) {
        self.lost.store(true, Ordering::SeqCst);
        for slot in &self.active {
            let index = slot.load(Ordering::SeqCst);
            if index != 0 {
                if let Some(flag) = self.flags[index - 1].get() {
                    flag.store(true, Ordering::Release);
                }
            }
        }
    }

    #[cfg(windows)]
    fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

struct Client {
    registration: usize,
    callback: Callback,
}

struct Subscribers {
    signals: Arc<SharedSignals>,
    clients: Mutex<[Option<Client>; MAX_ACTIVE_CLIENTS]>,
}

impl Subscribers {
    fn new() -> Self {
        Self {
            signals: Arc::new(SharedSignals::new()),
            clients: Mutex::new(std::array::from_fn(|_| None)),
        }
    }

    fn subscribe(&self, loss: Arc<AtomicBool>, callback: Callback) -> io::Result<(usize, usize)> {
        let mut clients = self.clients.lock().map_err(|_| {
            self.signals.revoke();
            loss.store(true, Ordering::Release);
            unavailable("Windows privacy subscriber state is unavailable")
        })?;
        let slot = clients.iter().position(Option::is_none).ok_or_else(|| {
            loss.store(true, Ordering::Release);
            unavailable("Windows privacy monitoring already has two clients")
        })?;
        let registration = self.signals.register(slot, loss)?;
        clients[slot] = Some(Client {
            registration,
            callback,
        });
        Ok((slot, registration))
    }

    fn unsubscribe(&self, slot: usize, registration: usize) -> io::Result<bool> {
        let (last, removed) = {
            let mut clients = self.clients.lock().map_err(|_| {
                self.signals.revoke();
                unavailable("Windows privacy subscriber state is unavailable")
            })?;
            if clients[slot]
                .as_ref()
                .is_none_or(|client| client.registration != registration)
            {
                self.signals.revoke();
                return Err(unavailable("Windows privacy subscription changed"));
            }
            self.signals.detach(slot, registration);
            let removed = clients[slot].take();
            (clients.iter().all(Option::is_none), removed)
        };
        drop(removed);
        Ok(last)
    }

    fn notify(&self) {
        let callbacks = {
            let clients = self
                .clients
                .lock()
                .expect("Windows privacy subscriber state was poisoned");
            clients
                .iter()
                .flatten()
                .map(|client| client.callback.clone())
                .collect::<Vec<_>>()
        };
        for callback in callbacks {
            callback();
        }
    }
}

fn unavailable(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, message)
}

#[cfg(windows)]
mod manager {
    use std::{
        sync::atomic::Ordering,
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        privacy::PrivacyMonitor, privacy_monitor_ownership, PrivacyCleanupState,
        PrivacyOwnershipState,
    };

    static MANAGER: OnceLock<Mutex<Manager>> = OnceLock::new();
    static BROKEN: AtomicBool = AtomicBool::new(false);
    const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Default)]
    struct Manager {
        active: Option<Active>,
    }

    struct Active {
        subscribers: Arc<Subscribers>,
        monitor: PrivacyMonitor,
    }

    struct Subscription {
        subscribers: Arc<Subscribers>,
        loss: Arc<AtomicBool>,
        slot: usize,
        registration: usize,
    }

    /// One client of the shared Windows listener. Admission is closed by the
    /// native dispatch before any subscriber callback is invoked.
    pub struct SharedPrivacyMonitor {
        subscription: Option<Subscription>,
    }

    impl SharedPrivacyMonitor {
        /// Install off the UI thread. A stopped or poisoned listener fails
        /// closed and must be explicitly installed again.
        pub fn install_with_loss_flag(
            callback: Callback,
            loss: Arc<AtomicBool>,
        ) -> io::Result<Self> {
            if BROKEN.load(Ordering::Acquire) {
                loss.store(true, Ordering::Release);
                return Err(unavailable(
                    "Windows privacy cleanup requires an app restart",
                ));
            }
            let mut manager = manager().lock().map_err(|_| {
                BROKEN.store(true, Ordering::Release);
                loss.store(true, Ordering::Release);
                unavailable("Windows privacy manager is unavailable")
            })?;
            if BROKEN.load(Ordering::Acquire) {
                loss.store(true, Ordering::Release);
                return Err(unavailable(
                    "Windows privacy cleanup requires an app restart",
                ));
            }
            if let Some(active) = &manager.active {
                if active.subscribers.signals.is_lost()
                    || active.monitor.cleanup_status().state() != PrivacyCleanupState::Active
                {
                    active.subscribers.signals.revoke();
                    loss.store(true, Ordering::Release);
                    return Err(unavailable(
                        "Windows privacy monitoring is no longer active",
                    ));
                }
                let (slot, registration) = active.subscribers.subscribe(loss.clone(), callback)?;
                if active.subscribers.signals.is_lost()
                    || active.monitor.cleanup_status().state() != PrivacyCleanupState::Active
                {
                    active.subscribers.signals.revoke();
                    loss.store(true, Ordering::Release);
                    if active.subscribers.unsubscribe(slot, registration).is_err() {
                        BROKEN.store(true, Ordering::Release);
                    }
                    return Err(unavailable("Windows privacy was lost during registration"));
                }
                return Ok(Self {
                    subscription: Some(Subscription {
                        subscribers: active.subscribers.clone(),
                        loss,
                        slot,
                        registration,
                    }),
                });
            }

            let subscribers = Arc::new(Subscribers::new());
            let (slot, registration) = subscribers.subscribe(loss.clone(), callback)?;
            let listener = subscribers.clone();
            let monitor = match PrivacyMonitor::install_shared(
                Arc::new(move || listener.notify()),
                subscribers.signals.clone(),
            ) {
                Ok(monitor) => monitor,
                Err(error) => {
                    loss.store(true, Ordering::Release);
                    subscribers.signals.revoke();
                    return Err(error);
                }
            };
            if subscribers.signals.is_lost()
                || monitor.cleanup_status().state() != PrivacyCleanupState::Active
            {
                loss.store(true, Ordering::Release);
                subscribers.signals.revoke();
                if !retire(monitor) {
                    BROKEN.store(true, Ordering::Release);
                }
                return Err(unavailable("Windows privacy was lost during installation"));
            }
            manager.active = Some(Active {
                subscribers: subscribers.clone(),
                monitor,
            });
            Ok(Self {
                subscription: Some(Subscription {
                    subscribers,
                    loss,
                    slot,
                    registration,
                }),
            })
        }

        /// Close off the UI thread; waits for native cleanup only for the last
        /// client. False means cleanup is uncertain and admission stays closed.
        pub fn close(mut self) -> bool {
            match self.subscription.take() {
                Some(subscription) => detach(subscription),
                None => false,
            }
        }
    }

    impl Drop for SharedPrivacyMonitor {
        fn drop(&mut self) {
            let Some(subscription) = self.subscription.take() else {
                return;
            };
            subscription.loss.store(true, Ordering::Release);
            let signals = subscription.subscribers.signals.clone();
            if let Err(error) = thread::Builder::new()
                .name("festerm-shared-privacy-cleanup".to_owned())
                .spawn(move || {
                    if !detach(subscription) {
                        eprintln!("fesTerm: Windows shared privacy cleanup is unresolved");
                    }
                })
            {
                signals.revoke();
                BROKEN.store(true, Ordering::Release);
                eprintln!("fesTerm: Windows shared privacy cleanup could not start: {error}");
            }
        }
    }

    fn manager() -> &'static Mutex<Manager> {
        MANAGER.get_or_init(|| Mutex::new(Manager::default()))
    }

    fn detach(subscription: Subscription) -> bool {
        subscription.loss.store(true, Ordering::Release);
        let mut manager = match manager().lock() {
            Ok(manager) => manager,
            Err(_) => {
                subscription.subscribers.signals.revoke();
                BROKEN.store(true, Ordering::Release);
                return false;
            }
        };
        let Some(active) = &manager.active else {
            subscription.subscribers.signals.revoke();
            BROKEN.store(true, Ordering::Release);
            return false;
        };
        if !Arc::ptr_eq(&active.subscribers, &subscription.subscribers) {
            subscription.subscribers.signals.revoke();
            active.subscribers.signals.revoke();
            BROKEN.store(true, Ordering::Release);
            return false;
        }
        let last = match active
            .subscribers
            .unsubscribe(subscription.slot, subscription.registration)
        {
            Ok(last) => last,
            Err(_) => {
                active.subscribers.signals.revoke();
                BROKEN.store(true, Ordering::Release);
                return false;
            }
        };
        if !last {
            return true;
        }
        let active = manager
            .active
            .take()
            .expect("the active listener was checked");
        let complete = retire(active.monitor);
        if !complete {
            BROKEN.store(true, Ordering::Release);
        }
        complete
    }

    fn retire(monitor: PrivacyMonitor) -> bool {
        let cleanup = monitor.close();
        let started = Instant::now();
        loop {
            match cleanup.state() {
                PrivacyCleanupState::Complete
                    if privacy_monitor_ownership() == PrivacyOwnershipState::Available =>
                {
                    return true;
                }
                PrivacyCleanupState::Unknown => return false,
                _ => {}
            }
            let remaining = CLEANUP_TIMEOUT.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return cleanup.quarantine_if_unresolved().is_ok()
                    && privacy_monitor_ownership() == PrivacyOwnershipState::Available;
            }
            thread::sleep(remaining.min(Duration::from_millis(50)));
        }
    }
}

#[cfg(windows)]
pub use manager::SharedPrivacyMonitor;

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Barrier},
        thread,
        time::Duration,
    };

    use super::*;

    #[test]
    fn native_loss_closes_every_client_before_a_stalled_callback() {
        let subscribers = Arc::new(Subscribers::new());
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let (entered, waiting) = mpsc::sync_channel(1);
        let (release, blocked) = mpsc::sync_channel::<()>(1);
        let blocked = Mutex::new(blocked);
        subscribers
            .subscribe(
                first.clone(),
                Arc::new(move || {
                    entered.send(()).unwrap();
                    blocked.lock().unwrap().recv().unwrap();
                }),
            )
            .unwrap();
        subscribers
            .subscribe(second.clone(), Arc::new(|| {}))
            .unwrap();
        let listener = subscribers.clone();
        let worker = thread::spawn(move || {
            listener.signals.revoke();
            listener.notify();
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.load(Ordering::Acquire));
        assert!(second.load(Ordering::Acquire));
        release.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn replaced_subscriber_gets_a_new_one_way_loss_flag() {
        let subscribers = Subscribers::new();
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        let replacement = Arc::new(AtomicBool::new(false));
        let (slot, registration) = subscribers
            .subscribe(first.clone(), Arc::new(|| {}))
            .unwrap();
        subscribers
            .subscribe(second.clone(), Arc::new(|| {}))
            .unwrap();
        first.store(true, Ordering::Release);
        assert!(!subscribers.unsubscribe(slot, registration).unwrap());
        subscribers
            .subscribe(replacement.clone(), Arc::new(|| {}))
            .unwrap();
        subscribers.signals.revoke();
        assert!(first.load(Ordering::Acquire));
        assert!(second.load(Ordering::Acquire));
        assert!(replacement.load(Ordering::Acquire));
    }

    #[test]
    fn subscription_limit_remains_closed_after_repeated_attempts() {
        let subscribers = Subscribers::new();
        subscribers
            .signals
            .next
            .store(MAX_SUBSCRIPTIONS - 1, Ordering::SeqCst);
        let (slot, registration) = subscribers
            .subscribe(Arc::new(AtomicBool::new(false)), Arc::new(|| {}))
            .unwrap();
        subscribers.unsubscribe(slot, registration).unwrap();
        for _ in 0..2 {
            let loss = Arc::new(AtomicBool::new(false));
            assert!(subscribers
                .subscribe(loss.clone(), Arc::new(|| {}))
                .is_err());
            assert!(loss.load(Ordering::Acquire));
        }
        assert_eq!(
            subscribers.signals.next.load(Ordering::SeqCst),
            MAX_SUBSCRIPTIONS
        );
    }

    #[test]
    fn concurrent_loss_and_subscription_cannot_leave_an_open_flag() {
        for _ in 0..256 {
            let subscribers = Arc::new(Subscribers::new());
            let loss = Arc::new(AtomicBool::new(false));
            let barrier = Arc::new(Barrier::new(2));
            thread::scope(|scope| {
                let listener = subscribers.clone();
                let start = barrier.clone();
                scope.spawn(move || {
                    start.wait();
                    listener.signals.revoke();
                });
                barrier.wait();
                let _ = subscribers.subscribe(loss.clone(), Arc::new(|| {}));
            });
            assert!(loss.load(Ordering::Acquire));
        }
    }
}
