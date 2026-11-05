use std::{
    fmt, io,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, Mutex, TryLockError,
    },
    thread::{self, JoinHandle},
};

use super::PrivacyRevocation;

const AVAILABLE: u8 = 0;
const OCCUPIED: u8 = 1;
const QUARANTINED: u8 = 2;

const STARTING: u8 = 0;
const ACTIVE: u8 = 1;
const STOPPING: u8 = 2;
const COMPLETE: u8 = 3;
const UNKNOWN: u8 = 4;

/// Process-slot ownership only. Neither this state nor callback delivery
/// qualifies admission, privacy, or recipient fencing; MCP bootstrap remains
/// closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyOwnershipState {
    Available,
    Occupied,
    Unknown,
}

/// Monitor cleanup only. Neither this state nor callback delivery qualifies
/// admission, privacy, or recipient fencing; MCP bootstrap remains closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivacyCleanupState {
    Active,
    Pending,
    Complete,
    Unknown,
}

struct Slot {
    state: AtomicU8,
    failed_submission: AtomicPtr<Job>,
}

impl fmt::Debug for Slot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Slot")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Slot {
    const fn new() -> Self {
        Self {
            state: AtomicU8::new(AVAILABLE),
            failed_submission: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn reserve(&self) -> Option<Permit<'_>> {
        self.state
            .compare_exchange(AVAILABLE, OCCUPIED, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Permit {
                slot: self,
                armed: true,
            })
    }

    fn quarantine(&self) {
        self.state.store(QUARANTINED, Ordering::Release);
    }

    fn retain_failed_submission(&'static self, job: Job) {
        // Only the unique reserved permit can reach this path. Quarantine is
        // sticky, so this process-lifetime slot receives at most one job. Its
        // pointer owns the allocation permanently; no captured destructor
        // runs on the submitting thread and no native resource is discarded.
        self.failed_submission
            .store(Box::into_raw(Box::new(job)), Ordering::Release);
    }

    fn state(&self) -> PrivacyOwnershipState {
        match self.state.load(Ordering::Acquire) {
            AVAILABLE => PrivacyOwnershipState::Available,
            OCCUPIED => PrivacyOwnershipState::Occupied,
            _ => PrivacyOwnershipState::Unknown,
        }
    }
}

struct Permit<'a> {
    slot: &'a Slot,
    armed: bool,
}

impl Permit<'_> {
    fn release_after_join(mut self) {
        let _ = self.slot.state.compare_exchange(
            OCCUPIED,
            AVAILABLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.armed = false;
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.slot.quarantine();
        }
    }
}

#[derive(Debug)]
pub(super) struct Lifecycle {
    slot: &'static Slot,
    phase: AtomicU8,
    cancelled: AtomicBool,
}

