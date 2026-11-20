//! Bounded historical evidence only. Never an admission, liveness or retry input.
use super::{managed, read_run_record, RunRecord, Store};
use crate::diagnostics::{self, Category};
use festerm_sessiond::private_storage;
use festerm_tunnel_bridge::{relay::HostFailureDisposition, worker_log::WorkerLog};
use std::{collections::BTreeMap, io, path::Path};

// WorkerLog checks rotation before appending: permit its one bounded final-line
// overshoot, never an unbounded file read. The stricter per-line cap still applies.
const FILE_BYTES: usize = 256 * 1024 + 1024;
const LINE_BYTES: usize = 1024;
const LINE_COUNT: usize = 2048;
const RECORD_COUNT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    RelayOpen,
    PolicyRefresh,
    RuntimeServe,
    RelayCleanup,
    PolicyWatch,
    HistoryPersist,
    OrdinaryOwnerDigest,
    OrdinaryCloudDigest,
    OrdinaryIdentityBefore,
    OrdinaryIdentityAfter,
    OrdinaryRenewal,
}
impl Phase {
    pub fn code(self) -> &'static str {
        match self {
            Self::RelayOpen => "relay_open",
            Self::PolicyRefresh => "policy_refresh",
            Self::RuntimeServe => "runtime_serve",
            Self::RelayCleanup => "relay_cleanup",
            Self::PolicyWatch => "policy_watch",
            Self::HistoryPersist => "history_persist",
            Self::OrdinaryOwnerDigest => "ordinary_owner_digest_changed",
            Self::OrdinaryCloudDigest => "ordinary_cloud_digest_changed",
            Self::OrdinaryIdentityBefore => "ordinary_identity_not_ready_before",
            Self::OrdinaryIdentityAfter => "ordinary_identity_not_ready_after",
            Self::OrdinaryRenewal => "ordinary_inplace_renewal",
        }
    }
    fn parse(code: &str) -> Option<Self> {
        [
            Self::RelayOpen,
            Self::PolicyRefresh,
            Self::RuntimeServe,
            Self::RelayCleanup,
            Self::PolicyWatch,
            Self::HistoryPersist,
            Self::OrdinaryOwnerDigest,
            Self::OrdinaryCloudDigest,
            Self::OrdinaryIdentityBefore,
            Self::OrdinaryIdentityAfter,
            Self::OrdinaryRenewal,
        ]
        .into_iter()
        .find(|phase| phase.code() == code)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Ok,
    Error,
    Closed,
    Cancelled,
}
impl Outcome {
    pub fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(code: &str) -> Option<Self> {
        [Self::Ok, Self::Error, Self::Closed, Self::Cancelled]
            .into_iter()
            .find(|outcome| outcome.code() == code)
    }
}

