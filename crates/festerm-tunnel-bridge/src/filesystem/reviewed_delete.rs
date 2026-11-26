//! One-shot reviewed deletion within the rooted engine. Reserved captures and
//! trusted parents are the naming boundary; Unix unlink is not identity-atomic
//! against arbitrary same-user OS writers between verification and the syscall.
use super::*;

pub(super) struct ReviewedEntry {
    pub(super) file: File,
    parent: Dir,
    path: String,
    name: String,
    directory: bool,
    stamp: Stamp,
    new_shape: bool,
}
fn open(parent: &Dir, name: &str, directory: bool, new_shape: bool) -> io::Result<File> {
    if directory {
        #[cfg(windows)]
        let file = festerm_windows_security::open_directory_no_reparse(
            &parent.try_clone()?.into_std_file(),
            Path::new(name),
        )?;
        #[cfg(unix)]
        let file = parent.open_dir_nofollow(name)?.into_std_file();
        trusted(&Dir::from_std_file(file.try_clone()?))?;
        Ok(file)
    } else {
        open_mutation_file(parent, name, new_shape)
    }
}
fn empty(file: &File) -> Outcome<()> {
    match Dir::from_std_file(file.try_clone()?).entries()?.next() {
        None => Ok(()),
        Some(Ok(_)) => Err(ErrorCode::NotEmpty.into()),
        Some(Err(error)) => Err(error.into()),
    }
}
pub(super) fn review(filesystem: &Filesystem, path: &str) -> Outcome<ReviewedEntry> {
    let (parent, name) = filesystem.parent(path)?;
    trusted(&parent)?;
    let info = parent.symlink_metadata(&name)?;
    if info.file_type().is_symlink() || !(info.is_file() || info.is_dir()) {
        return Err(ErrorCode::Invalid.into());
    }
    let directory = info.is_dir();
    let file = open(&parent, &name, directory, filesystem.has_locations())?;
    if directory && filesystem.has_locations() {
        mutation_trusted(&Dir::from_std_file(file.try_clone()?))?;
    }
    filesystem.check_mutation_parent(path, &parent)?;
    if directory {
        empty(&file)?;
    }
    let stamp = stamp_kind(&file, directory)?;
    Ok(ReviewedEntry {
        file,
        parent,
        path: path.into(),
        name,
        directory,
        stamp,
        new_shape: filesystem.has_locations(),
    })
}
fn move_handle(entry: &ReviewedEntry) -> io::Result<File> {
    #[cfg(unix)]
    return entry.file.try_clone();
    #[cfg(windows)]
    {
        let parent = entry.parent.try_clone()?.into_std_file();
        if entry.directory {
            festerm_windows_security::open_reviewed_directory_for_move(
                &parent,
                Path::new(&entry.name),
            )
        } else {
            festerm_windows_security::open_file_no_reparse_for_move(&parent, Path::new(&entry.name))
        }
    }
}
fn move_name(
    parent: &Dir,
    source: &str,
    mover: &File,
    target: &str,
    directory: bool,
) -> io::Result<()> {
    let parent = parent.try_clone()?.into_std_file();
    #[cfg(unix)]
    {
        let _ = (mover, directory);
        festerm_unix_security::rename_noreplace(
            &parent,
            Path::new(source),
            &parent,
            Path::new(target),
        )
    }
    #[cfg(windows)]
    {
        let _ = source;
        if directory {
            festerm_windows_security::rename_reviewed_directory_noreplace(
                mover,
                &parent,
                Path::new(target),
            )
        } else {
            festerm_windows_security::rename_file_noreplace(mover, &parent, Path::new(target))
        }
    }
}
fn recovery(entry: &ReviewedEntry, capture: &str) -> Failure {
    let parent = entry.path.rsplit_once('/').map_or("", |(parent, _)| parent);
    Failure {
        code: ErrorCode::RecoveryRequired,
        recovery: Some(format!("{parent}/{capture}")),
    }
}
fn current_capture(entry: &ReviewedEntry, capture: &str, captured: &File) -> io::Result<File> {
    let current = open(&entry.parent, capture, entry.directory, entry.new_shape)?;
    if !same(&current, captured)? {
        return Err(invalid());
    }
    Ok(current)
}
fn restore(
    entry: &ReviewedEntry,
    capture: &str,
    captured: &File,
    mover: &File,
    filesystem: &Filesystem,
) -> Outcome<()> {
    // Do not move a substituted capture or replace a concurrent public entry.
    trusted(&entry.parent).map_err(|_| recovery(entry, capture))?;
    if filesystem.has_locations() {
        mutation_trusted(&entry.parent).map_err(|_| recovery(entry, capture))?;
    }
    current_capture(entry, capture, captured).map_err(|_| recovery(entry, capture))?;
    move_name(&entry.parent, capture, mover, &entry.name, entry.directory)
        .map_err(|_| recovery(entry, capture))
}
fn remove_capture(entry: &ReviewedEntry, capture: &str, mover: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = mover;
        if entry.directory {
            entry.parent.remove_dir(capture)
        } else {
            entry.parent.remove_file(capture)
        }
    }
    #[cfg(windows)]
    {
        let _ = capture;
        if entry.directory {
            festerm_windows_security::remove_reviewed_empty_directory(mover)
        } else {
            festerm_windows_security::remove_reviewed_file(mover)
        }
    }
}
pub(super) fn remove(
    entry: ReviewedEntry,
    admission: &Admission,
    cancel: &CancellationToken,
    filesystem: &Filesystem,
) -> Outcome<()> {
    if cancel.is_cancelled() {
        return Err(ErrorCode::Denied.into());
    }
    admission(AdmissionStage::Publication)?;
    trusted(&entry.parent)?;
    filesystem.check_mutation_parent(&entry.path, &entry.parent)?;
    let current = open(&entry.parent, &entry.name, entry.directory, entry.new_shape)?;
    if !same(&current, &entry.file)? || stamp_kind(&current, entry.directory)? != entry.stamp {
        return Err(ErrorCode::Conflict.into());
    }
    let mover = move_handle(&entry)?;
    if !same(&mover, &entry.file)? || stamp_kind(&mover, entry.directory)? != entry.stamp {
        return Err(ErrorCode::Conflict.into());
    }
    let capture = format!("{RESERVED}delete-{}", uuid::Uuid::new_v4().simple());
    let parent_length = entry
        .path
        .rsplit_once('/')
        .map_or(0, |(parent, _)| parent.len());
    if parent_length + 1 + capture.len() > 4096 {
        // A capture must always have a representable bounded recovery locator.
        return Err(ErrorCode::Invalid.into());
    }
    filesystem.check_mutation_parent(&entry.path, &entry.parent)?;
    if entry.directory && filesystem.has_locations() {
        mutation_trusted(&Dir::from_std_file(entry.file.try_clone()?))?;
    }
    move_name(
        &entry.parent,
        &entry.name,
        &mover,
        &capture,
        entry.directory,
    )?;
    // From capture onward the original public pathname is never unlinked or
    // followed. Only no-replace restoration may address that name.
    let captured = open(&entry.parent, &capture, entry.directory, entry.new_shape)
        .map_err(|_| recovery(&entry, &capture))?;
    let captured_stamp =
        stamp_kind(&captured, entry.directory).map_err(|_| recovery(&entry, &capture))?;
    let result = (|| -> Outcome<()> {
        // Rename changes ctime/change-time, but not object identity/length/mtime.
        if !same(&captured, &entry.file)?
            || captured_stamp.length != entry.stamp.length
            || captured_stamp.modified != entry.stamp.modified
        {
            return Err(ErrorCode::Conflict.into());
        }
        admission(AdmissionStage::Publication)?;
        if cancel.is_cancelled() {
            return Err(ErrorCode::Denied.into());
        }
        trusted(&entry.parent)?;
        let current =
            current_capture(&entry, &capture, &captured).map_err(|_| recovery(&entry, &capture))?;
        if entry.directory {
            empty(&current)?;
        }
        if stamp_kind(&current, entry.directory)? != captured_stamp {
            return Err(ErrorCode::Conflict.into());
        }
        // Windows removes by exact retained handle. Unix uses the last verified
        // service-owned capture under the trusted parent, not a public path.
        filesystem.check_mutation_parent(&entry.path, &entry.parent)?;
        if entry.directory && filesystem.has_locations() {
            mutation_trusted(&Dir::from_std_file(current.try_clone()?))?;
        }
        remove_capture(&entry, &capture, &mover).map_err(|error| {
            if entry.directory && error.kind() == io::ErrorKind::DirectoryNotEmpty {
                Failure::from(ErrorCode::NotEmpty)
            } else {
                Failure::from(error)
            }
        })?;
        Ok(())
    })();
    if let Err(error) = result {
        restore(&entry, &capture, &captured, &mover, filesystem)?;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    fn session(delete: bool, reviewed: bool) -> (PathBuf, Session) {
        let root = crate::test_support::private_directory("festerm-reviewed-delete");
        let filesystem = Filesystem::open(&rpc::Policy {
            start_in: None,
            locations: None,
            root: root.clone(),
            write: true,
            delete,
        })
        .unwrap();
        let permit = Arc::new(filesystem.slots.clone().try_acquire_owned().unwrap());
        (
            root,
            Session {
                reviewed_operations: reviewed,
                admission: Arc::new(|_| Ok(())),
                filesystem,
                handles: HashMap::new(),
                next: 0,
                _permit: permit,
            },
        )
    }
    fn review(session: &mut Session, path: &str) -> Outcome<u32> {
        let (reply, _) = session.execute(
            Operation::ReviewEntry {
                path: path.into(),
                purpose: rpc::ReviewPurpose::Delete,
            },
            Vec::new(),
            &CancellationToken::new(),
        )?;
        let Reply::EntryReview { handle, .. } = reply else {
            panic!("expected exact review")
        };
        Ok(handle)
    }
    fn delete(session: &mut Session, review: u32, cancel: &CancellationToken) -> Outcome<()> {
        let (reply, _) = session.execute(Operation::Delete { review }, Vec::new(), cancel)?;
        assert!(matches!(reply, Reply::Deleted));
        Ok(())
    }
    fn captures(root: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(root)
            .unwrap()
            .map(|x| x.unwrap().path())
            .filter(|x| {
                x.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".festerm-transfer-delete-")
            })
            .collect()
    }

    #[test]
    fn reviewed_delete_removes_files_and_empty_directories_once_without_following_links() {
        let (root, mut session) = session(true, true);
        std::fs::write(root.join("file"), b"owned bytes").unwrap();
        std::fs::create_dir(root.join("empty")).unwrap();
        std::fs::create_dir(root.join("nonempty")).unwrap();
        std::fs::write(root.join("nonempty/child"), b"retained child").unwrap();
        for path in ["/file", "/empty"] {
            let handle = review(&mut session, path).unwrap();
            delete(&mut session, handle, &CancellationToken::new()).unwrap();
            assert!(!root.join(&path[1..]).exists());
            assert_eq!(
                delete(&mut session, handle, &CancellationToken::new())
                    .unwrap_err()
                    .code,
                ErrorCode::Invalid
            );
        }
        assert_eq!(
            review(&mut session, "/nonempty").unwrap_err().code,
            ErrorCode::NotEmpty
        );
        assert_eq!(
            std::fs::read(root.join("nonempty/child")).unwrap(),
            b"retained child"
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("nonempty", root.join("link")).unwrap();
            assert_eq!(
                review(&mut session, "/link").unwrap_err().code,
                ErrorCode::Invalid
            );
            assert!(root
                .join("link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink());
        }
        assert!(captures(&root).is_empty());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reviewed_delete_requires_separate_consent_and_open3_before_any_review() {
        for (grant, open3) in [(false, true), (true, false)] {
            let (root, mut session) = session(grant, open3);
            std::fs::write(root.join("file"), b"unchanged").unwrap();
            assert_eq!(
                review(&mut session, "/file").unwrap_err().code,
                ErrorCode::Denied
            );
            assert_eq!(
                delete(&mut session, 1, &CancellationToken::new())
                    .unwrap_err()
                    .code,
                ErrorCode::Denied
            );
            assert!(session.handles.is_empty());
            assert_eq!(std::fs::read(root.join("file")).unwrap(), b"unchanged");
            assert!(captures(&root).is_empty());
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn reviewed_delete_admission_bounds_handles_without_releasing_other_stream_charge() {
        let (root, mut session) = session(true, true);
        let other_permit = session
            .filesystem
            .slots
            .clone()
            .try_acquire_owned()
            .unwrap();
        let mut handles = Vec::new();
        for index in 0..rpc::HANDLE_LIMIT {
            std::fs::write(root.join(format!("file{index}")), b"kept").unwrap();
            handles.push(review(&mut session, &format!("/file{index}")).unwrap());
        }
        // Busy wins before even opening a missing pathname, without touching the
        // other stream's retained admission charge.
        assert_eq!(
            review(&mut session, "/missing").unwrap_err().code,
            ErrorCode::Busy
        );
        assert_eq!(session.filesystem.slots.available_permits(), 0);
        for handle in handles {
            session
                .execute(
                    Operation::Close { handle },
                    Vec::new(),
                    &CancellationToken::new(),
                )
                .unwrap();
        }
        assert!(session.handles.is_empty());
        assert_eq!(
            review(&mut session, "/missing").unwrap_err().code,
            ErrorCode::NotFound
        );
        assert_eq!(std::fs::read(root.join("file0")).unwrap(), b"kept");
        drop(other_permit);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reviewed_delete_detects_public_name_change_before_capture_without_touching_foreign_entry() {
        let (root, mut session) = session(true, true);
        std::fs::write(root.join("file"), b"reviewed").unwrap();
        let handle = review(&mut session, "/file").unwrap();
        let changed = root.clone();
        session.admission = Arc::new(move |_| {
            std::fs::rename(changed.join("file"), changed.join("original"))?;
            std::fs::write(changed.join("file"), b"foreign")
        });
        assert_eq!(
            delete(&mut session, handle, &CancellationToken::new())
                .unwrap_err()
                .code,
            ErrorCode::Conflict
        );
        assert_eq!(std::fs::read(root.join("file")).unwrap(), b"foreign");
        assert_eq!(std::fs::read(root.join("original")).unwrap(), b"reviewed");
        assert!(captures(&root).is_empty());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reviewed_delete_capture_substitution_and_restore_collision_preserve_foreign_names() {
        for substitute_capture in [false, true] {
            let (root, mut session) = session(true, true);
            std::fs::write(root.join("file"), b"reviewed").unwrap();
            let handle = review(&mut session, "/file").unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let stage = calls.clone();
            let changed = root.clone();
            let cancel = CancellationToken::new();
            let stop = cancel.clone();
            session.admission = Arc::new(move |_| {
                if stage.fetch_add(1, Ordering::SeqCst) == 1 {
                    if substitute_capture {
                        let capture = captures(&changed).pop().unwrap();
                        std::fs::rename(&capture, changed.join("original"))?;
                        std::fs::write(capture, b"foreign capture")?;
                    } else {
                        std::fs::write(changed.join("file"), b"foreign public")?;
                        stop.cancel();
                    }
                }
                Ok(())
            });
            let failure = delete(&mut session, handle, &cancel).unwrap_err();
            assert_eq!(failure.code, ErrorCode::RecoveryRequired);
            let virtual_path = failure.recovery.unwrap();
            assert!(virtual_path.starts_with("/.festerm-transfer-delete-"));
            assert!(!virtual_path.contains(root.to_str().unwrap()));
            let capture = root.join(&virtual_path[1..]);
            if substitute_capture {
                assert_eq!(std::fs::read(&capture).unwrap(), b"foreign capture");
                assert_eq!(std::fs::read(root.join("original")).unwrap(), b"reviewed");
                assert!(!root.join("file").exists());
            } else {
                assert_eq!(std::fs::read(&capture).unwrap(), b"reviewed");
                assert_eq!(std::fs::read(root.join("file")).unwrap(), b"foreign public");
            }
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn reviewed_delete_new_directory_child_after_capture_refuses_and_restores_exact_folder() {
        let (root, mut session) = session(true, true);
        std::fs::create_dir(root.join("folder")).unwrap();
        let handle = review(&mut session, "/folder").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let stage = count.clone();
        let changed = root.clone();
        session.admission = Arc::new(move |_| {
            if stage.fetch_add(1, Ordering::SeqCst) == 1 {
                let capture = captures(&changed).pop().unwrap();
                std::fs::write(capture.join("child"), b"must survive")?;
            }
            Ok(())
        });
        assert_eq!(
            delete(&mut session, handle, &CancellationToken::new())
                .unwrap_err()
                .code,
            ErrorCode::NotEmpty
        );
        assert_eq!(
            std::fs::read(root.join("folder/child")).unwrap(),
            b"must survive"
        );
        assert!(captures(&root).is_empty());
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn reviewed_delete_rechecks_parent_trust_without_repairing_changed_permissions() {
        use std::os::unix::fs::PermissionsExt;
        for after_capture in [false, true] {
            let (root, mut session) = session(true, true);
            std::fs::write(root.join("file"), b"preserved").unwrap();
            let review = review(&mut session, "/file").unwrap();
            let changed = root.clone();
            let calls = AtomicUsize::new(0);
            session.admission = Arc::new(move |_| {
                if calls.fetch_add(1, Ordering::SeqCst) == usize::from(after_capture) {
                    std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0o777))?;
                }
                Ok(())
            });
            let failure = delete(&mut session, review, &CancellationToken::new()).unwrap_err();
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o777
            );
            if after_capture {
                assert_eq!(failure.code, ErrorCode::RecoveryRequired);
                assert!(!root.join("file").exists());
                assert_eq!(
                    std::fs::read(captures(&root).pop().unwrap()).unwrap(),
                    b"preserved"
                );
            } else {
                assert_eq!(failure.code, ErrorCode::Denied);
                assert_eq!(std::fs::read(root.join("file")).unwrap(), b"preserved");
                assert!(captures(&root).is_empty());
            }
            // Test-owned synthetic directory only; production performed no repair.
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            drop(session);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
