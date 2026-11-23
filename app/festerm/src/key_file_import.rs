//! Transient, window-local delivery into existing SSH key entry fields.
//! Selected paths are never persisted; key text enters only the existing secret field.

use std::{
    hash::{Hash, Hasher},
    io::Read,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
};

use eframe::egui;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

const LIMIT: usize = festerm_secret_store::MAX_SECRET_BYTES;
static NEXT_BINDING: AtomicU64 = AtomicU64::new(1);
static IMPORT_RUNNING: AtomicBool = AtomicBool::new(false);

/// The actual worker owns admission, not the window, receiver or cancel button.
/// A closed window cannot free it while its native picker/read still runs.
struct WorkerPermit;

impl WorkerPermit {
    fn acquire() -> Option<Self> {
        IMPORT_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        IMPORT_RUNNING.store(false, Ordering::Release);
    }
}

/// Cloning temporary egui form state preserves identity. Constructors allocate
/// a new identity; revisions never reset when input changes back to its old value.
#[derive(Clone)]
pub(crate) struct Binding {
    id: u64,
    revision: u64,
    metadata: Option<[u8; 32]>,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            id: NEXT_BINDING
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .unwrap_or(0),
            revision: 0,
            metadata: None,
        }
    }
}

impl Binding {
    pub(crate) fn route(&self) -> u64 {
        self.id
    }

    pub(crate) fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    /// Borrow only destination/profile/mode metadata, never key/passphrase text.
    /// A finite digest detects programmatic edits without retaining even a
    /// malformed pasted destination string. Actual edit events cancel directly.
    pub(crate) fn observe(&mut self, metadata: impl Hash) {
        struct Fingerprint(Sha256);
        impl Hasher for Fingerprint {
            fn write(&mut self, bytes: &[u8]) {
                self.0.update(bytes);
            }
            fn finish(&self) -> u64 {
                0
            } // Hash only writes; retain the full SHA-256 below.
        }
        let mut fingerprint = Fingerprint(Sha256::new());
        metadata.hash(&mut fingerprint);
        let current: [u8; 32] = fingerprint.0.finalize().into();
        if self.metadata.is_some_and(|previous| previous != current) {
            self.changed();
        }
        self.metadata = Some(current);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Ticket {
    binding: u64,
    revision: u64,
    navigation: u64,
    route: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    Read,
    TooLarge,
    Text,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Self::Read => "Could not read this key file. Choose a regular file you can read.",
            Self::TooLarge => "The key file exceeds the supported size (64 KiB).",
            Self::Text => "The key file must contain UTF-8 text.",
        }
    }
}

type Payload = Result<Option<Zeroizing<String>>, Failure>;

struct Pending {
    ticket: Ticket,
    cancelled: Arc<AtomicBool>,
    receive: mpsc::Receiver<Payload>,
}

#[derive(Default)]
struct Window {
    navigation: u64,
    route: Option<u64>,
    pending: Option<Pending>,
    ready: Option<(Ticket, Payload)>,
}

impl Window {
    fn cancel(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.cancelled.store(true, Ordering::Release);
        }
        self.ready = None;
    }

    fn poll(&mut self) {
        let Some(pending) = &self.pending else {
            return;
        };
        let result = match pending.receive.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(Failure::Read),
        };
        let pending = self.pending.take().expect("pending import");
        if !pending.cancelled.load(Ordering::Acquire) {
            self.ready = Some((pending.ticket, result));
        }
    }

    fn ticket(&self, binding: &Binding) -> Option<Ticket> {
        (binding.id != 0 && binding.revision != u64::MAX && self.navigation != u64::MAX).then_some(
            Ticket {
                binding: binding.id,
                revision: binding.revision,
                navigation: self.navigation,
                route: self.route?,
            },
        )
    }
}

fn window(context: &egui::Context) -> Arc<Mutex<Window>> {
    context.data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<Window>>>(egui::Id::new("key_file_import_window"))
            .clone()
    })
}

/// An actual edit event cancels delivery even if undo restores the same text
/// in this pass. This never reads or retains the edited value.
pub(crate) fn edited(context: &egui::Context) {
    window(context)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .cancel();
}

/// Called with the existing monotonic workspace revision, including tab ABA.
pub(crate) fn navigation(context: &egui::Context, revision: u64) {
    let owner = window(context);
    let mut state = owner.lock().unwrap_or_else(|error| error.into_inner());
    if state.navigation != revision {
        state.cancel();
        state.navigation = revision;
    }
    state.poll();
}