impl Lifecycle {
    fn new(slot: &'static Slot) -> Self {
        Self {
            slot,
            phase: AtomicU8::new(STARTING),
            cancelled: AtomicBool::new(false),
        }
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn activate(&self) -> bool {
        !self.cancelled()
            && self
                .phase
                .compare_exchange(STARTING, ACTIVE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    pub(super) fn stop(&self) -> bool {
        let first = !self.cancelled.swap(true, Ordering::AcqRel);
        // The ordered states prevent a late close from undoing completion or
        // quarantine.
        let _ = self.phase.fetch_max(STOPPING, Ordering::AcqRel);
        first
    }

    pub(super) fn unknown(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.phase.store(UNKNOWN, Ordering::Release);
        self.slot.quarantine();
    }

    fn complete(&self) -> bool {
        self.phase
            .compare_exchange(STOPPING, COMPLETE, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn state(&self) -> PrivacyCleanupState {
        match self.phase.load(Ordering::Acquire) {
            ACTIVE if !self.cancelled() => PrivacyCleanupState::Active,
            COMPLETE => PrivacyCleanupState::Complete,
            UNKNOWN => PrivacyCleanupState::Unknown,
            _ => PrivacyCleanupState::Pending,
        }
    }
}

/// A nonblocking observation of one monitor's cleanup.
///
/// `Pending` does not prove that the listener or its TLS destructors finished.
/// `Unknown` is sticky and permanently prevents reuse of the process slot.
/// Neither this status nor callback entry or completion establishes an
/// admission fence, qualified privacy, or a physical final-recipient fence.
/// A callback can block on the admission lock. MCP bootstrap remains closed.
#[derive(Clone, Debug)]
pub struct PrivacyCleanupStatus(Arc<Lifecycle>);

impl PrivacyCleanupStatus {
    /// Observes cleanup only, not admission, privacy, or recipient fencing.
    /// Callback delivery does not qualify those fences; bootstrap remains closed.
    pub fn state(&self) -> PrivacyCleanupState {
        self.0.state()
    }

    /// Returns an explicit unresolved state rather than treating it as success.
    /// Completion is not an admission, privacy, or physical-recipient fence;
    /// callback delivery does not qualify one, and bootstrap remains closed.
    pub fn require_complete(&self) -> Result<(), PrivacyCleanupState> {
        match self.state() {
            PrivacyCleanupState::Complete => Ok(()),
            state => Err(state),
        }
    }

    /// At the owner's deadline, permanently quarantine an unresolved cleanup.
    /// A cleanup that already completed is left unchanged.
    /// This does not fence admission, privacy, or physical recipient writes;
    /// callback delivery does not qualify those fences, and bootstrap remains
    /// closed.
    pub fn quarantine_if_unresolved(&self) -> Result<(), PrivacyCleanupState> {
        self.0.cancelled.store(true, Ordering::Release);
        let mut phase = self.0.phase.load(Ordering::Acquire);
        // Every competing transition is monotonic, so a failed strong CAS
        // can only advance toward COMPLETE or UNKNOWN.
        loop {
            match phase {
                COMPLETE => return Ok(()),
                UNKNOWN => return Err(PrivacyCleanupState::Unknown),
                _ => match self.0.phase.compare_exchange(
                    phase,
                    UNKNOWN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        self.0.slot.quarantine();
                        return Err(PrivacyCleanupState::Unknown);
                    }
                    Err(current) => phase = current,
                },
            }
        }
    }

    pub(super) fn stop(&self) {
        let _ = self.0.stop();
    }

    pub(super) fn is_active(&self) -> bool {
        self.0.state() == PrivacyCleanupState::Active && !self.0.cancelled()
    }
}

/// Caller-owned inputs rejected before a listener slot was reserved.
///
/// Dropping this value can run caller-supplied destructors and may block or
/// panic. The caller must retain or dispose of it in an appropriate context.
/// Neither cleanup status nor callback delivery qualifies admission, privacy,
/// or recipient fencing; MCP bootstrap remains closed.
pub struct RejectedPrivacyInstall {
    _revocation: Arc<PrivacyRevocation>,
    _task: Task,
}

/// Details attached to an installation error.
///
/// Cleanup status and callback delivery do not qualify admission, privacy, or
/// physical-recipient fencing; MCP bootstrap remains closed.
pub struct PrivacyInstallFailure {
    cleanup: Option<PrivacyCleanupStatus>,
    rejected: Option<Mutex<RejectedPrivacyInstall>>,
}

impl fmt::Debug for PrivacyInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivacyInstallFailure")
            .field("cleanup", &self.cleanup)
            .field("has_rejected_inputs", &self.rejected.is_some())
            .finish()
    }
}

impl PrivacyInstallFailure {
    /// Returns cleanup state only; it is not an admission, privacy, or
    /// recipient fence. Callback delivery does not qualify one; bootstrap
    /// remains closed.
    pub fn cleanup_status(&self) -> Option<PrivacyCleanupStatus> {
        self.cleanup.clone()
    }

    /// Returns inputs that never acquired the process slot. Their disposal is
    /// the caller's responsibility and is not guaranteed to be nonblocking.
    /// Cleanup status and callback delivery do not qualify admission, privacy,
    /// or recipient fencing; bootstrap remains closed.
    pub fn take_rejected(&mut self) -> Option<RejectedPrivacyInstall> {
        self.rejected.take().map(|rejected| {
            rejected
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
    }
}

impl fmt::Display for PrivacyInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Windows privacy monitor unavailable or cleanup unresolved")
    }
}

impl std::error::Error for PrivacyInstallFailure {}

pub(super) fn unavailable(cleanup: Option<PrivacyCleanupStatus>) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        PrivacyInstallFailure {
            cleanup,
            rejected: None,
        },
    )
}

fn rejected(revocation: Arc<PrivacyRevocation>, task: Task) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        PrivacyInstallFailure {
            cleanup: None,
            rejected: Some(Mutex::new(RejectedPrivacyInstall {
                _revocation: revocation,
                _task: task,
            })),
        },
    )
}

type Task = Box<dyn FnOnce(Arc<Lifecycle>, Arc<PrivacyRevocation>) -> bool + Send>;

struct Ownership {
    run: Arc<Lifecycle>,
    permit: Option<Permit<'static>>,
}

impl Drop for Ownership {
    fn drop(&mut self) {
        if self.permit.is_some() {
            self.run.unknown();
        }
    }
}

struct Job {
    // Rust drops fields in declaration order; latch unknown before dropping
    // either caller-supplied closure on an unexpected unwind.
    ownership: Ownership,
    revocation: Arc<PrivacyRevocation>,
    task: Task,
}

struct BodyFinished(Arc<Lifecycle>);

impl Drop for BodyFinished {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}

struct ReaperExit(&'static Slot);

impl Drop for ReaperExit {
    fn drop(&mut self) {
        self.0.quarantine();
    }
}

pub(super) fn retain_forever<T>(resource: T) -> ! {
    loop {
        std::hint::black_box(&resource);
        thread::park();
    }
}

pub(super) fn invoke_revocation(revocation: &PrivacyRevocation, lifecycle: &Lifecycle) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| revocation.revoke())) {
        lifecycle.unknown();
        #[cfg(test)]
        REVOCATION_PANIC_CAUGHT.store(true, Ordering::Release);
        retain_forever(payload);
    }
}

#[cfg(test)]
static REVOCATION_PANIC_CAUGHT: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static FAIL_LISTENER_SPAWN: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static FAIL_LISTENER_WAIT: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static WAIT_FAILURE_AFTER_ACTIVATION: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static WAIT_FAILURE_CALLBACK_ATTEMPT_RETURNED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
static POST_JOIN_QUARANTINE: AtomicBool = AtomicBool::new(false);

fn spawn_listener(task: impl FnOnce() -> bool + Send + 'static) -> io::Result<JoinHandle<bool>> {
    #[cfg(test)]
    if FAIL_LISTENER_SPAWN.swap(false, Ordering::AcqRel) {
        return Err(io::Error::other("injected listener spawn failure"));
    }
    thread::Builder::new()
        .name("festerm-privacy-monitor".into())
        .spawn(task)
}

