use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
};

use eframe::egui::{self, TextEdit, Ui};
use festerm_ui_egui::theme;

const PATH_SUGGESTION_LIMIT: usize = 6;
const CACHE_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SearchKind {
    Executable,
    WorkingDirectory,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchRequest {
    kind: SearchKind,
    query: String,
    limit: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CachedResult {
    version: u64,
    result: festerm_pty::PathSearchResult,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct DisplayState {
    suggestions: Vec<std::path::PathBuf>,
    truncated: bool,
    limited: bool,
    error: Option<String>,
    pending: bool,
}

trait SearchBackend: Send + Sync {
    fn search(&self, request: &SearchRequest) -> festerm_pty::PathSearchResult;
}

#[derive(Default)]
struct LiveSearchBackend;

impl SearchBackend for LiveSearchBackend {
    fn search(&self, request: &SearchRequest) -> festerm_pty::PathSearchResult {
        match request.kind {
            SearchKind::Executable => {
                festerm_pty::search_path_executables_detailed(&request.query, request.limit)
            }
            SearchKind::WorkingDirectory => {
                festerm_pty::search_working_directories_detailed(&request.query, request.limit)
            }
        }
    }
}

type WorkerSpawner =
    Arc<dyn Fn(Box<dyn FnOnce() + Send>) -> Result<JoinHandle<()>, std::io::Error> + Send + Sync>;

fn spawn_worker(work: Box<dyn FnOnce() + Send>) -> Result<JoinHandle<()>, std::io::Error> {
    thread::Builder::new()
        .name("festerm-path-autocomplete".to_owned())
        .spawn(work)
}

#[derive(Clone)]
struct PathAutocompleteService {
    _owner: Arc<WorkerOwner>,
    shared: Arc<SharedWorker>,
}

struct WorkerOwner {
    shared: Arc<SharedWorker>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WorkerOwner {
    fn drop(&mut self) {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("autocomplete worker state lock is not poisoned");
            state.shutdown = true;
            state.pending = None;
        }
        self.shared.wake.notify_one();
        if self
            .handle
            .lock()
            .expect("autocomplete worker handle lock is not poisoned")
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let handle = self
                .handle
                .lock()
                .expect("autocomplete worker handle lock is not poisoned")
                .take();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
    }
}

struct SharedWorker {
    state: Mutex<WorkerState>,
    wake: Condvar,
    backend: Arc<dyn SearchBackend>,
}

struct WorkerState {
    cache: HashMap<SearchRequest, CachedResult>,
    cache_order: VecDeque<SearchRequest>,
    request_versions: HashMap<SearchRequest, u64>,
    running: Option<SearchJob>,
    pending: Option<SearchJob>,
    shutdown: bool,
}

struct SearchJob {
    owner_id: egui::Id,
    request: SearchRequest,
    version: u64,
    repaint: egui::Context,
}

impl WorkerState {
    fn prune_versions(&mut self, invalidating: Option<&SearchRequest>) {
        // Keep in-flight generations even after cache eviction so late results
        // cannot become valid again when an old query is revisited.
        self.request_versions.retain(|request, _| {
            self.cache.contains_key(request)
                || self
                    .running
                    .as_ref()
                    .is_some_and(|job| job.request == *request)
                || self
                    .pending
                    .as_ref()
                    .is_some_and(|job| job.request == *request)
                || invalidating == Some(request)
        });
    }

    fn version_for(&self, request: &SearchRequest) -> u64 {
        self.request_versions.get(request).copied().unwrap_or(0)
    }

    fn cache_result(
        &mut self,
        request: SearchRequest,
        version: u64,
        result: festerm_pty::PathSearchResult,
    ) {
        if self.version_for(&request) != version {
            return;
        }
        if self.cache.contains_key(&request) {
            self.cache_order.retain(|key| key != &request);
        }
        self.cache_order.push_back(request.clone());
        self.cache.insert(request, CachedResult { version, result });
        while self.cache_order.len() > CACHE_LIMIT {
            if let Some(oldest) = self.cache_order.pop_front() {
                self.cache.remove(&oldest);
            }
        }
        self.prune_versions(None);
    }

    fn invalidate(&mut self, request: &SearchRequest) {
        let next = self.version_for(request).wrapping_add(1);
        self.request_versions.insert(request.clone(), next);
        self.cache.remove(request);
        self.cache_order.retain(|cached| cached != request);
        if self
            .pending
            .as_ref()
            .is_some_and(|job| job.request == *request)
        {
            self.pending = None;
        }
        self.prune_versions(Some(request));
    }
}

impl PathAutocompleteService {
    fn for_context(context: &egui::Context) -> Result<Self, String> {
        let id = egui::Id::new("festerm-local-path-autocomplete-service");
        context.data_mut(|data| {
            if let Some(service) = data.get_temp::<Self>(id) {
                return Ok(service);
            }
            let service = Self::new(Arc::new(LiveSearchBackend), Arc::new(spawn_worker))
                .map_err(|error| error.to_string())?;
            data.insert_temp(id, service.clone());
            Ok(service)
        })
    }

    fn new(
        backend: Arc<dyn SearchBackend>,
        spawner: WorkerSpawner,
    ) -> Result<Self, std::io::Error> {
        let shared = Arc::new(SharedWorker {
            state: Mutex::new(WorkerState {
                cache: HashMap::new(),
                cache_order: VecDeque::new(),
                request_versions: HashMap::new(),
                running: None,
                pending: None,
                shutdown: false,
            }),
            wake: Condvar::new(),
            backend,
        });
        let worker = Arc::clone(&shared);
        let handle = spawner(Box::new(move || worker_loop(worker)))?;
        let owner = Arc::new(WorkerOwner {
            shared: Arc::clone(&shared),
            handle: Mutex::new(Some(handle)),
        });
        Ok(Self {
            _owner: owner,
            shared,
        })
    }

    fn invalidate(&self, request: &SearchRequest) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("autocomplete worker state lock is not poisoned");
        if state.shutdown {
            return;
        }
        state.invalidate(request);
    }

    fn ensure(&self, owner_id: egui::Id, request: SearchRequest, repaint: &egui::Context) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("autocomplete worker state lock is not poisoned");
        if state.shutdown {
            return;
        }
        let version = state.version_for(&request);
        if state
            .cache
            .get(&request)
            .is_some_and(|cached| cached.version == version)
        {
            return;
        }
        if state
            .running
            .as_ref()
            .is_some_and(|job| job.request == request && job.version == version)
        {
            return;
        }
        if state.pending.as_ref().is_some_and(|job| {
            job.owner_id == owner_id && job.request == request && job.version == version
        }) {
            return;
        }
        state.pending = Some(SearchJob {
            owner_id,
            request,
            version,
            repaint: repaint.clone(),
        });
        state.prune_versions(None);
        self.shared.wake.notify_one();
    }

    fn cancel_pending(&self, owner_id: egui::Id) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("autocomplete worker state lock is not poisoned");
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.owner_id == owner_id)
        {
            state.pending = None;
            state.prune_versions(None);
        }
    }

    fn display_state(&self, request: &SearchRequest) -> DisplayState {
        let state = self
            .shared
            .state
            .lock()
            .expect("autocomplete worker state lock is not poisoned");
        let version = state.version_for(request);
        let pending = state
            .running
            .as_ref()
            .is_some_and(|job| job.request == *request && job.version == version)
            || state
                .pending
                .as_ref()
                .is_some_and(|job| job.request == *request && job.version == version);
        state
            .cache
            .get(request)
            .filter(|cached| cached.version == version)
            .map(|cached| DisplayState {
                suggestions: cached.result.suggestions().to_vec(),
                truncated: cached.result.truncated(),
                limited: cached.result.limited(),
                error: cached.result.error().map(str::to_owned),
                pending,
            })
            .unwrap_or(DisplayState {
                pending,
                ..DisplayState::default()
            })
    }
}

