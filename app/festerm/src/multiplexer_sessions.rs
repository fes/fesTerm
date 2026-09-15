//! Enumerates locally running `tmux` and GNU `screen` sessions for the
//! Launcher's quick-connect widgets.
//!
//! Both multiplexers natively support multiple simultaneous clients
//! attaching to the same session (`tmux attach-session`, `screen -x`),
//! unlike fesTerm's own session daemon, which has single-client "steal"
//! semantics. That asymmetry is why an already-attached tmux/screen session
//! is still offered here (annotated, not hidden) while an already-attached
//! fesTerm-sessiond session is omitted elsewhere.
//!
//! Enumeration is split into a thin process-invoking wrapper
//! (`list_tmux_sessions`/`list_screen_sessions`) and a pure, unit-testable
//! parser (`parse_tmux_sessions`/`parse_screen_sessions`), mirroring the
//! `search_path_executables`/`search_path_executables_in` split already
//! used in `festerm-pty` for PATH discovery.

use std::time::Duration;

#[cfg(any(unix, test))]
#[path = "screen_process.rs"]
mod screen_process;

use festerm_ssh::PersistentSessionName;

#[cfg(all(test, unix))]
#[path = "multiplexer_churn.rs"]
mod churn;

/// A single enumerated tmux or GNU screen session, ready to offer as a
/// Launcher quick-connect entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiplexerSession {
    /// The user-facing label (e.g. `main`, without screen's `pid.` prefix).
    pub name: String,
    /// Provider-scoped identity: tmux server PID plus immutable $session_id;
    /// screen full pid.name plus process start time.
    /// The command builder extracts the exact attach-only provider target.
    pub match_key: String,
    /// Whether another client is already attached to this session.
    pub attached: bool,
    /// Session start time as seconds since the Unix epoch, when known.
    pub started_at_unix_seconds: Option<u64>,
}

impl MultiplexerSession {
    /// Returns the session start time as seconds since the Unix epoch, when known.
    pub fn started_at_unix_seconds(&self) -> Option<u64> {
        self.started_at_unix_seconds
    }
}

