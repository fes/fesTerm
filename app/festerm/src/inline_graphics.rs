//! Session eligibility for bounded inline terminal graphics (ADR 0089 §7).
//!
//! Eligibility is decided once, when a session is placed, and never changes
//! afterwards: neither the setting nor a later recording can flip it. Only an
//! opted-in, in-process Unix local PTY on macOS or Linux in a developer build
//! qualifies. Phase 1 is an unadvertised developer slice (ADR 0089 §9), so
//! release builds never enable Kitty and never answer its capability query.
//! An eligible session also holds a fixed protocol reservation from the
//! process-wide terminal-graphics pool (§5) for as long as its tab lives.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use festerm_core::kitty::{KittyConfig, KittyPngValidator, MAX_RGBA_BYTES};

/// Why a session is not eligible for inline graphics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineGraphicsRefusal {
    /// The `inline_graphics` setting was off when the session was created.
    NotOptedIn,
    /// Windows ConPTY and other platforms stay ineligible.
    UnsupportedPlatform,
    /// No backend started, so there is nothing to enable.
    StartFailed,
    /// `festerm-sessiond`, recovery, mirror and hosted-profile sessions.
    SessionDaemon,
    /// tmux, screen and other provider-backed local sessions.
    Multiplexer,
    /// Dev Tunnel, hosted-client and mobile paths.
    Tunnel,
    Ssh,
    Sftp,
    Serial,
    /// Structured PowerShell tabs never own a terminal session, so the
    /// classifier cannot produce this; the refusal is kept explicit for
    /// when one does.
    #[cfg_attr(not(test), allow(dead_code))]
    PowerShell,
    /// JavaScript-program backends (ADR 0088) remain explicitly ineligible.
    #[cfg_attr(not(test), allow(dead_code))]
    JavaScriptProgram,
    /// The session was created into an active synchronized recording.
    Recording,
    /// Release builds; Phase 1 is a developer-only preview.
    DeveloperPreviewOnly,
    /// The terminal-graphics pool has no room for another fixed session
    /// reservation.
    ReservationUnavailable,
    /// The terminal refused to enable Kitty graphics, for example because it
    /// had already ingested output.
    EnableFailed,
}

/// The decision frozen for one session at creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineGraphicsEligibility {
    Eligible,
    Disabled(InlineGraphicsRefusal),
}

impl InlineGraphicsEligibility {
    pub const fn is_eligible(self) -> bool {
        matches!(self, Self::Eligible)
    }
}

/// Window state consulted when a session is created.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InlineGraphicsPolicy {
    pub opted_in: bool,
    /// Whether the session is about to be enrolled in (or cannot be proven
    /// clear of) a synchronized recording.
    pub recording: bool,
}

/// Build-time availability, separate from the per-session decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineGraphicsBuild {
    /// Only macOS and Linux local PTYs are qualified for v1.
    pub platform_supported: bool,
    /// Phase 1 must not reach a shipped build (ADR 0089 §9).
    pub developer_preview: bool,
}

#[cfg(test)]
impl InlineGraphicsBuild {
    pub const fn is_available(self) -> bool {
        self.platform_supported && self.developer_preview
    }
}

pub const BUILD: InlineGraphicsBuild = InlineGraphicsBuild {
    platform_supported: cfg!(any(target_os = "macos", target_os = "linux")),
    developer_preview: cfg!(debug_assertions),
};

/// Decides eligibility for a backend already classified by the caller.
/// `backend_refusal` is `None` only for an in-process local PTY; a backend
/// refusal is reported ahead of the opt-in so the reason stays stable.
pub fn decide(
    policy: InlineGraphicsPolicy,
    backend_refusal: Option<InlineGraphicsRefusal>,
    build: InlineGraphicsBuild,
) -> InlineGraphicsEligibility {
    let refusal = backend_refusal
        .or((!build.platform_supported).then_some(InlineGraphicsRefusal::UnsupportedPlatform))
        .or((!build.developer_preview).then_some(InlineGraphicsRefusal::DeveloperPreviewOnly))
        .or((!policy.opted_in).then_some(InlineGraphicsRefusal::NotOptedIn))
        .or(policy.recording.then_some(InlineGraphicsRefusal::Recording));
    refusal.map_or(
        InlineGraphicsEligibility::Eligible,
        InlineGraphicsEligibility::Disabled,
    )
}

