use std::{
    collections::VecDeque,
    mem::size_of,
    sync::{Arc, Mutex},
};

use crate::sftp_transfer::{
    path_memory_proxy_bytes, SftpDirectoryItem, SftpPath, TransferPlanningLimit,
    TransferPlanningLimits, TransferWorkError, MAX_KEEP_BOTH_SUFFIX_BYTES,
    TRANSFER_PLAN_ITEM_OVERHEAD_BYTES,
};

#[derive(Clone)]
pub(crate) struct SharedPlanningBudget(Arc<Mutex<PlanningUsage>>);

struct PlanningUsage {
    limits: TransferPlanningLimits,
    items: usize,
    bytes: usize,
}

impl SharedPlanningBudget {
    pub(crate) fn new(limits: TransferPlanningLimits) -> Self {
        Self(Arc::new(Mutex::new(PlanningUsage {
            limits,
            items: 0,
            bytes: 0,
        })))
    }

    pub(crate) fn reserve(
        &self,
        items: usize,
        bytes: usize,
    ) -> Result<PlanningReservation, TransferWorkError> {
        let mut reservation = PlanningReservation {
            budget: self.clone(),
            items: 0,
            bytes: 0,
        };
        reservation.grow(items, bytes)?;
        Ok(reservation)
    }

    pub(crate) fn unit(
        &self,
        source: &SftpPath,
        destination: &SftpPath,
    ) -> Result<PlanningReservation, TransferWorkError> {
        self.reserve(
            1,
            TRANSFER_PLAN_ITEM_OVERHEAD_BYTES
                .saturating_add(path_memory_proxy_bytes(source).saturating_mul(2))
                .saturating_add(path_memory_proxy_bytes(destination).saturating_mul(6))
                .saturating_add(MAX_KEEP_BOTH_SUFFIX_BYTES.saturating_mul(2)),
        )
    }

    pub(crate) fn entry(
        &self,
        name_bytes: usize,
        path_bytes: usize,
    ) -> Result<PlanningReservation, TransferWorkError> {
        self.reserve(
            1,
            TRANSFER_PLAN_ITEM_OVERHEAD_BYTES
                .saturating_add(name_bytes)
                .saturating_add(path_bytes),
        )
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> (usize, usize) {
        let usage = self.0.lock().expect("SFTP planning budget is not poisoned");
        (usage.items, usage.bytes)
    }
}

pub(crate) struct PlanningReservation {
    budget: SharedPlanningBudget,
    items: usize,
    bytes: usize,
}

impl PlanningReservation {
    pub(crate) fn grow(&mut self, items: usize, bytes: usize) -> Result<(), TransferWorkError> {
        let mut usage = self
            .budget
            .0
            .lock()
            .expect("SFTP planning budget is not poisoned");
        let observed_items = usage.items.checked_add(items);
        if observed_items.is_none_or(|observed| observed > usage.limits.max_items) {
            return Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::Items,
                observed: observed_items.unwrap_or(usize::MAX),
                maximum: usage.limits.max_items,
            });
        }
        let observed_bytes = usage.bytes.checked_add(bytes);
        if observed_bytes.is_none_or(|observed| observed > usage.limits.max_memory_proxy_bytes) {
            return Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::MemoryProxyBytes,
                observed: observed_bytes.unwrap_or(usize::MAX),
                maximum: usage.limits.max_memory_proxy_bytes,
            });
        }
        usage.items = observed_items.expect("planning item growth is representable");
        usage.bytes = observed_bytes.expect("planning byte growth is representable");
        self.items += items;
        self.bytes += bytes;
        Ok(())
    }

    fn shrink_bytes(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.budget
            .0
            .lock()
            .expect("SFTP planning budget is not poisoned")
            .bytes -= self.bytes - bytes;
        self.bytes = bytes;
    }
}

impl Drop for PlanningReservation {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .0
            .lock()
            .expect("SFTP planning budget is not poisoned");
        usage.items -= self.items;
        usage.bytes -= self.bytes;
    }
}

pub(crate) struct Budgeted<T> {
    pub(crate) value: T,
    pub(crate) reservation: PlanningReservation,
}

pub(crate) struct PlanningQueue<T> {
    values: VecDeque<Budgeted<T>>,
    storage: PlanningReservation,
}

impl<T> PlanningQueue<T> {
    pub(crate) fn new(budget: &SharedPlanningBudget) -> Self {
        Self {
            values: VecDeque::new(),
            storage: budget
                .reserve(0, 0)
                .expect("empty SFTP planning reservation cannot exceed limits"),
        }
    }