fn worker_loop(shared: Arc<SharedWorker>) {
    loop {
        let job = {
            let mut state = shared
                .state
                .lock()
                .expect("autocomplete worker state lock is not poisoned");
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(job) = state.pending.take() {
                    state.running = Some(SearchJob {
                        owner_id: job.owner_id,
                        request: job.request.clone(),
                        version: job.version,
                        repaint: job.repaint.clone(),
                    });
                    break job;
                }
                state = shared
                    .wake
                    .wait(state)
                    .expect("autocomplete worker state lock is not poisoned");
            }
        };

        let result = shared.backend.search(&job.request);

        {
            let mut state = shared
                .state
                .lock()
                .expect("autocomplete worker state lock is not poisoned");
            state.cache_result(job.request.clone(), job.version, result);
            if state.running.as_ref().is_some_and(|running| {
                running.request == job.request && running.version == job.version
            }) {
                state.running = None;
            }
            state.prune_versions(None);
        }

        job.repaint.request_repaint();
    }
}

/// The Local profile editor's executable field, with a live `PATH`-search
/// dropdown.
pub(crate) fn local_executable_field(
    ui: &mut Ui,
    autocomplete_id: egui::Id,
    value: &mut String,
) -> egui::Response {
    local_path_field(
        ui,
        autocomplete_id,
        "Executable",
        value,
        SearchKind::Executable,
    )
}

