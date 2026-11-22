//! Private report-only upload intents. No remote operation is available here.
use festerm_sessiond::private_storage as storage;
use festerm_ssh::UploadRecoveryError as Error;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

const RECORD: &str = "reports.json";
const LOCK: &str = "reports.lock";
pub(super) const MAX_REPORTS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;
const MAX_FILES: usize = MAX_REPORTS * 2 + 8;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub fingerprint: String,
    pub profile: Option<String>,
    pub label: String,
}
impl Endpoint {
    fn validate(&self) -> Result<(), Error> {
        if self.port == 0
            || !text(&self.host, 253)
            || !text(&self.username, 256)
            || !text(&self.label, 256)
            || !text(&self.fingerprint, 128)
            || !self.fingerprint.starts_with("SHA256:")
            || self.profile.as_deref().is_some_and(|v| !text(v, 256))
        {
            return Err(Error::Changed);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    pub id: String,
    pub endpoint: Endpoint,
    pub temporary: String,
    pub destination: Option<String>,
}
impl Report {
    fn validate(&self) -> Result<(), Error> {
        self.endpoint.validate()?;
        if !Uuid::parse_str(&self.id)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == self.id)
            || !remote_path(&self.temporary)
            || self.destination.as_deref().is_some_and(|p| !remote_path(p))
        {
            return Err(Error::Changed);
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u8,
    reports: BTreeMap<String, Report>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: 1,
            reports: BTreeMap::new(),
        }
    }
}
impl Catalog {
    fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || self.reports.len() > MAX_REPORTS {
            return Err(Error::Changed);
        }
        for (id, report) in &self.reports {
            report.validate()?;
            if id != &report.id {
                return Err(Error::Changed);
            }
        }
        Ok(())
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn remote_path(value: &str) -> bool {
    // Report the exact server path, never a client-local filesystem name.
    // Existing SFTP servers may return drive-qualified or relative canonical paths.
    text(value, 4096)
}
fn unavailable(_: io::Error) -> Error {
    Error::Unavailable
}
fn same(a: &File, b: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (a, b) = (a.metadata()?, b.metadata()?);
        Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
    }
    #[cfg(windows)]
    {
        festerm_windows_security::same_file_identity(a, b)
    }
}
fn lock_name(id: &str) -> String {
    format!("{id}.lock")
}

// Match the existing workflow/recording private-parent creation policy, with
// a finite missing-component budget. Existing permissions are never repaired.
fn missing_private_components(path: &Path) -> Result<Vec<PathBuf>, Error> {
    if !path.is_absolute() {
        return Err(Error::Changed);
    }
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if missing.len() == 16 {
                    return Err(Error::Unavailable);
                }
                missing.push(current.to_owned());
                current = current.parent().ok_or(Error::Changed)?;
            }
            Err(_) => return Err(Error::Unavailable),
        }
    }
    // Validates every existing component without following aliases, including
    // a dangling intermediate alias that a full-path lstat reports as absent.
    storage::trusted_directory(current, false).map_err(unavailable)?;
    Ok(missing)
}

pub(super) fn root_is_missing(path: &Path) -> Result<bool, Error> {
    Ok(!missing_private_components(path)?.is_empty())
}

fn ensure_private_directory(path: &Path) -> Result<File, Error> {
    let missing = missing_private_components(path)?;
    for path in missing.into_iter().rev() {
        let _created = storage::private_directory(&path, true).map_err(unavailable)?;
        #[cfg(unix)]
        storage::trusted_directory(path.parent().ok_or(Error::Changed)?, false)
            .and_then(|parent| parent.sync_all())
            .map_err(unavailable)?;
    }
    storage::private_directory(path, false).map_err(unavailable)
}

