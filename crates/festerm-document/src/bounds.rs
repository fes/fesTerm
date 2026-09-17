//! Explicit editing bounds and honest refusal (ADR 0034 §11).
//!
//! A document beyond these limits is refused *before* an editable buffer is
//! allocated, and the refusal names the limit it exceeded. Refusing to edit
//! never means refusing to read: the Markdown viewer keeps its own, separate
//! bounds and can still show what it can show.

use std::fmt;

/// The limits an editable buffer must fit inside.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentBounds {
    max_bytes: usize,
    max_lines: usize,
    max_line_bytes: usize,
}

impl DocumentBounds {
    /// Maximum source size accepted for editing.
    ///
    /// Matches the Markdown viewer's snapshot limit so a document that can be
    /// previewed can also be edited; a file that is readable but not editable
    /// would be a confusing boundary to explain.
    pub const MAX_BYTES: usize = 4 * 1024 * 1024;
    /// Maximum decoded line count accepted for editing.
    pub const MAX_LINES: usize = 65_536;
    /// Maximum bytes in any single line.
    ///
    /// One enormous line is a different failure from a large file: layout and
    /// caret arithmetic degrade long before the byte budget is reached, so it
    /// is bounded separately and explained separately.
    pub const MAX_LINE_BYTES: usize = 64 * 1024;

    pub const DEFAULT: Self = Self {
        max_bytes: Self::MAX_BYTES,
        max_lines: Self::MAX_LINES,
        max_line_bytes: Self::MAX_LINE_BYTES,
    };

    pub const fn new(max_bytes: usize, max_lines: usize, max_line_bytes: usize) -> Self {
        Self {
            max_bytes,
            max_lines,
            max_line_bytes,
        }
    }

    pub const fn max_bytes(self) -> usize {
        self.max_bytes
    }

    pub const fn max_lines(self) -> usize {
        self.max_lines
    }

    pub const fn max_line_bytes(self) -> usize {
        self.max_line_bytes
    }

    /// Checks a declared size before any bytes are read, so an oversize file
    /// is refused without being pulled into memory first.
    pub const fn check_declared_size(self, bytes: usize) -> Result<(), RefusalReason> {
        if bytes > self.max_bytes {
            Err(RefusalReason::TooLarge {
                bytes,
                limit: self.max_bytes,
            })
        } else {
            Ok(())
        }
    }
}

impl Default for DocumentBounds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why a document cannot be opened for editing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefusalReason {
    TooLarge { bytes: usize, limit: usize },
    TooManyLines { lines: usize, limit: usize },
    LineTooLong { line: usize, limit: usize },
    NotUtf8,
    BinaryContent,
}

impl RefusalReason {
    /// The short, content-free sentence shown in place of the document.
    pub fn headline(self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "This file is too large to edit",
            Self::TooManyLines { .. } => "This file has too many lines to edit",
            Self::LineTooLong { .. } => "This file has a line that is too long to edit",
            Self::NotUtf8 => "This file is not valid UTF-8",
            Self::BinaryContent => "This file appears to be binary",
        }
    }

    /// The concrete limit, so the refusal can be acted on rather than guessed
    /// at. Never includes any of the document's content.
    pub fn detail(self) -> String {
        match self {
            Self::TooLarge { bytes, limit } => format!(
                "It is {} and the editing limit is {}. It can still be opened in the Markdown viewer.",
                describe_bytes(bytes),
                describe_bytes(limit)
            ),
            Self::TooManyLines { lines, limit } => {
                format!("It has {lines} lines and the editing limit is {limit}.")
            }
            Self::LineTooLong { line, limit } => format!(
                "Line {line} is longer than the {} editing limit for a single line.",
                describe_bytes(limit)
            ),
            Self::NotUtf8 => {
                "fesTerm edits UTF-8 text, and rewriting this file would corrupt it.".to_owned()
            }
            Self::BinaryContent => {
                "fesTerm edits text, and rewriting this file would corrupt it.".to_owned()
            }
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} — {}", self.headline(), self.detail())
    }
}

impl std::error::Error for RefusalReason {}

/// Formats a byte count the way the UI states a limit.
fn describe_bytes(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MB", bytes / MIB)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_oversize_file_is_refused_before_it_is_read() {
        let bounds = DocumentBounds::DEFAULT;
        let error = bounds
            .check_declared_size(DocumentBounds::MAX_BYTES + 1)
            .unwrap_err();
        assert!(matches!(error, RefusalReason::TooLarge { .. }));
        assert!(bounds
            .check_declared_size(DocumentBounds::MAX_BYTES)
            .is_ok());
    }

    #[test]
    fn every_refusal_states_its_limit_without_quoting_the_document() {
        let reasons = [
            RefusalReason::TooLarge {
                bytes: 8 * 1024 * 1024,
                limit: DocumentBounds::MAX_BYTES,
            },
            RefusalReason::TooManyLines {
                lines: 100_000,
                limit: DocumentBounds::MAX_LINES,
            },
            RefusalReason::LineTooLong {
                line: 12,
                limit: DocumentBounds::MAX_LINE_BYTES,
            },
            RefusalReason::NotUtf8,
            RefusalReason::BinaryContent,
        ];
        for reason in reasons {
            assert!(!reason.headline().is_empty());
            assert!(!reason.detail().is_empty());
            assert!(reason.to_string().contains(reason.headline()));
        }
    }

    #[test]
    fn an_oversize_refusal_points_at_the_viewer() {
        let reason = RefusalReason::TooLarge {
            bytes: 8 * 1024 * 1024,
            limit: DocumentBounds::MAX_BYTES,
        };
        assert_eq!(reason.detail(), "It is 8 MB and the editing limit is 4 MB. It can still be opened in the Markdown viewer.");
    }

    #[test]
    fn byte_counts_read_the_way_a_limit_is_stated() {
        assert_eq!(describe_bytes(512), "512 bytes");
        assert_eq!(describe_bytes(64 * 1024), "64 KB");
        assert_eq!(describe_bytes(4 * 1024 * 1024), "4 MB");
        assert_eq!(describe_bytes(6 * 1024 * 1024 / 4 * 3), "4.5 MB");
    }
}
