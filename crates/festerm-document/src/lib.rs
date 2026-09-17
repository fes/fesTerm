//! Shared mutable text documents for fesTerm's native editor (ADR 0034).
//!
//! This crate owns everything about an editable document that does not depend
//! on a GUI, a filesystem, or a network: its canonical identity, its text and
//! the encoding/line-ending policy that text must be written back with, its
//! undo history, its dirty and conflict state, the bounds beyond which a
//! document is refused, and the line-oriented comparison Compare renders.
//!
//! The split ADR 0034 §1 draws is the organising rule here: everything in this
//! crate is **document-scoped**, shared by every view of one file. Caret,
//! selection, scroll, find cursor, line numbers, fixed columns, and vi mode
//! are **view-scoped** and deliberately live in the UI layer instead.

mod bounds;
mod diff;
mod identity;
mod status;
mod text;
mod undo;

pub use bounds::{DocumentBounds, RefusalReason};
pub use diff::{DiffLine, DiffSide, LineChange, LineComparison, LineComparisonRow};
pub use identity::{
    DocumentId, DocumentKey, DocumentOrigin, LocalOrigin, OriginError, RemoteOrigin, RemoteOwner,
};
pub use status::{
    AutoSaveControl, Availability, BannerAction, ConflictState, DocumentStatus, SaveError,
    SaveOutcome, SaveProgress, Severity, StatusInputs, UnavailableReason,
};
pub use text::{Encoding, Indentation, LineEnding, TextDocument, TextEdit};
pub use undo::UndoHistory;
