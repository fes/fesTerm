use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    backtrace::Backtrace,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic::PanicHookInfo,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, OnceLock, TryLockError},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing_subscriber::{
    fmt::{self, MakeWriter},
    EnvFilter,
};

const PROTOCOL_TRACE_ENV: &str = "FESTERM_PROTOCOL_TRACE";
const DIAGNOSTICS_DIRECTORY: &str = "diagnostics";
const RUN_MARKER_FILE: &str = "current-run.json";
const EXIT_INTENT_FILE: &str = "exit-intent.json";
const LAST_EXIT_FILE: &str = "last-exit.json";
const CURRENT_LOG_FILE: &str = "festerm.log";
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PANIC_BYTES: usize = 256 * 1024;
const MAX_INACTIVE_RUNS_PER_CLASS: usize = 5;
const RUNS_DIRECTORY: &str = "runs-v2";
const CATALOG_LOCK_FILE: &str = "catalog.lock";
const RUN_LOCK_FILE: &str = "lifetime.lock";
const PANIC_OBSERVED_FILE: &str = "panic-observed";

static JOURNAL: OnceLock<Mutex<RunJournal>> = OnceLock::new();
static LAST_EXIT: OnceLock<Option<ExitRecord>> = OnceLock::new();
static PANICKED: AtomicBool = AtomicBool::new(false);
static DIAGNOSTICS_UNAVAILABLE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExitIntent {
    UserQuit,
    UpdateRestart,
    NativeSmokeComplete,
}

impl ExitIntent {
    const fn description(self) -> &'static str {
        match self {
            Self::UserQuit => "User requested quit",
            Self::UpdateRestart => "Update installed; restart requested",
            Self::NativeSmokeComplete => "Native smoke completed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExitStatus {
    Clean,
    RuntimeError,
    Panic,
    Unclean,
}

impl ExitStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Clean => "Clean",
            Self::RuntimeError => "Runtime error",
            Self::Panic => "Rust panic",
            Self::Unclean => "Unclean",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RunMarker {
    schema_version: u32,
    run_id: String,
    started_at_unix_ms: u128,
    version: String,
    os: String,
    architecture: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedIntent {
    run_id: String,
    intent: ExitIntent,
    recorded_at_unix_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ExitRecord {
    schema_version: u32,
    run_id: String,
    started_at_unix_ms: u128,
    ended_at_unix_ms: u128,
    status: ExitStatus,
    reason: String,
    version: String,
    os: String,
    architecture: String,
    artifact: Option<String>,
}

impl ExitRecord {
    fn support_summary(&self) -> String {
        let mut summary = format!("{} — {}", self.status.label(), self.reason);
        if let Some(artifact) = &self.artifact {
            summary.push_str(" (local report: ");
            summary.push_str(artifact);
            summary.push(')');
        }
        summary
    }
}

pub(crate) struct DiagnosticsGuard;

impl DiagnosticsGuard {
    pub(crate) fn finish(self, succeeded: bool) {
        let Some(journal) = JOURNAL.get() else {
            return;
        };
        let mut journal = match journal.lock() {
            Ok(journal) => journal,
            Err(error) => {
                DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                eprintln!("fesTerm is recovering its poisoned lifecycle journal: {error}");
                error.into_inner()
            }
        };
        journal.panic_seen |= PANICKED.load(Ordering::Relaxed);
        if let Err(error) = journal.finish(succeeded, now_unix_ms()) {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            tracing::error!(
                target: "festerm::diagnostics",
                %error,
                "could not record the application exit"
            );
        }
    }
}

pub fn init() -> DiagnosticsGuard {
    init_in(diagnostics_directory())
}

fn init_in(directory: Option<PathBuf>) -> DiagnosticsGuard {
    let (journal, previous_exit, diagnostics_error) = match directory {
        Some(directory) => match RunJournal::start_in(directory, now_unix_ms()) {
            Ok((journal, previous_exit)) => (Some(journal), previous_exit, None),
            Err(error) => (None, None, Some(error)),
        },
        None => (
            None,
            None,
            Some(io::Error::other(
                "no diagnostic directory was available from native state or the smoke result path",
            )),
        ),
    };

    let log_writer = journal
        .as_ref()
        .and_then(|journal| match BoundedLog::open(journal) {
            Ok(writer) => Some(Arc::new(Mutex::new(writer))),
            Err(error) => {
                eprintln!("fesTerm could not initialize its diagnostic log: {error}");
                DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                None
            }
        });
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("festerm=info,warn"));
    fmt::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false)
        .with_writer(DiagnosticMakeWriter { log_writer })
        .try_init()
        .unwrap_or_else(|error| {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            eprintln!("fesTerm tracing initialization failed: {error}");
        });

    let _ = LAST_EXIT.set(previous_exit);
    if let Some(journal) = journal {
        let directory = journal.directory.clone();
        let _ = JOURNAL.set(Mutex::new(journal));
        install_panic_hook(&directory);
    }
    if let Some(error) = diagnostics_error {
        DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
        tracing::warn!(
            target: "festerm::diagnostics",
            %error,
            "durable crash diagnostics are unavailable for this run"
        );
    }
    if std::env::var(PROTOCOL_TRACE_ENV).is_ok_and(|value| value == "1") {
        tracing::warn!(
            target: "festerm::diagnostics",
            "protocol tracing was requested; terminal content tracing is not implemented yet"
        );
    }
    DiagnosticsGuard
}

pub(crate) fn record_exit_intent(intent: ExitIntent) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };
    match journal.lock() {
        Ok(mut journal) => {
            if let Err(error) = journal.record_intent(intent, now_unix_ms()) {
                DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                tracing::error!(
                    target: "festerm::diagnostics",
                    %error,
                    "could not persist the requested exit reason"
                );
            }
        }
        Err(error) => {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            tracing::error!(
                target: "festerm::diagnostics",
                %error,
                "could not lock the application lifecycle journal"
            );
        }
    }
}