pub(crate) fn local_working_directory_field(
    ui: &mut Ui,
    autocomplete_id: egui::Id,
    value: &mut String,
) -> egui::Response {
    local_path_field(
        ui,
        autocomplete_id,
        "Working directory (optional)",
        value,
        SearchKind::WorkingDirectory,
    )
}

fn local_path_field(
    ui: &mut Ui,
    autocomplete_id: egui::Id,
    label_text: &str,
    value: &mut String,
    kind: SearchKind,
) -> egui::Response {
    let dropdown_rect_id = autocomplete_id.with("suggestions-rect");
    let focus_state_id = autocomplete_id.with("focused");
    ui.vertical(|ui| {
        let field = ui
            .horizontal(|ui| {
                let label = ui.add(
                    egui::Label::new(egui::RichText::new(label_text).color(theme::TEXT_SECONDARY))
                        .selectable(false),
                );
                let field = ui.add(TextEdit::singleline(value).desired_width(240.0));
                field.labelled_by(label.id)
            })
            .inner;

        let mut suppress = ui.data(|data| data.get_temp::<bool>(autocomplete_id).unwrap_or(false));
        if field.changed() {
            suppress = false;
        }

        let last_dropdown_rect: Option<egui::Rect> =
            ui.data(|data| data.get_temp(dropdown_rect_id));
        let click_started_in_dropdown = ui.input(|input| {
            input.pointer.primary_clicked()
                && input
                    .pointer
                    .interact_pos()
                    .zip(last_dropdown_rect)
                    .is_some_and(|(pos, rect)| rect.contains(pos))
        });
        let field_focused = field.has_focus();
        let was_focused = ui.data(|data| data.get_temp::<bool>(focus_state_id).unwrap_or(false));
        let focus_gained = field_focused && !was_focused;
        let query = value.trim();
        let active = (field_focused || click_started_in_dropdown) && !suppress && !query.is_empty();

        let mut display = DisplayState::default();
        let mut start_error = None;
        match PathAutocompleteService::for_context(ui.ctx()) {
            Ok(service) => {
                let request = SearchRequest {
                    kind,
                    query: query.to_owned(),
                    limit: PATH_SUGGESTION_LIMIT,
                };
                if query.is_empty() || !active {
                    service.cancel_pending(autocomplete_id);
                } else {
                    if focus_gained || field.changed() {
                        service.invalidate(&request);
                    }
                    service.ensure(autocomplete_id, request.clone(), ui.ctx());
                    display = service.display_state(&request);
                }
            }
            Err(error) => {
                if active {
                    start_error = Some(format!("Could not start local path suggestions: {error}."));
                }
            }
        }

        let has_feedback = !display.suggestions.is_empty()
            || start_error.is_some()
            || display.error.is_some()
            || display.limited
            || display.truncated
            || display.pending;
        // Completion changes must not reflow the form beneath a pointer click.
        let dropdown = egui::Popup::from_response(&field)
            .id(dropdown_rect_id)
            .open(active && has_feedback)
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE_TAB_INACTIVE)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .corner_radius(6.0)
                    .inner_margin(6.0),
            )
            .show(|ui| {
                for candidate in &display.suggestions {
                    let text = candidate.display().to_string();
                    let response = ui.add(
                        egui::Button::selectable(
                            false,
                            egui::RichText::new(&text).color(theme::TEXT_PRIMARY),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend),
                    );
                    if response.clicked() {
                        *value = text;
                        suppress = true;
                    }
                }
                if let Some(error) = start_error.or(display.error) {
                    ui.colored_label(theme::STATUS_ERROR, error);
                } else if display.limited {
                    ui.colored_label(theme::TEXT_SECONDARY, "Search limited; narrow query.");
                } else if display.truncated {
                    ui.colored_label(
                        theme::TEXT_SECONDARY,
                        format!("Showing the first {PATH_SUGGESTION_LIMIT} matches."),
                    );
                } else if display.pending && display.suggestions.is_empty() {
                    ui.colored_label(theme::TEXT_SECONDARY, "Searching…");
                }
            });
        if let Some(dropdown) = dropdown {
            ui.data_mut(|data| data.insert_temp(dropdown_rect_id, dropdown.response.rect));
        } else {
            ui.data_mut(|data| data.remove::<egui::Rect>(dropdown_rect_id));
        }

        ui.data_mut(|data| {
            data.insert_temp(autocomplete_id, suppress);
            data.insert_temp(focus_state_id, field_focused);
        });
        field
    })
    .inner
}

