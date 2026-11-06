use crate::{
    AppLock, AuthFailure, AuthMethod, AuthOutcome, Authentication, Availability, BiometryKind,
    LifecycleEvent, Locker,
};
use festerm_android_platform::{self as native, AuthResult};
use std::sync::Arc;

pub struct AndroidLocker;

impl Locker for AndroidLocker {
    fn availability(&self) -> Availability {
        if native::authentication_available() {
            Availability::Available(BiometryKind::None)
        } else {
            Availability::PasscodeNotSet
        }
    }

    fn authenticate(
        &self,
        method: AuthMethod,
        reason: &str,
        done: Box<dyn FnOnce(AuthOutcome) + Send>,
    ) -> Authentication {
        let token = native::authenticate(
            method == AuthMethod::DevicePasscode,
            reason,
            Box::new(move |result| done(outcome(result))),
        );
        // The guard names its own request, so retiring it never dismisses a newer prompt.
        Authentication::new(move || native::cancel_authentication(token))
    }
}

fn outcome(result: AuthResult) -> AuthOutcome {
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Cancelled => Err(AuthFailure::Cancelled),
        AuthResult::LockedOut => Err(AuthFailure::BiometryLockedOut),
        AuthResult::NoCredential => Err(AuthFailure::PasscodeNotSet),
        AuthResult::Unavailable => Err(AuthFailure::Unavailable),
    }
}

pub struct LockBridge {
    lock: AppLock,
}

impl LockBridge {
    pub fn new(_window: raw_window_handle::WindowHandle<'_>, lock: AppLock) -> Option<Self> {
        let observer = lock.clone();
        if let Err(error) = native::set_lifecycle(Arc::new(move |event| {
            let event = match event {
                0 => LifecycleEvent::WillResignActive,
                1 => LifecycleEvent::DidBecomeActive,
                2 => LifecycleEvent::DidEnterBackground,
                3 => LifecycleEvent::DeviceLocked,
                _ => return,
            };
            observer.handle_lifecycle(event);
        })) {
            eprintln!("fesTerm: {error}");
            lock.lock_now();
            return None;
        }
        Some(Self { lock })
    }

    pub fn sync(&self) {
        let settings = self.lock.settings();
        if let Err(error) = native::privacy(
            settings.enabled,
            self.lock.shield_should_cover(),
            settings.hide_app_switcher_preview,
        ) {
            self.lock.lock_now();
            eprintln!("fesTerm: {error}");
        }
    }
}
