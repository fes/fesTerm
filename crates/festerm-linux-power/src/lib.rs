//! Linux resume-from-suspend wake notification support for fesTerm.
//!
//! Listens for systemd-logind's `org.freedesktop.login1.Manager
//! .PrepareForSleep` signal on the system bus and calls back once per resume
//! (the signal fires with argument `false`, per logind's own documentation,
//! immediately after resuming; `true` fires just before suspending, which we
//! deliberately ignore). This matches ADR 0018's "resume from system sleep"
//! wake trigger for an on-demand SSH liveness probe.
//!
//! Only systemd-logind is supported: fesTerm does not attempt any
//! elogind/other-init-system equivalent, and a desktop with neither simply
//! never sees this signal, which is an ordinary, expected outcome (ADR 0018:
//! "platforms that cannot expose reliable wake/network events still use
//! transport errors and ordinary SSH liveness checks"). Network-interface/
//! route-change detection is also deliberately out of scope here; see issue
//! #48 for that follow-up.

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeEvent {
    Wake,
    Ignore,
}

#[cfg(any(target_os = "linux", test))]
const fn classify_prepare_for_sleep(preparing_for_sleep: Option<bool>) -> WakeEvent {
    match preparing_for_sleep {
        Some(false) => WakeEvent::Wake,
        Some(true) | None => WakeEvent::Ignore,
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};

    use futures_util::StreamExt;
    use tokio::sync::watch;
    use zbus::{message::Type as MessageType, Connection, MatchRule, MessageStream};

    use super::{classify_prepare_for_sleep, WakeEvent};

    const LOGIND_DESTINATION: &str = "org.freedesktop.login1";
    const LOGIND_PATH: &str = "/org/freedesktop/login1";
    const LOGIND_INTERFACE: &str = "org.freedesktop.login1.Manager";
    const PREPARE_FOR_SLEEP_MEMBER: &str = "PrepareForSleep";

    /// Runs a dedicated single-threaded async runtime for as long as the
    /// `WakeMonitor` stays alive, invoking `wake` once per systemd-logind
    /// `PrepareForSleep(false)` (resume) signal. Dropping it asks the
    /// listener to stop and joins its thread.
    pub struct WakeMonitor {
        thread: Option<JoinHandle<()>>,
        shutdown: watch::Sender<bool>,
    }

    impl WakeMonitor {
        pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
            let (shutdown_sender, shutdown_receiver) = watch::channel(false);
            let thread = thread::Builder::new()
                .name("festerm-wake-monitor".to_owned())
                .spawn(move || Self::run(wake, shutdown_receiver))
                .expect("wake-monitor thread should spawn");
            Self {
                thread: Some(thread),
                shutdown: shutdown_sender,
            }
        }

        fn run(wake: Arc<dyn Fn() + Send + Sync>, mut shutdown: watch::Receiver<bool>) {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            runtime.block_on(async move {
                let Ok(connection) = Connection::system().await else {
                    return;
                };
                let Ok(rule) = MatchRule::builder()
                    .msg_type(MessageType::Signal)
                    .interface(LOGIND_INTERFACE)
                    .and_then(|builder| builder.member(PREPARE_FOR_SLEEP_MEMBER))
                    .and_then(|builder| builder.path(LOGIND_PATH))
                    .and_then(|builder| builder.sender(LOGIND_DESTINATION))
                    .map(|builder| builder.build())
                else {
                    return;
                };
                let Ok(mut stream) =
                    MessageStream::for_match_rule(rule, &connection, Some(4)).await
                else {
                    return;
                };
                loop {
                    tokio::select! {
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                break;
                            }
                        }
                        message = stream.next() => {
                            let Some(Ok(message)) = message else {
                                break;
                            };
                            if classify_prepare_for_sleep(
                                message.body().deserialize::<bool>().ok(),
                            ) == WakeEvent::Wake
                            {
                                wake();
                            }
                        }
                    }
                }
            });
        }
    }

    impl Drop for WakeMonitor {
        fn drop(&mut self) {
            let _ = self.shutdown.send(true);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::WakeMonitor;

#[cfg(not(target_os = "linux"))]
pub struct WakeMonitor;

#[cfg(not(target_os = "linux"))]
impl WakeMonitor {
    pub fn install(_wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_prepare_for_sleep, WakeEvent};

    #[test]
    fn only_resume_prepare_for_sleep_events_wake() {
        assert_eq!(classify_prepare_for_sleep(Some(false)), WakeEvent::Wake);
        assert_eq!(classify_prepare_for_sleep(Some(true)), WakeEvent::Ignore);
        assert_eq!(classify_prepare_for_sleep(None), WakeEvent::Ignore);
    }
}
