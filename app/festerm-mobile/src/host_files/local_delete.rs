//! One Documents-only retained review; no remote RPC or durable recovery scan.
use super::*;
#[cfg(unix)]
use crate::files::deletion::{Outcome, Review, ReviewDetail};

#[cfg(unix)]
mod platform {
    use super::*;
    use festerm_unix_security as native;
    use std::{
        fs::File,
        io,
        os::unix::fs::MetadataExt,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    type Current = Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>;
    fn denied() -> io::Error {
        io::ErrorKind::PermissionDenied.into()
    }
    fn io<T>(result: Result<T, SftpError>) -> io::Result<T> {
        result.map_err(|_| denied())
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Stamp {
        dev: u64,
        ino: u64,
        size: u64,
        modified: (i64, i64),
        changed: (i64, i64),
        directory: bool,
    }
    fn stamp(file: &File) -> io::Result<Stamp> {
        let m = file.metadata()?;
        if (!m.is_file() && !m.is_dir())
            || (m.is_file() && m.nlink() != 1)
            || m.uid() != native::current_user_id()?
            || m.mode() & 0o022 != 0
        {
            return Err(denied());
        }
        Ok(Stamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
            directory: m.is_dir(),
        })
    }
    fn same(a: &File, b: &File) -> io::Result<bool> {
        let a = a.metadata()?;
        let b = b.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    fn exact(parent: &File, name: &str) -> io::Result<()> {
        if native::directory_has_exact_entry(
            parent,
            std::ffi::OsStr::new(name),
            festerm_sftp::DEFAULT_MAX_SCAN,
        )? {
            Ok(())
        } else {
            Err(denied())
        }
    }

    fn open(parent: &File, name: &str) -> io::Result<File> {
        exact(parent, name)?;
        let file = native::open_directory_read_nofollow(parent, Path::new(name))
            .or_else(|_| native::open_file_nofollow(parent, Path::new(name)))?;
        exact(parent, name)?;
        Ok(file)
    }
    struct Leaf {
        name: String,
        file: File,
        stamp: Stamp,
        security: native::SecurityMetadata,
    }
    struct Batch {
        id: u64,
        stop: CancellationToken,
        components: Vec<String>,
        parent: File,
        leaves: Vec<Leaf>,
    }
    #[cfg(test)]
    type CaptureObserver = Box<dyn Fn(&File, &str)>;
    pub(crate) struct Owner {
        root: Arc<local_root::Root>,
        current: Current,
        lifetime: CancellationToken,
        wake: Arc<dyn Fn() + Send + Sync>,
        batch: Option<Batch>,
        #[cfg(test)]
        pub(crate) after_capture: Option<CaptureObserver>,
    }
    impl Owner {
        pub(crate) fn new(
            root: Arc<local_root::Root>,
            current: Current,
            lifetime: CancellationToken,
            wake: Arc<dyn Fn() + Send + Sync>,
        ) -> Self {
            Self {
                root,
                current,
                lifetime,
                wake,
                batch: None,
                #[cfg(test)]
                after_capture: None,
            }
        }
        pub(crate) fn stop(&self) -> CancellationToken {
            self.batch
                .as_ref()
                .map(|b| b.stop.clone())
                .unwrap_or_default()
        }
        pub(crate) fn retire(&mut self) {
            self.batch = None;
        }
        fn current(&self) -> io::Result<()> {
            if self.lifetime.is_cancelled() {
                return Err(denied());
            }
            io((self.current)())?;
            io(self.root.check())?;
            if self.lifetime.is_cancelled() {
                return Err(denied());
            }
            Ok(())
        }
        fn parent(&self, components: &[String]) -> io::Result<File> {
            let mut parent = io(self.root.directory())?;
            let dev = parent.metadata()?.dev();
            for component in components {
                parent = open(&parent, component)?;
                native::trusted_private_storage_ancestor(&parent)?;
                if parent.metadata()?.dev() != dev {
                    return Err(denied());
                }
            }
            Ok(parent)
        }
        fn prepare(&self, paths: Vec<String>, stop: CancellationToken) -> io::Result<Batch> {
            self.current()?;
            if stop.is_cancelled() || paths.is_empty() || paths.len() > 8 {
                return Err(denied());
            }
            let mut common = None;
            let mut seen = std::collections::BTreeSet::new();
            let mut leaves = Vec::new();
            let mut retained = None;
            for path in paths {
                if path.is_empty()
                    || path.len() > 4096
                    || path.starts_with('/')
                    || !seen.insert(path.clone())
                {
                    return Err(denied());
                }
                let mut parts: Vec<String> = path.split('/').map(str::to_owned).collect();
                if parts.len() > 128
                    || parts.iter().any(|p| {
                        p.is_empty()
                            || p == "."
                            || p == ".."
                            || p.to_ascii_lowercase().starts_with(".festerm-")
                            || festerm_sftp::path::validate_name(p).is_err()
                    })
                {
                    return Err(denied());
                }
                let name = parts.pop().ok_or_else(denied)?;
                if common.as_ref().is_some_and(|p| p != &parts) {
                    return Err(denied());
                }
                let parent = self.parent(&parts)?;
                let file = open(&parent, &name)?;
                let value = stamp(&file)?;
                if value.dev != parent.metadata()?.dev()
                    || (value.directory && !native::directory_is_empty(&file)?)
                {
                    return Err(denied());
                }
                if let Some(previous) = retained.as_ref() {
                    if !same(previous, &parent)? {
                        return Err(denied());
                    }
                }
                leaves.push(Leaf {
                    name,
                    file: file.try_clone()?,
                    stamp: value,
                    security: native::security_metadata(&file)?,
                });
                common = Some(parts);
                retained = Some(parent);
            }
            self.current()?;
            if stop.is_cancelled() {
                return Err(denied());
            }
            let id = NEXT
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| denied())?;
            Ok(Batch {
                id,
                stop,
                components: common.ok_or_else(denied)?,
                parent: retained.ok_or_else(denied)?,
                leaves,
            })
        }
        fn rooted_parent(&self, b: &Batch) -> io::Result<()> {
            if !same(&self.parent(&b.components)?, &b.parent)? {
                return Err(denied());
            }
            Ok(())
        }
        fn checked_leaf(&self, b: &Batch, l: &Leaf, name: &str, expected: Stamp) -> io::Result<()> {
            self.rooted_parent(b)?;
            let current = open(&b.parent, name)?;
            if !same(&current, &l.file)?
                || stamp(&l.file)? != expected
                || !native::security_metadata_matches(&l.file, &l.security)?
            {
                return Err(denied());
            }
            Ok(())
        }
        fn commit(&self, b: Batch) -> Outcome {
            let mut out = Outcome {
                confirmed: 0,
                total: b.leaves.len(),
                unknown: false,
                failed: false,
                recovery: None,
            };
            let permit = festerm_ios_window::try_local_file_mutation(self.wake.clone());
            let Ok(_permit) = permit else {
                out.failed = true;
                return out;
            };
            for (index, l) in b.leaves.iter().enumerate() {
                if self.current().is_err()
                    || b.stop.is_cancelled()
                    || self.checked_leaf(&b, l, &l.name, l.stamp).is_err()
                    || self.current().is_err()
                    || b.stop.is_cancelled()
                    || self.lifetime.is_cancelled()
                {
                    out.failed = true;
                    break;
                }
                let capture = format!(".festerm-delete-{}-{}-{index}", std::process::id(), b.id);
                if native::rename_noreplace(
                    &b.parent,
                    Path::new(&l.name),
                    &b.parent,
                    Path::new(&capture),
                )
                .is_err()
                {
                    out.failed = true;
                    break;
                }
                #[cfg(test)]
                if let Some(observe) = &self.after_capture {
                    observe(&b.parent, &capture);
                }
                // Rename may change ctime. Identity and content must still match
                // before adopting the fresh captured stamp for final removal.
                let remove = (|| -> io::Result<()> {
                    let fresh = stamp(&l.file)?;
                    let mut original = l.stamp;
                    original.changed = fresh.changed;
                    if fresh != original {
                        return Err(denied());
                    }
                    self.current()?;
                    if b.stop.is_cancelled() || self.lifetime.is_cancelled() {
                        return Err(denied());
                    }
                    self.checked_leaf(&b, l, &capture, fresh)?;
                    self.current()?;
                    if b.stop.is_cancelled() || self.lifetime.is_cancelled() {
                        return Err(denied());
                    }
                    if fresh.directory {
                        native::remove_empty_directory(&b.parent, Path::new(&capture), &l.file)
                    } else {
                        native::remove_download_file(&b.parent, Path::new(&capture), &l.file)
                    }
                })();
                if remove.is_ok() {
                    out.confirmed += 1;
                    continue;
                }
                out.failed = true;
                // Cleanup is independent of UI cancellation. Restore only the
                // same retained entry in a still-rooted parent to a vacant name.
                let restored = (|| -> io::Result<()> {
                    self.rooted_parent(&b)?;
                    let held = open(&b.parent, &capture)?;
                    if !same(&held, &l.file)? {
                        return Err(denied());
                    }
                    native::rename_noreplace(
                        &b.parent,
                        Path::new(&capture),
                        &b.parent,
                        Path::new(&l.name),
                    )
                })();
                out.unknown = restored.is_err();
                if out.unknown
                    && self.current().is_ok()
                    && self.rooted_parent(&b).is_ok()
                    && open(&b.parent, &capture)
                        .and_then(|held| same(&held, &l.file))
                        .unwrap_or(false)
                {
                    let mut path = b.components.clone();
                    path.push(capture.clone());
                    out.recovery = Some(festerm_sftp::path::display_name(&path.join("/")));
                }
                break;
            }
            out
        }
        pub(crate) fn execute(&mut self, command: deletion::Command) {
            match command {
                deletion::Command::Cancel => self.retire(),
                deletion::Command::Prepare(paths, stop, reply) => {
                    self.retire();
                    let result = self.prepare(paths, stop);
                    let review = result
                        .map(|batch| {
                            let review = Review {
                                id: batch.id,
                                details: Some(
                                    batch
                                        .leaves
                                        .iter()
                                        .map(|leaf| {
                                            if leaf.stamp.directory {
                                                ReviewDetail::EmptyFolder
                                            } else {
                                                ReviewDetail::File(Some(leaf.stamp.size))
                                            }
                                        })
                                        .collect(),
                                ),
                                names: batch
                                    .leaves
                                    .iter()
                                    .map(|l| festerm_sftp::path::display_name(&l.name))
                                    .collect(),
                            };
                            self.batch = Some(batch);
                            review
                        })
                        .map_err(|_| {
                            "Could not review these entries. Refresh and select again.".into()
                        });
                    if reply.send(review).is_err() {
                        self.retire()
                    }
                }
                deletion::Command::Commit(id, reply) => {
                    let result = match self.batch.take() {
                        Some(batch) if batch.id == id => Ok(self.commit(batch)),
                        _ => Err("Deletion review expired. Select again.".into()),
                    };
                    let _ = reply.send(result);
                }
            }
        }
    }
}
#[cfg(unix)]
pub(super) use platform::Owner;

#[cfg(not(unix))]
pub(super) struct Owner;
#[cfg(not(unix))]
impl Owner {
    pub(super) fn new(
        _: Arc<local_root::Root>,
        _: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        _: CancellationToken,
        _: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self
    }
    pub(super) fn stop(&self) -> CancellationToken {
        CancellationToken::new()
    }
    pub(super) fn retire(&mut self) {}
    pub(super) fn execute(&mut self, command: deletion::Command) {
        match command {
            deletion::Command::Prepare(_, _, reply) => {
                let _ = reply.send(Err("Deletion is unavailable".into()));
            }
            deletion::Command::Commit(_, reply) => {
                let _ = reply.send(Err("Deletion is unavailable".into()));
            }
            deletion::Command::Cancel => {}
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, PermissionsExt},
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static FIXTURE: AtomicU64 = AtomicU64::new(1);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "festerm-local-delete-{}-{}",
                std::process::id(),
                FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
            Self(p)
        }
        fn owner(&self) -> Owner {
            Owner::new(
                local_root::Root::open(&self.0).unwrap(),
                Arc::new(|| Ok(())),
                CancellationToken::new(),
                Arc::new(|| {}),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn prepare(
        owner: &mut Owner,
        paths: &[&str],
        stop: CancellationToken,
    ) -> Result<Review, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        owner.execute(deletion::Command::Prepare(
            paths.iter().map(|p| (*p).into()).collect(),
            stop,
            tx,
        ));
        rx.recv().unwrap()
    }
    fn commit(owner: &mut Owner, id: u64) -> Result<Outcome, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        owner.execute(deletion::Command::Commit(id, tx));
        rx.recv().unwrap()
    }
    #[test]
    fn files_delete_local_file_empty_0755_folder_and_cancel() {
        let f = Fixture::new();
        fs::write(f.0.join("a.txt"), b"original").unwrap();
        fs::create_dir(f.0.join("empty")).unwrap();
        fs::set_permissions(f.0.join("empty"), fs::Permissions::from_mode(0o755)).unwrap();
        let mut owner = f.owner();
        let stop = CancellationToken::new();
        let review = prepare(&mut owner, &["a.txt"], stop.clone()).unwrap();
        stop.cancel();
        assert!(commit(&mut owner, review.id).unwrap().failed);
        assert_eq!(fs::read(f.0.join("a.txt")).unwrap(), b"original");
        let review = prepare(&mut owner, &["a.txt", "empty"], CancellationToken::new()).unwrap();
        assert_eq!(review.names, vec!["a.txt", "empty"]);
        assert_eq!(
            review.details,
            Some(vec![ReviewDetail::File(Some(8)), ReviewDetail::EmptyFolder])
        );
        let out = commit(&mut owner, review.id).unwrap();
        assert_eq!(out.confirmed, 2);
        assert!(!out.failed && !out.unknown);
        assert!(!f.0.join("a.txt").exists() && !f.0.join("empty").exists());
    }
    #[test]
    fn files_delete_local_capture_cancel_restore_collision_and_nonempty_preserve() {
        for scenario in 0..3 {
            let f = Fixture::new();
            let name = if scenario == 2 { "empty" } else { "a" };
            if scenario == 2 {
                fs::create_dir(f.0.join(name)).unwrap();
            } else {
                fs::write(f.0.join(name), b"original").unwrap();
            }
            let stop = CancellationToken::new();
            let mut owner = f.owner();
            let review = prepare(&mut owner, &[name], stop.clone()).unwrap();
            let root = f.0.clone();
            let canceled = stop.clone();
            owner.after_capture = Some(Box::new(move |_, capture| {
                if scenario == 2 {
                    fs::write(root.join(capture).join("new-child"), b"child").unwrap();
                } else {
                    canceled.cancel();
                    if scenario == 1 {
                        fs::write(root.join("a"), b"replacement").unwrap();
                    }
                }
            }));
            let epoch = festerm_ios_window::local_file_mutation_epoch();
            let out = commit(&mut owner, review.id).unwrap();
            assert!(out.failed);
            assert_eq!(out.confirmed, 0);
            assert!(festerm_ios_window::local_file_mutation_epoch() > epoch);
            assert_eq!(out.unknown, scenario == 1);
            assert_eq!(out.recovery.is_some(), scenario == 1);
            if scenario == 2 {
                assert_eq!(fs::read(f.0.join("empty/new-child")).unwrap(), b"child");
            } else if scenario == 1 {
                assert_eq!(fs::read(f.0.join("a")).unwrap(), b"replacement");
                let captured = fs::read_dir(&f.0)
                    .unwrap()
                    .find(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".festerm-delete-")
                    })
                    .unwrap()
                    .unwrap();
                assert_eq!(fs::read(captured.path()).unwrap(), b"original");
            } else {
                assert_eq!(fs::read(f.0.join("a")).unwrap(), b"original");
                assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
            }
        }
    }
    #[test]
    fn files_delete_local_postcapture_root_and_leaf_swap_preserve_without_locator() {
        for parent_swap in [false, true] {
            let f = Fixture::new();
            fs::write(f.0.join("a"), b"original").unwrap();
            let mut owner = f.owner();
            let review = prepare(&mut owner, &["a"], CancellationToken::new()).unwrap();
            let root = f.0.clone();
            let moved = f.0.with_extension("moved");
            let target = moved.clone();
            owner.after_capture = Some(Box::new(move |_, capture| {
                if parent_swap {
                    fs::rename(&root, &target).unwrap();
                    fs::create_dir(&root).unwrap();
                    fs::write(root.join("outside-canary"), b"outside").unwrap();
                } else {
                    fs::rename(root.join(capture), root.join("retained-original")).unwrap();
                    fs::write(root.join(capture), b"replacement").unwrap();
                }
            }));
            let out = commit(&mut owner, review.id).unwrap();
            assert!(out.failed && out.unknown);
            assert!(out.recovery.is_none());
            if parent_swap {
                assert_eq!(fs::read(f.0.join("outside-canary")).unwrap(), b"outside");
                let entry = fs::read_dir(&moved).unwrap().next().unwrap().unwrap();
                assert_eq!(fs::read(entry.path()).unwrap(), b"original");
                fs::remove_dir_all(moved).unwrap();
            } else {
                assert_eq!(
                    fs::read(f.0.join("retained-original")).unwrap(),
                    b"original"
                );
                let entry = fs::read_dir(&f.0)
                    .unwrap()
                    .find(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".festerm-delete-")
                    })
                    .unwrap()
                    .unwrap();
                assert_eq!(fs::read(entry.path()).unwrap(), b"replacement");
            }
        }
    }
    #[test]
    fn files_delete_local_detached_cleanup_settles_before_writer_wakeup() {
        use std::sync::atomic::AtomicBool;
        let f = Fixture::new();
        fs::write(f.0.join("a"), b"original").unwrap();
        let settled = Arc::new(AtomicBool::new(false));
        let notified = settled.clone();
        let root = f.0.clone();
        let lifetime = CancellationToken::new();
        let canceled = lifetime.clone();
        let mut owner = Owner::new(
            local_root::Root::open(&f.0).unwrap(),
            Arc::new(|| Ok(())),
            lifetime,
            Arc::new(move || {
                assert_eq!(fs::read(root.join("a")).unwrap(), b"original");
                notified.store(true, Ordering::SeqCst);
            }),
        );
        let review = prepare(&mut owner, &["a"], CancellationToken::new()).unwrap();
        let before = settled.clone();
        owner.after_capture = Some(Box::new(move |_, _| {
            assert!(!before.load(Ordering::SeqCst));
            canceled.cancel();
        }));
        let outcome = commit(&mut owner, review.id).unwrap();
        assert!(outcome.failed && !outcome.unknown);
        assert!(settled.load(Ordering::SeqCst));
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    }
    #[test]
    fn files_delete_local_cancel_at_final_admission_does_not_capture() {
        use std::sync::atomic::AtomicBool;
        let f = Fixture::new();
        fs::write(f.0.join("a"), b"original").unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let stop = CancellationToken::new();
        let retire = stop.clone();
        let mut owner = Owner::new(
            local_root::Root::open(&f.0).unwrap(),
            Arc::new(move || {
                if count.fetch_add(1, Ordering::SeqCst) == 3 {
                    retire.cancel();
                }
                Ok(())
            }),
            CancellationToken::new(),
            Arc::new(|| {}),
        );
        let captured = Arc::new(AtomicBool::new(false));
        let observer = captured.clone();
        owner.after_capture = Some(Box::new(move |_, _| {
            observer.store(true, Ordering::SeqCst);
        }));
        let review = prepare(&mut owner, &["a"], stop).unwrap();
        let out = commit(&mut owner, review.id).unwrap();
        assert!(out.failed && !out.unknown);
        assert!(!captured.load(Ordering::SeqCst));
        assert_eq!(fs::read(f.0.join("a")).unwrap(), b"original");
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    }
    #[test]
    fn files_delete_local_owner_lifetime_canceled_during_final_validation_refuses_or_restores() {
        use std::sync::atomic::AtomicBool;
        // Two preparation checks precede commit. The fourth current callback
        // is the final capture check; the sixth is the final unlink check.
        for (canceled_at, capture_expected) in [(3, false), (5, true)] {
            let f = Fixture::new();
            fs::write(f.0.join("a"), b"original").unwrap();
            let lifetime = CancellationToken::new();
            let retire = lifetime.clone();
            let calls = Arc::new(AtomicU64::new(0));
            let count = calls.clone();
            let mut owner = Owner::new(
                local_root::Root::open(&f.0).unwrap(),
                Arc::new(move || {
                    if count.fetch_add(1, Ordering::SeqCst) == canceled_at {
                        retire.cancel();
                    }
                    Ok(())
                }),
                lifetime,
                Arc::new(|| {}),
            );
            let captured = Arc::new(AtomicBool::new(false));
            let observed = captured.clone();
            owner.after_capture = Some(Box::new(move |_, _| {
                observed.store(true, Ordering::SeqCst);
            }));
            let review_stop = CancellationToken::new();
            let review = prepare(&mut owner, &["a"], review_stop.clone()).unwrap();
            let out = commit(&mut owner, review.id).unwrap();
            assert!(
                !review_stop.is_cancelled(),
                "the independent owner lifetime is the refusal witness"
            );
            assert!(out.failed && !out.unknown);
            assert_eq!(out.confirmed, 0);
            assert_eq!(captured.load(Ordering::SeqCst), capture_expected);
            assert_eq!(fs::read(f.0.join("a")).unwrap(), b"original");
            assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
        }
    }
    #[test]
    fn files_delete_local_rejects_alias_reserved_symlink_hardlink_and_batches() {
        let f = Fixture::new();
        fs::write(f.0.join("a"), b"canary").unwrap();
        fs::hard_link(f.0.join("a"), f.0.join("hard")).unwrap();
        symlink("a", f.0.join("link")).unwrap();
        fs::create_dir(f.0.join(".festerm-cache")).unwrap();
        fs::write(f.0.join(".festerm-cache/held"), b"private").unwrap();
        let mut owner = f.owner();
        for paths in [
            vec!["a"],
            vec!["hard"],
            vec!["link"],
            vec!["/a"],
            vec!["./a"],
            vec!["x//a"],
            vec!["x/../a"],
            vec![".festerm-cache/held"],
            vec![".FESTERM-cache/held"],
            vec!["a", "a"],
            vec!["a"; 9],
        ] {
            assert!(
                prepare(&mut owner, &paths, CancellationToken::new()).is_err(),
                "{paths:?}"
            );
        }
        assert_eq!(fs::read(f.0.join("a")).unwrap(), b"canary");
        assert_eq!(
            fs::read(f.0.join(".festerm-cache/held")).unwrap(),
            b"private"
        );
    }
    #[test]
    fn files_delete_local_stale_id_replacement_nonempty_and_root_move_refuse() {
        let f = Fixture::new();
        fs::write(f.0.join("a"), b"original").unwrap();
        fs::create_dir(f.0.join("empty")).unwrap();
        let mut owner = f.owner();
        let first = prepare(&mut owner, &["a"], CancellationToken::new()).unwrap();
        let second = prepare(&mut owner, &["empty"], CancellationToken::new()).unwrap();
        assert!(commit(&mut owner, first.id).is_err());
        assert!(commit(&mut owner, second.id).is_err());
        let review = prepare(&mut owner, &["a"], CancellationToken::new()).unwrap();
        fs::rename(f.0.join("a"), f.0.join("kept")).unwrap();
        fs::write(f.0.join("a"), b"replacement").unwrap();
        assert!(commit(&mut owner, review.id).unwrap().failed);
        assert_eq!(fs::read(f.0.join("kept")).unwrap(), b"original");
        assert_eq!(fs::read(f.0.join("a")).unwrap(), b"replacement");
        let review = prepare(&mut owner, &["empty"], CancellationToken::new()).unwrap();
        fs::write(f.0.join("empty/child"), b"child").unwrap();
        assert!(commit(&mut owner, review.id).unwrap().failed);
        assert_eq!(fs::read(f.0.join("empty/child")).unwrap(), b"child");
        let review = prepare(&mut owner, &["a"], CancellationToken::new()).unwrap();
        let moved = f.0.with_extension("moved");
        fs::rename(&f.0, &moved).unwrap();
        fs::create_dir(&f.0).unwrap();
        assert!(commit(&mut owner, review.id).unwrap().failed);
        assert_eq!(fs::read(moved.join("a")).unwrap(), b"replacement");
        fs::remove_dir_all(moved).unwrap();
    }
}
