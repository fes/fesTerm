use sha2::{Digest, Sha256};

/// Immutable binding to one cryptographically established SSH connection and
/// channel pair. This is not peer authorization; the client must still enforce
/// its host-key policy and the application must authenticate its principal.
///
/// There is deliberately no constructor or deserializer outside this crate.
#[derive(Clone, Eq, PartialEq)]
pub struct ChannelBinding([u8; 32]);

impl ChannelBinding {
    pub(crate) fn new(session_id: &[u8], client_channel: u32, server_channel: u32) -> Option<Self> {
        if session_id.len() < 32 {
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(b"russh SSH connection/channel binding v1\0");
        hash.update((session_id.len() as u32).to_be_bytes());
        hash.update(session_id);
        hash.update(b"client-channel\0");
        hash.update(client_channel.to_be_bytes());
        hash.update(b"server-channel\0");
        hash.update(server_channel.to_be_bytes());
        Some(Self(hash.finalize().into()))
    }

    /// Application protocols must use their own role/domain-separated
    /// transcript, never treat these bytes as a bearer credential.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for ChannelBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelBinding").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_channel_binding_separates_connection_and_role_ordered_channel_ids() {
        let binding = ChannelBinding::new(&[1; 32], 3, 7);
        assert_eq!(binding, ChannelBinding::new(&[1; 32], 3, 7));
        assert_ne!(binding, ChannelBinding::new(&[2; 32], 3, 7));
        assert_ne!(binding, ChannelBinding::new(&[1; 32], 7, 3));
        assert_ne!(binding, ChannelBinding::new(&[1; 32], 3, 8));
        assert!(ChannelBinding::new(&[], 3, 7).is_none());
        assert!(ChannelBinding::new(&[1; 31], 3, 7).is_none());
    }
}
