//! Enumerates locally running `tmux` and GNU `screen` sessions for the
//! Launcher's quick-connect widgets.
//!
//! Both multiplexers natively support multiple simultaneous clients
//! attaching to the same session (`tmux new-session -A`, `screen -xRR`),
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

use festerm_ssh::PersistentSessionName;

/// A single enumerated tmux or GNU screen session, ready to offer as a
/// Launcher quick-connect entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiplexerSession {
    /// The user-facing label (e.g. `main`, without screen's `pid.` prefix).
    pub name: String,
    /// The exact string passed back to
    /// [`festerm_config::PersistenceConfiguration::new`] to reattach this
    /// specific session. Identical to `name` for tmux, since tmux session
    /// names are already unique and exact. For screen, this is the full
    /// `pid.name` identifier, since screen's `-x`/`-r`/`-R` matching is
    /// substring-based and only the full identifier guarantees an
    /// unambiguous match to *this* session.
    pub match_key: String,
    /// Whether another client is already attached to this session.
    pub attached: bool,
}

/// Enumerates locally running tmux sessions by shelling out to
/// `tmux list-sessions`. Returns an empty list if tmux isn't installed, no
/// server is running, or no sessions exist -- all of which are ordinary,
/// silent conditions for a Launcher quick-connect widget, not errors.
pub fn list_tmux_sessions() -> Vec<MultiplexerSession> {
    let output = std::process::Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_attached}",
        ])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            parse_tmux_sessions(&String::from_utf8_lossy(&output.stdout))
        }
        _ => Vec::new(),
    }
}

/// Enumerates locally running GNU screen sessions by shelling out to
/// `screen -list`. Returns an empty list if screen isn't installed or no
/// sessions exist; `screen -list` exits non-zero when there are none, so
/// that case is treated the same as a missing binary.
pub fn list_screen_sessions() -> Vec<MultiplexerSession> {
    let output = std::process::Command::new("screen").arg("-list").output();
    match output {
        Ok(output) => parse_screen_sessions(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Parses `tmux list-sessions -F "#{session_name}\t#{session_attached}"`
/// output. `#{session_attached}` is the number of attached clients as a
/// decimal string (`0` when detached).
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
        let Some((name, attached)) = line.rsplit_once('\t') else {
            continue;
        };
        if PersistentSessionName::new(name).is_err() {
            continue;
        }
        let attached = attached.trim().parse::<u32>().unwrap_or(0) > 0;
        sessions.push(MultiplexerSession {
            name: name.to_owned(),
            match_key: name.to_owned(),
            attached,
        });
    }
    sessions
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
        let identifier = identifier.trim_end();
        let attached = match status {
            "(Attached)" => true,
            "(Detached)" => false,
            _ => continue,
        };
        if PersistentSessionName::new(identifier).is_err() {
            continue;
        }
        let name = identifier
            .split_once('.')
            .filter(|(pid, _)| !pid.is_empty() && pid.chars().all(|c| c.is_ascii_digit()))
            .map_or(identifier, |(_, rest)| rest);
        sessions.push(MultiplexerSession {
            name: name.to_owned(),
            match_key: identifier.to_owned(),
            attached,
        });
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tmux_sessions_with_attached_flag_and_filters_invalid_names() {
        let output = "main\t0\nbuild\t2\ninvalid name\t0\n";
        assert_eq!(
            parse_tmux_sessions(output),
            vec![
                MultiplexerSession {
                    name: "main".to_owned(),
                    match_key: "main".to_owned(),
                    attached: false,
                },
                MultiplexerSession {
                    name: "build".to_owned(),
                    match_key: "build".to_owned(),
                    attached: true,
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
                },
                MultiplexerSession {
                    name: "pts-0.host".to_owned(),
                    match_key: "12346.pts-0.host".to_owned(),
                    attached: true,
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
                },
                MultiplexerSession {
                    name: "a-much-longer-name".to_owned(),
                    match_key: "12346.a-much-longer-name".to_owned(),
                    attached: false,
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
            }]
        );
    }
}