    fn ensure_capacity(&mut self) -> Result<(), TransferWorkError> {
        if self.values.len() < self.values.capacity() {
            return Ok(());
        }
        let capacity = self.values.capacity().saturating_mul(2).max(4);
        let mut replacement = self
            .storage
            .budget
            .reserve(0, capacity.saturating_mul(size_of::<Budgeted<T>>()))?;
        let mut replacement_values = VecDeque::with_capacity(capacity);
        let actual_bytes = replacement_values
            .capacity()
            .saturating_mul(size_of::<Budgeted<T>>());
        if actual_bytes > replacement.bytes {
            replacement.grow(0, actual_bytes - replacement.bytes)?;
        }
        replacement_values.append(&mut self.values);
        let old_values = std::mem::replace(&mut self.values, replacement_values);
        let old_storage = std::mem::replace(&mut self.storage, replacement);
        drop(old_values);
        drop(old_storage);
        Ok(())
    }

    pub(crate) fn push_back(&mut self, value: Budgeted<T>) -> Result<(), TransferWorkError> {
        self.ensure_capacity()?;
        self.values.push_back(value);
        Ok(())
    }

    pub(crate) fn push_front(&mut self, value: Budgeted<T>) -> Result<(), TransferWorkError> {
        self.ensure_capacity()?;
        self.values.push_front(value);
        Ok(())
    }

    pub(crate) fn reserve_requeue_slot(&mut self) -> Result<(), TransferWorkError> {
        self.ensure_capacity()
    }

    pub(crate) fn pop_front(&mut self) -> Option<Budgeted<T>> {
        let result = self.values.pop_front();
        self.retire_sparse_storage();
        result
    }

    pub(crate) fn pop_back(&mut self) -> Option<Budgeted<T>> {
        let result = self.values.pop_back();
        self.retire_sparse_storage();
        result
    }

