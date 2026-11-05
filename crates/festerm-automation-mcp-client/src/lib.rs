//! Experimental fesTerm-specific transport for the official Rust MCP SDK.
//!
//! The Unix transport accepts an already-connected local socket. It does not
//! discover an endpoint, authenticate a process, qualify a native verifier or
//! privacy boundary, or enable any fesTerm feature. Both peers must obtain
//! independent trusted human comparisons of their complete fingerprints.
//! A matching handshake and socket EOF do not defeat an active relay.
//!
//! A successful send means only that the encrypted frame was physically
//! written to the local socket, not that the broker admitted an action. A
//! cancelled send or lost response can have an unknown outcome; never replay
//! it automatically. Issue #524's durable uncertain-action interlock belongs
//! to the client using this transport.
//!
//! This implementation is currently Unix-only and has no qualified production
//! endpoint on any platform. In particular, Unix support does not establish
//! iOS cross-application IPC or device/App Store feasibility.

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::{
    AdmissionCleanupFailed, BoundaryWake, ClientWriteAdmission, ConnectorStatus,
    DirectClientTransport, Error,
};
