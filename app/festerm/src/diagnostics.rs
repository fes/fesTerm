use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    backtrace::Backtrace,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic::PanicHookInfo,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, TryLockError},
    time::{SystemTime, UNIX_EPOCH},
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
const PREVIOUS_LOG_FILE: &str = "festerm.previous.log";
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PANIC_REPORTS: usize = 5;

static JOURNAL: OnceLock<Mutex<RunJournal>> = OnceLock::new();
static LAST_EXIT: OnceLock<Option<ExitRecord>> = OnceLock::new();

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
        match journal.lock() {
            Ok(mut journal) => {
                if let Err(error) = journal.finish(succeeded, now_unix_ms()) {
                    tracing::error!(
                        target: "festerm::diagnostics",
                        %error,
                        "could not record the application exit"
                    );
                }
            }
            Err(error) => tracing::error!(
                target: "festerm::diagnostics",
                %error,
                "could not lock the application lifecycle journal"
            ),
        }
    }
}

pub fn init() -> DiagnosticsGuard {
    let (journal, previous_exit, diagnostics_error) = match diagnostics_directory() {
        Some(directory) => match RunJournal::start_in(directory, now_unix_ms()) {
            Ok((journal, previous_exit)) => (Some(journal), previous_exit, None),
            Err(error) => (None, None, Some(error)),
        },
        None => (
            None,
            None,
            Some(io::Error::other(
                "the platform did not provide a per-user application data directory",
            )),
        ),
    };

    let log_writer =
        journal
            .as_ref()
            .and_then(|journal| match BoundedLog::open(&journal.directory) {
                Ok(writer) => Some(Arc::new(Mutex::new(writer))),
                Err(error) => {
                    eprintln!("fesTerm could not initialize its diagnostic log: {error}");
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
        .init();

    let _ = LAST_EXIT.set(previous_exit);
    if let Some(journal) = journal {
        let _ = JOURNAL.set(Mutex::new(journal));
        install_panic_hook();
    }
    if let Some(error) = diagnostics_error {
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
                tracing::error!(
                    target: "festerm::diagnostics",
                    %error,
                    "could not persist the requested exit reason"
                );
            }
        }
        Err(error) => tracing::error!(
            target: "festerm::diagnostics",
            %error,
            "could not lock the application lifecycle journal"
        ),
    }
}

pub(crate) fn last_exit_summary() -> Option<String> {
    LAST_EXIT
        .get()
        .and_then(|record| record.as_ref())
        .map(ExitRecord::support_summary)
}

fn diagnostics_directory() -> Option<PathBuf> {
    ProjectDirs::from("com", "fes", "fesTerm").map(|directories| {
        directories
            .state_dir()
            .unwrap_or_else(|| directories.data_local_dir())
            .join(DIAGNOSTICS_DIRECTORY)
    })
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |information| {
        record_panic(information);
        previous(information);
    }));
}

fn record_panic(information: &PanicHookInfo<'_>) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };
    let mut journal = match journal.try_lock() {
        Ok(journal) => journal,
        Err(TryLockError::Poisoned(error)) => {
            eprintln!("fesTerm could not lock its panic journal: {error}");
            return;
        }
        Err(TryLockError::WouldBlock) => {
            eprintln!("fesTerm panic journal was busy; no panic report was written");
            return;
        }
    };
    if let Err(error) = journal.record_panic(information, now_unix_ms()) {
        eprintln!("fesTerm could not write its panic report: {error}");
    }
}

struct RunJournal {
    directory: PathBuf,
    marker: RunMarker,
    intent: Option<ExitIntent>,
}