pub(crate) fn last_exit_summary() -> Option<String> {
    let summary = LAST_EXIT
        .get()
        .and_then(|record| record.as_ref())
        .map(ExitRecord::support_summary);
    if DIAGNOSTICS_UNAVAILABLE.load(Ordering::Relaxed) {
        Some(format!(
            "{}Local diagnostics are unavailable or incomplete for this run",
            summary.map_or_else(String::new, |summary| format!("{summary}; "))
        ))
    } else {
        summary
    }
}

fn diagnostics_directory() -> Option<PathBuf> {
    let native = ProjectDirs::from("com", "fes", "fesTerm").map(|directories| {
        directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir())
            .join(DIAGNOSTICS_DIRECTORY)
    });
    select_diagnostics_directory(
        native,
        crate::native_smoke::NativeWindowSmoke::requested(),
        crate::native_smoke::NativeWindowSmoke::result_path_from_environment(),
    )
}

fn select_diagnostics_directory(
    native: Option<PathBuf>,
    smoke_requested: bool,
    smoke_result: Option<PathBuf>,
) -> Option<PathBuf> {
    if smoke_requested {
        // Missing smoke configuration must not fall back to the user's real
        // journal. Keep each runner's diagnostics beside its isolated result.
        smoke_result.map(|result| {
            let mut directory = result.into_os_string();
            directory.push(".diagnostics");
            PathBuf::from(directory)
        })
    } else {
        native
    }
}

fn install_panic_hook(directory: &Path) {
    let panic_marker = directory.join(PANIC_OBSERVED_FILE);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |information| {
        // Independent of the journal mutex: a worker can panic while finish
        // publishes a clean record. This durable flag takes precedence on
        // recovery even when the richer panic report cannot acquire the mutex.
        if let Err(error) = atomic_write(&panic_marker, b"panic\n") {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            eprintln!("fesTerm could not persist its panic flag: {error}");
        }
        record_panic(information);
        previous(information);
    }));
}

fn record_panic(information: &PanicHookInfo<'_>) {
    // Set before trying the journal: a panic while it is held must never
    // deadlock or subsequently be reported as a clean event-loop exit.
    PANICKED.store(true, Ordering::Relaxed);
    let Some(journal) = JOURNAL.get() else {
        return;
    };
    let mut journal = match journal.try_lock() {
        Ok(journal) => journal,
        Err(TryLockError::Poisoned(error)) => {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            eprintln!("fesTerm could not lock its panic journal: {error}");
            return;
        }
        Err(TryLockError::WouldBlock) => {
            DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
            eprintln!("fesTerm panic journal was busy; no panic report was written");
            return;
        }
    };
    if let Err(error) = journal.record_panic(information, now_unix_ms()) {
        DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
        eprintln!("fesTerm could not write its panic report: {error}");
    }
}

struct RunJournal {
    directory: PathBuf,
    marker: RunMarker,
    intent: Option<ExitIntent>,
    lifetime: Arc<File>,
    panic_seen: bool,
    finished: bool,
}