// These are the existing relay's closed taxonomy, never error Display text.
const OUTER: &[&str] = &[
    "host.outer_closed",
    "host.outer_peer_disconnected",
    "host.outer_protocol_inconsistent",
    "host.outer_packet_auth",
    "host.outer_auth_state",
    "host.outer_algorithm_negotiation",
    "host.outer_unknown_algorithm",
    "host.outer_key_verification",
    "host.outer_key_exchange",
    "host.outer_packet_size",
    "host.outer_channel_state",
    "host.outer_channel",
    "host.outer_pending_limit",
    "host.outer_decryption",
    "host.outer_key_decode",
    "host.outer_ssh_key_decode",
    "host.outer_encoding",
    "host.outer_utf8",
    "host.outer_task",
    "host.outer_deadline",
    "host.outer_configuration",
    "host.outer_strict_exchange",
    "host.outer_protocol",
    "host.outer_request_denied",
    "host.outer_transport",
    "host.outer_timeout",
    "host.outer_other",
    "host.outer_channel_closed",
];
const CATEGORIES: &[(&str, Category)] = &[
    ("registration", Category::Registration),
    ("cancelled", Category::Cancelled),
    ("expired", Category::Expired),
    ("denied", Category::Denied),
    ("network", Category::Network),
    ("response", Category::Response),
    ("account_changed", Category::AccountChanged),
    ("identity_changed", Category::IdentityChanged),
    ("tunnel_not_found", Category::TunnelNotFound),
    ("tunnel", Category::Tunnel),
    ("ambiguous", Category::Ambiguous),
    ("store", Category::Store),
    ("busy", Category::Busy),
    ("rate_limited", Category::RateLimited),
    ("provider_rejected", Category::ProviderRejected),
    ("unsupported_user_acl", Category::UnsupportedUserAcl),
    ("unsupported_protocol", Category::UnsupportedProtocol),
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
    pub timestamp: String,
    pub pid: u32,
    pub epoch: u64,
    pub run_id: uuid::Uuid,
    pub phase: Phase,
    pub outcome: Outcome,
    pub outer: Option<&'static str>,
    pub disposition: Option<HostFailureDisposition>,
    pub category: Option<Category>,
    pub http_status: Option<u16>,
    pub os_code: Option<i32>,
    pub correlation: Correlation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Correlation {
    RuntimeRecord,
    ManagedHistory,
}

pub(super) struct Log {
    writer: WorkerLog,
    epoch: u64,
    run_id: Option<uuid::Uuid>,
}
impl Log {
    /// Separate line kind: old readers reject it without changing strict run
    /// history or mistaking a pre-serve admission observation for serving.
    pub(super) fn owner_admission(&self, event: crate::enrollment::OwnerAdmissionEvent) {
        use crate::enrollment::OwnerAdmissionEvent;
        let Some(run_id) = self.run_id else { return };
        let (stage, outcome, category) = match event {
            OwnerAdmissionEvent::Begin(stage) => (stage, "begin", "none"),
            OwnerAdmissionEvent::Pass(stage) => (stage, "ok", "none"),
            OwnerAdmissionEvent::Fail(stage, reason) => (stage, "error", reason.code()),
        };
        self.writer.event(
            "host.owner_admission",
            &[
                ("epoch", &self.epoch.to_string()),
                ("run", &run_id.to_string()),
                ("stage", stage.code()),
                ("outcome", outcome),
                ("category", category),
            ],
        );
    }

    pub(super) fn new(state: &Path, run: &RunRecord) -> Self {
        Self {
            writer: WorkerLog::new(state),
            epoch: run.epoch,
            run_id: uuid::Uuid::parse_str(&run.run_id)
                .ok()
                .filter(|id| id.to_string() == run.run_id),
        }
    }
    pub(super) fn readiness(
        &self,
        event: crate::ordinary::ReadinessEvent,
        error: Option<&io::Error>,
    ) {
        use crate::ordinary::ReadinessEvent;
        let phase = match event {
            ReadinessEvent::OwnerDigestChanged => Phase::OrdinaryOwnerDigest,
            ReadinessEvent::CloudDigestChanged => Phase::OrdinaryCloudDigest,
            ReadinessEvent::IdentityNotReadyBefore => Phase::OrdinaryIdentityBefore,
            ReadinessEvent::IdentityNotReadyAfter => Phase::OrdinaryIdentityAfter,
            ReadinessEvent::RenewalOk | ReadinessEvent::RenewalError => Phase::OrdinaryRenewal,
        };
        self.event(
            phase,
            if event == ReadinessEvent::RenewalOk {
                Outcome::Ok
            } else {
                Outcome::Error
            },
            error,
            None,
            None,
        );
    }

    pub(super) fn event(
        &self,
        phase: Phase,
        outcome: Outcome,
        error: Option<&io::Error>,
        outer: Option<&str>,
        disposition: Option<HostFailureDisposition>,
    ) {
        let Some(run_id) = self.run_id else { return };
        let diagnostic = error.and_then(diagnostics::from_io_error);
        let category = diagnostic
            .and_then(|value| {
                CATEGORIES
                    .iter()
                    .find(|(_, category)| *category == value.category)
                    .map(|(code, _)| *code)
            })
            .unwrap_or("none");
        let status = diagnostic
            .and_then(|value| value.http_status)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into());
        let os = error
            .and_then(io::Error::raw_os_error)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into());
        let outer = outer
            .and_then(|value| OUTER.iter().copied().find(|code| *code == value))
            .unwrap_or("not_observed");
        let disposition = match disposition {
            Some(HostFailureDisposition::Transient) => "transient",
            Some(HostFailureDisposition::Permanent) => "permanent",
            None => "not_observed",
        };
        // No Display/Debug of an error, and no caller-supplied string field.
        self.writer.event(
            "host.runtime",
            &[
                ("epoch", &self.epoch.to_string()),
                ("run", &run_id.to_string()),
                ("phase", phase.code()),
                ("outcome", outcome.code()),
                ("outer", outer),
                ("disposition", disposition),
                ("category", category),
                ("http", &status),
                ("os", &os),
            ],
        );
    }
}

