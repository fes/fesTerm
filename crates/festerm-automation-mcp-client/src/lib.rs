//! Experimental fesTerm-specific transport for the official Rust MCP SDK.
//!
//! The transport accepts an already-connected Unix socket or synchronous
//! Windows named pipe. It does not discover an endpoint, authenticate a
//! process, qualify a native verifier or privacy boundary, or enable any
//! fesTerm feature. Both peers must obtain
//! independent trusted human comparisons of their complete fingerprints.
//! A matching handshake and stream EOF do not defeat an active relay.
//!
//! A successful send means only that the encrypted frame was physically
//! written to the local socket, not that the broker admitted an action. A
//! cancelled send or lost response can have an unknown outcome; never replay
//! it automatically. Issue #524's durable uncertain-action interlock belongs
//! to the client using this transport.
//!
//! This implementation has no qualified production endpoint on any platform.
//! In particular, Unix support does not establish
//! iOS cross-application IPC or device/App Store feasibility.

#[cfg(any(unix, windows))]
mod direct;

#[cfg(any(unix, windows))]
pub use direct::{
    AdmissionCleanupFailed, BoundaryWake, ClientWriteAdmission, ConnectorStatus,
    DirectClientTransport, Error,
};

#[cfg(windows)]
pub use festerm_windows_security::named_pipe::Pipe as WindowsPipe;
