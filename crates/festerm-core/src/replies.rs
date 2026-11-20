use serde::{Deserialize, Serialize};

use crate::TRANSPORT_QUEUE_HIGH_WATERMARK;

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct ReplyQueue {
    bytes: Vec<u8>,
    #[serde(skip)]
    reserved: bool,
}

impl Clone for ReplyQueue {
    fn clone(&self) -> Self {
        let mut bytes = if self.reserved {
            Vec::with_capacity(TRANSPORT_QUEUE_HIGH_WATERMARK)
        } else {
            Vec::with_capacity(self.bytes.len())
        };
        bytes.extend_from_slice(&self.bytes);
        Self {
            bytes,
            reserved: self.reserved,
        }
    }
}

impl ReplyQueue {
    pub(crate) fn reserve(&mut self) -> Option<()> {
        let remaining = TRANSPORT_QUEUE_HIGH_WATERMARK.checked_sub(self.bytes.len())?;
        self.bytes.try_reserve_exact(remaining).ok()?;
        self.reserved = true;
        Some(())
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> QueuePushResult {
        queue_transport_bytes(&mut self.bytes, bytes)
    }

    pub(crate) fn queued(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn clear(&mut self) {
        self.bytes.clear();
    }

    pub(crate) fn drain(&mut self) -> Vec<u8> {
        if self.reserved {
            let bytes = self.bytes.clone();
            self.clear();
            bytes
        } else {
            std::mem::take(&mut self.bytes)
        }
    }
}

impl From<Vec<u8>> for ReplyQueue {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            reserved: false,
        }
    }
}

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