/// Every connection/editor surface (including its Back/List state) names its
/// current route. Leaving and returning cannot revive a completed old picker.
pub(crate) fn route(context: &egui::Context, route: u64) {
    let owner = window(context);
    let mut state = owner.lock().unwrap_or_else(|error| error.into_inner());
    if state.route != Some(route) {
        state.cancel();
        state.route = Some(route);
    }
    state.poll();
}

pub(crate) fn button(ui: &mut egui::Ui, binding: &Binding) {
    let owner = window(ui.ctx());
    let mut state = owner.lock().unwrap_or_else(|error| error.into_inner());
    state.poll();
    if let Some(pending) = &mut state.pending {
        if pending.ticket.binding == binding.id {
            ui.horizontal(|ui| {
                ui.label("Choosing key file…");
                if ui.button("Cancel key import").clicked() {
                    pending.cancelled.store(true, Ordering::Release);
                }
            });
        } else {
            ui.add_enabled(false, egui::Button::new("Choose key file…"));
        }
        return;
    }
    if !ui
        .add_enabled(
            !IMPORT_RUNNING.load(Ordering::Acquire),
            egui::Button::new("Choose key file…"),
        )
        .clicked()
    {
        return;
    }
    let Some(ticket) = state.ticket(binding) else {
        return;
    };
    state.ready = None;
    let Some(permit) = WorkerPermit::acquire() else {
        return;
    };
    let (send, receive) = mpsc::sync_channel(1);
    let future = rfd::AsyncFileDialog::new()
        .set_title("Choose an OpenSSH private key")
        .pick_file();
    let wake = ui.ctx().clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    match std::thread::Builder::new()
        .name("key-file-import".into())
        .spawn(move || {
            let _permit = permit;
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Failure::Read)
                .and_then(|runtime| {
                    runtime
                        .block_on(future)
                        .filter(|_| !worker_cancelled.load(Ordering::Acquire))
                        .map(|file| read(file.path()))
                        .transpose()
                });
            let result = if worker_cancelled.load(Ordering::Acquire) {
                Ok(None)
            } else {
                result
            };
            let _ = send.send(result);
            wake.request_repaint();
        }) {
        Ok(_) => {
            state.pending = Some(Pending {
                ticket,
                cancelled,
                receive,
            })
        }
        Err(_) => state.ready = Some((ticket, Err(Failure::Read))),
    }
}

/// Called after all edits and submission decisions, never before rendering.
/// The caller replaces only its key field; the passphrase is not an input here.
pub(crate) fn take(
    context: &egui::Context,
    binding: &Binding,
) -> Option<Result<Zeroizing<String>, &'static str>> {
    let owner = window(context);
    let mut state = owner.lock().unwrap_or_else(|error| error.into_inner());
    state.poll();
    let (ticket, _) = state.ready.as_ref()?;
    if ticket.binding != binding.id {
        return None;
    }
    let (ticket, result) = state.ready.take()?;
    if Some(ticket) != state.ticket(binding) {
        return None;
    }
    match result {
        Ok(Some(text)) => Some(Ok(text)),
        Ok(None) => None,
        Err(error) => Some(Err(error.message())),
    }
}

fn read(path: &Path) -> Result<Zeroizing<String>, Failure> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the leaf itself and reject reparse/special entries on its handle.
        options.custom_flags(0x0020_0000 | 0x0200_0000);
    }
    let file = options.open(path).map_err(|_| Failure::Read)?;
    let metadata = file.metadata().map_err(|_| Failure::Read)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Failure::Read);
    }
    if metadata.len() > LIMIT as u64 {
        return Err(Failure::TooLarge);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Read)?;
    if bytes.len() > LIMIT {
        return Err(Failure::TooLarge);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Text)?;
    Ok(Zeroizing::new(text.to_owned()))
}

pub(crate) fn replace(target: &mut String, text: &mut Zeroizing<String>) {
    target.zeroize();
    *target = std::mem::take(&mut **text);
}

