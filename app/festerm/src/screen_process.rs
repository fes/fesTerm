//! Screen socket timestamps/inodes can change without restarting the shell.
//! A bounded, batched ps query supplies process-generation identity instead.

use std::collections::BTreeMap;

/// Old GNU screen has no client-list query. Its server opens each attached
/// client's slave terminal, which distinguishes our display from other clients.
#[cfg(target_os = "linux")]
pub(super) fn server_has_terminal(pid: u32, device: &std::path::Path) -> Result<bool, String> {
    let directory = format!("/proc/{pid}/fd");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Could not inspect screen server terminals: {error}"
            ))
        }
    };
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(
                "Screen server exceeds 4096 open descriptors; cannot confirm attachment".into(),
            );
        }
        let entry =
            entry.map_err(|error| format!("Could not inspect screen descriptor: {error}"))?;
        match std::fs::read_link(entry.path()) {
            Ok(target) if target == device => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not inspect screen terminal descriptor: {error}"
                ))
            }
        }
    }
    Ok(false)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn server_has_terminal(pid: u32, device: &std::path::Path) -> Result<bool, String> {
    let device = device
        .to_str()
        .ok_or("Screen client terminal path is not UTF-8")?;
    let output = super::bounded_output("lsof", &["-nP", "-p", &pid.to_string(), "-Fpn"])?
        .ok_or("lsof is required to confirm this screen client's terminal attachment")?;
    if !output.stderr.is_empty() || (!output.status.success() && output.status.code() != Some(1)) {
        return Err(format!(
            "Could not confirm screen client terminal: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(terminal_is_open_by(
        &String::from_utf8_lossy(&output.stdout),
        pid,
        device,
    ))
}

#[cfg(any(test, all(unix, not(target_os = "linux"))))]
fn terminal_is_open_by(output: &str, pid: u32, device: &str) -> bool {
    let mut owner = None;
    output.lines().any(|line| {
        if let Some(value) = line.strip_prefix('p') {
            owner = value.parse::<u32>().ok();
        }
        owner == Some(pid) && line.strip_prefix('n') == Some(device)
    })
}

#[cfg(unix)]
pub(super) fn add_identity(sessions: &mut Vec<super::MultiplexerSession>) -> Result<(), String> {
    if sessions.is_empty() {
        return Ok(());
    }
    if sessions.len() > 4096 {
        return Err("screen inventory exceeds 4096 sessions".into());
    }
    let pids = sessions
        .iter()
        .filter_map(|session| session.match_key.split_once('.'))
        .filter(|(pid, _)| pid.parse::<u32>().is_ok())
        .map(|(pid, _)| pid)
        .collect::<Vec<_>>()
        .join(",");
    if pids.is_empty() {
        sessions.clear();
        return Ok(());
    }
    let output = super::bounded_output("ps", &["-p", &pids, "-o", "pid=,lstart="])?
        .ok_or("ps is required to validate GNU screen process identities")?;
    // ps returns 1 if all selected processes exited during discovery.
    if !output.stderr.is_empty() || (!output.status.success() && output.status.code() != Some(1)) {
        return Err(format!(
            "could not inspect screen processes: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let starts = parse_starts(&String::from_utf8_lossy(&output.stdout))?;
    sessions.retain_mut(|session| {
        let pid = session
            .match_key
            .split_once('.')
            .and_then(|(pid, _)| pid.parse::<u32>().ok());
        let Some(start) = pid.and_then(|pid| starts.get(&pid)).copied() else {
            return false;
        };
        session.match_key = format!("{}|{start}", session.match_key);
        session.started_at_unix_seconds = Some(start);
        true
    });
    Ok(())
}

fn parse_starts(output: &str) -> Result<BTreeMap<u32, u64>, String> {
    let mut starts = BTreeMap::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let (pid, start) = parse_start(line)
            .ok_or("unrecognized ps process start time; cannot safely identify screen sessions")?;
        starts.insert(pid, start);
    }
    Ok(starts)
}

fn parse_start(line: &str) -> Option<(u32, u64)> {
    // bounded_output fixes LC_ALL=C and TZ=UTC0 on both BSD and Linux.
    let fields: Vec<_> = line.split_whitespace().collect();
    if fields.len() != 6 {
        return None;
    }
    let pid = fields[0].parse::<u32>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| *month == fields[2])?;
    let day = fields[3].parse::<u64>().ok()?;
    let year = fields[5].parse::<u64>().ok()?;
    if !(1970..=9999).contains(&year) {
        return None;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > months[month] {
        return None;
    }
    let time: Vec<u64> = fields[4]
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if time.len() != 3 || time[0] > 23 || time[1] > 59 || time[2] > 59 {
        return None;
    }
    let days = 365 * (year - 1970) + (year - 1) / 4 - 1969 / 4 - ((year - 1) / 100 - 1969 / 100)
        + (year - 1) / 400
        - 1969 / 400
        + months[..month].iter().sum::<u64>()
        + day
        - 1;
    Some((pid, days * 86400 + time[0] * 3600 + time[1] * 60 + time[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_confirmation_requires_the_selected_server_and_exact_client_terminal() {
        let output = "p42\nn/dev/ttys001\nn/dev/ttys002\np43\nn/dev/ttys003\n";
        assert!(terminal_is_open_by(output, 42, "/dev/ttys002"));
        assert!(!terminal_is_open_by(output, 42, "/dev/ttys003"));
        assert!(!terminal_is_open_by(output, 42, "/dev/ttys00"));
        assert!(!terminal_is_open_by("", 42, "/dev/ttys002"));
    }

    #[test]
    fn screen_process_start_is_stable_utc_generation_metadata() {
        assert_eq!(parse_start("42 Thu Jan 1 00:00:00 1970"), Some((42, 0)));
        assert_eq!(
            parse_start("42 Sat Jan 1 00:00:00 2000"),
            Some((42, 946684800))
        );
        assert_eq!(
            parse_start("42 Tue Feb 29 12:34:56 2000"),
            Some((42, 951827696))
        );
        assert!(parse_start("42 Mon Feb 29 00:00:00 2100").is_none());
        assert!(parse_starts("unrecognized date").is_err());
        assert!(parse_starts("").unwrap().is_empty());
    }
}