impl RunJournal {
    fn start_in(directory: PathBuf, now: u128) -> io::Result<(Self, Option<ExitRecord>)> {
        fs::create_dir_all(&directory)?;
        let previous = Self::load_previous_exit(&directory, now);
        if let Some(previous) = &previous {
            write_json(&directory.join(LAST_EXIT_FILE), previous)?;
        }
        let marker = RunMarker {
            schema_version: 1,
            run_id: format!("{}-{now}", std::process::id()),
            started_at_unix_ms: now,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        };
        remove_if_exists(&directory.join(EXIT_INTENT_FILE))?;
        write_json(&directory.join(RUN_MARKER_FILE), &marker)?;
        prune_panic_reports(&directory, MAX_PANIC_REPORTS)?;
        Ok((
            Self {
                directory,
                marker,
                intent: None,
            },
            previous,
        ))
    }

    fn load_previous_exit(directory: &Path, now: u128) -> Option<ExitRecord> {
        let marker = match read_json::<RunMarker>(&directory.join(RUN_MARKER_FILE)) {
            Ok(marker) => marker,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return match read_json(&directory.join(LAST_EXIT_FILE)) {
                    Ok(record) => Some(record),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                    Err(error) => {
                        eprintln!("fesTerm could not read its last-exit record: {error}");
                        None
                    }
                };
            }
            Err(error) => {
                eprintln!("fesTerm could not read its previous run marker: {error}");
                return Some(ExitRecord {
                    schema_version: 1,
                    run_id: "unreadable".to_owned(),
                    started_at_unix_ms: 0,
                    ended_at_unix_ms: now,
                    status: ExitStatus::Unclean,
                    reason: "The previous run marker was unreadable".to_owned(),
                    version: "unknown".to_owned(),
                    os: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    artifact: None,
                });
            }
        };
        let intent = match read_json::<PersistedIntent>(&directory.join(EXIT_INTENT_FILE)) {
            Ok(intent) if intent.run_id == marker.run_id => Some(intent.intent),
            Ok(_) => None,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                eprintln!("fesTerm could not read its previous exit intent: {error}");
                None
            }
        };
        let panic_name = format!("panic-{}.txt", marker.run_id);
        let panic_path = directory.join(&panic_name);
        let (status, reason, artifact) = if panic_path.is_file() {
            (
                ExitStatus::Panic,
                "The previous run ended during a Rust panic".to_owned(),
                Some(panic_name),
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
        Some(ExitRecord {
            schema_version: 1,
            run_id: marker.run_id,
            started_at_unix_ms: marker.started_at_unix_ms,
            ended_at_unix_ms: now,
            status,
            reason,
            version: marker.version,
            os: marker.os,
            architecture: marker.architecture,
            artifact,
        })
    }

    fn record_intent(&mut self, intent: ExitIntent, now: u128) -> io::Result<()> {
        if self.intent.is_some() {
            return Ok(());
        }
        self.intent = Some(intent);
        write_json(
            &self.directory.join(EXIT_INTENT_FILE),
            &PersistedIntent {
                run_id: self.marker.run_id.clone(),
                intent,
                recorded_at_unix_ms: now,
            },
        )
    }

    fn finish(&mut self, succeeded: bool, now: u128) -> io::Result<()> {
        let (status, reason) = if succeeded {
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
            schema_version: 1,
            run_id: self.marker.run_id.clone(),
            started_at_unix_ms: self.marker.started_at_unix_ms,
            ended_at_unix_ms: now,
            status,
            reason,
            version: self.marker.version.clone(),
            os: self.marker.os.clone(),
            architecture: self.marker.architecture.clone(),
            artifact: None,
        };
        write_json(&self.directory.join(LAST_EXIT_FILE), &record)?;
        remove_if_exists(&self.directory.join(EXIT_INTENT_FILE))?;
        remove_if_exists(&self.directory.join(RUN_MARKER_FILE))
    }

    fn record_panic(&mut self, information: &PanicHookInfo<'_>, now: u128) -> io::Result<()> {
        prune_panic_reports(&self.directory, MAX_PANIC_REPORTS.saturating_sub(1))?;
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
        let mut file = File::create(path)?;
        file.write_all(report.as_bytes())?;
        file.sync_all()?;
        write_json(
            &self.directory.join(LAST_EXIT_FILE),
            &ExitRecord {
                schema_version: 1,
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

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    if fs::rename(&temporary, path).is_err() {
        remove_if_exists(path)?;
        fs::rename(temporary, path)?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn prune_panic_reports(directory: &Path, retain: usize) -> io::Result<()> {
    let mut reports = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("panic-") && name.ends_with(".txt")
        })
        .collect::<Vec<_>>();
    reports.sort_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    });
    let remove_count = reports.len().saturating_sub(retain);
    for report in reports.into_iter().take(remove_count) {
        remove_if_exists(&report.path())?;
    }
    Ok(())
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
                        eprintln!("fesTerm diagnostic log write failed: {error}");
                    } else {
                        log_succeeded = true;
                    }
                }
                Err(error) => eprintln!("fesTerm diagnostic log lock failed: {error}"),
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
            if let Ok(mut writer) = log_writer.lock() {
                writer.flush()?;
            }
        }
        Ok(())
    }
}

