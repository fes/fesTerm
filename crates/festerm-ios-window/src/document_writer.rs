//! Admission only, not filesystem authority. Native saves exclude app-owned
//! local mutations, including transfer workers that outlive their Files view.

use crate::DocumentFileError;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, LazyLock,
};

const SAVING: usize = usize::MAX;
static WRITERS: LazyLock<Arc<Writers>> = LazyLock::new(|| Arc::new(Writers::default()));

#[derive(Default)]
struct Writers {
    admitted: AtomicUsize,
    epoch: AtomicU64,
}

/// Keeps admission until actual I/O completion, not until the UI closes.
pub struct DocumentWritePermit {
    writers: Arc<Writers>,
    exclusive: bool,
    repaint: Arc<dyn Fn() + Send + Sync>,
}

impl Writers {
    fn acquire(
        self: &Arc<Self>,
        exclusive: bool,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<DocumentWritePermit, DocumentFileError> {
        loop {
            let count = self.admitted.load(Ordering::Acquire);
            let next = if exclusive {
                (count == 0).then_some(SAVING)
            } else {
                count.checked_add(1).filter(|next| *next < SAVING)
            }
            .ok_or(DocumentFileError::Busy)?;
            if self
                .admitted
                .compare_exchange(count, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        Ok(DocumentWritePermit {
            writers: self.clone(),
            exclusive,
            repaint,
        })
    }
}

/// Admits an app-owned local mutation; does not grant access to any path.
pub fn try_local_file_mutation(
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> Result<DocumentWritePermit, DocumentFileError> {
    WRITERS.acquire(false, repaint)
}

#[cfg(target_os = "ios")]
pub(crate) fn try_document_save(
    repaint: Arc<dyn Fn() + Send + Sync>,
) -> Result<DocumentWritePermit, DocumentFileError> {
    WRITERS.acquire(true, repaint)
}

/// Changes after admitted I/O settles. Existing authorized sources can refresh
/// after transfer/rename/delete, including when their Files view was closed.
pub fn local_file_mutation_epoch() -> u64 {
    WRITERS.epoch.load(Ordering::Acquire)
}

impl Drop for DocumentWritePermit {
    fn drop(&mut self) {
        self.writers.epoch.fetch_add(1, Ordering::AcqRel);
        if self.exclusive {
            self.writers.admitted.store(0, Ordering::Release);
        } else {
            self.writers.admitted.fetch_sub(1, Ordering::AcqRel);
        }
        (self.repaint)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_save_excludes_mutations_until_completion() {
        let writers = Arc::new(Writers::default());
        let save = writers.acquire(true, Arc::new(|| {})).unwrap();
        assert!(matches!(
            writers.acquire(false, Arc::new(|| {})),
            Err(DocumentFileError::Busy)
        ));
        assert!(writers.acquire(true, Arc::new(|| {})).is_err());
        drop(save);
        assert_eq!(writers.epoch.load(Ordering::Acquire), 1);
        assert!(writers.acquire(false, Arc::new(|| {})).is_ok());
    }

    #[test]
    fn all_detached_transfer_leases_must_finish_before_save() {
        let writers = Arc::new(Writers::default());
        let first = writers.acquire(false, Arc::new(|| {})).unwrap();
        let second = writers.acquire(false, Arc::new(|| {})).unwrap();
        drop(first);
        assert!(writers.acquire(true, Arc::new(|| {})).is_err());
        std::thread::spawn(move || drop(second)).join().unwrap();
        assert!(writers.acquire(true, Arc::new(|| {})).is_ok());
    }
}
