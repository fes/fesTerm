//! Read-only, bounded login1 proof; a user manager is never login authority.
use std::{future::Future, io, time::Duration};
use tokio_util::sync::CancellationToken;

const BUDGET: Duration = Duration::from_secs(2);
const INTERVAL: Duration = Duration::from_secs(1);

fn refused() -> io::Error {
    io::Error::other(crate::run_exit::LoginProofUnavailable)
}

pub(super) trait Query {
    async fn eligible(&self, cancel: &CancellationToken) -> io::Result<bool>;
}

pub(super) struct Logind;

impl Query for Logind {
    async fn eligible(&self, cancel: &CancellationToken) -> io::Result<bool> {
        #[cfg(any(target_os = "linux", all(test, unix)))]
        {
            let uid = festerm_unix_security::current_user_id()?;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(refused()),
                result = tokio::time::timeout(BUDGET, native::snapshot(uid)) =>
                    result.map_err(|_| refused())?,
            }
        }
        #[cfg(not(any(target_os = "linux", all(test, unix))))]
        {
            let _ = cancel;
            Err(refused())
        }
    }
}

pub(super) fn required(platform: &str, startup: crate::lifecycle::Startup) -> bool {
    platform == "linux" && startup == crate::lifecycle::Startup::UserSignIn
}

pub(super) async fn admit(query: &impl Query, cancel: &CancellationToken) -> io::Result<()> {
    match tokio::time::timeout(BUDGET, query.eligible(cancel)).await {
        Ok(Ok(true)) if !cancel.is_cancelled() => Ok(()),
        _ => Err(refused()),
    }
}

async fn watch(query: &impl Query, cancel: &CancellationToken) -> io::Result<()> {
    loop {
        admit(query, cancel).await?;
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(INTERVAL) => {}
        }
    }
}

/// Cancellation reaches the actual relay future, which must finish draining
/// before its caller releases the protected per-run lease.
pub(super) async fn guard(
    query: &impl Query,
    cancel: CancellationToken,
    hosting_cancel: CancellationToken,
    hosting: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    tokio::pin!(hosting);
    let outcome = tokio::select! {
        biased;
        result = &mut hosting => return result,
        _ = cancel.cancelled() => Ok(()),
        result = watch(query, &cancel) => result,
    };
    hosting_cancel.cancel();
    hosting.await?;
    outcome
}

#[cfg(any(target_os = "linux", test))]
mod parse {
    use super::*;
    use std::collections::BTreeSet;

    pub(super) const LIMIT: usize = 64 * 1024;
    pub(super) const MAX_SESSIONS: usize = 64;

    pub(super) fn text(bytes: &[u8]) -> io::Result<&str> {
        if bytes.len() > LIMIT {
            return Err(refused());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
        if text
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\n')
        {
            return Err(refused());
        }
        Ok(text)
    }

    fn quoted(value: &str) -> io::Result<&str> {
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(refused)?;
        if value.len() > 256
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/_:.-".contains(&byte))
        {
            return Err(refused());
        }
        Ok(value)
    }

    fn string(line: &str) -> io::Result<&str> {
        quoted(line.strip_prefix("s ").ok_or_else(refused)?)
    }

    pub(super) fn owner(bytes: &[u8]) -> io::Result<String> {
        let value = string(text(bytes)?.trim_end_matches('\n'))?;
        let parts: Vec<_> = value
            .strip_prefix(':')
            .ok_or_else(refused)?
            .split('.')
            .collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || part.len() > 10
                    || !part.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(refused());
        }
        Ok(value.into())
    }

    pub(super) fn root(bytes: &[u8]) -> io::Result<()> {
        if text(bytes)? != "u 0\n" && text(bytes)? != "u 0" {
            return Err(refused());
        }
        Ok(())
    }

