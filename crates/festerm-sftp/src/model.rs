use crate::cancel::CancelToken;
use std::cmp::Ordering;

/// Hard ceiling for entries scanned in one directory, whatever the caller asks.
pub const ABSOLUTE_MAX_SCAN: usize = 100_000;
pub const DEFAULT_MAX_SCAN: usize = 20_000;
pub const DEFAULT_MAX_ENTRIES: usize = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// What a symbolic link resolves to, as far as the pane's policy allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymlinkInfo {
    /// `None` when the link dangles, cannot be resolved, or (local side)
    /// resolves outside the sandbox root.
    pub target_kind: Option<EntryKind>,
    /// Link text as stored, when readable.
    pub target: Option<String>,
    /// Local side only: the link leaves the sandbox root and is not followable.
    pub escapes_root: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Name exactly as the file system reports it.
    pub name: String,
    /// `name` with control and bidirectional-override characters replaced,
    /// safe to paint.
    pub display_name: String,
    /// Kind of the entry itself (not followed).
    pub kind: EntryKind,
    /// Size in bytes; for a resolvable symlink, the target's size.
    pub size: Option<u64>,
    /// Modification time in seconds since the Unix epoch.
    pub modified: Option<i64>,
    /// POSIX permission bits (low 12 bits), when known.
    pub permissions: Option<u32>,
    pub symlink: Option<SymlinkInfo>,
    pub hidden: bool,
    /// False when the name could not safely become a local file name
    /// (NUL, separators, control/bidi characters, `.`/`..`, over-long,
    /// non-UTF-8). Such entries are listed but refused for transfers.
    pub name_safe: bool,
}

impl Entry {
    /// Kind after following a resolvable symlink; symlinks that do not
    /// resolve stay `Symlink`.
    pub fn effective_kind(&self) -> EntryKind {
        match (&self.kind, &self.symlink) {
            (EntryKind::Symlink, Some(info)) => info.target_kind.unwrap_or(EntryKind::Symlink),
            (kind, _) => *kind,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.effective_kind() == EntryKind::Directory
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

#[derive(Clone, Debug)]
pub struct ListOptions {
    /// Raw entries examined per directory (bounds memory and round trips).
    pub max_scan: usize,
    /// Entries returned after hidden/filter handling and sorting.
    pub max_entries: usize,
    pub show_hidden: bool,
    /// Case-insensitive substring over the name.
    pub filter: Option<String>,
    pub sort: SortKey,
    pub order: SortOrder,
    pub directories_first: bool,
    /// Optional cancellation of an in-flight remote listing.
    pub cancel: Option<CancelToken>,
}

impl Default for ListOptions {
    fn default() -> Self {
        Self {
            max_scan: DEFAULT_MAX_SCAN,
            max_entries: DEFAULT_MAX_ENTRIES,
            show_hidden: false,
            filter: None,
            sort: SortKey::Name,
            order: SortOrder::Ascending,
            directories_first: true,
            cancel: None,
        }
    }
}

impl ListOptions {
    pub(crate) fn scan_limit(&self) -> usize {
        self.max_scan.clamp(1, ABSOLUTE_MAX_SCAN)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// Normalized directory path that was listed.
    pub path: String,
    pub entries: Vec<Entry>,
    /// Raw entries examined (excluding `.` and `..`).
    pub scanned: usize,
    pub hidden_count: usize,
    /// True when either the scan bound or the result bound cut the listing.
    pub truncated: bool,
}

/// Applies hidden/filter/sort/result-bound handling to scanned entries.
pub(crate) fn finish_listing(
    path: String,
    mut entries: Vec<Entry>,
    scan_truncated: bool,
    options: &ListOptions,
) -> Listing {
    let scanned = entries.len();
    let mut hidden_count = 0;
    if !options.show_hidden {
        entries.retain(|entry| {
            if entry.hidden {
                hidden_count += 1;
                false
            } else {
                true
            }
        });
    }
    if let Some(filter) = options.filter.as_deref().filter(|f| !f.is_empty()) {
        let needle = filter.to_lowercase();
        entries.retain(|entry| entry.display_name.to_lowercase().contains(&needle));
    }
    entries.sort_by(|a, b| compare(a, b, options));
    let result_truncated = entries.len() > options.max_entries;
    entries.truncate(options.max_entries);
    Listing {
        path,
        entries,
        scanned,
        hidden_count,
        truncated: scan_truncated || result_truncated,
    }
}

fn compare(a: &Entry, b: &Entry, options: &ListOptions) -> Ordering {
    if options.directories_first {
        let ordering = b.is_dir().cmp(&a.is_dir());
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    let primary = match options.sort {
        SortKey::Name => Ordering::Equal,
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Modified => a.modified.cmp(&b.modified),
        SortKey::Kind => kind_rank(a).cmp(&kind_rank(b)),
    };
    let primary = match options.order {
        SortOrder::Ascending => primary,
        SortOrder::Descending => primary.reverse(),
    };
    let name = a
        .display_name
        .to_lowercase()
        .cmp(&b.display_name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name));
    let name = if options.sort == SortKey::Name && options.order == SortOrder::Descending {
        name.reverse()
    } else {
        name
    };
    primary.then(name)
}

fn kind_rank(entry: &Entry) -> u8 {
    match entry.effective_kind() {
        EntryKind::Directory => 0,
        EntryKind::File => 1,
        EntryKind::Symlink => 2,
        EntryKind::Other => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            display_name: name.to_owned(),
            kind,
            size: Some(size),
            modified: Some(size as i64),
            permissions: None,
            symlink: None,
            hidden: name.starts_with('.'),
            name_safe: true,
        }
    }

    #[test]
    fn listing_hides_filters_sorts_and_bounds() {
        let entries = vec![
            entry("b.txt", EntryKind::File, 5),
            entry("A.txt", EntryKind::File, 9),
            entry(".secret", EntryKind::File, 1),
            entry("zdir", EntryKind::Directory, 0),
        ];
        let listing = finish_listing("/".into(), entries.clone(), false, &ListOptions::default());
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "A.txt", "b.txt"]);
        assert_eq!((listing.scanned, listing.hidden_count), (4, 1));

        let options = ListOptions {
            show_hidden: true,
            sort: SortKey::Size,
            order: SortOrder::Descending,
            max_entries: 2,
            ..ListOptions::default()
        };
        let listing = finish_listing("/".into(), entries.clone(), false, &options);
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zdir", "A.txt"]);
        assert!(listing.truncated);

        let options = ListOptions {
            filter: Some("B.T".into()),
            ..ListOptions::default()
        };
        let listing = finish_listing("/".into(), entries, false, &options);
        assert_eq!(listing.entries.len(), 1);
    }
}
