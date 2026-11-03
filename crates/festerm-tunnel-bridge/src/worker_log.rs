//! Always-on, bounded event log for the managed worker: `worker.log` (and one
//! rotated `worker.log.1`) in the private state directory. Lines contain only
//! fixed event names and stage-code-shaped values, never tokens, paths,
//! terminal data or free-form error text. Logging never blocks serving.
use crate::setup_storage;
use std::{
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const NAME: &str = "worker.log";
const PREVIOUS: &str = "worker.log.1";
const LIMIT: u64 = 256 * 1024;
const VALUE_LIMIT: usize = 96;
/// Prefix the bridge uses for lines the worker may forward to this log.
pub const BRIDGE_EVENT_PREFIX: &str = "festerm-event ";

#[derive(Clone)]
pub struct WorkerLog {
    state: PathBuf,
}

/// Returns `value` when it looks like a stage code, else a fixed placeholder.
pub fn code(value: &str) -> &str {
    if !value.is_empty()
        && value.len() <= VALUE_LIMIT
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    {
        value
    } else {
        "unclassified"
    }
}

/// Validates one bridge stderr line, returning the event text to record.
pub fn bridge_event(line: &str) -> Option<String> {
    let mut tokens = line.strip_prefix(BRIDGE_EVENT_PREFIX)?.split_whitespace();
    let event = tokens.next().filter(|event| code(event) == *event)?;
    if !event.starts_with("bridge.") {
        return None;
    }
    let mut text = event.to_owned();
    for token in tokens.take(8) {
        let (key, value) = token.split_once('=')?;
        if code(key) != key {
            return None;
        }
        text.push_str(&format!(" {key}={}", code(value)));
    }
    Some(text)
}

impl WorkerLog {
    pub fn new(state: &Path) -> Self {
        Self {
            state: state.to_owned(),
        }
    }

    pub fn event(&self, event: &str, fields: &[(&str, &str)]) {
        let mut line = format!("{} pid={} {}", timestamp(), std::process::id(), code(event));
        for (key, value) in fields {
            line.push_str(&format!(" {}={}", code(key), code(value)));
        }
        let _ = self.append(&line);
    }

    pub fn raw_event(&self, text: &str) {
        let _ = self.append(&format!(
            "{} pid={} {text}",
            timestamp(),
            std::process::id()
        ));
    }

    fn append(&self, line: &str) -> io::Result<()> {
        let directory = setup_storage::private_directory(&self.state, false)?;
        let mut file = match setup_storage::open_private_file(&directory, Path::new(NAME), false) {
            Ok(file) if file.metadata()?.len() >= LIMIT => {
                drop(file);
                std::fs::rename(self.state.join(NAME), self.state.join(PREVIOUS))?;
                setup_storage::open_private_file(&directory, Path::new(NAME), true)?
            }
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                setup_storage::open_private_file(&directory, Path::new(NAME), true)?
            }
            Err(error) => return Err(error),
        };
        file.seek(SeekFrom::End(0))?;
        file.write_all(format!("{line}\n").as_bytes())
    }
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ` without a date/time dependency.
fn timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format_utc(seconds)
}

fn format_utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_log_values_are_stage_codes_only() {
        assert_eq!(code("host.outer_task_closed"), "host.outer_task_closed");
        for unsafe_value in [
            "",
            "Bearer abc",
            "/home/fes",
            "a\nb",
            "Token",
            &"a".repeat(97),
        ] {
            assert_eq!(code(unsafe_value), "unclassified");
        }
    }

    #[test]
    fn worker_log_forwards_only_well_formed_bridge_events() {
        assert_eq!(
            bridge_event("festerm-event bridge.client_closed n=3 result=error reason=x.y").unwrap(),
            "bridge.client_closed n=3 result=error reason=x.y"
        );
        assert_eq!(
            bridge_event("festerm-event bridge.exit reason=Some secret").as_deref(),
            None
        );
        assert_eq!(
            bridge_event("festerm-event bridge.exit reason=/tmp/x").unwrap(),
            "bridge.exit reason=unclassified"
        );
        for line in [
            "Experimental bridge ready",
            "festerm-event worker.exit",
            "festerm-event BRIDGE.exit",
        ] {
            assert_eq!(bridge_event(line), None);
        }
    }

    #[test]
    fn worker_log_timestamps_are_utc_iso8601() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_793_581_272), "2026-11-02T01:01:12Z");
    }
}
