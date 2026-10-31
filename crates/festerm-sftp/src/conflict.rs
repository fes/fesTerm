/// How to resolve a transfer whose target already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// Write beside it as `name (1).ext`, `name (2).ext`, ...
    KeepBoth,
    /// Leave the existing file and do not transfer this one.
    Skip,
    /// Atomically replace the existing file once the new data is complete.
    Replace,
}

/// The facts the UI needs to show for one side of a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: Option<u64>,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub existing: Fingerprint,
    pub incoming: Fingerprint,
    /// The existing target is a directory; `Replace` is refused for it.
    pub existing_is_directory: bool,
    /// Same size and modification times within [`MTIME_TOLERANCE_SECS`].
    pub likely_identical: bool,
    /// What `KeepBoth` would name the new file right now.
    pub suggested_name: String,
}

/// SFTP v3 carries whole-second mtimes and some servers/file systems round
/// to two seconds (FAT), so smaller differences are not differences.
pub const MTIME_TOLERANCE_SECS: i64 = 2;

pub fn likely_identical(a: Fingerprint, b: Fingerprint) -> bool {
    match (a.size, b.size, a.modified, b.modified) {
        (Some(sa), Some(sb), Some(ma), Some(mb)) => {
            sa == sb && (ma - mb).abs() <= MTIME_TOLERANCE_SECS
        }
        _ => false,
    }
}

/// `report.pdf` -> `report (1).pdf`; a leading dot is not an extension separator.
pub fn keep_both_name(name: &str, number: u32) -> String {
    match name.rfind('.') {
        Some(index) if index > 0 && index + 1 < name.len() => {
            format!("{} ({number}){}", &name[..index], &name[index..])
        }
        _ => format!("{name} ({number})"),
    }
}

/// Highest `name (N)` probe before giving up.
pub const MAX_KEEP_BOTH_ATTEMPTS: u32 = 999;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_both_names_preserve_extensions() {
        assert_eq!(keep_both_name("report.pdf", 1), "report (1).pdf");
        assert_eq!(keep_both_name("archive.tar.gz", 2), "archive.tar (2).gz");
        assert_eq!(keep_both_name(".bashrc", 1), ".bashrc (1)");
        assert_eq!(keep_both_name("noext", 3), "noext (3)");
        assert_eq!(keep_both_name("trailing.", 1), "trailing. (1)");
    }

    #[test]
    fn identical_needs_size_and_time_within_tolerance() {
        let a = Fingerprint {
            size: Some(10),
            modified: Some(100),
        };
        assert!(likely_identical(
            a,
            Fingerprint {
                modified: Some(102),
                ..a
            }
        ));
        assert!(!likely_identical(
            a,
            Fingerprint {
                modified: Some(103),
                ..a
            }
        ));
        assert!(!likely_identical(
            a,
            Fingerprint {
                size: Some(11),
                ..a
            }
        ));
        assert!(!likely_identical(
            a,
            Fingerprint {
                modified: None,
                ..a
            }
        ));
    }
}