    pub(super) fn sessions(bytes: &[u8]) -> io::Result<Vec<(String, String)>> {
        let text = text(bytes)?;
        let fields: Vec<_> = text.split_ascii_whitespace().collect();
        if fields.len() < 2 || fields[0] != "a(so)" {
            return Err(refused());
        }
        let count = fields[1].parse::<usize>().map_err(|_| refused())?;
        if count > MAX_SESSIONS || fields.len() != 2 + count * 2 {
            return Err(refused());
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut result = Vec::new();
        for pair in fields[2..].as_chunks::<2>().0 {
            let id = quoted(pair[0])?;
            let path = quoted(pair[1])?;
            let suffix = path
                .strip_prefix("/org/freedesktop/login1/session/")
                .ok_or_else(refused)?;
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || suffix.is_empty()
                || !suffix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                || !ids.insert(id)
                || !paths.insert(path)
            {
                return Err(refused());
            }
            result.push((id.into(), path.into()));
        }
        result.sort();
        Ok(result)
    }

    pub(super) fn eligible(bytes: &[u8], uid: u32, id: &str) -> io::Result<bool> {
        let text = text(bytes)?;
        let lines: Vec<_> = text.lines().collect();
        if lines.len() != 5
            || lines[0] != format!("(uo) {uid} \"/org/freedesktop/login1/user/_{uid}\"")
            || string(lines[1])? != id
        {
            return Err(refused());
        }
        let class = string(lines[2])?;
        let kind = string(lines[3])?;
        let state = string(lines[4])?;
        if !matches!(
            class,
            "user"
                | "user-early"
                | "user-light"
                | "greeter"
                | "lock-screen"
                | "background"
                | "background-light"
                | "manager"
                | "manager-early"
        ) || !matches!(
            kind,
            "tty" | "x11" | "wayland" | "unspecified" | "mir" | "web"
        ) || !matches!(state, "active" | "online" | "closing")
        {
            return Err(refused());
        }
        // Active/LockedHint are deliberately not authorization conditions:
        // the approved lifetime includes idle, locked and switched sessions.
        Ok(class == "user"
            && matches!(kind, "tty" | "x11" | "wayland")
            && matches!(state, "active" | "online"))
    }
}

#[cfg(any(target_os = "linux", all(test, unix)))]
mod native {
    use super::{parse, *};
    use std::{
        os::unix::fs::{FileTypeExt, MetadataExt},
        path::Path,
        process::Stdio,
    };
    use tokio::io::AsyncReadExt;

    const BUS: &str = "/run/dbus/system_bus_socket";
    const TOOL: &str = "/usr/bin/busctl";
    const DBUS: [&str; 3] = [
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    ];

    fn trusted_paths() -> io::Result<()> {
        for path in [TOOL, BUS] {
            let resolved = std::fs::canonicalize(path).map_err(|_| refused())?;
            if resolved != Path::new(path) {
                return Err(refused());
            }
            let mut prefix = std::path::PathBuf::new();
            for component in resolved.components() {
                prefix.push(component);
                let metadata = std::fs::symlink_metadata(&prefix).map_err(|_| refused())?;
                let leaf = prefix == resolved;
                if metadata.uid() != 0
                    || metadata.file_type().is_symlink()
                    || (!leaf && (!metadata.is_dir() || metadata.mode() & 0o022 != 0))
                    || (leaf
                        && path == TOOL
                        && (!metadata.is_file()
                            || metadata.mode() & 0o022 != 0
                            || metadata.mode() & 0o111 == 0))
                    || (leaf && path == BUS && !metadata.file_type().is_socket())
                {
                    return Err(refused());
                }
            }
        }
        Ok(())
    }

