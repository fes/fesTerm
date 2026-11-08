#[cfg(unix)]
fn main() {
    use std::{
        env,
        sync::{mpsc, Arc},
        thread,
        time::{Duration, Instant},
    };

    use festerm_pty::{LocalProfile, LocalPtySession};
    use festerm_session::{
        Session, SessionEvent, SessionEventNotifier, SessionLifecycle, SessionTryReceiveError,
        TerminalSize,
    };
    use nix::sys::{
        resource::{getrusage, UsageWho},
        time::TimeValLike,
    };

    struct Wake(mpsc::SyncSender<()>);
    impl SessionEventNotifier for Wake {
        fn notify(&self) {
            let _ = self.0.try_send(());
        }
    }

    fn drain(
        session: &LocalPtySession,
        wake: &mpsc::Receiver<()>,
        target: usize,
    ) -> (usize, usize) {
        let mut bytes = 0;
        let mut events = 0;
        while bytes < target {
            match session.try_recv_event() {
                Ok(SessionEvent::Output(output)) => {
                    bytes += output.len();
                    events += 1;
                }
                Ok(SessionEvent::Error(error)) => panic!("PTY output failed: {error:?}"),
                Ok(SessionEvent::Lifecycle(
                    lifecycle @ (SessionLifecycle::Exited(_)
                    | SessionLifecycle::Failed(_)
                    | SessionLifecycle::Stopping
                    | SessionLifecycle::Stopped),
                )) => panic!("PTY ended during output: {lifecycle:?}"),
                Ok(_) => {}
                Err(SessionTryReceiveError::Empty) => wake
                    .recv_timeout(Duration::from_secs(5))
                    .expect("timed out waiting for PTY output"),
                Err(SessionTryReceiveError::Closed) => panic!("PTY event queue closed"),
            }
        }
        (bytes, events)
    }

    let mut args = env::args().skip(1);
    let case = args.next().expect("expected paused or draining");
    let amount: u64 = args
        .next()
        .expect("expected seconds or MiB")
        .parse()
        .unwrap();
    assert!(args.next().is_none());
    let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
    let session = LocalPtySession::start_with_notifier(
        LocalProfile::new("/usr/bin/yes"),
        TerminalSize::new(80, 24).unwrap(),
        Arc::new(Wake(wake_sender)),
    )
    .expect("controlled PTY must start");
    if case == "paused" {
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.metrics().backpressure_count < 2 {
            assert!(Instant::now() < deadline, "output did not become blocked");
            let lifecycle = session.lifecycle();
            assert!(
                matches!(
                    lifecycle,
                    SessionLifecycle::Starting | SessionLifecycle::Running
                ),
                "PTY ended before output stalled: {lifecycle:?}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    } else if case == "draining" {
        drain(&session, &wake_receiver, 1024 * 1024);
    } else {
        panic!("expected paused or draining");
    }

    let before_metrics = session.metrics();
    let before = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let (bytes, events) = if case == "paused" {
        thread::sleep(Duration::from_secs(amount));
        (0, 0)
    } else {
        drain(
            &session,
            &wake_receiver,
            usize::try_from(amount).unwrap() * 1024 * 1024,
        )
    };
    let after = getrusage(UsageWho::RUSAGE_SELF).unwrap();
    let after_metrics = session.metrics();
    session
        .shutdown(Duration::from_secs(5))
        .expect("controlled PTY must stop");
    let user_us = after.user_time().num_microseconds() - before.user_time().num_microseconds();
    let system_us =
        after.system_time().num_microseconds() - before.system_time().num_microseconds();
    println!(
        "{case}: user_us={user_us} system_us={system_us} bytes={bytes} events={events} \
         pressure_attempts={}",
        after_metrics.backpressure_count - before_metrics.backpressure_count
    );
}

#[cfg(not(unix))]
fn main() {
    eprintln!("this PTY performance probe requires Unix and /usr/bin/yes");
    std::process::exit(2);
}
