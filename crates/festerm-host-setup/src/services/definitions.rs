use super::{refused, Context, Platform, ProcessState, Registration};
use crate::lifecycle::{Origin, Snapshot, Startup};
use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path, PathBuf},
};
#[cfg(unix)]
use std::{
    fs::File,
    io::{Read, Write},
};

pub(super) fn path_text(path: &Path) -> io::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| refused("host.service_invalid_path"))?;
    if !path.is_absolute()
        || text.len() > 4096
        || text.chars().any(char::is_control)
        || text
            .split(['/', '\\'])
            .any(|part| matches!(part, "." | ".."))
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(refused("host.service_invalid_path"));
    }
    Ok(text)
}

pub(super) fn resource(snapshot: &Snapshot, epoch: u64) -> String {
    format!(
        "fesTerm-Host-{}-{}-{}",
        match snapshot.origin {
            Origin::Desktop => "desktop",
            Origin::Headless => "headless",
        },
        snapshot.installation,
        epoch
    )
}

pub(super) fn replacement_resource(
    snapshot: &Snapshot,
    epoch: u64,
    reservation: &str,
    platform: Platform,
) -> io::Result<String> {
    if platform != Platform::Windows {
        return Ok(format!("{}-{reservation}", resource(snapshot, epoch)));
    }
    let installation = uuid::Uuid::parse_str(&snapshot.installation)
        .map_err(|_| refused("host.service_foreign_receipt"))?;
    let operation =
        uuid::Uuid::parse_str(reservation).map_err(|_| refused("host.service_foreign_receipt"))?;
    Ok(format!(
        "fesTerm-Host-{}-{}-{}",
        match snapshot.origin {
            Origin::Desktop => "desktop",
            Origin::Headless => "headless",
        },
        installation.simple(),
        operation.simple()
    ))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

pub(super) fn runner_verb(origin: Origin) -> &'static str {
    match origin {
        Origin::Desktop => "run-desktop",
        Origin::Headless => "run",
    }
}

pub(super) fn record_runner_verb(record: &Registration) -> &'static str {
    if matches!(record.version, 4..=6) {
        match record.origin {
            Origin::Desktop => "run-desktop-managed",
            Origin::Headless => "run-managed",
        }
    } else {
        runner_verb(record.origin)
    }
}

pub(super) fn render(record: &Registration) -> io::Result<String> {
    if record.settings.startup == Startup::BeforeLogin {
        return Err(refused("host.startup_before_login_unqualified"));
    }
    let executable = path_text(
        record
            .windows_managed
            .as_ref()
            .map_or(record.executable.as_path(), |worker| worker.path.as_path()),
    )?;
    let state = path_text(&record.state)?;
    let verb = record_runner_verb(record);
    let recovery = matches!(record.version, 4..=6);
    match record.platform {
        Platform::Linux => {
            let (limits, restart) = if recovery {
                let login = if record.settings.startup == Startup::UserSignIn {
                    format!("\nRestartForceExitStatus={}", crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE)
                } else {
                    String::new()
                };
                (if record.settings.startup == Startup::UserSignIn {
                    "StartLimitIntervalSec=0\n"
                } else {
                    "StartLimitIntervalSec=300\nStartLimitBurst=3\n"
                }, format!(
                    "Restart=on-failure\nRestartSec=60s{login}"
                ))
            } else if record.settings.startup == Startup::UserSignIn {
                ("StartLimitIntervalSec=0\n", format!(
                    "Restart=no\nRestartForceExitStatus={}\nRestartSec=2s",
                    crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
                ))
            } else {
                ("", "Restart=no".into())
            };
            Ok(format!(
                "[Unit]\nDescription=fesTerm host {}\n{}[Service]\nType=exec\nExecStart={} {} --state-dir {}\n{}\nTimeoutStartSec=10s\nTimeoutStopSec=10s\nSendSIGKILL=no\nKillMode=process\nStandardOutput=null\nStandardError=null\n[Install]\nWantedBy=default.target\n",
                record.resource, limits, systemd(executable), verb, systemd(state), restart))
        },
        Platform::Macos => Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>Label</key><string>{}</string><key>ProgramArguments</key><array>\
             <string>{}</string><string>{}</string><string>--state-dir</string><string>{}</string></array>\
             <key>RunAtLoad</key><{}/>{}<key>AbandonProcessGroup</key><true/>\
             <key>ExitTimeOut</key><integer>10</integer>\
             <key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string>\
             </dict></plist>\n", xml(&record.resource), xml(executable), verb, xml(state),
            if record.settings.startup == Startup::UserSignIn { "true" } else { "false" },
            if recovery {
                "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>60</integer>"
            } else {
                "<key>KeepAlive</key><false/>"
            })),
        Platform::Windows => {
            if !record.user.starts_with("S-1-") || !record.user.bytes().all(|c| c.is_ascii_digit() || c == b'-' || c == b'S')
                || executable.contains(['"', '%']) || state.contains(['"', '%'])
            { return Err(refused("host.service_invalid_task_path_or_user")); }
            let trailing = state.chars().rev().take_while(|c| *c == '\\').count();
            let arguments = format!("{verb} --state-dir \"{}{}\"", state, "\\".repeat(trailing));
            let trigger = if record.settings.startup == Startup::UserSignIn {
                format!("<LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger>", xml(&record.user))
            } else { String::new() };
            Ok(format!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\
                 <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\
                 <RegistrationInfo><URI>\\{}</URI><Source>fesTerm-host-reservation:{}</Source></RegistrationInfo><Triggers>{}</Triggers>\
                 <Principals><Principal id=\"User\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType>\
                 <RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\
                 <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\
                 <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\
                 <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>false</AllowHardTerminate>\
                 <StartWhenAvailable>false</StartWhenAvailable><Enabled>true</Enabled>\
                 <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>{}</Settings>\
                 <Actions Context=\"User\"><Exec><Command>{}</Command><Arguments>{}</Arguments></Exec></Actions></Task>",
                xml(&record.resource), xml(&record.reservation), trigger, xml(&record.user),
                if recovery {
                    "<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>"
                } else { "" },
                xml(executable), xml(&arguments)))
        }
    }
}