pub(super) struct Root {
    path: PathBuf,
    directory: File,
}
pub(super) struct Active {
    _file: storage::UnlockOnDrop,
}
pub(super) struct Token {
    root: Arc<Root>,
    pub report: Report,
    _active: Arc<Active>,
}
#[derive(Clone)]
pub(crate) struct ListedReport {
    pub report: Report,
    pub active: bool,
}
impl Root {
    pub fn open(path: &Path, create: bool) -> Result<Self, Error> {
        let directory = if create {
            ensure_private_directory(path)?
        } else {
            storage::private_directory(path, false).map_err(unavailable)?
        };
        Ok(Self {
            path: path.into(),
            directory,
        })
    }
    fn current(&self) -> Result<(), Error> {
        let fresh = storage::private_directory(&self.path, false).map_err(unavailable)?;
        if !same(&self.directory, &fresh).map_err(unavailable)? {
            return Err(Error::Changed);
        }
        Ok(())
    }
    fn catalog_lock(&self, create: bool) -> Result<storage::UnlockOnDrop, Error> {
        self.current()?;
        let name = Path::new(LOCK);
        let file = match storage::open_private_file(&self.directory, name, false) {
            Ok(f) => f,
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                match storage::open_private_file(&self.directory, name, true) {
                    Ok(f) => f,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        storage::open_private_file(&self.directory, name, false)
                            .map_err(unavailable)?
                    }
                    Err(e) => return Err(unavailable(e)),
                }
            }
            Err(e) => return Err(unavailable(e)),
        };
        let file = storage::UnlockOnDrop(file);
        file.try_lock_exclusive().map_err(|e| {
            if e.raw_os_error()
                .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
            {
                Error::Busy
            } else {
                Error::Unavailable
            }
        })?;
        let fresh =
            storage::open_private_file(&self.directory, name, false).map_err(unavailable)?;
        if !same(&file, &fresh).map_err(unavailable)? {
            return Err(Error::Changed);
        }
        self.current()?;
        Ok(file)
    }
    fn read(&self) -> Result<Catalog, Error> {
        let bytes = match storage::read_private(&self.directory, Path::new(RECORD), MAX_BYTES) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Catalog::default()),
            Err(e) => return Err(unavailable(e)),
        };
        let catalog: Catalog = serde_json::from_slice(&bytes).map_err(|_| Error::Changed)?;
        catalog.validate()?;
        Ok(catalog)
    }
    fn save(&self, catalog: &Catalog) -> Result<(), Error> {
        catalog.validate()?;
        let bytes = serde_json::to_vec(catalog).map_err(|_| Error::Changed)?;
        if bytes.len() > MAX_BYTES {
            return Err(Error::Capacity);
        }
        self.current()?;
        storage::replace_private(&self.directory, Path::new(RECORD), &bytes)
            .map_err(unavailable)?;
        self.current()
    }
    fn active(&self, id: &str, create: bool) -> Result<Active, Error> {
        let name = lock_name(id);
        let path = Path::new(&name);
        let file =
            storage::open_private_file(&self.directory, path, create).map_err(unavailable)?;
        let file = storage::UnlockOnDrop(file);
        file.try_lock_exclusive().map_err(|e| {
            if e.raw_os_error()
                .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
            {
                Error::Busy
            } else {
                Error::Unavailable
            }
        })?;
        let fresh =
            storage::open_private_file(&self.directory, path, false).map_err(unavailable)?;
        if !same(&file, &fresh).map_err(unavailable)? {
            return Err(Error::Changed);
        }
        Ok(Active { _file: file })
    }
    pub fn begin(self: Arc<Self>, endpoint: Endpoint, temporary: String) -> Result<Token, Error> {
        let report = Report {
            id: Uuid::new_v4().to_string(),
            endpoint,
            temporary,
            destination: None,
        };
        report.validate()?;
        let _held = self.catalog_lock(true)?;
        let mut catalog = self.read()?;
        if catalog.reports.len() >= MAX_REPORTS
            || std::fs::read_dir(&self.path)
                .map_err(unavailable)?
                .take(MAX_FILES + 1)
                .collect::<Result<Vec<_>, _>>()
                .map_err(unavailable)?
                .len()
                >= MAX_FILES
        {
            return Err(Error::Capacity);
        }
        self.current()?;
        let active = Arc::new(self.active(&report.id, true)?);
        catalog.reports.insert(report.id.clone(), report.clone());
        if let Err(error) = self.save(&catalog) {
            // A failed sync may follow a committed local checkpoint. Keep the
            // report/lock unless a fresh read positively proves no such entry.
            if self
                .read()
                .is_ok_and(|c| !c.reports.contains_key(&report.id))
            {
                let _ = storage::remove_private_file(
                    &self.directory,
                    Path::new(&lock_name(&report.id)),
                );
            }
            return Err(error);
        }
        Ok(Token {
            root: self.clone(),
            report,
            _active: active,
        })
    }
    pub fn list(&self) -> Result<Vec<ListedReport>, Error> {
        match storage::open_private_file(&self.directory, Path::new(RECORD), false) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.current()?;
                return Ok(Vec::new());
            }
            Err(e) => return Err(unavailable(e)),
            Ok(_) => {}
        }
        let _held = self.catalog_lock(false)?;
        let catalog = self.read()?;
        let mut result = Vec::new();
        for report in catalog.reports.into_values() {
            let active = match self.active(&report.id, false) {
                Ok(_) => false,
                Err(Error::Busy) => true,
                Err(e) => return Err(e),
            };
            result.push(ListedReport { report, active });
        }
        self.current()?;
        Ok(result)
    }
    pub fn acknowledge(&self, expected: &Report) -> Result<(), Error> {
        expected.validate()?;
        let _held = self.catalog_lock(false)?;
        // Never wait for an active owner while holding the short catalog lock.
        let _active = self.active(&expected.id, false)?;
        let mut catalog = self.read()?;
        if catalog.reports.get(&expected.id) != Some(expected) {
            return Err(Error::Changed);
        }
        catalog.reports.remove(&expected.id);
        self.save(&catalog)?;
        storage::remove_private_file(&self.directory, Path::new(&lock_name(&expected.id)))
            .map_err(unavailable)
    }
}
impl Token {
    pub fn checkpoint(&mut self, destination: String) -> Result<(), Error> {
        if !remote_path(&destination) {
            return Err(Error::Changed);
        }
        let _held = self.root.catalog_lock(false)?;
        let mut catalog = self.root.read()?;
        if catalog.reports.get(&self.report.id) != Some(&self.report) {
            return Err(Error::Changed);
        }
        if let Some(current) = &self.report.destination {
            return if current == &destination {
                Ok(())
            } else {
                Err(Error::Changed)
            };
        }
        let mut next = self.report.clone();
        next.destination = Some(destination);
        catalog.reports.insert(next.id.clone(), next.clone());
        self.root.save(&catalog)?;
        self.report = next;
        Ok(())
    }
    pub fn retire(&self) -> Result<(), Error> {
        let _held = self.root.catalog_lock(false)?;
        let mut catalog = self.root.read()?;
        if catalog.reports.get(&self.report.id) != Some(&self.report) {
            return Err(Error::Changed);
        }
        catalog.reports.remove(&self.report.id);
        self.root.save(&catalog)?;
        storage::remove_private_file(&self.root.directory, Path::new(&lock_name(&self.report.id)))
            .map_err(unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn endpoint() -> Endpoint {
        Endpoint {
            host: "host.test".into(),
            port: 22,
            username: "owner".into(),
            fingerprint: "SHA256:owned-fixture".into(),
            profile: Some("fixture".into()),
            label: "Owned fixture".into(),
        }
    }
    #[test]
    fn durable_upload_reports_refuse_active_acknowledgement_and_retire_local_metadata() {
        let private = storage::private_test_directory();
        let path = private.path().join("reports");
        let root = Arc::new(Root::open(&path, true).unwrap());
        let mut token = root
            .clone()
            .begin(endpoint(), "/owned/file.festerm-part".into())
            .unwrap();
        assert!(root.list().unwrap()[0].active);
        assert_eq!(root.acknowledge(&token.report).err(), Some(Error::Busy));
        token.checkpoint("/owned/file".into()).unwrap();
        let expected = token.report.clone();
        drop(token);
        let reopened = Root::open(&path, false).unwrap();
        let listed = reopened.list().unwrap();
        assert_eq!(listed[0].report, expected);
        assert!(!listed[0].active);
        reopened.acknowledge(&expected).unwrap();
        assert!(reopened.list().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
        for _ in 0..MAX_REPORTS + 2 {
            let token = root
                .clone()
                .begin(endpoint(), "/owned/next.festerm-part".into())
                .unwrap();
            token.retire().unwrap();
        }
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
    }
    #[test]
    fn durable_upload_reports_create_missing_private_application_parents_without_repair() {
        let private = storage::private_test_directory();
        let application = private.path().join("missing-data").join("application");
        let path = application.join("sftp-upload-recovery-v1");
        let root = Arc::new(Root::open(&path, true).unwrap());
        let token = root
            .clone()
            .begin(endpoint(), "home/owner/file.festerm-part".into())
            .unwrap();
        drop(token);
        assert_eq!(Root::open(&path, false).unwrap().list().unwrap().len(), 1);
        storage::private_directory(&application, false).unwrap();
        storage::private_directory(application.parent().unwrap(), false).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let alias = private.path().join("alias");
            symlink(&application, &alias).unwrap();
            assert!(Root::open(&alias.join("other-report-root"), true).is_err());
            assert!(!application.join("other-report-root").exists());
            let untrusted = private.path().join("untrusted");
            std::fs::create_dir(&untrusted).unwrap();
            std::fs::set_permissions(&untrusted, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(Root::open(&untrusted.join("report-root"), true).is_err());
            assert_eq!(
                std::fs::metadata(&untrusted).unwrap().permissions().mode() & 0o777,
                0o777
            );
            assert!(!untrusted.join("report-root").exists());
        }
    }

    #[test]
    fn durable_upload_reports_explicitly_release_catalog_and_operation_locks_with_live_duplicates()
    {
        let private = storage::private_test_directory();
        let root = Arc::new(Root::open(&private.path().join("reports"), true).unwrap());
        let token = root
            .clone()
            .begin(endpoint(), "/owned/file.festerm-part".into())
            .unwrap();
        let report = token.report.clone();
        let catalog = root.catalog_lock(false).unwrap();
        let retained_catalog = catalog.try_clone().unwrap();
        drop(catalog);
        drop(
            root.catalog_lock(false).expect(
                "catalog Drop must unlock even while an inherited description remains open",
            ),
        );
        let retained_operation = token._active._file.try_clone().unwrap();
        drop(token);
        assert!(!root.list().unwrap()[0].active,"operation Drop must explicitly unlock, not wait for all inherited descriptions to close");
        root.acknowledge(&report).unwrap();
        assert!(root.list().unwrap().is_empty());
        drop(retained_catalog);
        drop(retained_operation);
    }

    #[test]
    fn durable_upload_reports_preserve_server_path_forms_without_local_interpretation() {
        let private = storage::private_test_directory();
        let path = private.path().join("reports");
        let root = Arc::new(Root::open(&path, true).unwrap());
        for temporary in [
            "/home/owner/file.festerm-part",
            "C:/Users/owner/file.festerm-part",
            "home/owner/file.festerm-part",
            r"C:\Users\owner\file.festerm-part",
            "//server/share/file.festerm-part",
        ] {
            let destination = temporary.strip_suffix(".festerm-part").unwrap().to_owned();
            let mut token = root.clone().begin(endpoint(), temporary.into()).unwrap();
            token.checkpoint(destination.clone()).unwrap();
            let expected = token.report.clone();
            drop(token);
            let reopened = Root::open(&path, false).unwrap();
            let reports = reopened.list().unwrap();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].report.temporary, temporary);
            assert_eq!(
                reports[0].report.destination.as_deref(),
                Some(destination.as_str())
            );
            assert_eq!(reports[0].report, expected);
            reopened.acknowledge(&expected).unwrap();
        }
        for invalid in ["", "file\nname", "file\0name", &"a".repeat(4097)] {
            assert_eq!(
                root.clone().begin(endpoint(), invalid.into()).err(),
                Some(Error::Changed)
            );
        }
        assert!(root.list().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(&path).unwrap().count(),
            2,
            "remote paths never become local filenames"
        );
    }

    #[test]
    fn durable_upload_reports_refuse_byte_overflow_and_changed_ack_without_eviction() {
        let private = storage::private_test_directory();
        let root = Arc::new(Root::open(&private.path().join("reports"), true).unwrap());
        let mut first = None;
        loop {
            match root
                .clone()
                .begin(endpoint(), format!("/{}", "a".repeat(4000)))
            {
                Ok(mut token) => {
                    // A publication checkpoint must obey the same whole-catalog byte cap.
                    match token.checkpoint(format!("/{}", "b".repeat(4000))) {
                        Ok(()) => {}
                        Err(Error::Capacity) => {
                            if first.is_none() {
                                first = Some(token.report.clone());
                            }
                            break;
                        }
                        Err(e) => panic!("unexpected {e:?}"),
                    }
                    if first.is_none() {
                        first = Some(token.report.clone());
                    }
                }
                Err(Error::Capacity) => break,
                Err(e) => panic!("unexpected {e:?}"),
            }
        }
        let before = std::fs::read(root.path.join(RECORD)).unwrap();
        assert!(before.len() <= MAX_BYTES);
        let reports = root.list().unwrap();
        assert!(!reports.is_empty());
        assert!(reports.len() < MAX_REPORTS);
        let mut changed = first.unwrap();
        changed.endpoint.fingerprint = "SHA256:other-endpoint".into();
        assert_eq!(root.acknowledge(&changed), Err(Error::Changed));
        assert_eq!(std::fs::read(root.path.join(RECORD)).unwrap(), before);
        assert_eq!(root.list().unwrap().len(), reports.len());
    }

    #[test]
    fn durable_upload_reports_keep_endpoint_separation_and_bounded_unresolved_records() {
        let private = storage::private_test_directory();
        let root = Arc::new(Root::open(&private.path().join("reports"), true).unwrap());
        for n in 0..MAX_REPORTS {
            let mut endpoint = endpoint();
            endpoint.host = format!("host{n}.test");
            let token = root
                .clone()
                .begin(endpoint, format!("/owned/{n}.festerm-part"))
                .unwrap();
            drop(token);
        }
        assert_eq!(
            root.clone()
                .begin(endpoint(), "/owned/overflow.festerm-part".into())
                .err(),
            Some(Error::Capacity)
        );
        let reports = root.list().unwrap();
        assert_eq!(reports.len(), MAX_REPORTS);
        let hosts: std::collections::BTreeSet<_> =
            reports.iter().map(|r| &r.report.endpoint.host).collect();
        assert_eq!(hosts.len(), MAX_REPORTS);
        assert!(reports.iter().all(|r| !r.active));
        let raw = std::fs::read_to_string(root.path.join(RECORD)).unwrap();
        assert!(
            !raw.contains("access_token")
                && !raw.contains("credential_reference")
                && !raw.contains("PRIVATE_PASSWORD")
        );
    }
}

#[cfg(test)]
mod crash_tests {
    use super::*;
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn endpoint() -> Endpoint {
        Endpoint {
            host: "crash.test".into(),
            port: 22,
            username: "owner".into(),
            fingerprint: "SHA256:owned-crash-fixture".into(),
            profile: None,
            label: "Crash fixture".into(),
        }
    }
    #[test]
    fn upload_report_child_retains_operation_until_killed() {
        let Some(path) = std::env::var_os("FESTERM_UPLOAD_REPORT_CHILD_ROOT") else {
            return;
        };
        let path = PathBuf::from(path);
        assert_eq!(
            std::fs::read(path.join("owned-fixture-proof")).unwrap(),
            b"upload-report-crash-fixture-v1"
        );
        let root = Arc::new(Root::open(&path.join("reports"), true).unwrap());
        let _token = root
            .begin(endpoint(), "/owned/crash.festerm-part".into())
            .unwrap();
        std::fs::write(path.join("ready"), b"durable intent and active lock").unwrap();
        loop {
            std::thread::park();
        }
    }
    #[test]
    fn durable_upload_report_survives_killed_owner_and_refuses_other_process_ack() {
        let private = storage::private_test_directory();
        std::fs::write(
            private.path().join("owned-fixture-proof"),
            b"upload-report-crash-fixture-v1",
        )
        .unwrap();
        let mut child = OwnedChild(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::sftp_recovery::store::crash_tests::upload_report_child_retains_operation_until_killed", "--test-threads=1"])
            .env("FESTERM_UPLOAD_REPORT_CHILD_ROOT", private.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn().unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ready = loop {
            if private.path().join("ready").exists() {
                break true;
            }
            if std::time::Instant::now() >= deadline || child.0.try_wait().unwrap().is_some() {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        if !ready {
            let _ = child.0.kill();
            let _ = child.0.wait();
            panic!("owned child did not publish its durable intent");
        }
        let root = Root::open(&private.path().join("reports"), false).unwrap();
        let reports = root.list().unwrap();
        let before = reports[0].report.clone();
        let active = reports[0].active;
        let refused = root.acknowledge(&before).err();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(active);
        assert_eq!(refused, Some(Error::Busy));
        let reopened = Root::open(&private.path().join("reports"), false).unwrap();
        let after = reopened.list().unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].report, before);
        assert!(!after[0].active);
        reopened.acknowledge(&before).unwrap();
        assert!(reopened.list().unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(private.path().join("reports"))
                .unwrap()
                .count(),
            2
        );
    }
    #[test]
    fn upload_report_failed_checkpoint_and_retirement_preserve_exact_saved_intent() {
        let private = storage::private_test_directory();
        let path = private.path().join("reports");
        let root = Arc::new(Root::open(&path, true).unwrap());
        let mut token = root
            .clone()
            .begin(endpoint(), "/owned/file.festerm-part".into())
            .unwrap();
        let expected = token.report.clone();
        let held = root.catalog_lock(false).unwrap();
        assert_eq!(
            token.checkpoint("/owned/file".into()).err(),
            Some(Error::Busy)
        );
        assert_eq!(token.retire().err(), Some(Error::Busy));
        drop(held);
        assert_eq!(root.list().unwrap()[0].report, expected);
        drop(token);
        assert!(!root.list().unwrap()[0].active);
        let guard = root.catalog_lock(false).unwrap();
        assert_eq!(
            root.clone()
                .begin(endpoint(), "/owned/other.festerm-part".into())
                .err(),
            Some(Error::Busy)
        );
        drop(guard);
        assert_eq!(root.list().unwrap().len(), 1);
    }
    #[test]
    fn upload_report_concurrent_writers_never_lose_or_evict_a_saved_intent() {
        let private = storage::private_test_directory();
        let root = Arc::new(Root::open(&private.path().join("reports"), true).unwrap());
        // Materialize the catalog/lock through an ordinary first report.
        let initial = root
            .clone()
            .begin(endpoint(), "/owned/first.festerm-part".into())
            .unwrap();
        drop(initial);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut jobs = Vec::new();
        for n in 0..2 {
            let root = root.clone();
            let barrier = barrier.clone();
            jobs.push(std::thread::spawn(move || {
                barrier.wait();
                root.begin(endpoint(), format!("/owned/{n}.festerm-part"))
                    .map(|token| token.report.id.clone())
            }));
        }
        barrier.wait();
        let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert!(results
            .iter()
            .all(|r| r.is_ok() || matches!(r, Err(Error::Busy))));
        let reports = root.list().unwrap();
        assert_eq!(
            reports.len(),
            1 + results.iter().filter(|r| r.is_ok()).count()
        );
        assert!(reports
            .iter()
            .any(|r| r.report.temporary == "/owned/first.festerm-part"));
    }
}
