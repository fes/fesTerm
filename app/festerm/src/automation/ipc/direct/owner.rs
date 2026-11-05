//! Readiness-driven ownership for one dormant direct host peer.

use super::{retire_admission, Error, NativeAdmission, Peer, PeerContext, Step, Wake};
use festerm_automation_mcp::secure_transport::{
    self, Role, SecureTransport, TrustedNativeVerification,
};
use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Duration,
};
use tokio::{net::UnixStream, sync::watch};

const FAIRNESS_BUDGET: usize = 32;

struct RegisteredStream(Arc<UnixStream>);

impl Read for RegisteredStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.try_read(bytes)
    }
}

impl Write for RegisteredStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.try_write(bytes)
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
/// connection closes wake it after releasing admission. This loop does not
/// qualify those boundaries or attach a listener. Keep `Status` until cleanup
/// finishes before reusing a native surface; failed or unobserved cleanup
/// requires quarantine.
/// An interrupted owner may have been cancelled or panicked; its task result
/// must also be observed.
pub(super) struct Owner<N: TrustedNativeVerification, A: NativeAdmission> {
    peer: Peer<RegisteredStream, N, A>,
    socket: Option<Arc<UnixStream>>,
    wake: Wake,
    completion: watch::Sender<Option<Result<(), Error>>>,
    finished: bool,
}

impl<N: TrustedNativeVerification, A: NativeAdmission> Owner<N, A> {
    pub(super) fn new(
        socket: UnixStream,
        native: N,
        admission: A,
        context: PeerContext,
        bootstrap_duration: Duration,
        frame_duration: Duration,
    ) -> Result<(Self, Status), Error> {
        let socket = Arc::new(socket);
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
        let peer = Peer::new(channel, admission, context);
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
        let socket = self.socket.as_ref().ok_or(Error::Closed)?;
        let interest = self.peer.io_interest();
        let deadline = self.peer.next_deadline().ok_or(Error::Closed)?;
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

impl<N: TrustedNativeVerification, A: NativeAdmission> Drop for Owner<N, A> {
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