/// Enumerates locally running tmux sessions by shelling out to
/// `tmux list-sessions`. Returns an empty list if tmux isn't installed, no
/// server is running, or no sessions exist -- all of which are ordinary,
/// silent conditions for a Launcher quick-connect widget, not errors. Other
/// failures, output limits and timeouts are explicit.
pub fn list_tmux_sessions() -> Result<Vec<MultiplexerSession>, String> {
    let output = bounded_output(
        "tmux",
        &[
            "list-sessions",
            "-F",
            "#{session_name}|#{session_attached}|#{session_created}|#{session_id}|#{pid}",
        ],
    )?;
    let Some(output) = output else {
        return Ok(Vec::new());
    };
    if output.status.success() {
        Ok(parse_tmux_sessions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    } else {
        let error = String::from_utf8_lossy(&output.stderr);
        if error.contains("no server running") || error.contains("No such file or directory") {
            Ok(Vec::new())
        } else {
            Err(format!("list-sessions failed: {}", error.trim()))
        }
    }
}

/// Enumerates locally running GNU screen sessions by shelling out to
/// `screen -list`. Returns an empty list if screen isn't installed or no
/// sessions exist. Old GNU screen also exits nonzero for successful listings;
/// recognized output, not exit status alone, distinguishes those from errors.
pub fn list_screen_sessions() -> Result<Vec<MultiplexerSession>, String> {
    let Some(output) = bounded_output("screen", &["-list"])? else {
        return Ok(Vec::new());
    };
    let text = String::from_utf8_lossy(&output.stdout);
    // Older GNU screen (including macOS's 4.00.03) returns a nonzero
    // status even for a successful listing containing live sessions.
    if (output.status.success()
        || text.contains("No Sockets found")
        || extract_screen_socket_directory(&text).is_some())
        && output.stderr.is_empty()
    {
        #[allow(unused_mut)]
        let mut sessions = parse_screen_sessions(&text);
        #[cfg(unix)]
        screen_process::add_identity(&mut sessions)?;
        Ok(sessions)
    } else {
        Err(format!(
            "screen -list failed: {} {}",
            text.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// Async pipes avoid per-command reader threads that can outlive a timeout
/// when a misbehaving child leaves an inherited pipe open.
fn bounded_output(program: &str, args: &[&str]) -> Result<Option<std::process::Output>, String> {
    bounded_output_with_timeout(program, args, COMMAND_TIMEOUT)
}

fn bounded_output_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Option<std::process::Output>, String> {
    let profile = provider_environment(festerm_pty::LocalProfile::new(program));
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let festerm_pty::EnvironmentPolicy::InheritWith(overrides) = profile.environment() {
        command.envs(overrides);
    }
    command.env("LC_ALL", "C").env("TZ", "UTC0");
    crate::local_command::output(command, timeout)
}

fn provider_environment(profile: festerm_pty::LocalProfile) -> festerm_pty::LocalProfile {
    // Preserve an explicitly inherited provider namespace (including test
    // wrappers); Finder's minimal PATH falls back to the established login
    // environment correction. Discovery and PTY attach use the same policy.
    #[cfg(target_os = "macos")]
    {
        let mut profile = profile;
        if profile
            .executable()
            .to_str()
            .is_some_and(festerm_pty::is_executable_on_path)
        {
            if let Some(path) = std::env::var_os("PATH") {
                profile = profile.with_environment(festerm_pty::EnvironmentPolicy::InheritWith(
                    std::collections::BTreeMap::from([("PATH".into(), path)]),
                ));
            }
        }
        crate::environment::with_corrected_local_path(profile)
    }
    #[cfg(not(target_os = "macos"))]
    profile
}

/// Resume never uses the saved-profile attach-or-create command. tmux's
/// immutable $id targets one session within the selected server generation;
/// screen uses the full pid.name identifier and never -R/-RR.
pub fn attach_profile(
    provider: festerm_config::PersistenceProviderKind,
    selected: &MultiplexerSession,
) -> Result<festerm_pty::LocalProfile, String> {
    use festerm_config::PersistenceProviderKind;
    let sessions = match provider {
        PersistenceProviderKind::Tmux => list_tmux_sessions()?,
        PersistenceProviderKind::Screen => list_screen_sessions()?,
        _ => return Err("Not a local multiplexer provider".into()),
    };
    if !sessions.iter().any(|current| {
        current.match_key == selected.match_key
            && current.name == selected.name
            && current.started_at_unix_seconds == selected.started_at_unix_seconds
    }) {
        return Err(
            "The selected session exited or was replaced. Refresh Running Sessions and try again."
                .into(),
        );
    }

    let profile = match provider {
        PersistenceProviderKind::Tmux => {
            let (pid, id) = selected
                .match_key
                .split_once(':')
                .ok_or("Invalid tmux session identity; Refresh to retry")?;
            if pid.parse::<u32>().is_err()
                || !id.starts_with('$')
                || id[1..].parse::<u64>().is_err()
            {
                return Err("Invalid tmux session identity; Refresh to retry".into());
            }
            // Test the server generation in the same server command queue as
            // the immutable session-id attach. Even a restarted server with a
            // reused $0 cannot turn an old click into a different shell.
            festerm_pty::LocalProfile::new("tmux").with_arguments([
                "if-shell".to_owned(),
                "-F".to_owned(),
                format!("#{{==:#{{pid}},{pid}}}"),
                format!("attach-session -t {id}"),
                "display-message -p 'Selected tmux server exited; refresh Running Sessions'"
                    .to_owned(),
            ])
        }
        PersistenceProviderKind::Screen => festerm_pty::LocalProfile::new("screen")
            .with_arguments(["-x", screen_target(&selected.match_key)]),
        _ => unreachable!(),
    };
    Ok(provider_environment(profile))
}

pub fn screen_target(identity: &str) -> &str {
    identity
        .split_once('|')
        .map_or(identity, |(target, _)| target)
}

pub fn client_attached(
    provider: festerm_config::PersistenceProviderKind,
    selected: &MultiplexerSession,
    pid: u32,
) -> Result<bool, String> {
    use festerm_config::PersistenceProviderKind;
    match provider {
        PersistenceProviderKind::Tmux => {
            let Some(output) = bounded_output(
                "tmux",
                &["list-clients", "-F", "#{client_pid}|#{session_id}|#{pid}"],
            )?
            else {
                return Err("tmux is no longer available".into());
            };
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).into_owned());
            }
            Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
                let parts: Vec<_> = line.split('|').collect();
                parts.len() == 3
                    && parts[0] == pid.to_string()
                    && format!("{}:{}", parts[2], parts[1]) == selected.match_key
            }))
        }
        PersistenceProviderKind::Screen => Ok(list_screen_sessions()?
            .iter()
            .any(|session| session.match_key == selected.match_key && session.attached)),
        _ => Err("Not a multiplexer provider".into()),
    }
}
/// Parses
/// `tmux list-sessions -F "#{session_name}|#{session_attached}|#{session_created}|#{session_id}|#{pid}"`
/// output. `#{session_attached}` is the number of attached clients as a
/// decimal string (`0` when detached), and `#{session_created}` is seconds
/// since the Unix epoch.
/// Literal tabs are accepted for old fixtures, but not requested: tmux 3.7
/// sanitizes control characters in arguments to underscores.
///
/// Sessions whose name fails [`PersistentSessionName`] validation are
/// silently omitted: fesTerm reuses that same validated path for local
/// tmux/screen resume (rather than a separate, local-only relaxation) for
/// consistency with the shared persistence-configuration security
/// philosophy, and real-world tmux session names virtually always fit its
/// charset (ASCII alphanumerics, `-`, `_`, `.`).
fn parse_tmux_sessions(output: &str) -> Vec<MultiplexerSession> {
    let mut sessions = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(['\t', '|']);
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(attached) = fields.next() else {
            continue;
        };
        if PersistentSessionName::new(name).is_err() {
            continue;
        }
        let attached = attached.trim().parse::<u32>().unwrap_or(0) > 0;
        let started_at_unix_seconds = fields
            .next()
            .and_then(|value| value.trim().parse::<u64>().ok());
        let id = fields.next();
        let pid = fields.next();
        let match_key = match (id, pid) {
            (Some(id), Some(pid))
                if id.starts_with('$')
                    && id[1..].parse::<u64>().is_ok()
                    && pid.parse::<u32>().is_ok() =>
            {
                format!("{pid}:{id}")
            }
            // Legacy parser fixtures have no stable identity; discovery always
            // requests it, and attach_profile refuses these fallback keys.
            _ => name.to_owned(),
        };
        sessions.push(MultiplexerSession {
            name: name.to_owned(),
            match_key,
            attached,
            started_at_unix_seconds,
        });
    }
    canonicalize(&mut sessions);
    sessions
}