    async fn call(args: &[&str], budget: &mut usize) -> io::Result<Vec<u8>> {
        let mut child = tokio::process::Command::new(TOOL)
            .args([
                "--address=unix:path=/run/dbus/system_bus_socket",
                "--no-pager",
                "--auto-start=no",
                "--timeout=1s",
            ])
            .args(args)
            .env_clear()
            .env("LC_ALL", "C")
            .env("SYSTEMD_COLORS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| refused())?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(refused)?
            .take(*budget as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| refused())?;
        *budget = budget.checked_sub(bytes.len()).ok_or_else(refused)?;
        if !child.wait().await.map_err(|_| refused())?.success() {
            return Err(refused());
        }
        Ok(bytes)
    }

    pub(super) trait Reader {
        async fn read(&self, args: &[&str], budget: &mut usize) -> io::Result<Vec<u8>>;
    }

    struct SystemReader;

    impl Reader for SystemReader {
        async fn read(&self, args: &[&str], budget: &mut usize) -> io::Result<Vec<u8>> {
            call(args, budget).await
        }
    }

    async fn owner(reader: &impl Reader, budget: &mut usize) -> io::Result<String> {
        parse::owner(
            &reader
                .read(
                    &[
                        "call",
                        DBUS[0],
                        DBUS[1],
                        DBUS[2],
                        "GetNameOwner",
                        "s",
                        "org.freedesktop.login1",
                    ],
                    budget,
                )
                .await?,
        )
    }

    pub(super) async fn snapshot(uid: u32) -> io::Result<bool> {
        trusted_paths()?;
        proof(uid, &SystemReader).await
    }

    pub(super) async fn proof(uid: u32, reader: &impl Reader) -> io::Result<bool> {
        let mut budget = parse::LIMIT;
        let issuer = owner(reader, &mut budget).await?;
        parse::root(
            &reader
                .read(
                    &[
                        "call",
                        DBUS[0],
                        DBUS[1],
                        DBUS[2],
                        "GetConnectionUnixUser",
                        "s",
                        &issuer,
                    ],
                    &mut budget,
                )
                .await?,
        )?;
        let user = format!("/org/freedesktop/login1/user/_{uid}");
        let list = [
            "get-property",
            &issuer,
            &user,
            "org.freedesktop.login1.User",
            "Sessions",
        ];
        let before = parse::sessions(&reader.read(&list, &mut budget).await?)?;
        let mut eligible = false;
        for (id, path) in &before {
            eligible |= parse::eligible(
                &reader
                    .read(
                        &[
                            "get-property",
                            &issuer,
                            path,
                            "org.freedesktop.login1.Session",
                            "User",
                            "Id",
                            "Class",
                            "Type",
                            "State",
                        ],
                        &mut budget,
                    )
                    .await?,
                uid,
                id,
            )?;
        }
        if parse::sessions(&reader.read(&list, &mut budget).await?)? != before
            || owner(reader, &mut budget).await? != issuer
        {
            return Err(refused());
        }
        Ok(eligible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "read-only real logind proof: explicit ordinary-user Linux lab inputs required"]
    async fn linux_login_native_read_only_logind_proof() {
        assert_eq!(
            std::env::var("FESTERM_ADR84_LOGIND_READ_ONLY").as_deref(),
            Ok("1"),
            "explicit FESTERM_ADR84_LOGIND_READ_ONLY=1 is required"
        );
        let expected_uid = std::env::var("FESTERM_ADR84_LOGIND_EXPECT_UID")
            .expect("FESTERM_ADR84_LOGIND_EXPECT_UID is required")
            .parse::<u32>()
            .expect("expected UID must be numeric");
        let actual_uid = festerm_unix_security::current_user_id()
            .expect("real/effective UID must match an ordinary non-root caller");
        assert_eq!(expected_uid, actual_uid, "expected UID must be the caller");
        let expected = match std::env::var("FESTERM_ADR84_LOGIND_EXPECT").as_deref() {
            Ok("eligible") => true,
            Ok("ineligible") => false,
            _ => panic!("FESTERM_ADR84_LOGIND_EXPECT must be eligible or ineligible"),
        };
        let observed = Logind
            .eligible(&CancellationToken::new())
            .await
            .expect("production logind query failed: unknown is never ineligibility evidence");
        assert_eq!(observed, expected, "verified current-login proof differs");
        println!("read-only production logind proof: uid={actual_uid} eligible={observed}");
    }

    fn session(class: &str, kind: &str, state: &str) -> Vec<u8> {
        format!("(uo) 1000 \"/org/freedesktop/login1/user/_1000\"\ns \"c1\"\ns \"{class}\"\ns \"{kind}\"\ns \"{state}\"\n").into_bytes()
    }

    #[test]
    fn linux_login_approved_graphical_tty_and_locked_idle_lifetime() {
        for kind in ["tty", "x11", "wayland"] {
            for state in ["active", "online"] {
                assert!(parse::eligible(&session("user", kind, state), 1000, "c1").unwrap());
            }
        }
        for (class, kind, state) in [
            ("manager", "tty", "active"),
            ("background", "tty", "active"),
            ("user-light", "tty", "active"),
            ("user", "unspecified", "online"),
            ("user", "tty", "closing"),
        ] {
            assert!(!parse::eligible(&session(class, kind, state), 1000, "c1").unwrap());
        }
    }

    #[test]
    fn linux_login_refuses_foreign_uid_identity_unknown_or_hostile_metadata() {
        assert!(parse::eligible(&session("user", "tty", "active"), 1001, "c1").is_err());
        assert!(parse::eligible(&session("user", "tty", "active"), 1000, "c2").is_err());
        for value in [
            session("future", "tty", "active"),
            session("user", "future", "active"),
            session("user", "tty", "opening"),
            vec![b'a'; parse::LIMIT + 1],
            vec![0xff],
            b"s \"bad\\nmetadata\"".to_vec(),
        ] {
            assert!(parse::eligible(&value, 1000, "c1").is_err());
        }
        assert_eq!(parse::owner(b"s \":1.42\"\n").unwrap(), ":1.42");
        for owner in [
            b"s \"org.freedesktop.login1\"".as_slice(),
            b"s \":1.2\" trailing",
            b"s \":1.2\\n\"",
        ] {
            assert!(parse::owner(owner).is_err());
        }
        assert!(parse::root(b"u 0\n").is_ok());
        assert!(parse::root(b"u 1000\n").is_err());
    }

    #[test]
    fn linux_login_inventory_is_bounded_unique_and_not_manager_presence() {
        assert!(parse::sessions(b"a(so) 0\n").unwrap().is_empty());
        assert_eq!(
            parse::sessions(b"a(so) 1 \"c1\" \"/org/freedesktop/login1/session/c1\"\n")
                .unwrap()
                .len(),
            1
        );
        for value in [
            "a(so) 65", "a(so) 1 \"c1\" \"/foreign\"",
            "a(so) 2 \"c1\" \"/org/freedesktop/login1/session/c1\" \"c1\" \"/org/freedesktop/login1/session/c2\"",
            "a(so) 1 \"c1\" \"/org/freedesktop/login1/session/../x\"",
            "a(so) 0 trailing",
        ] {
            assert!(parse::sessions(value.as_bytes()).is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linux_login_proof_pins_root_owner_uid_and_refuses_inventory_or_issuer_race() {
        use std::collections::VecDeque;
        struct Bus {
            replies: RefCell<VecDeque<Vec<u8>>>,
            requests: RefCell<Vec<Vec<String>>>,
        }
        impl native::Reader for Bus {
            async fn read(&self, args: &[&str], budget: &mut usize) -> io::Result<Vec<u8>> {
                self.requests
                    .borrow_mut()
                    .push(args.iter().map(|value| (*value).into()).collect());
                let bytes = self.replies.borrow_mut().pop_front().unwrap();
                *budget = budget.checked_sub(bytes.len()).ok_or_else(refused)?;
                Ok(bytes)
            }
        }
        let inventory = b"a(so) 1 \"c1\" \"/org/freedesktop/login1/session/c1\"\n".to_vec();
        let replies = vec![
            b"s \":1.42\"\n".to_vec(),
            b"u 0\n".to_vec(),
            inventory.clone(),
            session("user", "tty", "online"),
            inventory.clone(),
            b"s \":1.42\"\n".to_vec(),
        ];
        let bus = |replies: Vec<Vec<u8>>| Bus {
            replies: RefCell::new(replies.into()),
            requests: RefCell::new(Vec::new()),
        };
        let trusted = bus(replies.clone());
        assert!(native::proof(1000, &trusted).await.unwrap());
        {
            let requests = trusted.requests.borrow();
            assert_eq!(requests[1].last().unwrap(), ":1.42");
            for request in &requests[2..5] {
                assert_eq!(request[1], ":1.42");
            }
            assert_eq!(requests[2][2], "/org/freedesktop/login1/user/_1000");
        }
        for (index, replacement) in [
            (1, b"u 1001\n".to_vec()),
            (3, session("user", "tty", "opening")),
            (4, b"a(so) 0\n".to_vec()),
            (5, b"s \":1.43\"\n".to_vec()),
        ] {
            let mut changed = replies.clone();
            changed[index] = replacement;
            assert!(native::proof(1000, &bus(changed)).await.is_err());
        }
        let empty = bus(vec![
            b"s \":1.42\"\n".to_vec(),
            b"u 0\n".to_vec(),
            b"a(so) 0\n".to_vec(),
            b"a(so) 0\n".to_vec(),
            b"s \":1.42\"\n".to_vec(),
        ]);
        assert!(!native::proof(1000, &empty).await.unwrap());
    }

    struct Script(RefCell<Vec<io::Result<bool>>>, Cell<usize>);
    impl Query for Script {
        async fn eligible(&self, _: &CancellationToken) -> io::Result<bool> {
            self.1.set(self.1.get() + 1);
            self.0.borrow_mut().remove(0)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn linux_login_missing_initial_proof_is_typed_retryable() {
        let query = Script(RefCell::new(vec![Ok(false)]), Cell::new(0));
        let error = admit(&query, &CancellationToken::new()).await.unwrap_err();
        assert_eq!(
            crate::run_exit::error_exit_code(&error),
            crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
        );
        assert_eq!(query.1.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn linux_login_last_logout_or_uncertainty_cancels_and_drains_only_host() {
        for last in [Ok(false), Err(refused())] {
            let query = Script(RefCell::new(vec![Ok(true), Ok(true), last]), Cell::new(0));
            let shutdown = CancellationToken::new();
            let drained = Cell::new(false);
            let serving = async {
                shutdown.cancelled().await;
                drained.set(true);
                Ok(())
            };
            let error = guard(&query, CancellationToken::new(), shutdown.clone(), serving)
                .await
                .unwrap_err();
            assert_eq!(
                crate::run_exit::error_exit_code(&error),
                crate::run_exit::LOGIN_PROOF_UNAVAILABLE_EXIT_CODE
            );
            assert!(drained.get());
            assert!(shutdown.is_cancelled());
            assert_eq!(query.1.get(), 3);
        }
    }

    #[tokio::test]
    async fn linux_login_loss_never_masks_non_retryable_host_failure_while_draining() {
        for reason in [
            "host.runtime_current_provisioned_consent_required",
            "host.runtime_current_owner_consent_required",
            "host.runtime_consent_changed",
            "host.authenticated_bundle_required",
            "host.runtime_daemon_boundary_unavailable",
            "host.cloud_offline_or_unknown",
            "host.runtime_stream_closed",
            "host.runtime_failed",
        ] {
            let query = Script(RefCell::new(vec![Ok(false)]), Cell::new(0));
            let shutdown = CancellationToken::new();
            let serving = async {
                shutdown.cancelled().await;
                Err(io::Error::other(reason))
            };
            let error = guard(&query, CancellationToken::new(), shutdown.clone(), serving)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), reason);
            assert_eq!(crate::run_exit::error_exit_code(&error), 1);
            assert!(shutdown.is_cancelled());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn linux_login_query_deadline_and_run_cancellation_leave_no_watcher() {
        struct Hung;
        impl Query for Hung {
            async fn eligible(&self, _: &CancellationToken) -> io::Result<bool> {
                std::future::pending().await
            }
        }
        assert!(admit(&Hung, &CancellationToken::new()).await.is_err());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let query = Script(RefCell::new(vec![]), Cell::new(0));
        let shutdown = CancellationToken::new();
        let serving = async {
            shutdown.cancelled().await;
            Ok(())
        };
        assert!(guard(&query, cancel, shutdown.clone(), serving)
            .await
            .is_ok());
        assert_eq!(query.1.get(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn linux_login_finished_run_never_keeps_polling_or_restarts_hosting() {
        let query = Script(RefCell::new(vec![Ok(true)]), Cell::new(0));
        assert!(guard(
            &query,
            CancellationToken::new(),
            CancellationToken::new(),
            async { Ok(()) }
        )
        .await
        .is_ok());
        let calls = query.1.get();
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(query.1.get(), calls);
    }
}
