use crate::TRANSPORT_QUEUE_HIGH_WATERMARK;

/// The observable outcome of adding bytes to a session transport queue.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueuePushResult {
    pub(crate) accepted: usize,
    pub(crate) overflowed: bool,
}

impl QueuePushResult {
    /// The number of bytes accepted, either the complete write or zero.
    pub const fn accepted(self) -> usize {
        self.accepted
    }

    /// Whether the write was rejected because the queue high watermark was met.
    pub const fn overflowed(self) -> bool {
        self.overflowed
    }
}

pub(crate) fn queue_transport_bytes(queue: &mut Vec<u8>, bytes: &[u8]) -> QueuePushResult {
    let Some(remaining) = TRANSPORT_QUEUE_HIGH_WATERMARK.checked_sub(queue.len()) else {
        return QueuePushResult {
            accepted: 0,
            overflowed: true,
        };
    };
    if bytes.len() > remaining || queue.try_reserve_exact(bytes.len()).is_err() {
        return QueuePushResult {
            accepted: 0,
            overflowed: true,
        };
    }
    queue.extend_from_slice(bytes);
    QueuePushResult {
        accepted: bytes.len(),
        overflowed: false,
    }
}