fn wait_for_termination(listener: &JoinHandle<bool>) -> bool {
    #[cfg(test)]
    if FAIL_LISTENER_WAIT.swap(false, Ordering::AcqRel) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while !WAIT_FAILURE_AFTER_ACTIVATION.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            thread::sleep(std::time::Duration::from_millis(1));
        }
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};

        // SAFETY: the reaper exclusively owns `listener`, so its native
        // thread handle remains open until this wait returns.
        unsafe { WaitForSingleObject(listener.as_raw_handle(), INFINITE) == WAIT_OBJECT_0 }
    }
    #[cfg(not(windows))]
    {
        // This branch is only the portable model; production is Windows-only.
        let _ = listener;
        true
    }
}

fn run_job(job: Job) {
    let Job {
        ownership,
        revocation,
        task,
    } = job;
    let mut ownership = ownership;
    let run = ownership.run.clone();
    let listener_run = run.clone();
    let listener_revocation = revocation.clone();
    let task_storage = Arc::new(Mutex::new(Some(task)));
    let listener_task = task_storage.clone();
    let listener = spawn_listener(move || {
        let _finished = BodyFinished(listener_run.clone());
        let task = match listener_task.try_lock() {
            Ok(mut pending) => pending.take(),
            Err(_) => None,
        };
        let Some(task) = task else {
            listener_run.unknown();
            return false;
        };
        drop(listener_task);
        task(listener_run, listener_revocation)
    });
    let listener = match listener {
        Ok(listener) => listener,
        Err(_) => {
            run.unknown();
            invoke_revocation(&revocation, &run);
            retain_forever((ownership, revocation, task_storage));
        }
    };

    if !wait_for_termination(&listener) {
        run.unknown();
        // Unknown and callback entry do not fence external admission: the
        // callback can wait on that admission lock. Bootstrap remains closed.
        invoke_revocation(&revocation, &run);
        #[cfg(test)]
        WAIT_FAILURE_CALLBACK_ATTEMPT_RETURNED.store(true, Ordering::Release);
        retain_forever((ownership, revocation, task_storage, listener));
    }
    // This is the sole join owner. The permit remains held while join waits
    // through listener TLS destruction, including listener panic or stalls.
    let joined = match catch_unwind(AssertUnwindSafe(|| listener.join())) {
        Ok(joined) => joined,
        Err(payload) => {
            run.unknown();
            invoke_revocation(&revocation, &run);
            retain_forever((ownership, revocation, task_storage, payload));
        }
    };
    match joined {
        Ok(true) => {}
        Ok(false) => {
            run.unknown();
            invoke_revocation(&revocation, &run);
            retain_forever((ownership, revocation, task_storage));
        }
        Err(payload) => {
            run.unknown();
            invoke_revocation(&revocation, &run);
            retain_forever((ownership, revocation, task_storage, payload));
        }
    }
    let task_taken = match task_storage.try_lock() {
        Ok(pending) => pending.is_none(),
        Err(_) => false,
    };
    if !task_taken {
        run.unknown();
        invoke_revocation(&revocation, &run);
        retain_forever((ownership, revocation, task_storage));
    }
    drop(task_storage);
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(revocation))) {
        run.unknown();
        retain_forever((ownership, payload));
    }
    if !run.complete() {
        run.unknown();
        #[cfg(test)]
        POST_JOIN_QUARANTINE.store(true, Ordering::Release);
        retain_forever(ownership);
    }
    if let Some(permit) = ownership.permit.take() {
        permit.release_after_join();
    } else {
        run.unknown();
    }
}

struct Supervisor {
    sender: SyncSender<Job>,
    _reaper: JoinHandle<()>,
}

#[cfg(test)]
static FAIL_REAPER_SPAWN: AtomicBool = AtomicBool::new(false);

impl Supervisor {
    fn spawn(slot: &'static Slot) -> io::Result<Self> {
        #[cfg(test)]
        if FAIL_REAPER_SPAWN.swap(false, Ordering::AcqRel) {
            return Err(io::Error::other("injected reaper spawn failure"));
        }
        let (sender, receiver): (SyncSender<Job>, Receiver<Job>) = mpsc::sync_channel(1);
        let reaper = thread::Builder::new()
            .name("festerm-privacy-reaper".into())
            .spawn(move || {
                let _exit = ReaperExit(slot);
                while let Ok(job) = receiver.recv() {
                    run_job(job);
                }
            })?;
        Ok(Self {
            sender,
            _reaper: reaper,
        })
    }
}

fn reserve_job(
    slot: &'static Slot,
    revocation: Arc<PrivacyRevocation>,
    task: Task,
) -> io::Result<Job> {
    let Some(permit) = slot.reserve() else {
        return Err(rejected(revocation, task));
    };
    let run = Arc::new(Lifecycle::new(slot));
    Ok(Job {
        ownership: Ownership {
            run,
            permit: Some(permit),
        },
        revocation,
        task,
    })
}

fn quarantine_submission(job: Job) -> io::Error {
    let run = job.ownership.run.clone();
    run.unknown();
    run.slot.retain_failed_submission(job);
    unavailable(Some(PrivacyCleanupStatus(run)))
}

fn enqueue(sender: &SyncSender<Job>, job: Job) -> io::Result<PrivacyCleanupStatus> {
    let status = PrivacyCleanupStatus(job.ownership.run.clone());
    match sender.try_send(job) {
        Ok(()) => Ok(status),
        Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
            Err(quarantine_submission(job))
        }
    }
}