pub(super) fn definition_path(context: &Context, record: &Registration) -> PathBuf {
    let directory = if record.platform == Platform::Macos
        && matches!(record.version, 4..=6)
        && record.settings.startup == Startup::Manual
    {
        record.state.join("native-manual-services")
    } else {
        context.registration.clone()
    };
    directory.join(format!(
        "{}.{}",
        record.resource,
        if context.platform == Platform::Macos {
            "plist"
        } else {
            "service"
        }
    ))
}

#[cfg(unix)]
fn directory(path: &Path, create: bool) -> io::Result<File> {
    path_text(path)?;
    let mut directory = File::open("/")?;
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(refused("host.service_invalid_path"));
        };
        festerm_unix_security::trusted_private_storage_ancestor(&directory)?;
        directory = match festerm_unix_security::open_directory_read_nofollow(
            &directory,
            Path::new(name),
        ) {
            Ok(directory) => directory,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                festerm_unix_security::create_private_directory(&directory, Path::new(name))?
            }
            Err(error) => return Err(error),
        };
    }
    festerm_unix_security::trusted_private_storage_ancestor(&directory)?;
    Ok(directory)
}

pub(super) fn check_file(
    context: &Context,
    #[cfg(unix)] record: &Registration,
    #[cfg(not(unix))] _: &Registration,
) -> io::Result<bool> {
    if context.platform == Platform::Windows {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        let path = definition_path(context, record);
        let directory = match directory(path.parent().unwrap(), false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let file = match festerm_unix_security::open_private_file(
            &directory,
            Path::new(path.file_name().unwrap()),
            false,
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if !festerm_unix_security::is_current_user_only(&file, false)? {
            return Err(refused("host.service_definition_permissions"));
        }
        let mut bytes = Vec::new();
        file.take(super::OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes != record.definition.as_bytes() {
            return Err(refused("host.service_foreign_definition"));
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    Err(refused("host.service_platform_mismatch"))
}

pub(super) fn publish_file(context: &Context, record: &Registration) -> io::Result<()> {
    if check_file(context, record)? {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let path = definition_path(context, record);
        let directory = directory(path.parent().unwrap(), true)?;
        let staging = PathBuf::from(format!(".host-service-{}", uuid::Uuid::new_v4()));
        let mut file = festerm_unix_security::open_private_file(&directory, &staging, true)?;
        let name = Path::new(path.file_name().unwrap());
        let result = (|| {
            file.write_all(record.definition.as_bytes())?;
            file.sync_all()?;
            festerm_unix_security::rename_noreplace(&directory, &staging, &directory, name)?;
            directory.sync_all()
        })();
        if result.is_err() {
            let _ = festerm_unix_security::remove_private_staging_file(&directory, &staging);
        }
        result
    }
    #[cfg(not(unix))]
    Err(refused("host.service_platform_mismatch"))
}

pub(super) fn remove_file(context: &Context, record: &Registration) -> io::Result<()> {
    if !check_file(context, record)? {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let path = definition_path(context, record);
        let directory = directory(path.parent().unwrap(), false)?;
        festerm_unix_security::remove_private_staging_file(
            &directory,
            Path::new(path.file_name().unwrap()),
        )?;
        directory.sync_all()
    }
    #[cfg(not(unix))]
    Err(refused("host.service_platform_mismatch"))
}

fn properties(bytes: &[u8]) -> io::Result<BTreeMap<&str, &str>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| refused("host.service_invalid_observation"))?;
    let mut properties = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| refused("host.service_invalid_observation"))?;
        if properties.insert(key, value).is_some() {
            return Err(refused("host.service_invalid_observation"));
        }
    }
    Ok(properties)
}

pub(super) fn linux_state(bytes: &[u8], path: &Path) -> io::Result<ProcessState> {
    let properties = properties(bytes)?;
    let value = |key| properties.get(key).copied();
    if value("DropInPaths") != Some("")
        || value("Id") != path.file_name().and_then(|name| name.to_str())
    {
        return Err(refused("host.service_foreign_native_definition"));
    }
    if value("LoadState") == Some("not-found")
        && value("FragmentPath") == Some("")
        && value("ActiveState") == Some("inactive")
        && value("SubState") == Some("dead")
        && value("MainPID") == Some("0")
    {
        return Ok(ProcessState::Absent);
    }
    let retired_failure = value("LoadState") == Some("not-found")
        && value("FragmentPath") == Some("")
        && value("ActiveState") == Some("failed");
    if !retired_failure
        && (value("LoadState") != Some("loaded") || value("FragmentPath") != Some(path_text(path)?))
    {
        return Err(refused("host.service_foreign_native_definition"));
    }
    match (value("ActiveState"), value("SubState"), value("MainPID")) {
        (Some("active"), Some("running"), Some(pid))
            if pid.parse::<u32>().is_ok_and(|pid| pid != 0) =>
        {
            Ok(ProcessState::Running)
        }
        (Some("inactive"), Some("dead"), Some("0"))
            if value("ExecMainCode").is_some_and(|code| code.parse::<u32>().is_ok())
                && value("ExecMainStatus").is_some_and(|status| status.parse::<i32>().is_ok()) =>
        {
            match value("Result") {
                Some("success") => Ok(ProcessState::Stopped),
                Some(
                    "exit-code" | "signal" | "core-dump" | "timeout" | "watchdog"
                    | "start-limit-hit" | "resources" | "protocol" | "oom-kill",
                ) => Ok(ProcessState::Failed),
                _ => Err(refused("host.service_observation_unqualified")),
            }
        }
        (Some("failed"), Some("failed"), Some("0")) => Ok(ProcessState::Failed),
        (Some("activating" | "deactivating"), _, _) => Ok(ProcessState::Transitioning),
        _ => Err(refused("host.service_observation_unqualified")),
    }
}

pub(super) fn linux_missing_unit(bytes: &[u8]) -> io::Result<bool> {
    Ok(properties(bytes)?.get("LoadState") == Some(&"not-found"))
}

/// Keep only the service's own fields. launchctl also prints nested coalition
/// dictionaries containing identically named state/PID/exit fields.
fn mac_service_lines(text: &str) -> io::Result<Vec<&str>> {
    fn indent(line: &str) -> &str {
        &line[..line.len() - line.trim_start().len()]
    }
    let invalid = || refused("host.service_invalid_observation");
    let raw: Vec<_> = text.lines().collect();
    let first = raw
        .iter()
        .position(|line| !line.trim().is_empty())
        .ok_or_else(invalid)?;
    let first_text = raw[first].trim();
    let scalar = |line: &str| {
        [
            "path = ",
            "program = ",
            "state = ",
            "pid = ",
            "last exit code = ",
        ]
        .iter()
        .any(|key| line.starts_with(key))
    };
    // Real output has one enclosing service dictionary. Existing injected
    // manager fixtures use a flat, unindented list with the same exact argv.
    let framed = first_text.ends_with(" {") && first_text != "arguments = {" && !scalar(first_text);
    if framed && !indent(raw[first]).is_empty() {
        return Err(invalid());
    }
    let mut frames: Vec<(Option<&str>, Option<&str>)> = vec![if framed {
        (Some(""), None)
    } else {
        (None, Some(""))
    }];
    let mut result = Vec::new();
    let mut at = first + usize::from(framed);
    while at < raw.len() {
        let line = raw[at].trim();
        let indentation = indent(raw[at]);
        at += 1;
        if line.is_empty() {
            continue;
        }
        let frame = frames.last_mut().ok_or_else(invalid)?;
        if line == "}" {
            if frame.0 != Some(indentation) {
                return Err(invalid());
            }
            frames.pop();
            continue;
        }
        if frame.0.is_some_and(|opening| {
            indentation.len() <= opening.len() || !indentation.starts_with(opening)
        }) {
            return Err(invalid());
        }
        if let Some(expected) = frame.1 {
            if indentation != expected {
                return Err(invalid());
            }
        } else {
            frame.1 = Some(indentation);
        }
        let direct = frames.len() == 1;
        if direct {
            result.push(line);
        }
        if direct && line == "arguments = {" {
            // Consume literal arguments without interpreting braces in a path
            // as dictionaries. Neither extra argv nor nested identity is valid.
            let mut argument_indent = None;
            for _ in 0..4 {
                let argument = raw.get(at).ok_or_else(invalid)?;
                let child = indent(argument);
                if (framed && child.len() <= indentation.len())
                    || !child.starts_with(indentation)
                    || argument_indent.is_some_and(|expected| expected != child)
                {
                    return Err(invalid());
                }
                argument_indent = Some(child);
                result.push(argument.trim());
                at += 1;
            }
            let closing = raw.get(at).ok_or_else(invalid)?;
            if closing.trim() != "}" || indent(closing) != indentation {
                return Err(invalid());
            }
            result.push("}");
            at += 1;
        } else if !scalar(line) && (line.ends_with(" {") || line == "{") {
            frames.push((Some(indentation), None));
        }
    }
    if (framed && !frames.is_empty()) || (!framed && frames.len() != 1) {
        return Err(invalid());
    }
    Ok(result)
}

pub(super) fn mac_state(
    bytes: &[u8],
    path: &Path,
    record: &Registration,
) -> io::Result<ProcessState> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| refused("host.service_invalid_observation"))?;
    let lines = mac_service_lines(text)?;
    let count = |key: &str| lines.iter().filter(|line| line.starts_with(key)).count();
    let unique = |value: &str| lines.iter().filter(|line| **line == value).count() == 1;
    let argv = [
        path_text(&record.executable)?,
        record_runner_verb(record),
        "--state-dir",
        path_text(&record.state)?,
    ];
    if count("path = ") != 1
        || count("program = ") != 1
        || count("arguments = {") != 1
        || !unique(&format!("path = {}", path_text(path)?))
        || !unique(&format!("program = {}", argv[0]))
        || lines
            .windows(6)
            .filter(|lines| lines[0] == "arguments = {" && lines[1..5] == argv && lines[5] == "}")
            .count()
            != 1
    {
        return Err(refused("host.service_foreign_native_definition"));
    }
    if count("state = ") != 1 || count("pid = ") > 1 || count("last exit code = ") > 1 {
        return Err(refused("host.service_observation_unqualified"));
    }
    let state: Vec<_> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("state = "))
        .collect();
    let pids: Vec<_> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("pid = "))
        .collect();
    match (state.as_slice(), pids.as_slice()) {
        (["running"], [pid]) if pid.parse::<u32>().is_ok_and(|pid| pid != 0) => {
            Ok(ProcessState::Running)
        }
        (["waiting" | "not running" | "exited"], []) => {
            let exits: Vec<_> = lines
                .iter()
                .filter_map(|line| line.strip_prefix("last exit code = "))
                .collect();
            match exits.as_slice() {
                [] | ["0" | "(never exited)"] => Ok(ProcessState::Stopped),
                [status] if status.parse::<i32>().is_ok_and(|status| status != 0) => {
                    Ok(ProcessState::Failed)
                }
                _ => Err(refused("host.service_observation_unqualified")),
            }
        }
        (["spawn scheduled"], _) => Ok(ProcessState::Transitioning),
        _ => Err(refused("host.service_observation_unqualified")),
    }
}