/// The ADR 0089 §5 aggregate decoded-RGBA reservation across terminals.
const AGGREGATE_RGBA_BYTES: usize = 256 * 1024 * 1024;

/// Every eligible session gets the full per-terminal ceiling, so the pool has
/// room for this many fixed session reservations.
const POOL_SESSIONS: usize = AGGREGATE_RGBA_BYTES / MAX_RGBA_BYTES;

/// The separately reserved terminal-graphics protocol pool. Each eligible
/// session claims its core protocol memory once, at creation, so protocol
/// acceptance never depends on other terminals or on render-side pressure.
#[derive(Debug)]
pub struct KittyReservationPool {
    capacity: usize,
    claimed: AtomicUsize,
}

impl KittyReservationPool {
    pub(crate) fn with_sessions(sessions: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity: session_protocol_bytes().map_or(0, |bytes| bytes.saturating_mul(sessions)),
            claimed: AtomicUsize::new(0),
        })
    }

    /// The pool shared by every window in this process. Tests get one per
    /// thread so parallel tests cannot exhaust each other's pool.
    pub fn process() -> Arc<Self> {
        #[cfg(not(test))]
        {
            static POOL: std::sync::OnceLock<Arc<KittyReservationPool>> =
                std::sync::OnceLock::new();
            Arc::clone(POOL.get_or_init(|| Self::with_sessions(POOL_SESSIONS)))
        }
        #[cfg(test)]
        {
            thread_local! {
                static POOL: Arc<KittyReservationPool> =
                    KittyReservationPool::with_sessions(POOL_SESSIONS);
            }
            POOL.with(Arc::clone)
        }
    }

    /// Claims one fixed session reservation, or `None` when the pool is full.
    pub fn reserve(self: &Arc<Self>) -> Option<KittyReservation> {
        let bytes = session_protocol_bytes()?;
        let mut claimed = self.claimed.load(Ordering::Acquire);
        loop {
            let next = claimed
                .checked_add(bytes)
                .filter(|next| *next <= self.capacity)?;
            match self.claimed.compare_exchange_weak(
                claimed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => claimed = actual,
            }
        }
        Some(KittyReservation {
            pool: Arc::clone(self),
            bytes,
        })
    }

    #[cfg(test)]
    pub fn claimed_bytes(&self) -> usize {
        self.claimed.load(Ordering::Acquire)
    }
}

fn session_protocol_bytes() -> Option<usize> {
    KittyConfig {
        rgba_budget: MAX_RGBA_BYTES,
        png_validator: None,
    }
    .protocol_memory_bytes()
}

/// One session's claim; released when the owning tab is dropped.
#[derive(Debug)]
pub struct KittyReservation {
    pool: Arc<KittyReservationPool>,
    bytes: usize,
}

impl KittyReservation {
    /// The core configuration this reservation pays for.
    pub fn config(&self) -> KittyConfig {
        #[cfg(test)]
        if let Some(validator) = TEST_PNG_VALIDATOR.with(|slot| slot.borrow().clone()) {
            return KittyConfig {
                rgba_budget: MAX_RGBA_BYTES,
                png_validator: Some(validator),
            };
        }
        KittyConfig {
            rgba_budget: MAX_RGBA_BYTES,
            png_validator: Some(KittyPngValidator::new(festerm_ui_egui::validate_kitty_png)),
        }
    }

    pub const fn rgba_budget(&self) -> usize {
        MAX_RGBA_BYTES
    }
}

#[cfg(test)]
thread_local! {
    static TEST_PNG_VALIDATOR: std::cell::RefCell<Option<KittyPngValidator>> =
        const { std::cell::RefCell::new(None) };
}

/// Replaces the PNG validator for sessions whose graphics are enabled on this
/// test thread until the guard drops. The validator itself runs on the
/// session's validation worker, so it can block until a test releases it.
#[cfg(test)]
#[must_use = "the override ends when the guard drops"]
pub struct TestPngValidatorGuard(Option<KittyPngValidator>);