#[cfg(test)]
fn submit(
    slot: &'static Slot,
    sender: &SyncSender<Job>,
    revocation: Arc<PrivacyRevocation>,
    task: Task,
) -> io::Result<PrivacyCleanupStatus> {
    let job = reserve_job(slot, revocation, task)?;
    enqueue(sender, job)
}

static SLOT: Slot = Slot::new();
// The static retains the one reaper handle even if the reaper itself exits.
static SUPERVISOR: Mutex<Option<Supervisor>> = Mutex::new(None);

/// Reports slot ownership only. Neither this result nor callback delivery
/// qualifies admission, privacy, or physical-recipient fencing; bootstrap
/// remains closed.
pub fn privacy_monitor_ownership() -> PrivacyOwnershipState {
    SLOT.state()
}

pub(super) fn launch(
    revocation: Arc<PrivacyRevocation>,
    task: impl FnOnce(Arc<Lifecycle>, Arc<PrivacyRevocation>) -> bool + Send + 'static,
) -> io::Result<PrivacyCleanupStatus> {
    let job = reserve_job(&SLOT, revocation, Box::new(task))?;
    let mut supervisor = match SUPERVISOR.try_lock() {
        Ok(supervisor) => supervisor,
        Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
            return Err(quarantine_submission(job));
        }
    };
    if supervisor.is_none() {
        match Supervisor::spawn(&SLOT) {
            Ok(started) => *supervisor = Some(started),
            Err(_) => {
                drop(supervisor);
                return Err(quarantine_submission(job));
            }
        }
    }
    let sender = match supervisor.as_ref() {
        Some(supervisor) => supervisor.sender.clone(),
        None => {
            drop(supervisor);
            return Err(quarantine_submission(job));
        }
    };
    drop(supervisor);
    enqueue(&sender, job)
}

#[cfg(all(test, windows))]
pub(super) fn test_lifecycle() -> Arc<Lifecycle> {
    static TEST_SLOT: Slot = Slot::new();
    Arc::new(Lifecycle::new(&TEST_SLOT))
}

#[cfg(test)]
mod tests {
    // These are ownership/notification models, not proofs of admission,
    // privacy, or final-recipient fencing. Callback delivery is not such a
    // fence; production MCP bootstrap remains closed.
    use super::*;
    use std::cell::RefCell;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    const WAIT: Duration = Duration::from_secs(3);
    const CHILD_TIMEOUT: Duration = Duration::from_secs(15);
    const CHILD_CASE: &str = "FESTERM_PRIVACY_LIFECYCLE_TEST_CASE";
    static MODEL_SLOT: Slot = Slot::new();

    struct TlsStall {
        entered: SyncSender<()>,
        release: Receiver<()>,
    }

    impl Drop for TlsStall {
        fn drop(&mut self) {
            let _ = self.entered.try_send(());
            let _ = self.release.recv_timeout(Duration::from_secs(8));
        }
    }

    thread_local! {
        static STALLED_TLS: RefCell<Option<TlsStall>> = const { RefCell::new(None) };
    }

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct DropStall {
        entered: SyncSender<()>,
        release: Arc<AtomicBool>,
    }

    impl Drop for DropStall {
        fn drop(&mut self) {
            let _ = self.entered.try_send(());
            while !self.release.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    struct DropPanic;

    impl Drop for DropPanic {
        fn drop(&mut self) {
            panic!("injected callback cleanup panic");
        }
    }

    enum DropFault {
        None,
        Stall(Arc<AtomicBool>),
        Panic,
    }

    struct FaultyDrop {
        drops: Arc<AtomicUsize>,
        fault: DropFault,
    }

    impl Drop for FaultyDrop {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            match &self.fault {
                DropFault::None => {}
                DropFault::Stall(release) => {
                    while !release.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                DropFault::Panic => panic!("injected retained-input destructor panic"),
            }
        }
    }

    struct OwnedChild(Option<Child>);

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn revocation() -> Arc<PrivacyRevocation> {
        Arc::new(PrivacyRevocation::new(Arc::new(|| {})))
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !condition() {
            assert!(Instant::now() < deadline, "lifecycle transition timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn start(
        supervisor: &Supervisor,
        task: impl FnOnce(Arc<Lifecycle>, Arc<PrivacyRevocation>) -> bool + Send + 'static,
    ) -> PrivacyCleanupStatus {
        submit(
            &MODEL_SLOT,
            &supervisor.sender,
            revocation(),
            Box::new(task),
        )
        .expect("model worker should be accepted")
    }

    fn assert_refused(supervisor: &Supervisor) {
        for _ in 0..64 {
            assert!(submit(
                &MODEL_SLOT,
                &supervisor.sender,
                revocation(),
                Box::new(|_, _| true),
            )
            .is_err());
        }
    }

    fn stop_supervisor(supervisor: Supervisor) {
        let Supervisor { sender, _reaper } = supervisor;
        drop(sender);
        assert!(_reaper.join().is_ok());
    }

    fn tls_then_repeated_monitor_slot_reservation() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let status = start(&supervisor, move |run, _| {
            assert!(run.activate());
            STALLED_TLS.with(|slot| {
                *slot.borrow_mut() = Some(TlsStall {
                    entered,
                    release: release_rx,
                });
            });
            true
        });
        entered_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(status.state(), PrivacyCleanupState::Pending);
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Pending));
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Occupied);
        status.stop();
        assert_refused(&supervisor);

        release.send(()).unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        assert_eq!(status.require_complete(), Ok(()));
        let completed = status.clone();