fn optional<T>(value: &str, parse: impl FnOnce(&str) -> Option<T>) -> Option<Option<T>> {
    if value == "none" || value == "not_observed" {
        Some(None)
    } else {
        parse(value).map(Some)
    }
}

fn parse(line: &str) -> Option<Record> {
    if line.len() > LINE_BYTES || !line.is_ascii() {
        return None;
    }
    let tokens: Vec<_> = line.split_ascii_whitespace().take(13).collect();
    if tokens.len() != 12 || tokens[2] != "host.runtime" {
        return None;
    }
    let timestamp = tokens[0];
    if timestamp.len() != 20
        || timestamp.bytes().enumerate().any(|(i, b)| match i {
            4 | 7 => b != b'-',
            10 => b != b'T',
            13 | 16 => b != b':',
            19 => b != b'Z',
            _ => !b.is_ascii_digit(),
        })
    {
        return None;
    }
    for (range, min, max) in [
        (5..7, 1, 12),
        (8..10, 1, 31),
        (11..13, 0, 23),
        (14..16, 0, 59),
        (17..19, 0, 59),
    ] {
        let value: u8 = timestamp.get(range)?.parse().ok()?;
        if !(min..=max).contains(&value) {
            return None;
        }
    }
    let fields: BTreeMap<_, _> = tokens
        .iter()
        .skip(1)
        .filter(|token| **token != "host.runtime")
        .map(|token| token.split_once('='))
        .collect::<Option<_>>()?;
    if fields.len() != 10
        || fields.keys().copied().collect::<Vec<_>>()
            != [
                "category",
                "disposition",
                "epoch",
                "http",
                "os",
                "outcome",
                "outer",
                "phase",
                "pid",
                "run",
            ]
    {
        return None;
    }
    let pid = fields["pid"].parse().ok()?;
    if pid == 0 {
        return None;
    }
    let run_id = uuid::Uuid::parse_str(fields["run"]).ok()?;
    if run_id.to_string() != fields["run"] {
        return None;
    }
    Some(Record {
        timestamp: timestamp.into(),
        pid,
        epoch: fields["epoch"].parse().ok()?,
        run_id,
        phase: Phase::parse(fields["phase"])?,
        outcome: Outcome::parse(fields["outcome"])?,
        outer: optional(fields["outer"], |value| {
            OUTER.iter().copied().find(|code| *code == value)
        })?,
        disposition: optional(fields["disposition"], |value| match value {
            "transient" => Some(HostFailureDisposition::Transient),
            "permanent" => Some(HostFailureDisposition::Permanent),
            _ => None,
        })?,
        category: optional(fields["category"], |value| {
            CATEGORIES
                .iter()
                .find(|(code, _)| *code == value)
                .map(|(_, category)| *category)
        })?,
        http_status: optional(fields["http"], |value| {
            value
                .parse::<u16>()
                .ok()
                .filter(|value| (100..=599).contains(value))
        })?,
        os_code: optional(fields["os"], |value| value.parse().ok())?,
        correlation: Correlation::RuntimeRecord,
    })
}

/// Refresh-only optional history. Missing, old or malformed evidence never fails
/// status observation. Protected history correlates records; it grants no authority.
pub fn read(store: &Store, state: &Path) -> Vec<Record> {
    let managed = managed::last_report(store).ok().flatten();
    read_history(store, state, managed.as_ref()).unwrap_or_default()
}

