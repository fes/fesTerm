//! Readiness- or retry-driven ownership for one dormant direct host peer.

use super::{
    retire_admission, Error, HostVerification, NativeAdmission, Peer, PeerContext, Step, Wake,
};
#[cfg(unix)]
use festerm_automation_mcp::secure_transport;
use festerm_automation_mcp::secure_transport::{Role, SecureTransport};
#[cfg(windows)]
use festerm_windows_security::named_pipe::Pipe as HostStream;
#[cfg(windows)]
use std::sync::Mutex;
use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Duration,
};
#[cfg(unix)]
use tokio::net::UnixStream as HostStream;
use tokio::sync::watch;

const FAIRNESS_BUDGET: usize = 32;
#[cfg(windows)]
const PIPE_RETRY: Duration = Duration::from_millis(20);

#[cfg(unix)]
type SharedStream = HostStream;
#[cfg(windows)]
type SharedStream = Mutex<HostStream>;

struct RegisteredStream(Arc<SharedStream>);

impl Read for RegisteredStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.0.try_read(bytes)
        }
        #[cfg(windows)]
        {
            let mut pipe = self
                .0
                .try_lock()
                .map_err(|_| io::Error::other("named pipe ownership unavailable"))?;
            pipe.try_read(bytes)
        }
    }
}

impl Write for RegisteredStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.0.try_write(bytes)
        }
        #[cfg(windows)]
        {
            let mut pipe = self
                .0
                .try_lock()
                .map_err(|_| io::Error::other("named pipe ownership unavailable"))?;
            pipe.try_write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Completion is published only after peer cleanup, including task cancellation.
/// `OwnerLost` means cleanup could not be observed and the surface must be
/// quarantined, just as for `Cleanup`.
#[derive(Clone)]
pub(super) struct Status(watch::Receiver<Option<Result<(), Error>>>);

impl Status {
    pub(super) async fn wait(&mut self) -> Result<(), Error> {
        loop {
            if let Some(result) = *self.0.borrow_and_update() {
                return result;
            }
            self.0.changed().await.map_err(|_| Error::OwnerLost)?;
        }
    }
}

/// The caller supplies one exclusively owned socket and all native boundaries.
///
/// Run off the UI thread on a Tokio runtime with I/O and time enabled. Callers
/// must wake it for native decisions and lifecycle/admission cuts that do not
/// close an announced connection, including stop before announcement. Guarded
/// connection closes wake it after releasing admission. The verifier's
/// UI-owner wake must use this same owner wake. This loop does not
/// qualify those boundaries or attach a listener. Keep `Status` until cleanup
/// finishes before reusing a native surface; failed or unobserved cleanup
/// requires quarantine.
/// An interrupted owner may have been cancelled or panicked; its task result
/// must also be observed.
pub(super) struct Owner<N: HostVerification, A: NativeAdmission> {
    peer: Peer<RegisteredStream, N, A>,
    socket: Option<Arc<SharedStream>>,
    wake: Wake,
    completion: watch::Sender<Option<Result<(), Error>>>,
    finished: bool,
}

impl<N: HostVerification, A: NativeAdmission> Owner<N, A> {
    pub(super) fn new(
        socket: HostStream,
        native: N,
        admission: A,
        context: PeerContext,
        bootstrap_duration: Duration,
        frame_duration: Duration,
    ) -> Result<(Self, Status), Error> {
        #[cfg(windows)]
        let socket = { Mutex::new(socket) };
        let socket = Arc::new(socket);
        let owner_liveness = native.owner_liveness();
        let channel = match SecureTransport::new(
            RegisteredStream(socket.clone()),
            Role::Application,
            native,
            bootstrap_duration,
            frame_duration,
        ) {
            Ok(channel) => channel,
            Err(error) => {
                return Err(if retire_admission(admission) {
                    Error::Channel(error)
                } else {
                    Error::Cleanup
                });
            }
        };
        let wake = context.wake.clone();
        let peer = Peer::new(channel, admission, context, owner_liveness);
        let (completion, status) = watch::channel(None);
        Ok((
            Self {
                peer,
                socket: Some(socket),
                wake,
                completion,
                finished: false,
            },
            Status(status),
        ))
    }

    pub(super) async fn run(mut self) -> Result<(), Error> {
        let result = self.drive().await;
        self.finish(result)
    }

    async fn drive(&mut self) -> Result<(), Error> {
        let mut budget = 0;
        loop {
            match self.peer.step()? {
                Step::Closed => return Ok(()),
                Step::Blocked => {
                    budget = 0;
                    self.wait().await?;
                }
                Step::Advanced => {
                    budget += 1;
                    if budget >= FAIRNESS_BUDGET {
                        budget = 0;
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }

    async fn wait(&mut self) -> Result<(), Error> {
        #[cfg(unix)]
        let socket = self.socket.as_ref().ok_or(Error::Closed)?;
        let interest = self.peer.io_interest();
        let deadline = self.peer.next_deadline().ok_or(Error::Closed)?;
        #[cfg(unix)]
        tokio::select! {
            biased;
            _ = self.wake.notified() => Ok(()),
            result = socket.readable(), if interest.readable => {
                result.map_err(|error| Error::Channel(secure_transport::Error::Io(error.kind())))
            }
            result = socket.writable(), if interest.writable => {
                result.map_err(|error| Error::Channel(secure_transport::Error::Io(error.kind())))
            }
            _ = tokio::time::sleep_until(deadline.into()) => Ok(()),
        }
        #[cfg(windows)]
        tokio::select! {
            biased;
            _ = self.wake.notified() => Ok(()),
            _ = tokio::time::sleep(PIPE_RETRY),
                if interest.readable || interest.writable => Ok(()),
            _ = tokio::time::sleep_until(deadline.into()) => Ok(()),
        }
    }

    fn finish(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        if self.finished {
            return result;
        }
        let result = match self.peer.finish() {
            Ok(()) => result,
            Err(error) => Err(error),
        };
        self.socket.take();
        self.finished = true;
        self.completion.send_replace(Some(result));
        result
    }
}

impl<N: HostVerification, A: NativeAdmission> Drop for Owner<N, A> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(Err(Error::OwnerInterrupted));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn lost_completion_is_distinct_from_completed_cleanup() {
        let (sender, receiver) = watch::channel(None);
        drop(sender);
        assert_eq!(Status(receiver).wait().await, Err(Error::OwnerLost));
    }
}