#[cfg(test)]
pub(super) fn install_executable_fixture(context: &egui::Context) -> std::path::PathBuf {
    struct FixtureBackend(std::path::PathBuf);
    impl SearchBackend for FixtureBackend {
        fn search(&self, request: &SearchRequest) -> festerm_pty::PathSearchResult {
            let suggestions = if request.kind == SearchKind::Executable && request.query == "cargo"
            {
                vec![self.0.clone()]
            } else {
                Vec::new()
            };
            festerm_pty::PathSearchResult::new(suggestions, false, false, None)
        }
    }
    let path = std::env::current_exe()
        .expect("test executable has an absolute path")
        .with_file_name(if cfg!(windows) { "cargo.exe" } else { "cargo" });
    let service = PathAutocompleteService::new(
        Arc::new(FixtureBackend(path.clone())),
        Arc::new(spawn_worker),
    )
    .expect("fixture worker starts");
    context.data_mut(|data| {
        data.insert_temp(
            egui::Id::new("festerm-local-path-autocomplete-service"),
            service,
        );
    });
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct ControlledBackend {
        started_sender: Mutex<Option<mpsc::Sender<String>>>,
        releases: Mutex<
            HashMap<
                String,
                std::collections::VecDeque<mpsc::Receiver<festerm_pty::PathSearchResult>>,
            >,
        >,
        call_count: Mutex<HashMap<String, usize>>,
    }

    impl ControlledBackend {
        fn with_query(&self, query: &str) -> mpsc::Sender<festerm_pty::PathSearchResult> {
            let (sender, receiver) = mpsc::channel();
            self.releases
                .lock()
                .expect("release map lock is not poisoned")
                .entry(query.to_owned())
                .or_default()
                .push_back(receiver);
            sender
        }

        fn take_started(&self) -> mpsc::Receiver<String> {
            let (sender, receiver) = mpsc::channel();
            *self
                .started_sender
                .lock()
                .expect("started sender lock is not poisoned") = Some(sender);
            receiver
        }

        fn calls(&self, query: &str) -> usize {
            *self
                .call_count
                .lock()
                .expect("call count lock is not poisoned")
                .get(query)
                .unwrap_or(&0)
        }
    }

    impl SearchBackend for ControlledBackend {
        fn search(&self, request: &SearchRequest) -> festerm_pty::PathSearchResult {
            *self
                .call_count
                .lock()
                .expect("call count lock is not poisoned")
                .entry(request.query.clone())
                .or_default() += 1;
            if let Some(sender) = self
                .started_sender
                .lock()
                .expect("started sender lock is not poisoned")
                .as_ref()
            {
                let _ = sender.send(request.query.clone());
            }
            self.releases
                .lock()
                .expect("release map lock is not poisoned")
                .get_mut(&request.query)
                .and_then(std::collections::VecDeque::pop_front)
                .expect("query must have a prepared release receiver")
                .recv()
                .expect("test result must be delivered")
        }
    }

    fn service_for_test(backend: Arc<dyn SearchBackend>) -> PathAutocompleteService {
        PathAutocompleteService::new(backend, Arc::new(spawn_worker))
            .expect("test autocomplete worker should start")
    }

    fn service_for_test_with_spawner(
        backend: Arc<dyn SearchBackend>,
        spawner: WorkerSpawner,
    ) -> PathAutocompleteService {
        PathAutocompleteService::new(backend, spawner)
            .expect("test autocomplete worker should start")
    }

    fn wait_for(timeout: Duration, predicate: impl Fn() -> bool, message: &str) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("{message}");
    }

    fn request(query: &str) -> SearchRequest {
        SearchRequest {
            kind: SearchKind::WorkingDirectory,
            query: query.to_owned(),
            limit: PATH_SUGGESTION_LIMIT,
        }
    }

    #[test]
    fn autocomplete_updates_and_focus_loss_do_not_move_save() {
        struct Form {
            query: String,
            save_rect: egui::Rect,
            saved: bool,
        }
        let backend = Arc::new(ControlledBackend::default());
        let release = backend.with_query("alpha");
        let service = service_for_test(backend);
        let mut harness = Harness::builder().build_ui_state(
            |ui, state: &mut Form| {
                local_working_directory_field(ui, egui::Id::new("directory"), &mut state.query);
                ui.add_space(200.0);
                let save = ui.button("Save");
                state.save_rect = save.rect;
                state.saved |= save.clicked();
            },
            Form {
                query: "alpha".to_owned(),
                save_rect: egui::Rect::NOTHING,
                saved: false,
            },
        );
        harness.ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("festerm-local-path-autocomplete-service"),
                service.clone(),
            );
        });
        harness.run();
        let save_rect = harness.state().save_rect;
        harness.get_by_label("Working directory (optional)").focus();
        harness.run();
        assert!(harness.query_by_label("Searching…").is_some());
        let pending_rect = harness.state().save_rect;

        release
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/alpha")],
                false,
                false,
                None,
            ))
            .unwrap();
        wait_for(
            Duration::from_secs(1),
            || {
                !service
                    .display_state(&request("alpha"))
                    .suggestions
                    .is_empty()
            },
            "controlled suggestions should arrive",
        );
        harness.run();
        let ready_rect = harness.state().save_rect;
        harness.get_by_label("Save").click();
        harness.run();

        assert_eq!(
            pending_rect, save_rect,
            "pending feedback must not move Save"
        );
        assert_eq!(ready_rect, save_rect, "arriving results must not move Save");
        assert_eq!(
            harness.state().save_rect,
            save_rect,
            "focus loss must not move Save"
        );
        assert!(
            harness.state().saved,
            "one real pointer click must save the form"
        );
    }

    #[test]
    fn same_query_results_are_reused_from_cache() {
        let backend = Arc::new(ControlledBackend::default());
        let started = backend.take_started();
        let first = backend.with_query("alpha");
        let service = service_for_test(backend.clone());
        let context = egui::Context::default();

        service.ensure(egui::Id::new("field"), request("alpha"), &context);
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should start the first query"),
            "alpha"
        );
        first
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/alpha")],
                false,
                false,
                None,
            ))
            .expect("first result should be sent");
        wait_for(
            Duration::from_secs(1),
            || {
                !service
                    .display_state(&request("alpha"))
                    .suggestions
                    .is_empty()
            },
            "cached alpha result should become visible",
        );

        service.ensure(egui::Id::new("field"), request("alpha"), &context);
        thread::sleep(Duration::from_millis(25));
        assert_eq!(
            backend.calls("alpha"),
            1,
            "the cached query should not be submitted to the worker again"
        );
    }

    #[test]
    fn latest_query_replaces_pending_work_and_stale_results_stay_hidden() {
        let backend = Arc::new(ControlledBackend::default());
        let started = backend.take_started();
        let first = backend.with_query("a");
        let latest = backend.with_query("abc");
        let service = service_for_test(backend.clone());
        let context = egui::Context::default();

        service.ensure(egui::Id::new("field"), request("a"), &context);
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should start the first query"),
            "a"
        );

        service.ensure(egui::Id::new("field"), request("ab"), &context);
        service.ensure(egui::Id::new("field"), request("abc"), &context);

        let before = Instant::now();
        service.ensure(egui::Id::new("field"), request("abc"), &context);
        assert!(
            before.elapsed() < Duration::from_millis(50),
            "queueing while the worker is stalled must stay off the UI thread"
        );

        first
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/a")],
                false,
                false,
                None,
            ))
            .expect("stale result should be sent");

        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should skip straight to the latest pending query"),
            "abc"
        );
        assert_eq!(
            backend.calls("ab"),
            0,
            "the superseded query must never run"
        );

        let stale = service.display_state(&request("a"));
        assert_eq!(stale.suggestions, vec![std::path::PathBuf::from("/a")]);
        let current = service.display_state(&request("abc"));
        assert!(
            current.suggestions.is_empty() && current.pending,
            "the current field value must not show the stale query result"
        );

        latest
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/abc")],
                false,
                false,
                None,
            ))
            .expect("latest result should be sent");
        wait_for(
            Duration::from_secs(1),
            || {
                service.display_state(&request("abc")).suggestions
                    == vec![std::path::PathBuf::from("/abc")]
            },
            "latest query result should become visible",
        );
    }

    #[test]
    fn invalidation_discards_the_previous_generation_and_requeries() {
        let backend = Arc::new(ControlledBackend::default());
        let started = backend.take_started();
        let first = backend.with_query("alpha");
        let second = backend.with_query("alpha");
        let service = service_for_test(backend.clone());
        let context = egui::Context::default();
        let request = request("alpha");
        let owner = egui::Id::new("field");

        service.ensure(owner, request.clone(), &context);
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should start the first generation"),
            "alpha"
        );
        service.invalidate(&request);
        service.ensure(owner, request.clone(), &context);
        for index in 0..CACHE_LIMIT * 4 {
            service.invalidate(&self::request(&format!("abandoned-{index}")));
        }
        {
            let state = service.shared.state.lock().unwrap();
            assert!(state.request_versions.len() <= 3);
            assert_eq!(state.version_for(&request), 1);
        }
        first
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/stale")],
                false,
                false,
                None,
            ))
            .expect("first generation should complete");
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should requery after invalidation"),
            "alpha"
        );
        assert!(
            service.display_state(&request).pending,
            "the invalidated query should keep waiting for its refreshed result"
        );
        second
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/fresh")],
                false,
                false,
                None,
            ))
            .expect("fresh generation should complete");
        wait_for(
            Duration::from_secs(1),
            || {
                service.display_state(&request).suggestions
                    == vec![std::path::PathBuf::from("/fresh")]
            },
            "the refreshed generation should replace the stale one",
        );
    }

    #[test]
    fn request_versions_are_bounded_after_cache_eviction_and_abandoned_queries() {
        let mut state = WorkerState {
            cache: HashMap::new(),
            cache_order: VecDeque::new(),
            request_versions: HashMap::new(),
            running: None,
            pending: None,
            shutdown: false,
        };
        for index in 0..CACHE_LIMIT * 4 {
            let request = request(&format!("cached-{index}"));
            state.invalidate(&request);
            let version = state.version_for(&request);
            state.cache_result(request, version, festerm_pty::PathSearchResult::default());
            assert!(state.request_versions.len() <= CACHE_LIMIT);
            assert_eq!(state.request_versions.len(), state.cache.len());
        }
        for index in 0..CACHE_LIMIT * 4 {
            state.invalidate(&request(&format!("abandoned-{index}")));
            assert!(state.request_versions.len() <= CACHE_LIMIT + 1);
        }
        state.prune_versions(None);
        assert_eq!(state.request_versions.len(), CACHE_LIMIT);
        assert_eq!(state.version_for(&request("cached-0")), 0);
    }

    #[test]
    fn cancel_pending_only_clears_the_same_field() {
        let backend = Arc::new(ControlledBackend::default());
        let started = backend.take_started();
        let first = backend.with_query("alpha");
        let latest = backend.with_query("beta");
        let service = service_for_test(backend.clone());
        let context = egui::Context::default();
        let alpha_owner = egui::Id::new("alpha");
        let beta_owner = egui::Id::new("beta");

        service.ensure(alpha_owner, request("alpha"), &context);
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("worker should start alpha"),
            "alpha"
        );
        service.ensure(beta_owner, request("beta"), &context);
        service.cancel_pending(alpha_owner);

        first
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/alpha")],
                false,
                false,
                None,
            ))
            .expect("alpha result should be sent");
        assert_eq!(
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("beta should stay queued for the second field"),
            "beta"
        );
        latest
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/beta")],
                false,
                false,
                None,
            ))
            .expect("beta result should be sent");
        wait_for(
            Duration::from_secs(1),
            || {
                service.display_state(&request("beta")).suggestions
                    == vec![std::path::PathBuf::from("/beta")]
            },
            "the second field's pending request should survive the first field being cancelled",
        );
    }

    #[test]
    fn dropping_the_last_service_stops_an_idle_worker() {
        let backend = Arc::new(ControlledBackend::default());
        let (exit_sender, exit_receiver) = mpsc::channel();
        let spawner: WorkerSpawner = Arc::new(move |work| {
            let exit_sender = exit_sender.clone();
            thread::Builder::new()
                .name("test-path-autocomplete".to_owned())
                .spawn(move || {
                    work();
                    let _ = exit_sender.send(());
                })
        });
        let service = service_for_test_with_spawner(backend, spawner);

        drop(service);

        exit_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("idle worker should exit after the last service is dropped");
    }

    #[test]
    fn dropping_a_service_does_not_wait_for_a_blocked_scan() {
        let backend = Arc::new(ControlledBackend::default());
        let started = backend.take_started();
        let release = backend.with_query("alpha");
        let exited = Arc::new(AtomicBool::new(false));
        let exited_flag = Arc::clone(&exited);
        let spawner: WorkerSpawner = Arc::new(move |work| {
            let exited_flag = Arc::clone(&exited_flag);
            thread::Builder::new()
                .name("test-path-autocomplete".to_owned())
                .spawn(move || {
                    work();
                    exited_flag.store(true, Ordering::SeqCst);
                })
        });
        let service = service_for_test_with_spawner(backend.clone(), spawner);
        let context = egui::Context::default();
        service.ensure(egui::Id::new("field"), request("alpha"), &context);
        started
            .recv_timeout(Duration::from_secs(1))
            .expect("worker should start the blocked query");

        let started_drop = Instant::now();
        drop(service);
        assert!(
            started_drop.elapsed() < Duration::from_millis(50),
            "dropping a blocked autocomplete service must not wait for the scan to finish"
        );
        assert!(
            !exited.load(Ordering::SeqCst),
            "the blocked worker should still be running until the search returns"
        );

        release
            .send(festerm_pty::PathSearchResult::new(
                vec![std::path::PathBuf::from("/alpha")],
                false,
                false,
                None,
            ))
            .expect("blocked query should be released");
        wait_for(
            Duration::from_secs(1),
            || exited.load(Ordering::SeqCst),
            "worker should exit once the blocked scan finishes",
        );
    }
}