    fn retire_sparse_storage(&mut self) {
        const COPY_LIMIT: usize = 128;
        if self.values.len() > COPY_LIMIT || self.values.capacity() <= 4 * COPY_LIMIT {
            return;
        }
        let mut retained: [Option<Budgeted<T>>; COPY_LIMIT] = std::array::from_fn(|_| None);
        let mut old = std::mem::take(&mut self.values);
        let length = old.len();
        for (slot, value) in retained.iter_mut().zip(old.drain(..)) {
            *slot = Some(value);
        }
        drop(old);
        // The old allowance covers rebuilding after its allocation has retired.
        self.values = VecDeque::with_capacity(length.saturating_add(1).max(4));
        for value in retained.into_iter().take(length) {
            self.values
                .push_back(value.expect("retained SFTP queue entry exists"));
        }
        self.storage
            .shrink_bytes(self.values.capacity() * size_of::<Budgeted<T>>());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn entries_mut(&mut self) -> impl Iterator<Item = &mut Budgeted<T>> {
        self.values.iter_mut()
    }
}

impl PlanningQueue<SftpDirectoryItem> {
    pub(crate) fn sort_by_name(&mut self) {
        self.values
            .make_contiguous()
            .sort_unstable_by(|left, right| left.value.name.cmp(&right.value.name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn test_budget(items: usize, bytes: usize) -> SharedPlanningBudget {
        SharedPlanningBudget::new(TransferPlanningLimits {
            max_items: items,
            max_memory_proxy_bytes: bytes,
        })
    }

    #[test]
    fn shared_planning_admission_is_atomic_and_restores_exact_credits() {
        let budget = test_budget(2, 1_000);
        let first = budget.reserve(1, 250).unwrap();
        let second = budget.reserve(1, 750).unwrap();
        assert!(matches!(
            budget.reserve(1, 0),
            Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::Items,
                observed: 3,
                maximum: 2,
            })
        ));
        assert!(matches!(
            budget.reserve(0, 1),
            Err(TransferWorkError::PlanningLimitExceeded {
                limit: TransferPlanningLimit::MemoryProxyBytes,
                observed: 1_001,
                maximum: 1_000,
            })
        ));
        assert_eq!(budget.usage(), (2, 1_000));
        drop(first);
        assert_eq!(budget.usage(), (1, 750));
        drop(second);
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn failed_and_overflowing_growth_never_changes_shared_accounting() {
        for (items, bytes, growth_items, growth_bytes) in
            [(usize::MAX, 0, 1, 0), (0, usize::MAX, 1, 1)]
        {
            let budget = test_budget(usize::MAX, usize::MAX);
            let mut reservation = budget.reserve(items, bytes).unwrap();
            assert!(reservation.grow(growth_items, growth_bytes).is_err());
            assert_eq!(budget.usage(), (items, bytes));
            drop(reservation);
            assert_eq!(budget.usage(), (0, 0));
        }
    }

    struct DropProbe {
        budget: SharedPlanningBudget,
        observed: Arc<AtomicBool>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            assert_eq!(self.budget.usage().0, 1);
            self.observed.store(true, Ordering::Release);
        }
    }

    #[test]
    fn planning_data_drops_before_its_credit_and_outlives_a_removed_queue() {
        let budget = test_budget(1, 8_192);
        let observed = Arc::new(AtomicBool::new(false));
        let mut queue = PlanningQueue::new(&budget);
        queue
            .push_back(Budgeted {
                value: DropProbe {
                    budget: budget.clone(),
                    observed: Arc::clone(&observed),
                },
                reservation: budget.reserve(1, 32).unwrap(),
            })
            .unwrap();
        let value = queue.pop_front().unwrap();
        drop(queue);
        assert_eq!(budget.usage(), (1, 32));
        assert!(budget.reserve(1, 1).is_err());
        drop(value);
        assert!(observed.load(Ordering::Acquire));
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn planning_queue_refuses_storage_before_allocation_and_preserves_admitted_rows() {
        let slot_bytes = size_of::<Budgeted<u64>>();
        let budget = test_budget(10, 4 * slot_bytes + 6);
        let mut queue = PlanningQueue::new(&budget);
        for value in 0..4 {
            queue
                .push_back(Budgeted {
                    value,
                    reservation: budget.reserve(1, 1).unwrap(),
                })
                .unwrap();
        }
        let capacity = queue.values.capacity();
        assert!(queue
            .push_back(Budgeted {
                value: 4,
                reservation: budget.reserve(1, 1).unwrap(),
            })
            .is_err());
        assert_eq!(queue.values.capacity(), capacity);
        assert_eq!(budget.usage(), (4, 4 * slot_bytes + 4));
        for value in 0..4 {
            assert_eq!(queue.pop_front().unwrap().value, value);
        }
        drop(queue);
        assert_eq!(budget.usage(), (0, 0));

        let budget = test_budget(1, 1);
        let mut queue = PlanningQueue::new(&budget);
        assert!(queue
            .push_back(Budgeted {
                value: 0_u64,
                reservation: budget.reserve(1, 1).unwrap(),
            })
            .is_err());
        assert_eq!(queue.values.capacity(), 0);
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn planning_queue_replacement_charges_actual_capacity_before_retiring_old_storage() {
        let budget = test_budget(10, usize::MAX);
        let mut queue = PlanningQueue::new(&budget);
        for value in 0..5 {
            queue
                .push_back(Budgeted {
                    value,
                    reservation: budget.reserve(1, 1).unwrap(),
                })
                .unwrap();
        }
        assert_eq!(
            budget.usage(),
            (5, 5 + queue.values.capacity() * size_of::<Budgeted<u64>>())
        );
        drop(queue);
        assert_eq!(budget.usage(), (0, 0));
    }

    #[test]
    fn sparse_planning_queue_retires_backing_and_keeps_a_collision_requeue_slot() {
        let budget = test_budget(4_096, 4 * 1024 * 1024);
        let mut queue = PlanningQueue::new(&budget);
        for value in 0..4_096_u64 {
            queue
                .push_back(Budgeted {
                    value,
                    reservation: budget.reserve(1, 8).unwrap(),
                })
                .unwrap();
        }
        for _ in 0..(4_096 - 129) {
            drop(queue.pop_front().unwrap());
        }
        let current = queue.pop_front().unwrap();
        assert_eq!(queue.values.len(), 128);
        assert!(queue.values.capacity() <= 256);
        assert!(queue.values.capacity() > queue.values.len());
        assert_eq!(
            budget.usage(),
            (
                129,
                129 * 8 + queue.values.capacity() * size_of::<Budgeted<u64>>()
            )
        );
        queue.push_front(current).unwrap();
        assert_eq!(queue.values.len(), 129);
        drop(queue);
        assert_eq!(budget.usage(), (0, 0));
    }
}