#[cfg(test)]
pub(crate) fn stage_for_test(context: &egui::Context, binding: &Binding) -> impl FnOnce(&str) {
    let owner = window(context);
    let mut state = owner.lock().unwrap();
    assert!(state.pending.is_none());
    let ticket = state.ticket(binding).expect("bound form");
    let (send, receive) = mpsc::sync_channel(1);
    state.pending = Some(Pending {
        ticket,
        cancelled: Arc::new(AtomicBool::new(false)),
        receive,
    });
    move |text| {
        let _ = send.send(Ok(Some(Zeroizing::new(text.to_owned()))));
    }
}

#[cfg(test)]
pub(crate) fn stage_read_for_test(context: &egui::Context, binding: &Binding, path: &Path) {
    let owner = window(context);
    let mut state = owner.lock().unwrap();
    assert!(state.pending.is_none());
    let ticket = state.ticket(binding).unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    send.send(read(path).map(Some))
        .unwrap_or_else(|_| panic!("test receiver retained"));
    state.pending = Some(Pending {
        ticket,
        cancelled: Arc::new(AtomicBool::new(false)),
        receive,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_worker_admission_survives_closed_window_until_actual_return() {
        let context = egui::Context::default();
        let binding = Binding::default();
        route(&context, binding.route());
        let permit = WorkerPermit::acquire().expect("only actual worker in this test group");
        let (release, wait) = mpsc::sync_channel::<()>(1);
        let (send, receive) = mpsc::sync_channel(1);
        let ticket = window(&context).lock().unwrap().ticket(&binding).unwrap();
        window(&context).lock().unwrap().pending = Some(Pending {
            ticket,
            cancelled: Arc::new(AtomicBool::new(false)),
            receive,
        });
        let worker = std::thread::spawn(move || {
            let _permit = permit;
            wait.recv().unwrap();
            // The original window receiver is gone; failed send drops and
            // zeroizes the result rather than delivering it to a new window.
            let _ = send.send(Ok(Some(Zeroizing::new("closed window material".into()))));
        });
        drop(context);
        let replacement_window = egui::Context::default();
        route(&replacement_window, Binding::default().route());
        assert!(WorkerPermit::acquire().is_none());
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(WorkerPermit::acquire().is_some());
        assert!(window(&replacement_window).lock().unwrap().ready.is_none());
    }

    #[test]
    fn key_file_reader_bounds_utf8_regular_handle_and_preserves_exact_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("chosen");
        let text = "\u{feff}placeholder encrypted key\r\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(read(&path).ok().unwrap().as_str(), text);
        std::fs::write(&path, vec![b'x'; LIMIT]).unwrap();
        assert_eq!(read(&path).ok().unwrap().len(), LIMIT);
        std::fs::write(&path, vec![b'x'; LIMIT + 1]).unwrap();
        assert!(matches!(read(&path), Err(Failure::TooLarge)));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(matches!(read(&path), Err(Failure::Text)));
        assert!(matches!(read(directory.path()), Err(Failure::Read)));
        assert!(matches!(
            read(&directory.path().join("missing")),
            Err(Failure::Read)
        ));
        #[cfg(unix)]
        {
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(&path, &alias).unwrap();
            assert!(matches!(read(&alias), Err(Failure::Read)));
            let fifo = directory.path().join("fifo");
            nix::unistd::mkfifo(
                &fifo,
                nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
            )
            .unwrap();
            assert!(matches!(read(&fifo), Err(Failure::Read)));
        }
    }

    #[test]
    fn key_file_pending_slot_survives_cancel_and_stale_delivery_is_discarded() {
        let context = egui::Context::default();
        let mut binding = Binding::default();
        route(&context, binding.route());
        let finish = stage_for_test(&context, &binding);
        {
            let owner = window(&context);
            let mut state = owner.lock().unwrap();
            state.cancel();
            state.poll();
            assert!(
                state.pending.is_some(),
                "cancel must not admit another OS job"
            );
        }
        finish("discarded material");
        assert!(take(&context, &binding).is_none());
        assert!(window(&context).lock().unwrap().pending.is_none());

        for change in 0..4 {
            route(&context, binding.route());
            let finish = stage_for_test(&context, &binding);
            match change {
                0 => {
                    binding.changed();
                    binding.changed();
                }
                1 => {
                    navigation(&context, 1);
                    navigation(&context, 2);
                }
                2 => {
                    route(&context, 0);
                    route(&context, binding.route());
                }
                _ => {
                    binding.observe("other host");
                    binding.observe("original host");
                }
            }
            finish("late material");
            assert!(take(&context, &binding).is_none());
        }
    }
}
