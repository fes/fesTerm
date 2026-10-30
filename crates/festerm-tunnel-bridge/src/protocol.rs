use std::io;

use festerm_core::{ColorScheme, Terminal};
use festerm_sessiond::RecoverySyncCommand;
use serde::{Deserialize, Serialize};

pub const VERSION: u16 = 1;
pub const SNAPSHOT_LIMIT: usize = 8 * 1024 * 1024;
pub const CHUNK_LIMIT: usize = 64 * 1024;
pub const HELLO: u8 = 1;
pub const SNAPSHOT: u8 = 2;
pub const ADOPTED: u8 = 3;
pub const INPUT: u8 = 4;
pub const RESIZE: u8 = 5;
pub const OUTPUT: u8 = 6;
pub const RESIZED: u8 = 7;
pub const DETACH: u8 = 8;
pub const REFUSED: u8 = 9;
pub const SYNC: u8 = 10;
pub const EXITED: u8 = 11;
pub const CAPABILITIES: u8 = 12;
pub const DISCOVER: u8 = 13;
pub const DESCRIPTION: u8 = 14;
pub const FILE_READ: u8 = 20;
pub const FILE_RESULT: u8 = 21;
pub const FILE_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub name: String,
    pub pid: u32,
    /// String avoids JSON u128 interoperability surprises.
    pub generation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    #[serde(default)]
    pub files: FileAccess,
    pub version: u16,
    pub snapshot_schema: u16,
    pub target: Target,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    pub version: u16,
    pub target: Target,
    pub files: FileAccess,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileAccess {
    pub read: bool,
    pub windows_paths: bool,
}

#[derive(Deserialize, Serialize)]
pub enum Sync {
    Scrollback(usize),
    Colors(ColorScheme),
    Bytes(Vec<u8>),
    Reset,
}

impl TryFrom<Sync> for RecoverySyncCommand {
    type Error = io::Error;
    fn try_from(value: Sync) -> io::Result<Self> {
        Ok(match value {
            Sync::Scrollback(limit_bytes) if limit_bytes <= SNAPSHOT_LIMIT => {
                Self::SetScrollbackLimit { limit_bytes }
            }
            Sync::Colors(colors) => Self::SetColorScheme(colors),
            Sync::Bytes(bytes) if bytes.len() <= CHUNK_LIMIT => Self::MirrorBytes(bytes),
            Sync::Reset => Self::ResetToInitialState,
            _ => return Err(invalid()),
        })
    }
}

impl From<RecoverySyncCommand> for Sync {
    fn from(value: RecoverySyncCommand) -> Self {
        match value {
            RecoverySyncCommand::SetScrollbackLimit { limit_bytes } => {
                Self::Scrollback(limit_bytes)
            }
            RecoverySyncCommand::SetColorScheme(colors) => Self::Colors(colors),
            RecoverySyncCommand::MirrorBytes(bytes) => Self::Bytes(bytes),
            RecoverySyncCommand::ResetToInitialState => Self::Reset,
        }
    }
}

pub fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "experimental tunnel protocol refused",
    )
}

pub fn validate_length(kind: u8, length: usize) -> io::Result<()> {
    let maximum = match kind {
        FILE_READ => 8192,
        FILE_RESULT => FILE_LIMIT + 9,
        SNAPSHOT => SNAPSHOT_LIMIT,
        HELLO | DESCRIPTION => 4096,
        DISCOVER => 2,
        INPUT | OUTPUT | SYNC => CHUNK_LIMIT,
        RESIZE | RESIZED => 8,
        CAPABILITIES => 2,
        ADOPTED | DETACH | REFUSED | EXITED => 0,
        _ => return Err(invalid()),
    };
    if length > maximum {
        return Err(invalid());
    }
    Ok(())
}

/// JSON has no attacker-supplied Vec length prefixes. Its byte/depth limits are
/// enforced before decoding; logical allocation charges are checked before
/// restoring capacities. The trusted-local bincode decoder is not network-facing.
pub fn decode_snapshot(bytes: &[u8]) -> io::Result<Terminal> {
    validate_length(SNAPSHOT, bytes.len())?;
    let mut terminal: Terminal = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    terminal
        .validate_recovery_snapshot()
        .map_err(|_| invalid())?;
    let size = terminal.dimensions();
    if size.columns() > 256
        || size.rows() > 128
        || terminal.scrollback_stats().charged_bytes() > SNAPSHOT_LIMIT
        || !terminal.queued_input().is_empty()
        || !terminal.queued_replies().is_empty()
        || terminal.take_input_queue_overflowed()
        || terminal.take_reply_queue_overflowed()
    {
        return Err(invalid());
    }
    terminal.restore_recovery_snapshot_allocations();
    Ok(terminal)
}

pub fn encode_snapshot(terminal: &Terminal) -> io::Result<Vec<u8>> {
    struct Limited(Vec<u8>);
    impl io::Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.0.len() + bytes.len() > SNAPSHOT_LIMIT {
                return Err(invalid());
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut output = Limited(Vec::new());
    serde_json::to_writer(&mut output, terminal).map_err(|_| invalid())?;
    // Apply the same semantic/resource policy at both ends.
    decode_snapshot(&output.0)?;
    Ok(output.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Dimensions;

    #[test]
    fn remote_snapshot_roundtrips_and_rejects_malformed_and_oversized_state() {
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        terminal.ingest(b"retained \xe2\x9c\x93\r\n");
        let bytes = encode_snapshot(&terminal.recovery_clone()).unwrap();
        let recovered = decode_snapshot(&bytes).unwrap();
        assert_eq!(recovered.dimensions(), terminal.dimensions());
        assert_eq!(recovered.row_text(0), terminal.row_text(0));
        assert!(
            decode_snapshot(br#"{"primary":{"screen":{"rows":18446744073709551615}}}"#).is_err()
        );
        assert!(decode_snapshot(&vec![b' '; SNAPSHOT_LIMIT + 1]).is_err());
        let large = Terminal::new(Dimensions::new(257, 24).unwrap()).unwrap();
        assert!(encode_snapshot(&large).is_err());
        terminal.queue_input(b"must not replay");
        assert!(decode_snapshot(&serde_json::to_vec(&terminal).unwrap()).is_err());
        terminal.drain_input();
        terminal.queue_reply(b"must not duplicate");
        assert!(decode_snapshot(&serde_json::to_vec(&terminal).unwrap()).is_err());
    }

    #[test]
    fn bridge_refuses_unknown_operations_and_incompatible_negotiation() {
        assert!(validate_length(99, 0).is_err());
        assert!(validate_length(INPUT, CHUNK_LIMIT + 1).is_err());
        assert!(validate_length(ADOPTED, 1).is_err());
        assert!(RecoverySyncCommand::try_from(Sync::Scrollback(usize::MAX)).is_err());
    }
}
