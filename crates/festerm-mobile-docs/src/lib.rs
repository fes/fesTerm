//! Read-only document viewer model for the iOS host (ADR 0059 note in 0058).
//!
//! Input is bytes from the app sandbox or an SFTP read (anything `Read`),
//! read under a hard size cap. Output is a `festerm_markdown::MarkdownDocument`
//! plus link/image *decisions*: this crate never touches the filesystem or the
//! network, so every resource policy is deterministic and testable.

mod paths;
mod render;

use festerm_markdown::{
    MarkdownBounds, MarkdownCancellation, MarkdownDocument, MarkdownLoadError, MarkdownLoader,
    MarkdownSource, ResourceReferenceClass,
};
use std::{fmt, io::Read};

pub use paths::PathRejection;
pub use render::render_plain_text;

/// Mobile source cap, below the desktop 4 MiB loader limit.
pub const MOBILE_MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// One embedded image.
pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// Images admitted per document.
pub const MAX_IMAGES: usize = 16;
/// Decoded-bytes budget across all images of one document.
pub const MAX_TOTAL_IMAGE_BYTES: usize = 8 * 1024 * 1024;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "mkd"];

#[derive(Debug, Eq, PartialEq)]
pub enum ViewerError {
    /// More than `limit` bytes were available.
    TooLarge {
        limit: usize,
    },
    Read(std::io::ErrorKind),
    Markdown(MarkdownLoadError),
}

impl fmt::Display for ViewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "Document is larger than {limit} bytes"),
            Self::Read(kind) => write!(f, "Could not read the document ({kind})"),
            Self::Markdown(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ViewerError {}

/// Reads at most `cap` bytes; fails (without buffering more than `cap + 1`)
/// when the source is longer. `declared` (e.g. an SFTP `stat` size) rejects
/// early.
pub fn read_capped(
    reader: impl Read,
    declared: Option<u64>,
    cap: usize,
) -> Result<Vec<u8>, ViewerError> {
    if declared.is_some_and(|d| d > cap as u64) {
        return Err(ViewerError::TooLarge { limit: cap });
    }
    let mut bytes = Vec::new();
    reader
        .take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ViewerError::Read(e.kind()))?;
    if bytes.len() > cap {
        return Err(ViewerError::TooLarge { limit: cap });
    }
    Ok(bytes)
}