struct BoundedLog {
    file: File,
    remaining: u64,
}

impl BoundedLog {
    fn open(directory: &Path) -> io::Result<Self> {
        let current = directory.join(CURRENT_LOG_FILE);
        let previous = directory.join(PREVIOUS_LOG_FILE);
        remove_if_exists(&previous)?;
        if current.exists() {
            fs::rename(&current, previous)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(current)?;
        Ok(Self {
            file,
            remaining: MAX_LOG_BYTES,
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
        let directory = std::env::temp_dir().join(format!(
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

        assert!(!directory.join(RUN_MARKER_FILE).exists());
        assert!(!directory.join(EXIT_INTENT_FILE).exists());
        let record: ExitRecord = read_json(&directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.status, ExitStatus::Clean);
        assert_eq!(record.reason, "User requested quit");
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

        let (_current, previous) = RunJournal::start_in(directory.clone(), 300).unwrap();
        let previous = previous.expect("the abandoned run must be reported");
        assert_eq!(previous.status, ExitStatus::Unclean);
        assert!(
            previous
                .reason
                .contains("Update installed; restart requested"),
            "persisted user intent should survive the abrupt exit"
        );
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

        let record: ExitRecord = read_json(&directory.join(LAST_EXIT_FILE)).unwrap();
        assert_eq!(record.reason, "Update installed; restart requested");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn next_start_reuses_the_last_clean_exit_when_no_run_was_abandoned() {
        let directory = temporary_directory("previous-clean");
        let (mut first, _) = RunJournal::start_in(directory.clone(), 400).unwrap();
        first.finish(true, 410).unwrap();

        let (_second, previous) = RunJournal::start_in(directory.clone(), 500).unwrap();
        let previous = previous.expect("the clean exit should remain available");
        assert_eq!(previous.status, ExitStatus::Clean);
        assert_eq!(previous.reason, "Application event loop returned normally");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bounded_log_never_grows_past_its_per_run_limit() {
        let directory = temporary_directory("bounded-log");
        let mut writer = BoundedLog::open(&directory).unwrap();
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..40 {
            writer.write_all(&chunk).unwrap();
        }
        writer.flush().unwrap();

        assert_eq!(
            fs::metadata(directory.join(CURRENT_LOG_FILE))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn startup_caps_panic_report_retention_at_five() {
        let directory = temporary_directory("panic-retention");
        for index in 0..7 {
            fs::write(
                directory.join(format!("panic-test-{index}.txt")),
                format!("report {index}"),
            )
            .unwrap();
        }

        let (_journal, _) = RunJournal::start_in(directory.clone(), 600).unwrap();

        let retained = fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("panic-"))
            .count();
        assert_eq!(retained, MAX_PANIC_REPORTS);
        fs::remove_dir_all(directory).unwrap();
    }
}