impl RunJournal {
    fn start_in(directory: PathBuf, now: u128) -> io::Result<(Self, Option<ExitRecord>)> {
        fs::create_dir_all(&directory)?;
        // The catalog inode is permanent. All creation, probing and removal of
        // run directories happens under this lock; panic/finish never need it.
        let _catalog = lock_catalog(&directory)?;
        let runs = directory.join(RUNS_DIRECTORY);
        fs::create_dir_all(&runs)?;
        let previous = scan_and_prune(&runs, now)?;
        static NEXT_RUN: AtomicU64 = AtomicU64::new(0);
        let (directory, run_id) = loop {
            let id = format!(
                "{}-{now}-{}",
                std::process::id(),
                NEXT_RUN.fetch_add(1, Ordering::Relaxed)
            );
            let path = runs.join(&id);
            match fs::create_dir(&path) {
                Ok(()) => break (path, id),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let lifetime = Arc::new(open_lock(&directory.join(RUN_LOCK_FILE))?);
        fs2::FileExt::try_lock_exclusive(lifetime.as_ref())?;
        let marker = RunMarker {
            schema_version: 2,
            run_id,
            started_at_unix_ms: now,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        };
        write_json(&directory.join(RUN_MARKER_FILE), &marker)?;
        Ok((
            Self {
                directory,
                marker,
                intent: None,
                lifetime,
                panic_seen: false,
                finished: false,
            },
            previous,
        ))
    }

    fn load_previous_exit(directory: &Path, now: u128) -> io::Result<Option<ExitRecord>> {
        if let Some(mut record) = read_optional_json::<ExitRecord>(&directory.join(LAST_EXIT_FILE))?
        {
            if directory.join(PANIC_OBSERVED_FILE).try_exists()? {
                record.status = ExitStatus::Panic;
                record.reason = "The run encountered a Rust panic".to_owned();
            }
            return Ok(Some(record));
        }
        let Some(marker): Option<RunMarker> = read_optional_json(&directory.join(RUN_MARKER_FILE))?
        else {
            // Startup was interrupted before publishing a marker.
            return Ok(None);
        };
        let intent = read_optional_json::<PersistedIntent>(&directory.join(EXIT_INTENT_FILE))?
            .filter(|intent| intent.run_id == marker.run_id)
            .map(|intent| intent.intent);
        let panic_name = format!("panic-{}.txt", marker.run_id);
        let panic_path = directory.join(&panic_name);
        let has_report = panic_path.try_exists()?;
        let (status, reason, artifact) =
            if has_report || directory.join(PANIC_OBSERVED_FILE).try_exists()? {
                (
                    ExitStatus::Panic,
                    "The run encountered a Rust panic; no final exit was recorded".to_owned(),
                    has_report.then_some(panic_name),
                )
            } else {
                let reason = intent.map_or_else(
                    || "No clean shutdown was recorded".to_owned(),
                    |intent| {
                        format!(
                            "No clean shutdown followed this intent: {}",
                            intent.description()
                        )
                    },
                );
                (ExitStatus::Unclean, reason, None)
            };
        Ok(Some(ExitRecord {
            schema_version: 2,
            run_id: marker.run_id,
            started_at_unix_ms: marker.started_at_unix_ms,
            ended_at_unix_ms: now,
            status,
            reason,
            version: marker.version,
            os: marker.os,
            architecture: marker.architecture,
            artifact,
        }))
    }

    fn record_intent(&mut self, intent: ExitIntent, now: u128) -> io::Result<()> {
        if self.intent.is_some() {
            return Ok(());
        }
        write_json(
            &self.directory.join(EXIT_INTENT_FILE),
            &PersistedIntent {
                run_id: self.marker.run_id.clone(),
                intent,
                recorded_at_unix_ms: now,
            },
        )?;
        self.intent = Some(intent);
        Ok(())
    }

    fn finish(&mut self, succeeded: bool, now: u128) -> io::Result<()> {
        self.panic_seen |= self.directory.join(PANIC_OBSERVED_FILE).try_exists()?;
        if self.finished {
            return Ok(());
        }
        let (status, reason) = if self.panic_seen {
            (
                ExitStatus::Panic,
                "The run encountered a Rust panic (even if the event loop later returned)"
                    .to_owned(),
            )
        } else if succeeded {
            (
                ExitStatus::Clean,
                self.intent.map_or_else(
                    || "Application event loop returned normally".to_owned(),
                    |intent| intent.description().to_owned(),
                ),
            )
        } else {
            (
                ExitStatus::RuntimeError,
                "The application event loop returned an error".to_owned(),
            )
        };
        let record = ExitRecord {
            schema_version: 2,
            run_id: self.marker.run_id.clone(),
            started_at_unix_ms: self.marker.started_at_unix_ms,
            ended_at_unix_ms: now,
            status,
            reason,
            version: self.marker.version.clone(),
            os: self.marker.os.clone(),
            architecture: self.marker.architecture.clone(),
            artifact: self.panic_artifact()?,
        };
        write_json(&self.directory.join(LAST_EXIT_FILE), &record)?;
        remove_if_exists(&self.directory.join(EXIT_INTENT_FILE))?;
        remove_if_exists(&self.directory.join(RUN_MARKER_FILE))?;
        self.finished = true;
        Ok(())
    }

    fn panic_artifact(&self) -> io::Result<Option<String>> {
        let name = format!("panic-{}.txt", self.marker.run_id);
        Ok(self.directory.join(&name).try_exists()?.then_some(name))
    }

    fn record_panic(&mut self, information: &PanicHookInfo<'_>, now: u128) -> io::Result<()> {
        self.panic_seen = true;
        let report_name = format!("panic-{}.txt", self.marker.run_id);
        let payload = information
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                information
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("non-string panic payload");
        let location = information.location().map_or_else(
            || "unknown".to_owned(),
            |location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            },
        );
        let report = format!(
            "fesTerm panic report\n\
             version={}\n\
             os={}\n\
             architecture={}\n\
             run_id={}\n\
             started_at_unix_ms={}\n\
             panic_at_unix_ms={now}\n\
             location={location}\n\
             message={payload}\n\n\
             backtrace:\n{}",
            self.marker.version,
            self.marker.os,
            self.marker.architecture,
            self.marker.run_id,
            self.marker.started_at_unix_ms,
            Backtrace::force_capture(),
        );
        let path = self.directory.join(&report_name);
        atomic_write(
            &path,
            &report.as_bytes()[..report.len().min(MAX_PANIC_BYTES)],
        )?;
        write_json(
            &self.directory.join(LAST_EXIT_FILE),
            &ExitRecord {
                schema_version: 2,
                run_id: self.marker.run_id.clone(),
                started_at_unix_ms: self.marker.started_at_unix_ms,
                ended_at_unix_ms: now,
                status: ExitStatus::Panic,
                reason: "The run encountered a Rust panic".to_owned(),
                version: self.marker.version.clone(),
                os: self.marker.os.clone(),
                architecture: self.marker.architecture.clone(),
                artifact: Some(report_name),
            },
        )
    }
}

fn open_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn try_lock(file: &File) -> io::Result<bool> {
    match fs2::FileExt::try_lock_exclusive(file) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn lock_catalog(directory: &Path) -> io::Result<File> {
    let file = open_lock(&directory.join(CATALOG_LOCK_FILE))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !try_lock(&file)? {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "diagnostic catalog is busy",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(file)
}

fn scan_and_prune(runs: &Path, now: u128) -> io::Result<Option<ExitRecord>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(runs)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let directory = entry.path();
        let lifetime = open_lock(&directory.join(RUN_LOCK_FILE))?;
        if !try_lock(&lifetime)? {
            continue;
        }
        let record = RunJournal::load_previous_exit(&directory, now)?;
        if let Some(record) = record {
            write_json(&directory.join(LAST_EXIT_FILE), &record)?;
            records.push((directory, record));
        } else {
            // Windows requires closing the probe before unlinking. Catalog
            // ownership excludes every possible new opener until removal ends.
            drop(lifetime);
            fs::remove_dir_all(directory)?;
        }
    }
    records.sort_by(|(_, a), (_, b)| {
        (b.ended_at_unix_ms, &b.run_id).cmp(&(a.ended_at_unix_ms, &a.run_id))
    });
    let previous = records
        .iter()
        .find(|(_, record)| record.status != ExitStatus::Clean)
        .or_else(|| records.first())
        .map(|(_, record)| record.clone());
    let (mut clean, mut failed) = (0, 0);
    for (directory, record) in records {
        // Keep separate failure/clean quotas so unrelated clean runs cannot
        // conceal the last failure. Active runs were excluded before reading.
        let count = if record.status == ExitStatus::Clean {
            &mut clean
        } else {
            &mut failed
        };
        *count += 1;
        if *count > MAX_INACTIVE_RUNS_PER_CLASS {
            fs::remove_dir_all(directory)?;
        }
    }
    Ok(previous)
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn read_optional_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<Option<T>> {
    match read_json(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    // persist replaces atomically on Windows as well as Unix, without an
    // unlink-first fallback that could discard the previous valid record.
    file.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone)]
struct DiagnosticMakeWriter {
    log_writer: Option<Arc<Mutex<BoundedLog>>>,
}

impl<'a> MakeWriter<'a> for DiagnosticMakeWriter {
    type Writer = DiagnosticWriter;

    fn make_writer(&'a self) -> Self::Writer {
        DiagnosticWriter {
            stderr: io::stderr(),
            log_writer: self.log_writer.clone(),
        }
    }
}

struct DiagnosticWriter {
    stderr: io::Stderr,
    log_writer: Option<Arc<Mutex<BoundedLog>>>,
}

impl Write for DiagnosticWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let stderr_result = self.stderr.write_all(buffer);
        let mut log_succeeded = false;
        if let Some(log_writer) = &self.log_writer {
            match log_writer.lock() {
                Ok(mut writer) => {
                    if let Err(error) = writer.write_all(buffer) {
                        DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                        eprintln!("fesTerm diagnostic log write failed: {error}");
                    } else {
                        log_succeeded = true;
                    }
                }
                Err(error) => {
                    DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                    eprintln!("fesTerm diagnostic log lock failed: {error}");
                }
            }
        }
        if stderr_result.is_ok() || log_succeeded {
            Ok(buffer.len())
        } else {
            stderr_result.map(|()| buffer.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()?;
        if let Some(log_writer) = &self.log_writer {
            let result = log_writer
                .lock()
                .map_err(|error| io::Error::other(error.to_string()))
                .and_then(|mut writer| writer.flush());
            if let Err(error) = result {
                DIAGNOSTICS_UNAVAILABLE.store(true, Ordering::Relaxed);
                eprintln!("fesTerm diagnostic log flush failed: {error}");
                return Err(error);
            }
        }
        Ok(())
    }
}

struct BoundedLog {
    file: File,
    remaining: u64,
    _lifetime: Arc<File>,
}

impl BoundedLog {
    fn open(journal: &RunJournal) -> io::Result<Self> {
        let current = journal.directory.join(CURRENT_LOG_FILE);
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(current)?;
        Ok(Self {
            file,
            remaining: MAX_LOG_BYTES,
            _lifetime: journal.lifetime.clone(),
        })
    }
}

impl Write for BoundedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let length = buffer.len().min(self.remaining as usize);
        if length == 0 {
            return Ok(buffer.len());
        }
        self.file.write_all(&buffer[..length])?;
        self.remaining -= length as u64;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temporary_directory(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "festerm-diagnostics-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn clean_exit_records_the_requested_reason_and_clears_the_run_marker() {
        let directory = temporary_directory("clean");
        let (mut journal, previous) = RunJournal::start_in(directory.clone(), 100).unwrap();
        assert!(previous.is_none());

        journal.record_intent(ExitIntent::UserQuit, 110).unwrap();
        journal.finish(true, 120).unwrap();

        assert!(!journal.directory.join(RUN_MARKER_FILE).exists());
        assert!(!journal.directory.join(EXIT_INTENT_FILE).exists());
        let record: ExitRecord = read_json(&journal.directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.status, ExitStatus::Clean);
        assert_eq!(record.reason, "User requested quit");
        drop(journal);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn next_start_reports_a_missing_clean_shutdown_and_preserves_exit_intent() {
        let directory = temporary_directory("unclean");
        let (mut abandoned, _) = RunJournal::start_in(directory.clone(), 200).unwrap();
        abandoned
            .record_intent(ExitIntent::UpdateRestart, 210)
            .unwrap();
        drop(abandoned);

        let (current, previous) = RunJournal::start_in(directory.clone(), 300).unwrap();
        let previous = previous.expect("the abandoned run must be reported");
        assert_eq!(previous.status, ExitStatus::Unclean);
        assert!(
            previous
                .reason
                .contains("Update installed; restart requested"),
            "persisted user intent should survive the abrupt exit"
        );
        drop(current);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn first_exit_intent_wins_when_framework_close_delivery_follows_it() {
        let directory = temporary_directory("first-intent");
        let (mut journal, _) = RunJournal::start_in(directory.clone(), 350).unwrap();

        journal
            .record_intent(ExitIntent::UpdateRestart, 360)
            .unwrap();
        journal.record_intent(ExitIntent::UserQuit, 370).unwrap();
        journal.finish(true, 380).unwrap();

        let record: ExitRecord = read_json(&journal.directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.reason, "Update installed; restart requested");
        drop(journal);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn next_start_reuses_the_last_clean_exit_when_no_run_was_abandoned() {
        let directory = temporary_directory("previous-clean");
        let (mut first, _) = RunJournal::start_in(directory.clone(), 400).unwrap();
        first.finish(true, 410).unwrap();
        drop(first);

        let (second, previous) = RunJournal::start_in(directory.clone(), 500).unwrap();
        let previous = previous.expect("the clean exit should remain available");
        assert_eq!(previous.status, ExitStatus::Clean);
        assert_eq!(previous.reason, "Application event loop returned normally");
        drop(second);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bounded_log_never_grows_past_its_per_run_limit() {
        let directory = temporary_directory("bounded-log");
        let (journal, _) = RunJournal::start_in(directory.clone(), 100).unwrap();
        let mut writer = BoundedLog::open(&journal).unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..40 {
            writer.write_all(&chunk).unwrap();
        }
        writer.flush().unwrap();

        assert_eq!(
            fs::metadata(journal.directory.join(CURRENT_LOG_FILE))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        drop(writer);
        drop(journal);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_bounds_inactive_runs_but_protects_active_runs_and_failure_evidence() {
        let directory = temporary_directory("retention");
        let (active, _) = RunJournal::start_in(directory.clone(), 1).unwrap();
        let mut writer = BoundedLog::open(&active).unwrap();
        writer.write_all(b"active log").unwrap();
        let active_path = active.directory.clone();
        // A surviving log writer also owns the lifetime lock.
        drop(active);
        let mut failure_path = PathBuf::new();
        for now in 2..12 {
            let (mut run, _) = RunJournal::start_in(directory.clone(), now).unwrap();
            run.finish(false, now).unwrap();
            failure_path = run.directory.clone();
        }
        for now in 12..22 {
            let (mut run, previous) = RunJournal::start_in(directory.clone(), now).unwrap();
            assert_eq!(previous.unwrap().status, ExitStatus::RuntimeError);
            run.finish(true, now).unwrap();
        }
        let (last, previous) = RunJournal::start_in(directory.clone(), 30).unwrap();
        assert_eq!(previous.unwrap().status, ExitStatus::RuntimeError);
        assert!(failure_path.exists());
        assert_eq!(
            fs::read(active_path.join(CURRENT_LOG_FILE)).unwrap(),
            b"active log"
        );
        assert_eq!(
            fs::read_dir(directory.join(RUNS_DIRECTORY))
                .unwrap()
                .count(),
            12
        );
        drop(last);
        drop(writer);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn overlapping_same_process_runs_are_unique_and_do_not_erase_each_other() {
        let directory = temporary_directory("overlap");
        let (mut a, _) = RunJournal::start_in(directory.clone(), 100).unwrap();
        let (b, previous) = RunJournal::start_in(directory.clone(), 100).unwrap();
        assert!(previous.is_none(), "a live run is not an unclean exit");
        assert_ne!(a.marker.run_id, b.marker.run_id);
        let b_id = b.marker.run_id.clone();
        let mut a_log = BoundedLog::open(&a).unwrap();
        let mut b_log = BoundedLog::open(&b).unwrap();
        a_log.write_all(b"a").unwrap();
        b_log.write_all(b"b").unwrap();
        a.finish(true, 110).unwrap();
        assert!(b.directory.join(RUN_MARKER_FILE).exists());
        assert_eq!(fs::read(a.directory.join(CURRENT_LOG_FILE)).unwrap(), b"a");
        assert_eq!(fs::read(b.directory.join(CURRENT_LOG_FILE)).unwrap(), b"b");
        drop((a_log, b_log, a, b));
        let (c, previous) = RunJournal::start_in(directory.clone(), 120).unwrap();
        let previous = previous.unwrap();
        assert_eq!(previous.run_id, b_id);
        assert_eq!(previous.status, ExitStatus::Unclean);
        drop(c);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_atomic_replacement_preserves_evidence_and_intent_can_retry() {
        let directory = temporary_directory("write-error");
        let (mut run, _) = RunJournal::start_in(directory.clone(), 100).unwrap();
        let intent = run.directory.join(EXIT_INTENT_FILE);
        fs::create_dir(&intent).unwrap();
        fs::write(intent.join("evidence"), b"keep").unwrap();
        assert!(run.record_intent(ExitIntent::UserQuit, 101).is_err());
        assert!(run.intent.is_none());
        assert_eq!(fs::read(intent.join("evidence")).unwrap(), b"keep");
        fs::remove_dir_all(&intent).unwrap();
        run.record_intent(ExitIntent::NativeSmokeComplete, 102)
            .unwrap();
        run.finish(false, 103).unwrap();
        let record: ExitRecord = read_json(&run.directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.status, ExitStatus::RuntimeError);
        assert_eq!(
            record.reason,
            "The application event loop returned an error"
        );
        assert!(BoundedLog::open(&run).is_ok());
        assert!(
            BoundedLog::open(&run).is_err(),
            "never truncate an existing log"
        );
        drop(run);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn catalog_serializes_same_process_openers_and_incomplete_creation_is_recovered() {
        let directory = temporary_directory("catalog");
        let catalog = lock_catalog(&directory).unwrap();
        let other = open_lock(&directory.join(CATALOG_LOCK_FILE)).unwrap();
        assert!(!try_lock(&other).unwrap());
        let incomplete = directory.join(RUNS_DIRECTORY).join("incomplete");
        fs::create_dir_all(&incomplete).unwrap();
        drop((catalog, other));
        let (run, previous) = RunJournal::start_in(directory.clone(), 100).unwrap();
        assert!(previous.is_none());
        assert!(!incomplete.exists());
        drop(run);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unreadable_evidence_and_catalog_errors_are_propagated() {
        let directory = temporary_directory("read-error");
        let (run, _) = RunJournal::start_in(directory.clone(), 100).unwrap();
        fs::write(run.directory.join(RUN_MARKER_FILE), b"not json").unwrap();
        drop(run);
        assert!(RunJournal::start_in(directory.clone(), 101).is_err());
        fs::remove_dir_all(directory.join(RUNS_DIRECTORY)).unwrap();
        fs::remove_file(directory.join(CATALOG_LOCK_FILE)).unwrap();
        fs::create_dir(directory.join(CATALOG_LOCK_FILE)).unwrap();
        assert!(RunJournal::start_in(directory.clone(), 102).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn panic_artifact_without_final_record_survives_recovery_and_retention() {
        let directory = temporary_directory("panic-artifact");
        for now in 1..8 {
            let (run, _) = RunJournal::start_in(directory.clone(), now).unwrap();
            fs::write(
                run.directory
                    .join(format!("panic-{}.txt", run.marker.run_id)),
                b"controlled report",
            )
            .unwrap();
        }
        let (run, previous) = RunJournal::start_in(directory.clone(), 8).unwrap();
        let previous = previous.unwrap();
        assert_eq!(previous.status, ExitStatus::Panic);
        assert!(previous.artifact.is_some());
        assert_eq!(
            fs::read_dir(directory.join(RUNS_DIRECTORY))
                .unwrap()
                .count(),
            6
        );
        drop(run);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_smoke_exit_is_clean_and_finishing_twice_is_idempotent() {
        let directory = temporary_directory("smoke");
        let (mut run, _) = RunJournal::start_in(directory.clone(), 100).unwrap();
        run.record_intent(ExitIntent::NativeSmokeComplete, 101)
            .unwrap();
        run.finish(true, 102).unwrap();
        run.finish(false, 103).unwrap();
        let record: ExitRecord = read_json(&run.directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.status, ExitStatus::Clean);
        assert_eq!(record.reason, "Native smoke completed");
        drop(run);
        fs::remove_dir_all(directory).unwrap();
    }

    // Re-enter only this test in owned child processes, never the GUI or daemon.
    #[test]
    fn diagnostics_child_process() {
        let Some(directory) = std::env::var_os("FESTERM_DIAGNOSTICS_TEST_ROOT") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let label = std::env::var("FESTERM_DIAGNOSTICS_TEST_LABEL").unwrap();
        let mode = std::env::var("FESTERM_DIAGNOSTICS_TEST_MODE").unwrap();
        if mode == "smoke-init" || mode == "smoke-missing-result" {
            let guard = init();
            if mode == "smoke-init" {
                let journal = JOURNAL.get().unwrap().lock().unwrap();
                assert!(journal
                    .directory
                    .starts_with(directory.join("result.txt.diagnostics")));
            } else {
                assert!(JOURNAL.get().is_none());
                assert!(last_exit_summary().unwrap().contains("unavailable"));
            }
            guard.finish(true);
            return;
        }
        if mode == "startup-error" || mode == "catalog-busy" {
            if mode == "startup-error" {
                fs::create_dir(directory.join(CATALOG_LOCK_FILE)).unwrap();
            }
            let guard = init_in(Some(directory));
            assert!(last_exit_summary().unwrap().contains("unavailable"));
            guard.finish(true);
            return;
        }
        let (mut journal, previous) = RunJournal::start_in(directory.clone(), 100).unwrap();
        assert!(previous.is_none());
        journal
            .record_intent(ExitIntent::UpdateRestart, 101)
            .unwrap();
        let mut log = BoundedLog::open(&journal).unwrap();
        log.write_all(label.as_bytes()).unwrap();
        log.flush().unwrap();
        let id = journal.marker.run_id.clone();
        let run_directory = journal.directory.clone();
        let mut journal = Some(journal);
        if mode.starts_with("panic") {
            assert!(JOURNAL.set(Mutex::new(journal.take().unwrap())).is_ok());
            install_panic_hook(&run_directory);
        }
        atomic_write(&directory.join(format!("{label}.ready")), id.as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !directory.join(format!("{label}.go")).exists() {
            assert!(Instant::now() < deadline, "child handshake timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        match mode.as_str() {
            "clean" => journal.as_mut().unwrap().finish(true, 110).unwrap(),
            "abrupt" => std::process::exit(23),
            "panic-clean" => {
                assert!(std::thread::spawn(|| panic!("controlled worker panic"))
                    .join()
                    .is_err());
                DiagnosticsGuard.finish(true);
                let record: ExitRecord =
                    read_json(&directory.join(RUNS_DIRECTORY).join(id).join(LAST_EXIT_FILE))
                        .unwrap();
                assert_eq!(record.status, ExitStatus::Panic);
                assert!(record.artifact.is_some());
            }
            "panic-busy" => {
                let guard = JOURNAL.get().unwrap().lock().unwrap();
                assert!(std::panic::catch_unwind(|| panic!("controlled busy panic")).is_err());
                drop(guard);
                DiagnosticsGuard.finish(true);
                let record: ExitRecord =
                    read_json(&directory.join(RUNS_DIRECTORY).join(id).join(LAST_EXIT_FILE))
                        .unwrap();
                assert_eq!(record.status, ExitStatus::Panic);
            }
            "panic-poisoned" => {
                assert!(std::panic::catch_unwind(|| {
                    let _guard = JOURNAL.get().unwrap().lock().unwrap();
                    panic!("controlled poisoned journal");
                })
                .is_err());
                DiagnosticsGuard.finish(true);
                let record: ExitRecord =
                    read_json(&directory.join(RUNS_DIRECTORY).join(id).join(LAST_EXIT_FILE))
                        .unwrap();
                assert_eq!(record.status, ExitStatus::Panic);
            }
            "panic-during-finish" => {
                let mut guard = JOURNAL.get().unwrap().lock().unwrap();
                guard.finish(true, 110).unwrap();
                assert!(
                    std::thread::spawn(|| panic!("panic after clean publication"))
                        .join()
                        .is_err()
                );
                // Model the race after finish chose Clean but before releasing
                // its mutex: only the independent flag can preserve this panic.
                let record: ExitRecord = read_json(&run_directory.join(LAST_EXIT_FILE)).unwrap();
                assert_eq!(record.status, ExitStatus::Clean);
            }
            _ => panic!("unexpected child mode"),
        }
    }

    struct OwnedChild(std::process::Child);

    impl OwnedChild {
        fn spawn(directory: &Path, label: &str, mode: &str) -> Self {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "diagnostics::tests::diagnostics_child_process",
                    "--nocapture",
                ])
                .env("FESTERM_DIAGNOSTICS_TEST_ROOT", directory)
                .env("FESTERM_DIAGNOSTICS_TEST_LABEL", label)
                .env("FESTERM_DIAGNOSTICS_TEST_MODE", mode)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null());
            if mode == "smoke-init" || mode == "smoke-missing-result" {
                command.env("FESTERM_NATIVE_WINDOW_SMOKE", "1");
                if mode == "smoke-init" {
                    command.env(
                        "FESTERM_NATIVE_SMOKE_RESULT_PATH",
                        directory.join("result.txt"),
                    );
                } else {
                    command.env_remove("FESTERM_NATIVE_SMOKE_RESULT_PATH");
                }
            }
            Self(command.spawn().unwrap())
        }

        fn wait(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                assert!(Instant::now() < deadline, "owned child did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn ready(&mut self, directory: &Path, label: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(20);
            let path = directory.join(format!("{label}.ready"));
            loop {
                if path.exists() {
                    return fs::read_to_string(path).unwrap();
                }
                assert!(
                    self.0.try_wait().unwrap().is_none(),
                    "child exited before ready"
                );
                assert!(
                    Instant::now() < deadline,
                    "owned child did not become ready"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
                self.wait();
            }
        }
    }

    #[test]
    fn concurrent_child_processes_preserve_killed_and_abrupt_exit_evidence() {
        for mode in ["killed", "abrupt"] {
            let directory = temporary_directory(mode);
            let mut a = OwnedChild::spawn(&directory, "a", "clean");
            let mut b = OwnedChild::spawn(&directory, "b", "abrupt");
            let a_id = a.ready(&directory, "a");
            let b_id = b.ready(&directory, "b");
            let (probe, previous) = RunJournal::start_in(directory.clone(), 105).unwrap();
            assert!(previous.is_none(), "both real processes are live");
            fs::write(directory.join("a.go"), b"").unwrap();
            assert!(a.wait().success());
            let b_path = directory.join(RUNS_DIRECTORY).join(&b_id);
            assert!(b_path.join(RUN_MARKER_FILE).exists());
            assert_eq!(fs::read(b_path.join(CURRENT_LOG_FILE)).unwrap(), b"b");
            assert_eq!(
                fs::read(
                    directory
                        .join(RUNS_DIRECTORY)
                        .join(a_id)
                        .join(CURRENT_LOG_FILE)
                )
                .unwrap(),
                b"a"
            );
            if mode == "killed" {
                b.0.kill().unwrap();
            } else {
                fs::write(directory.join("b.go"), b"").unwrap();
            }
            assert!(!b.wait().success());
            let (mut clean, previous) = RunJournal::start_in(directory.clone(), 120).unwrap();
            let previous = previous.unwrap();
            assert_eq!(previous.run_id, b_id);
            assert_eq!(previous.status, ExitStatus::Unclean);
            assert!(previous.reason.contains("restart requested"));
            clean.finish(true, 121).unwrap();
            drop(clean);
            let (next, previous) = RunJournal::start_in(directory.clone(), 130).unwrap();
            assert_eq!(
                previous.unwrap().run_id,
                b_id,
                "a later clean exit must not hide b"
            );
            drop((a, b, probe, next));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn child_panics_survive_clean_finish_and_busy_hook_never_deadlocks() {
        for mode in [
            "panic-clean",
            "panic-busy",
            "panic-poisoned",
            "panic-during-finish",
        ] {
            let directory = temporary_directory(mode);
            let mut child = OwnedChild::spawn(&directory, "panic", mode);
            child.ready(&directory, "panic");
            fs::write(directory.join("panic.go"), b"").unwrap();
            assert!(child.wait().success());
            let (run, previous) = RunJournal::start_in(directory.clone(), 150).unwrap();
            assert_eq!(previous.unwrap().status, ExitStatus::Panic);
            drop((child, run));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn unavailable_diagnostics_do_not_abort_application_initialization() {
        for mode in ["startup-error", "catalog-busy"] {
            let directory = temporary_directory(mode);
            let catalog = (mode == "catalog-busy").then(|| lock_catalog(&directory).unwrap());
            let mut child = OwnedChild::spawn(&directory, "error", mode);
            assert!(child.wait().success());
            drop((child, catalog));
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn native_smoke_diagnostics_never_use_the_user_journal() {
        let directory = temporary_directory("smoke-directory");
        let native = directory.join("user");
        let result = directory.join("smoke-result.txt");
        let smoke =
            select_diagnostics_directory(Some(native.clone()), true, Some(result.clone())).unwrap();
        assert_eq!(smoke, directory.join("smoke-result.txt.diagnostics"));
        let (mut journal, previous) = RunJournal::start_in(smoke, 100).unwrap();
        assert!(previous.is_none());
        journal
            .record_intent(ExitIntent::NativeSmokeComplete, 101)
            .unwrap();
        journal.finish(true, 102).unwrap();
        assert!(!native.exists());
        assert!(select_diagnostics_directory(Some(native.clone()), true, None).is_none());
        assert_eq!(
            select_diagnostics_directory(Some(native.clone()), false, Some(result)),
            Some(native)
        );
        drop(journal);
        for mode in ["smoke-init", "smoke-missing-result"] {
            let mut child = OwnedChild::spawn(&directory, "smoke", mode);
            assert!(child.wait().success());
            drop(child);
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
