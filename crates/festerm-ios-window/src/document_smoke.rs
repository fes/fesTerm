//! Opt-in Simulator qualification against a fresh app-owned fixture, never a
//! picked file. Compiled out of normal and physical-device builds.

use super::*;
use std::os::unix::fs::PermissionsExt;

const INITIAL: &[u8] = b"\xef\xbb\xbf# Native fixture\r\ninitial\r\n";
const EDITED: &[u8] = b"\xef\xbb\xbf# Native fixture\r\nsaved \xc3\xa9\r\n";
const EXTERNAL: &[u8] = b"external replacement\n";

#[derive(Default)]
pub struct DocumentSmoke {
    case: Option<Case>,
    finished: bool,
}

struct Case {
    document: DocumentFile,
    path: PathBuf,
    phase: u8,
    initial_version: [u8; 32],
}

impl DocumentSmoke {
    pub fn poll(&mut self, repaint: Arc<dyn Fn() + Send + Sync>) {
        if self.finished {
            return;
        }
        let result = if let Some(case) = self.case.as_mut() {
            case.poll()
        } else {
            Case::start(repaint).map(|case| {
                self.case = Some(case);
                false
            })
        };
        match result {
            Ok(false) => {}
            Ok(true) => {
                self.finished = true;
                let case = self.case.take().expect("completed native smoke");
                case.close();
            }
            Err(error) => {
                self.finished = true;
                eprintln!("festerm-document-smoke: FAIL: {}", error.message());
            }
        }
    }
}

