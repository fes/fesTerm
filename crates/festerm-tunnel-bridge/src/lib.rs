//! Experimental attach-only bridge. Network dependencies are explicitly opt-in.
#[cfg(feature = "relay")]
pub mod files;
#[cfg(feature = "relay")]
pub mod host_identity;
pub mod protocol;
#[cfg(feature = "relay")]
pub mod relay;
#[cfg(feature = "relay")]
pub mod setup_storage;
#[cfg(feature = "relay")]
pub mod transport;

#[cfg(feature = "relay")]
fn reserve_sequence(counter: &std::sync::atomic::AtomicU64) -> std::io::Result<u64> {
    use std::sync::atomic::Ordering;
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let next = current.checked_add(1).ok_or_else(protocol::invalid)?;
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(value) => return Ok(value),
            Err(actual) => current = actual,
        }
    }
}

#[cfg(all(test, feature = "relay"))]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicU64, Arc, Barrier};

    #[test]
    fn sequence_reservations_fail_closed_before_wrap_and_remain_unique() {
        let exhausted = AtomicU64::new(u64::MAX - 1);
        assert_eq!(reserve_sequence(&exhausted).unwrap(), u64::MAX - 1);
        assert!(reserve_sequence(&exhausted).is_err());
        assert!(reserve_sequence(&exhausted).is_err());
        let counter = Arc::new(AtomicU64::new(1));
        let start = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let counter = counter.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    (0..64)
                        .map(|_| reserve_sequence(&counter).unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut values: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        let count = values.len();
        values.sort_unstable();
        values.dedup();
        assert_eq!(values.len(), count);
    }
}