fn canonicalize(sessions: &mut Vec<MultiplexerSession>) {
    sessions.sort_by(|a, b| a.name.cmp(&b.name).then(a.match_key.cmp(&b.match_key)));
    sessions.dedup_by(|a, b| a.match_key == b.match_key);
}

/// Parses `screen -list` output, e.g.:
///
/// ```text
/// There are screens on:
///     12345.main      (Detached)
///     12346.pts-0.host        (Attached)
/// 2 Sockets in /run/screen/S-user.
/// ```
///
/// or, when no sessions exist, a single line such as
/// `No Sockets found in /run/screen/S-user.`
///
/// Only indented `pid.name  (Status)` lines are session entries; the header
/// and trailing summary lines are ignored. The full `pid.name` identifier
/// becomes `match_key` (see [`MultiplexerSession::match_key`]); the display
/// `name` strips the numeric `pid.` prefix when present, falling back to
/// the full identifier if it isn't in that shape.
fn parse_screen_sessions(output: &str) -> Vec<MultiplexerSession> {
    parse_screen_sessions_with_started_at(output, |_| None)
}

fn parse_screen_sessions_with_started_at(
    output: &str,
    started_at_for_identifier: impl Fn(&str) -> Option<u64>,
) -> Vec<MultiplexerSession> {
    let mut sessions = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        let Some((identifier, status)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        // Real `screen -list` output column-aligns the `pid.name` field
        // with padding spaces before the status when session names have
        // varying lengths, so `identifier` may retain trailing whitespace
        // here; trim it before validating/using it, or a real session
        // would otherwise fail `PersistentSessionName` validation (which
        // rejects whitespace) and be silently dropped.
        // GNU screen 4.9+ can insert a creation-date column before status.
        let identifier = identifier
            .split_once("\t(")
            .map_or(identifier, |(identifier, _)| identifier)
            .trim_end();
        let attached = match status {
            "(Attached)" => true,
            "(Detached)" => false,
            _ => continue,
        };
        let name = identifier
            .split_once('.')
            .filter(|(pid, _)| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
            .map_or(identifier, |(_, rest)| rest);
        if PersistentSessionName::new(name).is_err() {
            continue;
        }
        sessions.push(MultiplexerSession {
            name: name.to_owned(),
            match_key: identifier.to_owned(),
            attached,
            started_at_unix_seconds: started_at_for_identifier(identifier),
        });
    }
    canonicalize(&mut sessions);
    sessions
}

fn extract_screen_socket_directory(output: &str) -> Option<&str> {
    for line in output.lines() {
        let line = line.trim();
        let directory = line
            .split_once(" Sockets in ")
            .or_else(|| line.split_once(" Socket in "))
            .and_then(|(count, directory)| count.trim().parse::<u32>().ok().map(|_| directory));
        if let Some(directory) = directory {
            return Some(
                directory
                    .trim()
                    .strip_suffix('.')
                    .unwrap_or(directory.trim()),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn provider_attachment_keeps_inherited_executable_search_path() {
        assert!(festerm_pty::is_executable_on_path("sh"));
        let profile = provider_environment(festerm_pty::LocalProfile::new("sh"));
        let festerm_pty::EnvironmentPolicy::InheritWith(overrides) = profile.environment() else {
            panic!("provider executable resolution must remain explicit");
        };
        let inherited = std::env::var_os("PATH").expect("the executable search path is present");
        assert_eq!(
            overrides.get(std::ffi::OsStr::new("PATH")),
            Some(&inherited)
        );
        let discovered = bounded_output("sh", &["-c", "printf '%s' \"$PATH\""])
            .unwrap()
            .expect("the executable is available for discovery");
        assert!(discovered.status.success());
        assert_eq!(discovered.stdout, inherited.as_encoded_bytes());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn absolute_provider_executable_uses_login_environment_correction() {
        let profile = festerm_pty::LocalProfile::new("/bin/sh");
        assert_eq!(
            provider_environment(profile.clone()),
            crate::environment::with_corrected_local_path(profile)
        );
    }

    #[test]
    fn stable_tmux_ids_are_sorted_deduplicated_and_do_not_follow_recreated_names() {
        let sessions = parse_tmux_sessions("z|1|12|$9|42\na|0|10|$2|42\na|0|10|$2|42\n");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].match_key, "42:$2");
        assert_eq!(sessions[1].name, "z");
        assert!(sessions[1].attached);
        let recreated = parse_tmux_sessions("a|0|11|$3|42\n");
        assert_ne!(sessions[0].match_key, recreated[0].match_key);
        let restarted = parse_tmux_sessions("a|0|11|$2|43\n");
        assert_ne!(sessions[0].match_key, restarted[0].match_key);
    }

    #[test]
    fn modern_screen_dates_do_not_change_full_pid_identity() {
        let sessions = parse_screen_sessions(
            "\t123.main\t(09/15/2026 10:00:00 AM)\t(Attached)\n\t124.main\t(Detached)\n",
        );
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].match_key, "123.main");
        assert!(sessions[0].attached);
        assert_eq!(sessions[1].match_key, "124.main");
    }

    #[test]
    fn unavailable_binary_is_not_a_provider_error() {
        assert!(bounded_output("festerm-nonexistent-multiplexer-155", &[])
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn provider_process_failures_timeouts_and_output_are_bounded() {
        let output = bounded_output("/bin/sh", &["-c", "printf 'denied' >&2; exit 7"])
            .unwrap()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stderr, b"denied");
        let start = std::time::Instant::now();
        let error = bounded_output_with_timeout(
            "/bin/sh",
            &["-c", "while :; do :; done"],
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
        let error = bounded_output("/bin/sh", &["-c", "s=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=0; while [ \"$i\" -lt 16 ]; do s=$s$s; i=$((i+1)); done; printf '%s' \"$s\""]).unwrap_err();
        assert!(error.contains("exceeds 1 MiB"), "{error}");
    }

    #[test]
    fn parses_tmux_sessions_with_attached_flag_and_filters_invalid_names() {
        let output = "main\t0\t1700000000\nbuild\t2\t1700000001\ninvalid name\t0\t1700000002\n";
        assert_eq!(
            parse_tmux_sessions(output),
            vec![
                MultiplexerSession {
                    name: "build".to_owned(),
                    match_key: "build".to_owned(),
                    attached: true,
                    started_at_unix_seconds: Some(1_700_000_001),
                },
                MultiplexerSession {
                    name: "main".to_owned(),
                    match_key: "main".to_owned(),
                    attached: false,
                    started_at_unix_seconds: Some(1_700_000_000),
                },
            ]
        );
    }

    #[test]
    fn parses_tmux_sessions_treats_missing_or_garbage_timestamps_as_unknown() {
        let output = "main\t0\nbuild\t2\tgarbage\n";
        assert_eq!(
            parse_tmux_sessions(output),
            vec![
                MultiplexerSession {
                    name: "build".to_owned(),
                    match_key: "build".to_owned(),
                    attached: true,
                    started_at_unix_seconds: None,
                },
                MultiplexerSession {
                    name: "main".to_owned(),
                    match_key: "main".to_owned(),
                    attached: false,
                    started_at_unix_seconds: None,
                },
            ]
        );
    }

    #[test]
    fn parses_tmux_sessions_ignores_blank_lines_and_missing_binary_output() {
        assert!(parse_tmux_sessions("").is_empty());
        assert!(parse_tmux_sessions("\n\n").is_empty());
    }

    #[test]
    fn parses_screen_sessions_extracts_display_name_and_full_match_key() {
        let output = "There are screens on:\n\
             \t12345.main\t(Detached)\n\
             \t12346.pts-0.host\t(Attached)\n\
             2 Sockets in /run/screen/S-user.\n";
        assert_eq!(
            parse_screen_sessions(output),
            vec![
                MultiplexerSession {
                    name: "main".to_owned(),
                    match_key: "12345.main".to_owned(),
                    attached: false,
                    started_at_unix_seconds: None,
                },
                MultiplexerSession {
                    name: "pts-0.host".to_owned(),
                    match_key: "12346.pts-0.host".to_owned(),
                    attached: true,
                    started_at_unix_seconds: None,
                },
            ]
        );
    }

    #[test]
    fn parses_screen_sessions_trims_column_alignment_padding_before_the_status() {
        // Real `screen -list` right-pads the pid.name column with extra
        // whitespace before the status when session names have differing
        // lengths, e.g. a short name alongside a longer one.
        let output = "There are screens on:\n\
             \t12345.a\t\t(Detached)\n\
             \t12346.a-much-longer-name\t(Detached)\n\
             2 Sockets in /run/screen/S-user.\n";
        assert_eq!(
            parse_screen_sessions(output),
            vec![
                MultiplexerSession {
                    name: "a".to_owned(),
                    match_key: "12345.a".to_owned(),
                    attached: false,
                    started_at_unix_seconds: None,
                },
                MultiplexerSession {
                    name: "a-much-longer-name".to_owned(),
                    match_key: "12346.a-much-longer-name".to_owned(),
                    attached: false,
                    started_at_unix_seconds: None,
                },
            ]
        );
    }

    #[test]
    fn parses_screen_sessions_reports_none_when_no_sockets_exist() {
        let output = "No Sockets found in /run/screen/S-user.\n";
        assert!(parse_screen_sessions(output).is_empty());
    }

    #[test]
    fn parses_screen_sessions_falls_back_to_the_full_identifier_without_a_numeric_prefix() {
        let output = "\tunusual-name\t(Detached)\n";
        assert_eq!(
            parse_screen_sessions(output),
            vec![MultiplexerSession {
                name: "unusual-name".to_owned(),
                match_key: "unusual-name".to_owned(),
                attached: false,
                started_at_unix_seconds: None,
            }]
        );
    }

    #[test]
    fn extracts_screen_socket_directory_from_plural_summary_line() {
        let output = "There are screens on:\n\
             \t12345.main\t(Detached)\n\
             2 Sockets in /var/folders/xx/fesTerm/T/.screen.\n";
        assert_eq!(
            extract_screen_socket_directory(output),
            Some("/var/folders/xx/fesTerm/T/.screen")
        );
    }

    #[test]
    fn extracts_screen_socket_directory_from_singular_summary_line() {
        let output = "There is a screen on:\n\
             \t12345.main\t(Detached)\n\
             1 Socket in /run/screen/S-user.\n";
        assert_eq!(
            extract_screen_socket_directory(output),
            Some("/run/screen/S-user")
        );
    }
}
