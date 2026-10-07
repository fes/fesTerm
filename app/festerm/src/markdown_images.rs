use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
};

use eframe::egui;
use festerm_config::ImageMemoryBudgetPreference;

pub(crate) const MAX_CONCURRENT_IMAGE_LOADS: usize = 4;
pub(crate) const RETAINED_IMAGE_OVERHEAD: u64 = 4096;
const MAX_RETIREMENTS_PER_CHECK: usize = 128;

#[derive(Clone)]
pub(crate) struct ImageMemoryBudget(Arc<Mutex<BudgetState>>);

struct BudgetState {
    preference: ImageMemoryBudgetPreference,
    limit: u64,
    used: u64,
    workers: usize,
    generation: u64,
    retired: VecDeque<RetiredImage>,
    retirement_scan_remaining: usize,
}

struct RetiredImage {
    bytes: u64,
    pixels: Weak<egui::ColorImage>,
}

#[derive(Debug)]
pub(crate) enum ImageLoadFailure {
    Permanent(String),
    Budget {
        message: String,
        required_bytes: u64,
    },
    Busy,
}

impl ImageLoadFailure {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Permanent(message) | Self::Budget { message, .. } => message,
            Self::Busy => {
                "Four image loads are already pending or running. Try again when one finishes."
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum ImageRetry {
    PendingQueue,
    Workers,
    Memory(u64),
}

pub(crate) struct ImageReservation {
    budget: ImageMemoryBudget,
    bytes: u64,
    repaint: Option<egui::Context>,
    texture_pixels: Option<Weak<egui::ColorImage>>,
}

pub(crate) struct ImageWorker {
    budget: ImageMemoryBudget,
    repaint: egui::Context,
}

pub(crate) struct DecodedImage {
    pub(crate) pixels: egui::ColorImage,
    pub(crate) reservation: ImageReservation,
}

impl ImageMemoryBudget {
    pub(crate) fn for_context(
        context: &egui::Context,
        preference: ImageMemoryBudgetPreference,
    ) -> Self {
        let budget = context.data_mut(|data| {
            data.get_temp_mut_or_insert_with(egui::Id::new("markdown-image-memory-budget"), || {
                Self::new(preference)
            })
            .clone()
        });
        let scan_pending = {
            let mut state = budget.0.lock().expect("image budget lock poisoned");
            let before = state.used;
            state.reap_retired();
            state.retirement_scan_remaining != 0
                || (state.used < before && !state.retired.is_empty())
        };
        if scan_pending {
            context.request_repaint();
            context.request_repaint_of(context.viewport_id());
        }
        budget
    }

    fn new(preference: ImageMemoryBudgetPreference) -> Self {
        Self(Arc::new(Mutex::new(BudgetState {
            preference,
            limit: preference.bytes(),
            used: 0,
            workers: 0,
            generation: 0,
            retired: VecDeque::new(),
            retirement_scan_remaining: 0,
        })))
    }

    pub(crate) fn preference(&self) -> ImageMemoryBudgetPreference {
        self.0
            .lock()
            .expect("image budget lock poisoned")
            .preference
    }

    pub(crate) fn set_preference(&self, preference: ImageMemoryBudgetPreference) {
        let mut state = self.0.lock().expect("image budget lock poisoned");
        if state.preference != preference {
            state.preference = preference;
            state.limit = preference.bytes();
            state.generation = state.generation.wrapping_add(1);
        }
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        let mut state = self.0.lock().expect("image budget lock poisoned");
        state.reap_retired();
        state.generation
    }

    pub(crate) fn limit(&self) -> u64 {
        self.0.lock().expect("image budget lock poisoned").limit
    }

    pub(crate) fn is_over_budget(&self) -> bool {
        let state = self.0.lock().expect("image budget lock poisoned");
        state.used > state.limit
    }

    pub(crate) fn availability(&self) -> (u64, usize) {
        let mut state = self.0.lock().expect("image budget lock poisoned");
        state.reap_retired();
        (state.limit.saturating_sub(state.used), state.workers)
    }

    #[cfg(test)]
    pub(crate) fn usage_for_test(&self) -> (u64, usize) {
        let state = self.0.lock().unwrap();
        (state.used, state.workers)
    }

    pub(crate) fn start_worker(
        &self,
        context: &egui::Context,
    ) -> Result<ImageWorker, ImageLoadFailure> {
        let mut state = self.0.lock().expect("image budget lock poisoned");
        if state.workers >= MAX_CONCURRENT_IMAGE_LOADS {
            return Err(ImageLoadFailure::Busy);
        }
        state.workers += 1;
        Ok(ImageWorker {
            budget: self.clone(),
            repaint: context.clone(),
        })
    }

    pub(crate) fn reserve(
        &self,
        bytes: u64,
        context: Option<&egui::Context>,
    ) -> Result<ImageReservation, ImageLoadFailure> {
        let mut state = self.0.lock().expect("image budget lock poisoned");
        state.reap_retired();
        state.admit(bytes)?;
        Ok(ImageReservation {
            budget: self.clone(),
            bytes,
            repaint: context.cloned(),
            texture_pixels: None,
        })
    }
}

impl BudgetState {
    fn admit(&mut self, additional: u64) -> Result<(), ImageLoadFailure> {
        let Some(candidate) = self.used.checked_add(additional) else {
            return Err(self.refusal(additional));
        };
        if candidate > self.limit {
            return Err(self.refusal(additional));
        }
        self.used = candidate;
        Ok(())
    }

    fn refusal(&self, additional: u64) -> ImageLoadFailure {
        ImageLoadFailure::Budget {
            message: format!(
                "Image memory budget exhausted: {} bytes reserved, {} more required, {} allowed. \
             Existing images are unchanged. Close or reload image documents, or increase \
             Image memory budget in Settings.",
                self.used, additional, self.limit
            ),
            required_bytes: additional,
        }
    }

    fn release(&mut self, bytes: u64) {
        self.used = self
            .used
            .checked_sub(bytes)
            .expect("image reservation accounting underflow");
        self.generation = self.generation.wrapping_add(1);
    }

    fn reap_retired(&mut self) {
        // Rotate a bounded batch: one still-live renderer delta must not pin
        // later dead entries, nor turn every admission into a full queue walk.
        let count = self.retired.len().min(MAX_RETIREMENTS_PER_CHECK);
        self.retirement_scan_remaining = self.retirement_scan_remaining.saturating_sub(count);
        for _ in 0..count {
            let retired = self
                .retired
                .pop_front()
                .expect("retirement queue is nonempty");
            if retired.pixels.strong_count() == 0 {
                self.release(retired.bytes);
            } else {
                self.retired.push_back(retired);
            }
        }
        if self.retired.len() <= MAX_RETIREMENTS_PER_CHECK
            && self.retired.capacity() > 4 * MAX_RETIREMENTS_PER_CHECK
        {
            self.retired.shrink_to_fit();
        }
    }
}

impl ImageReservation {
    pub(crate) fn split(&mut self, bytes: u64) -> Self {
        self.bytes = self
            .bytes
            .checked_sub(bytes)
            .expect("image reservation split exceeds its owner");
        Self {
            budget: self.budget.clone(),
            bytes,
            repaint: self.repaint.clone(),
            texture_pixels: None,
        }
    }

    pub(crate) fn track_texture_pixels(&mut self, pixels: Weak<egui::ColorImage>) {
        self.texture_pixels = Some(pixels);
    }

    pub(crate) fn resize(&mut self, bytes: u64) -> Result<(), ImageLoadFailure> {
        let mut state = self.budget.0.lock().expect("image budget lock poisoned");
        state.reap_retired();
        if bytes > self.bytes {
            if let Err(mut error) = state.admit(bytes - self.bytes) {
                if let ImageLoadFailure::Budget { required_bytes, .. } = &mut error {
                    *required_bytes = bytes;
                }
                return Err(error);
            }
        } else if bytes < self.bytes {
            state.release(self.bytes - bytes);
        }
        self.bytes = bytes;
        Ok(())
    }

    pub(crate) fn retire(&mut self, pixels: Weak<egui::ColorImage>) {
        let mut state = self.budget.0.lock().expect("image budget lock poisoned");
        if pixels.strong_count() == 0 {
            state.release(self.bytes);
        } else {
            state.retired.push_back(RetiredImage {
                bytes: self.bytes,
                pixels,
            });
            state.retirement_scan_remaining = state.retired.len();
        }
        self.bytes = 0;
    }
}

impl Drop for ImageReservation {
    fn drop(&mut self) {
        if let Some(pixels) = self.texture_pixels.take() {
            self.retire(pixels);
        }
        if self.bytes != 0 {
            self.budget
                .0
                .lock()
                .expect("image budget lock poisoned")
                .release(self.bytes);
        }

        if let Some(context) = &self.repaint {
            wake_image_viewports(context);
        }
    }
}

impl Drop for ImageWorker {
    fn drop(&mut self) {
        {
            let mut state = self.budget.0.lock().expect("image budget lock poisoned");
            state.workers = state
                .workers
                .checked_sub(1)
                .expect("image worker accounting underflow");
            state.generation = state.generation.wrapping_add(1);
        }
        wake_image_viewports(&self.repaint);
    }
}

pub(crate) fn wake_image_viewports(context: &egui::Context) {
    let viewports = context.input(|input| input.raw.viewports.keys().copied().collect::<Vec<_>>());
    for viewport in viewports {
        context.request_repaint_of(viewport);
    }
    context.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controlled_budget(limit: u64) -> ImageMemoryBudget {
        let budget = ImageMemoryBudget::new(ImageMemoryBudgetPreference::default());
        budget.0.lock().unwrap().limit = limit;
        budget
    }

    #[test]
    fn image_budget_retirement_ledger_releases_exceptional_capacity_when_sparse() {
        let budget = controlled_budget(2048);
        let mut uploads = Vec::new();
        for _ in 0..1024 {
            let pixels = Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE]));
            let mut reservation = budget.reserve(1, None).unwrap();
            reservation.track_texture_pixels(Arc::downgrade(&pixels));
            drop(reservation);
            uploads.push(pixels);
        }
        assert!(budget.0.lock().unwrap().retired.capacity() > 4 * MAX_RETIREMENTS_PER_CHECK);
        let pinned = uploads.pop().unwrap();
        drop(uploads);
        for _ in 0..16 {
            budget.generation();
        }
        assert_eq!(budget.usage_for_test(), (1, 0));
        assert_eq!(budget.0.lock().unwrap().retired.len(), 1);
        assert!(budget.0.lock().unwrap().retired.capacity() <= MAX_RETIREMENTS_PER_CHECK);
        drop(pinned);
        budget.generation();
        assert_eq!(budget.usage_for_test(), (0, 0));
    }

    #[test]
    fn image_budget_reservation_is_atomic_and_released_by_its_actual_owner() {
        let budget = controlled_budget(100);
        let mut reservation = budget.reserve(80, None).unwrap();
        assert!(reservation.resize(101).is_err());
        assert_eq!(budget.0.lock().unwrap().used, 80);
        assert!(budget.reserve(21, None).is_err());
        reservation.resize(20).unwrap();
        let second = budget.reserve(80, None).unwrap();
        assert_eq!(budget.0.lock().unwrap().used, 100);
        drop(reservation);
        assert_eq!(budget.0.lock().unwrap().used, 80);
        drop(second);
        assert_eq!(budget.0.lock().unwrap().used, 0);
    }

    #[test]
    fn image_budget_is_shared_without_stale_window_initialization_overwriting_it() {
        let context = egui::Context::default();
        let first = ImageMemoryBudget::for_context(&context, ImageMemoryBudgetPreference::MiB512);
        first.set_preference(ImageMemoryBudgetPreference::MiB64);
        let second = ImageMemoryBudget::for_context(&context, ImageMemoryBudgetPreference::MiB512);
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(second.preference(), ImageMemoryBudgetPreference::MiB64);
        let retained = first.reserve(second.limit(), None).unwrap();
        assert!(second.reserve(1, None).is_err());
        drop(retained);
        assert!(second.reserve(1, None).is_ok());
    }

    #[test]
    fn image_budget_lowering_preserves_existing_admissions_and_refuses_additional_growth() {
        let budget = ImageMemoryBudget::new(ImageMemoryBudgetPreference::MiB128);
        let retained = budget.reserve(100 * 1024 * 1024, None).unwrap();
        budget.set_preference(ImageMemoryBudgetPreference::MiB64);
        assert_eq!(budget.0.lock().unwrap().used, 100 * 1024 * 1024);
        assert!(budget.reserve(1, None).is_err());
        budget.set_preference(ImageMemoryBudgetPreference::MiB256);
        assert!(budget.reserve(1, None).is_ok());
        budget.set_preference(ImageMemoryBudgetPreference::MiB64);
        drop(retained);
        assert!(budget.reserve(64 * 1024 * 1024, None).is_ok());
    }

    #[test]
    fn image_budget_workers_release_slots_without_waiting_for_a_hidden_viewer_to_poll() {
        let context = egui::Context::default();
        let budget = controlled_budget(100);
        let workers = (0..MAX_CONCURRENT_IMAGE_LOADS)
            .map(|_| budget.start_worker(&context).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            budget.start_worker(&context),
            Err(ImageLoadFailure::Busy)
        ));
        let queued_result = budget.reserve(100, None).unwrap();
        drop(workers);
        assert!(budget.start_worker(&context).is_ok());
        assert!(budget.reserve(1, None).is_err());
        drop(queued_result);
        assert_eq!(budget.0.lock().unwrap().used, 0);
    }

    #[test]
    fn image_budget_texture_retirement_waits_for_the_real_cpu_upload_owner() {
        let budget = controlled_budget(100);
        let pixels = Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::WHITE));
        let mut reservation = budget.reserve(100, None).unwrap();
        reservation.retire(Arc::downgrade(&pixels));
        drop(reservation);
        assert!(budget.reserve(1, None).is_err());
        drop(pixels);
        assert!(budget.reserve(100, None).is_ok());
        assert!(budget.0.lock().unwrap().retired.is_empty());
    }

    #[test]
    fn image_budget_one_live_upload_does_not_pin_later_completed_retirements() {
        let budget = controlled_budget(200);
        let first_pixels = Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::WHITE));
        let second_pixels = Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::WHITE));
        let mut first = budget.reserve(100, None).unwrap();
        let mut second = budget.reserve(100, None).unwrap();
        first.retire(Arc::downgrade(&first_pixels));
        second.retire(Arc::downgrade(&second_pixels));
        drop(second_pixels);
        assert!(budget.reserve(100, None).is_ok());
        assert_eq!(budget.0.lock().unwrap().used, 100);
        drop(first_pixels);
        budget.generation();
        assert_eq!(budget.0.lock().unwrap().used, 0);
    }

    #[test]
    fn image_budget_context_storage_does_not_hold_a_context_or_self_reference_cycle() {
        let context = egui::Context::default();
        let budget =
            ImageMemoryBudget::for_context(&context, ImageMemoryBudgetPreference::default());
        let weak = Arc::downgrade(&budget.0);
        drop(budget);
        drop(context);
        assert!(weak.upgrade().is_none());
    }
}