#[derive(Clone, Debug, Default)]
pub struct ViewerOptions {
    /// Directory (sandbox root or SFTP home/project) relative links and images
    /// must stay inside. `None` only forbids climbing above `/`.
    pub root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockReason {
    DangerousScheme,
    Path(PathRejection),
    /// Not an allow-listed image type (SVG files are excluded; inline SVG is
    /// handled by the Markdown model).
    UnsupportedImageType,
}

/// What the host should do when a link is tapped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkAction {
    /// Scroll to the heading or explicit anchor starting at `byte_offset`.
    ScrollTo {
        anchor: String,
        byte_offset: usize,
    },
    UnknownAnchor {
        anchor: String,
    },
    /// Open another Markdown file from the same source.
    OpenDocument {
        path: String,
        anchor: Option<String>,
    },
    /// Open any other same-source file with the host's file viewer.
    OpenFile {
        path: String,
    },
    /// Never fetched by this crate; the host must confirm and hand it to the OS.
    ExternalHttps {
        url: String,
    },
    Blocked(BlockReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImageAction {
    /// Read `path` from the same source, at most `max_bytes`, then call
    /// `ImageBudget::admit` with the real size.
    Load {
        path: String,
        max_bytes: usize,
    },
    /// Remote images are never fetched; show alt text with a placeholder.
    RemoteBlocked,
    Blocked(BlockReason),
}

/// Running per-document image accounting.
#[derive(Debug, Default)]
pub struct ImageBudget {
    count: usize,
    bytes: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub enum BudgetError {
    TooManyImages,
    ImageTooLarge,
    TotalTooLarge,
}

impl ImageBudget {
    pub fn admit(&mut self, size: usize) -> Result<(), BudgetError> {
        if size > MAX_IMAGE_BYTES {
            return Err(BudgetError::ImageTooLarge);
        }
        if self.count >= MAX_IMAGES {
            return Err(BudgetError::TooManyImages);
        }
        if self.bytes + size > MAX_TOTAL_IMAGE_BYTES {
            return Err(BudgetError::TotalTooLarge);
        }
        self.count += 1;
        self.bytes += size;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlineEntry {
    pub level: u8,
    pub text: String,
    pub anchor: String,
    pub byte_offset: usize,
}

pub struct DocumentViewer {
    document: MarkdownDocument,
    options: ViewerOptions,
    source_path: String,
}

impl DocumentViewer {
    /// Loads a document under the mobile caps. Tables, task lists, code blocks
    /// (highlighted with `festerm-syntax`) and anchors come from the model.
    pub fn open(
        source: MarkdownSource,
        declared_size: Option<u64>,
        reader: impl Read,
        options: ViewerOptions,
        cancellation: &MarkdownCancellation,
    ) -> Result<Self, ViewerError> {
        let bytes = read_capped(reader, declared_size, MOBILE_MAX_SOURCE_BYTES)?;
        let source_path = match &source {
            MarkdownSource::Local(l) => l.path().to_string_lossy().into_owned(),
            MarkdownSource::Remote(r) => r.remote_path().to_owned(),
        };
        let loader = MarkdownLoader::new(MarkdownBounds::DEFAULT);
        let document = loader
            .load(source, bytes.len(), &bytes, cancellation)
            .map_err(ViewerError::Markdown)?;
        Ok(Self {
            document,
            options,
            source_path,
        })
    }

    pub fn document(&self) -> &MarkdownDocument {
        &self.document
    }

    pub fn plain_text(&self) -> String {
        render_plain_text(&self.document)
    }

    pub fn outline(&self) -> Vec<OutlineEntry> {
        self.document
            .headings()
            .iter()
            .map(|h| OutlineEntry {
                level: h.level(),
                text: h.text().to_owned(),
                anchor: h.anchor().to_owned(),
                byte_offset: h.span().start().byte_offset(),
            })
            .collect()
    }

    fn anchor_offset(&self, anchor: &str) -> Option<usize> {
        let anchor = anchor.to_ascii_lowercase();
        self.document
            .headings()
            .iter()
            .find(|h| h.anchor() == anchor)
            .map(|h| h.span().start().byte_offset())
            .or_else(|| {
                self.document
                    .named_anchor(&anchor)
                    .map(|s| s.start().byte_offset())
            })
    }

    fn resolve(&self, path: &str) -> Result<String, PathRejection> {
        paths::resolve_relative(&self.source_path, self.options.root.as_deref(), path)
    }

    pub fn resolve_link(&self, target: &str) -> LinkAction {
        match self.document.source().classify_target(target) {
            ResourceReferenceClass::DangerousScheme => {
                LinkAction::Blocked(BlockReason::DangerousScheme)
            }
            ResourceReferenceClass::HttpsAbsolute => LinkAction::ExternalHttps {
                url: target.to_owned(),
            },
            ResourceReferenceClass::DocumentFragment => {
                let anchor = target.trim_start_matches('#').to_owned();
                match self.anchor_offset(&anchor) {
                    Some(byte_offset) => LinkAction::ScrollTo {
                        anchor,
                        byte_offset,
                    },
                    None => LinkAction::UnknownAnchor { anchor },
                }
            }
            _ => {
                let parts = paths::split_target(target);
                if parts.path.is_empty() {
                    return LinkAction::Blocked(BlockReason::Path(PathRejection::Empty));
                }
                match self.resolve(parts.path) {
                    Ok(path) if has_extension(&path, MARKDOWN_EXTENSIONS) => {
                        LinkAction::OpenDocument {
                            path,
                            anchor: parts.fragment.map(str::to_owned),
                        }
                    }
                    Ok(path) => LinkAction::OpenFile { path },
                    Err(e) => LinkAction::Blocked(BlockReason::Path(e)),
                }
            }
        }
    }

    pub fn resolve_image(&self, target: &str) -> ImageAction {
        match self.document.source().classify_target(target) {
            ResourceReferenceClass::HttpsAbsolute => ImageAction::RemoteBlocked,
            ResourceReferenceClass::LocalRelative
            | ResourceReferenceClass::RemoteRelativeViaSftpOrigin => {
                let parts = paths::split_target(target);
                match self.resolve(parts.path) {
                    Ok(path) if has_extension(&path, IMAGE_EXTENSIONS) => ImageAction::Load {
                        path,
                        max_bytes: MAX_IMAGE_BYTES,
                    },
                    Ok(_) => ImageAction::Blocked(BlockReason::UnsupportedImageType),
                    Err(e) => ImageAction::Blocked(BlockReason::Path(e)),
                }
            }
            _ => ImageAction::Blocked(BlockReason::DangerousScheme),
        }
    }
}

fn has_extension(path: &str, allowed: &[&str]) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| allowed.contains(&ext.to_ascii_lowercase().as_str()))
}