impl Case {
    fn start(repaint: Repaint) -> Result<Self, DocumentFileError> {
        let mtm = MainThreadMarker::new().ok_or(DocumentFileError::Unavailable)?;
        let home = std::env::var_os("HOME").ok_or(DocumentFileError::Unavailable)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| DocumentFileError::Io)?
            .as_nanos();
        let directory = PathBuf::from(home)
            .join("Library/Caches")
            .join(format!("festerm-document-smoke-{nonce}"));
        std::fs::create_dir(&directory).map_err(|_| DocumentFileError::Io)?;
        let path = directory.join("fixture-\u{e9} space.txt");
        std::fs::write(&path, INITIAL).map_err(|_| DocumentFileError::Io)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| DocumentFileError::Io)?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(
            path.to_str().ok_or(DocumentFileError::Unavailable)?,
        ));
        drop(DocumentFile::selected(&url, repaint.clone(), mtm)?);
        let cancelled = DocumentFile::selected(&url, repaint.clone(), mtm)?;
        cancelled.open()?;
        drop(cancelled);
        let document = DocumentFile::selected(&url, repaint, mtm)?;
        document.open()?;
        eprintln!("festerm-document-smoke: started");
        Ok(Self {
            document,
            path,
            phase: 0,
            initial_version: [0; 32],
        })
    }

    fn poll(&mut self) -> Result<bool, DocumentFileError> {
        if self.phase == 4 {
            let (working, unavailable) = {
                let state = self
                    .document
                    .document
                    .ivars()
                    .state
                    .lock()
                    .expect("native document state");
                (state.working, state.unavailable)
            };
            if working {
                return Ok(false);
            }
            if unavailable != Some(DocumentFileError::TooLarge) {
                return Err(DocumentFileError::Io);
            }
            if self.document.refresh() != Err(DocumentFileError::Unavailable)
                || self.document.save(999, [0; 32], Vec::new())
                    != Err(DocumentFileError::Unavailable)
                || self.document.open() != Err(DocumentFileError::Unavailable)
            {
                return Err(DocumentFileError::Io);
            }
            std::fs::write(&self.path, EXTERNAL).map_err(|_| DocumentFileError::Io)?;
            let url = self.document.document.fileURL();
            let fresh = DocumentFile::selected(
                &url,
                self.document.document.ivars().repaint.clone(),
                MainThreadMarker::new().ok_or(DocumentFileError::Unavailable)?,
            )?;
            self.document = fresh;
            // Model a pending older provider failure independently of the
            // real failed-revert case, which UIKit closes before recovery.
            self.document
                .document
                .ivars()
                .state
                .lock()
                .expect("native document state")
                .unavailable = Some(DocumentFileError::TooLarge);
            self.document.open()?;
            self.phase += 1;
            return Ok(false);
        }
        let events = self.document.take_events();
        if matches!(self.phase, 5 | 6)
            && events
                .iter()
                .any(|event| matches!(event, DocumentFileEvent::Unavailable(_)))
        {
            return Err(DocumentFileError::Io);
        }
        for event in events {
            match (self.phase, event) {
                (0, DocumentFileEvent::Loaded(snapshot)) => {
                    if snapshot.bytes != INITIAL || !snapshot.writable {
                        return Err(DocumentFileError::Io);
                    }
                    self.initial_version = snapshot.version;
                    self.document.save(1, snapshot.version, EDITED.to_vec())?;
                    if crate::try_local_file_mutation(Arc::new(|| {})).is_ok() {
                        return Err(DocumentFileError::Io);
                    }
                    self.phase = 1;
                }
                (1, DocumentFileEvent::Saved { operation: 1, .. }) => {
                    self.verify_bytes(EDITED)?;
                    if std::fs::metadata(&self.path)
                        .map_err(|_| DocumentFileError::Io)?
                        .permissions()
                        .mode()
                        & 0o777
                        != 0o600
                    {
                        return Err(DocumentFileError::Io);
                    }
                    eprintln!("festerm-document-smoke: actual save and permissions PASS");
                    self.document
                        .save(2, self.initial_version, b"stale write".to_vec())?;
                    self.phase = 2;
                }
                (
                    2,
                    DocumentFileEvent::SaveFailed {
                        operation: 2,
                        error: DocumentFileError::Changed,
                    },
                ) => {
                    self.verify_bytes(EDITED)?;
                    eprintln!("festerm-document-smoke: stale overwrite refused PASS");
                    std::fs::write(&self.path, EXTERNAL).map_err(|_| DocumentFileError::Io)?;
                    self.document.refresh()?;
                    self.phase = 3;
                }
                (3, DocumentFileEvent::Loaded(snapshot)) => {
                    if snapshot.bytes != EXTERNAL {
                        return Err(DocumentFileError::Io);
                    }
                    eprintln!("festerm-document-smoke: external refresh PASS");
                    self.fail_next_refresh()?;
                    self.phase = 4;
                }
                (5, DocumentFileEvent::Loaded(snapshot)) => {
                    if snapshot.bytes != EXTERNAL {
                        return Err(DocumentFileError::Io);
                    }
                    eprintln!(
                        "festerm-document-smoke: fresh authority supersedes pending failure PASS"
                    );
                    self.document.save(3, snapshot.version, EDITED.to_vec())?;
                    self.phase = 6;
                }
                (6, DocumentFileEvent::Saved { operation: 3, .. }) => {
                    self.verify_bytes(EDITED)?;
                    eprintln!("festerm-document-smoke: reopened source saves cleanly PASS");
                    if LIVE_DOCUMENTS.load(Ordering::Acquire) != 1 {
                        return Err(DocumentFileError::Io);
                    }
                    eprintln!(
                        "festerm-document-smoke: unopened and in-flight drop release leases PASS"
                    );
                    return Ok(true);
                }
                (_, DocumentFileEvent::SaveFailed { error, .. })
                | (_, DocumentFileEvent::Unavailable(error)) => return Err(error),
                (_, DocumentFileEvent::Loaded(_)) => {}
                _ => return Err(DocumentFileError::Io),
            }
        }
        Ok(false)
    }

    fn fail_next_refresh(&self) -> Result<(), DocumentFileError> {
        std::fs::write(&self.path, vec![b'x'; MAX_DOCUMENT_BYTES + 1])
            .map_err(|_| DocumentFileError::Io)?;
        self.document.refresh()
    }

    fn verify_bytes(&self, expected: &[u8]) -> Result<(), DocumentFileError> {
        if std::fs::read(&self.path).map_err(|_| DocumentFileError::Io)? != expected {
            return Err(DocumentFileError::Io);
        }
        Ok(())
    }

    fn close(self) {
        self.document
            .document
            .ivars()
            .state
            .lock()
            .expect("native document state")
            .cancelled = true;
        let held = self.document.document.clone();
        let path = self.path;
        let completed = RcBlock::new(move |success: Bool| {
            let _ = &held;
            let removed = success.as_bool()
                && std::fs::remove_file(&path).is_ok()
                && path
                    .parent()
                    .is_some_and(|parent| std::fs::remove_dir(parent).is_ok());
            eprintln!(
                "festerm-document-smoke: {}",
                if removed {
                    "PASS"
                } else {
                    "FAIL: close/cleanup"
                }
            );
        });
        self.document
            .document
            .closeWithCompletionHandler(Some(&completed));
    }
}