#[cfg(test)]
pub fn override_png_validator_for_test(validator: KittyPngValidator) -> TestPngValidatorGuard {
    TestPngValidatorGuard(TEST_PNG_VALIDATOR.with(|slot| slot.replace(Some(validator))))
}

#[cfg(test)]
impl Drop for TestPngValidatorGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_PNG_VALIDATOR.with(|slot| *slot.borrow_mut() = previous);
    }
}

impl Drop for KittyReservation {
    fn drop(&mut self) {
        self.pool.claimed.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTED_IN: InlineGraphicsPolicy = InlineGraphicsPolicy {
        opted_in: true,
        recording: false,
    };

    const AVAILABLE: InlineGraphicsBuild = InlineGraphicsBuild {
        platform_supported: true,
        developer_preview: true,
    };

    #[test]
    fn only_an_opted_in_unrecorded_local_pty_on_a_supported_platform_is_eligible() {
        assert_eq!(
            decide(OPTED_IN, None, AVAILABLE),
            InlineGraphicsEligibility::Eligible
        );
        assert_eq!(
            decide(InlineGraphicsPolicy::default(), None, AVAILABLE),
            InlineGraphicsEligibility::Disabled(InlineGraphicsRefusal::NotOptedIn)
        );
        assert_eq!(
            decide(
                OPTED_IN,
                None,
                InlineGraphicsBuild {
                    platform_supported: false,
                    ..AVAILABLE
                },
            ),
            InlineGraphicsEligibility::Disabled(InlineGraphicsRefusal::UnsupportedPlatform)
        );
        assert_eq!(
            decide(
                OPTED_IN,
                None,
                InlineGraphicsBuild {
                    developer_preview: false,
                    ..AVAILABLE
                },
            ),
            InlineGraphicsEligibility::Disabled(InlineGraphicsRefusal::DeveloperPreviewOnly)
        );
        assert_eq!(
            decide(
                InlineGraphicsPolicy {
                    opted_in: true,
                    recording: true,
                },
                None,
                AVAILABLE,
            ),
            InlineGraphicsEligibility::Disabled(InlineGraphicsRefusal::Recording)
        );
    }

    #[test]
    fn every_non_local_backend_is_refused_even_when_opted_in() {
        for refusal in [
            InlineGraphicsRefusal::StartFailed,
            InlineGraphicsRefusal::SessionDaemon,
            InlineGraphicsRefusal::Multiplexer,
            InlineGraphicsRefusal::Tunnel,
            InlineGraphicsRefusal::Ssh,
            InlineGraphicsRefusal::Sftp,
            InlineGraphicsRefusal::Serial,
            InlineGraphicsRefusal::PowerShell,
            InlineGraphicsRefusal::JavaScriptProgram,
        ] {
            for platform_supported in [true, false] {
                for developer_preview in [true, false] {
                    let build = InlineGraphicsBuild {
                        platform_supported,
                        developer_preview,
                    };
                    assert_eq!(
                        decide(OPTED_IN, Some(refusal), build),
                        InlineGraphicsEligibility::Disabled(refusal),
                        "{refusal:?} must win over {build:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn release_builds_never_enable_the_developer_preview() {
        assert_eq!(BUILD.developer_preview, cfg!(debug_assertions));
    }

    #[test]
    fn the_pool_admits_fixed_session_reservations_up_to_the_aggregate_and_releases_on_drop() {
        let pool = KittyReservationPool::with_sessions(POOL_SESSIONS);
        let per_session = session_protocol_bytes().unwrap();
        assert!(per_session > MAX_RGBA_BYTES);
        let held: Vec<_> = (0..POOL_SESSIONS)
            .map(|_| pool.reserve().expect("within the aggregate"))
            .collect();
        assert_eq!(POOL_SESSIONS, 4);
        assert_eq!(pool.claimed_bytes(), per_session * POOL_SESSIONS);
        assert!(pool.reserve().is_none(), "a fifth session is refused");
        assert_eq!(held[0].config().rgba_budget, held[0].rgba_budget());
        assert_eq!(held[0].config().protocol_memory_bytes(), Some(per_session));
        drop(held);
        assert_eq!(pool.claimed_bytes(), 0);
        assert!(pool.reserve().is_some());
    }
}