        for _ in 0..16 {
            let (entered, entered_rx) = mpsc::sync_channel(1);
            let status = start(&supervisor, move |run, _| {
                assert!(run.activate());
                entered.send(()).unwrap();
                while !run.cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                true
            });
            entered_rx.recv_timeout(WAIT).unwrap();
            assert!(status.is_active());
            assert_eq!(completed.quarantine_if_unresolved(), Ok(()));
            assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Occupied);
            status.stop();
            wait_for(|| {
                status.state() == PrivacyCleanupState::Complete
                    && MODEL_SLOT.state() == PrivacyOwnershipState::Available
            });
        }
        stop_supervisor(supervisor);
    }

    fn blocked_admission_lock() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let admission = Arc::new(Mutex::new(()));
        let held = admission.clone();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let status = start(&supervisor, move |run, _| {
            let _guard = held.lock().unwrap();
            assert!(run.activate());
            entered.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
            true
        });
        entered_rx.recv_timeout(WAIT).unwrap();
        assert!(admission.try_lock().is_err());
        let close_status = status.clone();
        let (closed, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            close_status.stop();
            closed.send(()).unwrap();
        });
        closed_rx.recv_timeout(WAIT).unwrap();
        closer.join().unwrap();
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Pending));
        assert_refused(&supervisor);
        release.send(()).unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        assert!(admission.try_lock().is_ok());
        let next = start(&supervisor, |_, _| true);
        wait_for(|| {
            next.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        stop_supervisor(supervisor);
    }

    fn cancelled_during_startup() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let status = start(&supervisor, move |run, _| {
            entered.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
            assert!(run.cancelled());
            assert!(!run.activate());
            true
        });
        entered_rx.recv_timeout(WAIT).unwrap();
        status.stop();
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Pending));
        assert_refused(&supervisor);
        release.send(()).unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        stop_supervisor(supervisor);
    }

    fn concurrent_monitor_slot_reservation() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let release = Arc::new(AtomicBool::new(false));
        let (results, results_rx) = mpsc::channel();
        let mut callers = Vec::new();
        for _ in 0..8 {
            let barrier = barrier.clone();
            let release = release.clone();
            let results = results.clone();
            let sender = supervisor.sender.clone();
            callers.push(thread::spawn(move || {
                barrier.wait();
                let result = submit(
                    &MODEL_SLOT,
                    &sender,
                    revocation(),
                    Box::new(move |run, _| {
                        assert!(run.activate());
                        while !release.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(1));
                        }
                        true
                    }),
                );
                results.send(result).unwrap();
            }));
        }
        drop(results);
        barrier.wait();
        let mut accepted = Vec::new();
        for _ in 0..8 {
            if let Ok(status) = results_rx.recv_timeout(WAIT).unwrap() {
                accepted.push(status);
            }
        }
        for caller in callers {
            caller.join().unwrap();
        }
        assert_eq!(accepted.len(), 1);
        assert_refused(&supervisor);
        release.store(true, Ordering::Release);
        let status = accepted.pop().unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        stop_supervisor(supervisor);
    }

    fn unresolved_native_cleanup(startup: bool) {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let registration = DropProbe(drops.clone());
        let joined = Arc::new(AtomicUsize::new(0));
        let joined_probe = DropProbe(joined.clone());
        let revocation = Arc::new(PrivacyRevocation::new(Arc::new(move || {
            std::hint::black_box(&joined_probe);
        })));
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            revocation,
            Box::new(move |run, _| {
                if !startup {
                    assert!(run.activate());
                }
                run.unknown();
                entered.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
                drop(registration);
                true
            }),
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Unknown));
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_refused(&supervisor);
        release.send(()).unwrap();
        wait_for(|| {
            drops.load(Ordering::SeqCst) == 1
                && joined.load(Ordering::SeqCst) == 1
                && POST_JOIN_QUARANTINE.load(Ordering::Acquire)
        });
        assert_eq!(status.state(), PrivacyCleanupState::Unknown);
        assert_refused(&supervisor);
    }

    fn shutdown_deadline_quarantine() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let joined = Arc::new(AtomicUsize::new(0));
        let joined_probe = DropProbe(joined.clone());
        let revocation = Arc::new(PrivacyRevocation::new(Arc::new(move || {
            std::hint::black_box(&joined_probe);
        })));
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            revocation,
            Box::new(move |run, _| {
                assert!(run.activate());
                entered.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(8)).unwrap();
                true
            }),
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        status.stop();
        assert_eq!(
            status.quarantine_if_unresolved(),
            Err(PrivacyCleanupState::Unknown)
        );
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
        assert_refused(&supervisor);
        release.send(()).unwrap();
        wait_for(|| {
            joined.load(Ordering::SeqCst) == 1 && POST_JOIN_QUARANTINE.load(Ordering::Acquire)
        });
        assert_eq!(status.state(), PrivacyCleanupState::Unknown);
        assert_refused(&supervisor);
    }

    fn callback_destructor_stall() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new(AtomicBool::new(false));
        let stall = DropStall {
            entered,
            release: release.clone(),
        };
        let callback = Arc::new(move || {
            std::hint::black_box(&stall);
        });
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            Arc::new(PrivacyRevocation::new(callback)),
            Box::new(|_, _| true),
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Pending));
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Occupied);
        status.stop();
        assert_refused(&supervisor);
        release.store(true, Ordering::Release);
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        stop_supervisor(supervisor);
    }

    fn callback_destructor_panic() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let probe = DropPanic;
        let callback = Arc::new(move || {
            std::hint::black_box(&probe);
        });
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            Arc::new(PrivacyRevocation::new(callback)),
            Box::new(|_, _| true),
        )
        .unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Unknown
                && MODEL_SLOT.state() == PrivacyOwnershipState::Unknown
        });
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Unknown));
        assert_refused(&supervisor);
    }

    fn worker_failure(case: &str) {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let probe = DropProbe(drops.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let callback = Arc::new(move || {
            std::hint::black_box(&probe);
            callback_calls.fetch_add(1, Ordering::SeqCst);
        });
        let revocation = Arc::new(PrivacyRevocation::new(callback));
        let spawn_failure = case.starts_with("listener-spawn");
        if spawn_failure {
            FAIL_LISTENER_SPAWN.store(true, Ordering::Release);
        }
        let panic_worker = case == "worker-panic";
        let task_drops = Arc::new(AtomicUsize::new(0));
        let task_probe = FaultyDrop {
            drops: task_drops.clone(),
            fault: match case {
                "listener-spawn-stall" => DropFault::Stall(Arc::new(AtomicBool::new(false))),
                "listener-spawn-panic" => DropFault::Panic,
                _ => DropFault::None,
            },
        };
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            revocation,
            Box::new(move |run, _| {
                std::hint::black_box(&task_probe);
                assert!(run.activate());
                assert!(!panic_worker, "injected listener failure");
                false
            }),
        )
        .unwrap();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Unknown
                && MODEL_SLOT.state() == PrivacyOwnershipState::Unknown
                && calls.load(Ordering::SeqCst) == 1
        });
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        if spawn_failure {
            assert_eq!(task_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Unknown));
        assert_refused(&supervisor);
    }

    fn listener_wait_failure(case: &str) {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let (called, called_rx) = mpsc::sync_channel(1);
        let (worker_done, worker_done_rx) = mpsc::sync_channel(1);
        let (release_worker, worker_release) = mpsc::sync_channel(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let release_callback = Arc::new(AtomicBool::new(false));
        let callback_release = release_callback.clone();
        let stall = case == "listener-wait-callback-stall";
        let panic = case == "listener-wait-callback-panic";
        let callback = Arc::new(move || {
            callback_calls.fetch_add(1, Ordering::SeqCst);
            let name = thread::current().name().map(str::to_owned);
            let _ = called.try_send(name);
            while stall && !callback_release.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(!panic, "injected supervisor revocation panic");
        });
        FAIL_LISTENER_WAIT.store(true, Ordering::Release);
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            Arc::new(PrivacyRevocation::new(callback)),
            Box::new(move |run, revocation| {
                assert!(run.activate());
                entered.send(()).unwrap();
                WAIT_FAILURE_AFTER_ACTIVATION.store(true, Ordering::Release);
                while !run.cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                worker_release.recv_timeout(Duration::from_secs(8)).unwrap();
                revocation.revoke();
                worker_done.send(()).unwrap();
                true
            }),
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(
            called_rx.recv_timeout(WAIT).unwrap().as_deref(),
            Some("festerm-privacy-reaper")
        );
        wait_for(|| {
            status.state() == PrivacyCleanupState::Unknown
                && MODEL_SLOT.state() == PrivacyOwnershipState::Unknown
        });
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Unknown));
        assert_refused(&supervisor);
        status.stop();
        release_callback.store(true, Ordering::Release);
        release_worker.send(()).unwrap();
        worker_done_rx.recv_timeout(WAIT).unwrap();
        if panic {
            wait_for(|| REVOCATION_PANIC_CAUGHT.load(Ordering::Acquire));
        } else {
            wait_for(|| WAIT_FAILURE_CALLBACK_ATTEMPT_RETURNED.load(Ordering::Acquire));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
    }

    fn wait_failure_does_not_fence_model_admission() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let admission = Arc::new(Mutex::new(()));
        let admitted = admission.lock().unwrap();
        let open = Arc::new(AtomicBool::new(true));
        let callback_lock = admission.clone();
        let callback_open = open.clone();
        let (callback_entered, callback_entered_rx) = mpsc::sync_channel(1);
        let (gate_closed, gate_closed_rx) = mpsc::sync_channel(1);
        let callback = Arc::new(move || {
            let _ = callback_entered.try_send(());
            let _guard = callback_lock.lock().unwrap();
            callback_open.store(false, Ordering::Release);
            let _ = gate_closed.try_send(());
        });
        let (release_worker, worker_release) = mpsc::sync_channel(1);
        let (worker_finished, worker_finished_rx) = mpsc::sync_channel(1);
        FAIL_LISTENER_WAIT.store(true, Ordering::Release);
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            Arc::new(PrivacyRevocation::new(callback)),
            Box::new(move |run, _| {
                assert!(run.activate());
                WAIT_FAILURE_AFTER_ACTIVATION.store(true, Ordering::Release);
                while !run.cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                worker_release.recv_timeout(Duration::from_secs(8)).unwrap();
                worker_finished.send(()).unwrap();
                true
            }),
        )
        .unwrap();
        callback_entered_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(status.state(), PrivacyCleanupState::Unknown);
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
        assert!(matches!(
            gate_closed_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        // An already-admitted model operation can still reach this local
        // memory sink; this is not a physical-recipient or bootstrap test.
        let mut model_sink = Vec::new();
        if open.load(Ordering::Acquire) {
            model_sink.extend_from_slice(b"model");
        }
        assert_eq!(model_sink.as_slice(), b"model");
        status.stop();
        assert_refused(&supervisor);
        drop(admitted);
        gate_closed_rx.recv_timeout(WAIT).unwrap();
        assert!(!open.load(Ordering::Acquire));
        release_worker.send(()).unwrap();
        worker_finished_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(status.state(), PrivacyCleanupState::Unknown);
    }

    fn ordinary_shutdown_does_not_invoke_loss_callback() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let calls = Arc::new(AtomicUsize::new(0));
        let callback_calls = calls.clone();
        let callback = Arc::new(move || {
            callback_calls.fetch_add(1, Ordering::SeqCst);
        });
        let status = submit(
            &MODEL_SLOT,
            &supervisor.sender,
            Arc::new(PrivacyRevocation::new(callback)),
            Box::new(move |run, _| {
                assert!(run.activate());
                entered.send(()).unwrap();
                while !run.cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                true
            }),
        )
        .unwrap();
        entered_rx.recv_timeout(WAIT).unwrap();
        status.stop();
        wait_for(|| {
            status.state() == PrivacyCleanupState::Complete
                && MODEL_SLOT.state() == PrivacyOwnershipState::Available
        });
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop_supervisor(supervisor);
    }

    fn faulted_inputs(
        callback_fault: DropFault,
        task_fault: DropFault,
    ) -> (
        Arc<PrivacyRevocation>,
        Task,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let callback_drops = Arc::new(AtomicUsize::new(0));
        let task_drops = Arc::new(AtomicUsize::new(0));
        let callback_capture = FaultyDrop {
            drops: callback_drops.clone(),
            fault: callback_fault,
        };
        let task_capture = FaultyDrop {
            drops: task_drops.clone(),
            fault: task_fault,
        };
        let callback = Arc::new(move || {
            std::hint::black_box(&callback_capture);
        });
        let task: Task = Box::new(move |_, _| {
            std::hint::black_box(&task_capture);
            true
        });
        (
            Arc::new(PrivacyRevocation::new(callback)),
            task,
            callback_drops,
            task_drops,
        )
    }

    fn rejected_inputs_remain_caller_owned(sender: &SyncSender<Job>) {
        for panic_on_drop in [false, true] {
            let release = Arc::new(AtomicBool::new(false));
            let callback_fault = if panic_on_drop {
                DropFault::None
            } else {
                DropFault::Stall(release.clone())
            };
            let task_fault = if panic_on_drop {
                DropFault::Panic
            } else {
                DropFault::None
            };
            let (revocation_input, task, callback_drops, task_drops) =
                faulted_inputs(callback_fault, task_fault);
            let sender = sender.clone();
            let (finished, finished_rx) = mpsc::sync_channel(1);
            let caller = thread::spawn(move || {
                let result = submit(&MODEL_SLOT, &sender, revocation_input, task);
                let _ = finished.send(result);
            });
            let mut error = finished_rx.recv_timeout(WAIT).unwrap().unwrap_err();
            caller.join().unwrap();
            let failure = error
                .get_mut()
                .and_then(|error| error.downcast_mut::<PrivacyInstallFailure>())
                .unwrap();
            assert!(failure.cleanup_status().is_none());
            let inputs = failure.take_rejected().expect("rejected inputs returned");
            drop(error);
            assert_eq!(callback_drops.load(Ordering::SeqCst), 0);
            assert_eq!(task_drops.load(Ordering::SeqCst), 0);
            release.store(true, Ordering::Release);
            let dropped = catch_unwind(AssertUnwindSafe(|| drop(inputs)));
            assert_eq!(dropped.is_err(), panic_on_drop);
            assert_eq!(callback_drops.load(Ordering::SeqCst), 1);
            assert_eq!(task_drops.load(Ordering::SeqCst), 1);
        }
    }

    fn failed_submission_retains_storage(case: &str) {
        let full = case.contains("full");
        let (sender, receiver) = mpsc::sync_channel(if full { 0 } else { 1 });
        let _receiver = if full {
            Some(receiver)
        } else {
            drop(receiver);
            None
        };
        let release = Arc::new(AtomicBool::new(false));
        let (callback_fault, task_fault) = match case {
            "submission-full-callback-stall" => {
                (DropFault::Stall(release.clone()), DropFault::None)
            }
            "submission-disconnected-callback-panic" => (DropFault::Panic, DropFault::None),
            "submission-full-task-panic" => (DropFault::None, DropFault::Panic),
            "submission-disconnected-task-stall" => {
                (DropFault::None, DropFault::Stall(release.clone()))
            }
            _ => panic!("unknown submission failure case"),
        };
        let (revocation_input, task, callback_drops, task_drops) =
            faulted_inputs(callback_fault, task_fault);
        let submitter = sender.clone();
        let (finished, finished_rx) = mpsc::sync_channel(1);
        let caller = thread::spawn(move || {
            let result = submit(&MODEL_SLOT, &submitter, revocation_input, task);
            let _ = finished.send(result);
        });
        let error = finished_rx.recv_timeout(WAIT).unwrap().unwrap_err();
        caller.join().unwrap();
        let failure = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<PrivacyInstallFailure>())
            .unwrap();
        assert_eq!(
            failure.cleanup_status().unwrap().require_complete(),
            Err(PrivacyCleanupState::Unknown)
        );
        assert_eq!(MODEL_SLOT.state(), PrivacyOwnershipState::Unknown);
        let retained = MODEL_SLOT.failed_submission.load(Ordering::Acquire);
        assert!(!retained.is_null());
        assert_eq!(callback_drops.load(Ordering::SeqCst), 0);
        assert_eq!(task_drops.load(Ordering::SeqCst), 0);
        for _ in 0..64 {
            assert!(submit(&MODEL_SLOT, &sender, revocation(), Box::new(|_, _| true)).is_err());
        }
        rejected_inputs_remain_caller_owned(&sender);
        assert!(MODEL_SLOT.failed_submission.load(Ordering::Acquire) == retained);
        assert_eq!(callback_drops.load(Ordering::SeqCst), 0);
        assert_eq!(task_drops.load(Ordering::SeqCst), 0);
    }

    fn reaper_spawn_failure() {
        FAIL_REAPER_SPAWN.store(true, Ordering::Release);
        let (revocation_input, task, callback_drops, task_drops) =
            faulted_inputs(DropFault::None, DropFault::None);
        let error = launch(revocation_input, task).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotConnected);
        let failure = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<PrivacyInstallFailure>())
            .unwrap();
        assert_eq!(
            failure.cleanup_status().unwrap().require_complete(),
            Err(PrivacyCleanupState::Unknown)
        );
        assert_eq!(privacy_monitor_ownership(), PrivacyOwnershipState::Unknown);
        assert!(!SLOT.failed_submission.load(Ordering::Acquire).is_null());
        assert_eq!(callback_drops.load(Ordering::SeqCst), 0);
        assert_eq!(task_drops.load(Ordering::SeqCst), 0);
        assert!(launch(revocation(), |_, _| true).is_err());
    }

    fn permanent_guard_stall() {
        let supervisor = Supervisor::spawn(&MODEL_SLOT).unwrap();
        let admission = Arc::new(Mutex::new(()));
        let held = admission.clone();
        let (entered, entered_rx) = mpsc::sync_channel(1);
        let status = start(&supervisor, move |run, _| {
            let _guard = held.lock().unwrap();
            assert!(run.activate());
            entered.send(()).unwrap();
            loop {
                thread::park();
            }
        });
        entered_rx.recv_timeout(WAIT).unwrap();
        status.stop();
        assert!(admission.try_lock().is_err());
        assert_eq!(status.require_complete(), Err(PrivacyCleanupState::Pending));
        assert_refused(&supervisor);
    }

    #[test]
    fn submission_failure_exposes_unknown_cleanup() {
        static SLOT: Slot = Slot::new();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let error = submit(&SLOT, &sender, revocation(), Box::new(|_, _| true)).unwrap_err();
        let failure = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<PrivacyInstallFailure>())
            .unwrap();
        assert_eq!(
            failure.cleanup_status().unwrap().require_complete(),
            Err(PrivacyCleanupState::Unknown)
        );
        assert_eq!(SLOT.state(), PrivacyOwnershipState::Unknown);
        assert!(SLOT.reserve().is_none());
    }

    #[test]
    fn monitor_ownership_fault_models_use_killable_subprocesses() {
        if let Ok(case) = std::env::var(CHILD_CASE) {
            match case.as_str() {
                "permanent-guard" => permanent_guard_stall(),
                "tls" => tls_then_repeated_monitor_slot_reservation(),
                "guard" => blocked_admission_lock(),
                "startup" => cancelled_during_startup(),
                "concurrent" => concurrent_monitor_slot_reservation(),
                "native" => unresolved_native_cleanup(false),
                "native-startup" => unresolved_native_cleanup(true),
                "deadline" => shutdown_deadline_quarantine(),
                "callback-drop" => callback_destructor_stall(),
                "callback-panic" => callback_destructor_panic(),
                "ordinary-shutdown" => ordinary_shutdown_does_not_invoke_loss_callback(),
                "listener-wait"
                | "listener-wait-callback-stall"
                | "listener-wait-callback-panic" => listener_wait_failure(&case),
                "listener-wait-admission-gap" => wait_failure_does_not_fence_model_admission(),
                "submission-full-callback-stall"
                | "submission-disconnected-callback-panic"
                | "submission-full-task-panic"
                | "submission-disconnected-task-stall" => failed_submission_retains_storage(&case),
                "listener-spawn"
                | "listener-spawn-stall"
                | "listener-spawn-panic"
                | "worker-false"
                | "worker-panic" => worker_failure(&case),
                "reaper-spawn" => reaper_spawn_failure(),
                _ => panic!("unknown lifecycle test case"),
            }
            return;
        }

        for case in [
            "permanent-guard",
            "tls",
            "guard",
            "startup",
            "concurrent",
            "native",
            "native-startup",
            "deadline",
            "callback-drop",
            "callback-panic",
            "ordinary-shutdown",
            "listener-wait",
            "listener-wait-callback-stall",
            "listener-wait-callback-panic",
            "listener-wait-admission-gap",
            "submission-full-callback-stall",
            "submission-disconnected-callback-panic",
            "submission-full-task-panic",
            "submission-disconnected-task-stall",
            "listener-spawn",
            "listener-spawn-stall",
            "listener-spawn-panic",
            "worker-false",
            "worker-panic",
            "reaper-spawn",
        ] {
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "privacy_lifecycle::tests::monitor_ownership_fault_models_use_killable_subprocesses",
                    "--nocapture",
                ])
                .env(CHILD_CASE, case)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut child = OwnedChild(Some(child));
            let deadline = Instant::now() + CHILD_TIMEOUT;
            loop {
                if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "lifecycle test child {case} timed out"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let output = child.0.take().unwrap().wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "lifecycle test child {case} failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "lifecycle test child {case} did not run its scenario"
            );
        }
    }
}