fn read_history(
    store: &Store,
    state: &Path,
    managed: Option<&managed::Report>,
) -> io::Result<Vec<Record>> {
    let allowed = store.with_locked(|directory, snapshot| {
        let mut allowed = Vec::new();
        if let Some(run) = read_run_record(directory, snapshot)? {
            allowed.push((run.epoch, run.run_id, Correlation::RuntimeRecord));
        }
        if let Some(report) = managed.filter(|report| {
            report.installation == snapshot.installation
                && report.origin == snapshot.origin
                && report.epoch <= snapshot.epoch
        }) {
            if let Some(run) = &report.run_id {
                allowed.push((report.epoch, run.clone(), Correlation::ManagedHistory));
            }
        }
        Ok(allowed)
    })?;
    // Correlation is only a protected-history snapshot. Release the authority
    // lease before all optional log I/O and parsing; history grants no authority.
    if allowed.is_empty() {
        return Ok(Vec::new());
    }
    let logs = private_storage::private_directory(state, false)?;
    let mut records = Vec::new();
    for name in ["worker.log.1", "worker.log"] {
        let Ok(bytes) = private_storage::read_private(&logs, Path::new(name), FILE_BYTES) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if text.lines().take(LINE_COUNT + 1).count() > LINE_COUNT {
            continue;
        }
        for mut record in text.lines().filter_map(parse) {
            if let Some((_, _, correlation)) = allowed
                .iter()
                .rev()
                .find(|(epoch, run, _)| *epoch == record.epoch && *run == record.run_id.to_string())
            {
                record.correlation = *correlation;
                if records.len() == RECORD_COUNT {
                    records.remove(0);
                }
                records.push(record);
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod owner_admission_tests {
    use super::*;
    use crate::{
        controller::RunState,
        enrollment::{
            OwnerAdmissionEvent as Event, OwnerAdmissionFailure as Failure,
            OwnerAdmissionStage as Stage,
        },
    };

    #[test]
    fn host_owner_admission_log_is_bounded_and_old_runtime_reader_skips_it() {
        let root = private_storage::private_test_directory();
        let run = RunRecord {
            version: 1,
            installation: "public-fixture".into(),
            epoch: 2,
            run_id: uuid::Uuid::new_v4().to_string(),
            state: RunState::Starting,
            failure: None,
        };
        let log = Log::new(root.path(), &run);
        log.event(Phase::HistoryPersist, Outcome::Ok, None, None, None);
        log.owner_admission(Event::Begin(Stage::StoreOpen));
        log.owner_admission(Event::Fail(Stage::AnchorRead, Failure::LockedOrUnavailable));
        log.event(Phase::RuntimeServe, Outcome::Closed, None, None, None);
        let bytes = std::fs::read(root.path().join("worker.log")).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.lines().all(|line| line.len() < LINE_BYTES));
        let parsed: Vec<_> = text.lines().filter_map(parse).collect();
        assert_eq!(
            parsed.len(),
            2,
            "new line kind must not pollute old runtime records"
        );
        assert_eq!(parsed[0].phase, Phase::HistoryPersist);
        assert_eq!(parsed[1].phase, Phase::RuntimeServe);
        assert!(text
            .lines()
            .all(|line| line.contains(&format!("pid={} ", std::process::id()))));
        // Exercise existing rotation with one bounded new-kind append.
        std::fs::write(root.path().join("worker.log"), vec![b'x'; 256 * 1024]).unwrap();
        log.owner_admission(Event::Pass(Stage::Snapshot));
        assert_eq!(
            std::fs::metadata(root.path().join("worker.log.1"))
                .unwrap()
                .len(),
            256 * 1024
        );
        let newest = std::fs::read_to_string(root.path().join("worker.log")).unwrap();
        assert!(newest.len() < LINE_BYTES);
        assert!(newest.contains("host.owner_admission"));
        assert!(parse(newest.trim()).is_none());
    }
}
