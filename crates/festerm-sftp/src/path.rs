//! Name and path sanitization shared by both panes and the transfer engine.

use crate::error::{Result, SftpError};

pub const MAX_NAME_BYTES: usize = 255;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_PATH_DEPTH: usize = 128;

fn is_bidi_or_invisible_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Why `name` cannot be used as a single path component, if it cannot.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("empty name")
    } else if name == "." || name == ".." {
        Some("dot name")
    } else if name.len() > MAX_NAME_BYTES {
        Some("name too long")
    } else if name.contains('\0') {
        Some("NUL in name")
    } else if name.contains('/') || name.contains('\\') {
        Some("path separator in name")
    } else if name.chars().any(|c| c.is_control()) {
        Some("control character in name")
    } else if name.chars().any(is_bidi_or_invisible_control) {
        Some("bidirectional or invisible control character in name")
    } else {
        None
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    match name_problem(name) {
        None => Ok(()),
        Some(reason) => Err(SftpError::InvalidPath(format!(
            "{reason}: {}",
            display_name(name)
        ))),
    }
}

/// Replaces control and bidirectional-override characters so a name is safe to paint.
pub fn display_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_control() || is_bidi_or_invisible_control(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Splits a sandbox-relative path (`"" ` is the root) into validated
/// components. `.` and empty components are dropped; `..`, NUL, control
/// characters and over-long input are rejected, so the result can never
/// ascend.
pub fn local_components(relative: &str) -> Result<Vec<&str>> {
    if relative.len() > MAX_PATH_BYTES {
        return Err(SftpError::InvalidPath("path too long".into()));
    }
    if relative.starts_with('/') && relative.trim_start_matches('/').is_empty() {
        return Ok(Vec::new());
    }
    let mut parts = Vec::new();
    for part in relative.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        validate_name(part)?;
        parts.push(part);
    }
    if parts.len() > MAX_PATH_DEPTH {
        return Err(SftpError::InvalidPath("path too deep".into()));
    }
    Ok(parts)
}

/// Normalized form of [`local_components`], joined with `/`.
pub fn normalize_local(relative: &str) -> Result<String> {
    Ok(local_components(relative)?.join("/"))
}

pub fn join_local(dir: &str, name: &str) -> Result<String> {
    validate_name(name)?;
    let dir = normalize_local(dir)?;
    Ok(if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    })
}

pub fn parent_local(path: &str) -> Option<String> {
    let parts = local_components(path).ok()?;
    if parts.is_empty() {
        None
    } else {
        Some(parts[..parts.len() - 1].join("/"))
    }
}

/// Lexically normalizes a remote path. The server owns symlink resolution, so
/// `..` is collapsed textually and can never rise above `/` (absolute) or
/// become a leading `..` (relative paths are rejected if they would).
pub fn normalize_remote(path: &str) -> Result<String> {
    if path.is_empty() {
        return Err(SftpError::InvalidPath("empty remote path".into()));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(SftpError::InvalidPath("remote path too long".into()));
    }
    // Other control characters are legal in POSIX names and harmless on the wire;
    // only NUL is refused here. Local names are validated separately.
    if path.contains('\0') {
        return Err(SftpError::InvalidPath("NUL in remote path".into()));
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() && !absolute {
                    return Err(SftpError::InvalidPath(
                        "relative remote path escapes its base".into(),
                    ));
                }
            }
            part => {
                if part.len() > MAX_NAME_BYTES {
                    return Err(SftpError::InvalidPath("remote name too long".into()));
                }
                parts.push(part);
            }
        }
    }
    if parts.len() > MAX_PATH_DEPTH {
        return Err(SftpError::InvalidPath("remote path too deep".into()));
    }
    Ok(if absolute {
        format!("/{}", parts.join("/"))
    } else if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    })
}

pub fn join_remote(dir: &str, name: &str) -> Result<String> {
    validate_name(name)?;
    let dir = normalize_remote(dir)?;
    Ok(if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    })
}

pub fn parent_remote(path: &str) -> Option<String> {
    let path = normalize_remote(path).ok()?;
    if path == "/" || path == "." {
        return None;
    }
    match path.rfind('/') {
        Some(0) => Some("/".to_owned()),
        Some(index) => Some(path[..index].to_owned()),
        None => Some(".".to_owned()),
    }
}

pub fn file_name_remote(path: &str) -> Result<String> {
    let path = normalize_remote(path)?;
    let name = path.rsplit('/').next().unwrap_or("");
    validate_name(name)?;
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reject_traversal_nul_separators_controls_and_bidi() {
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            "a\nb",
            "a\u{202E}b",
            "\u{7f}",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_name(&"x".repeat(256)).is_err());
        for good in ["a b", "é.txt", ".hidden", "...", "a..b", "日本語"] {
            assert!(validate_name(good).is_ok(), "{good:?}");
        }
        assert_eq!(display_name("a\u{202E}b\n"), "a\u{FFFD}b\u{FFFD}");
    }

    #[test]
    fn local_paths_never_ascend() {
        assert_eq!(normalize_local("").unwrap(), "");
        assert_eq!(normalize_local("/").unwrap(), "");
        assert_eq!(normalize_local("a//./b/").unwrap(), "a/b");
        for bad in ["..", "a/../b", "../x", "a/\0"] {
            assert!(normalize_local(bad).is_err(), "{bad}");
        }
        assert_eq!(join_local("a", "b").unwrap(), "a/b");
        assert_eq!(join_local("", "b").unwrap(), "b");
        assert_eq!(parent_local("a/b").as_deref(), Some("a"));
        assert_eq!(parent_local("a").as_deref(), Some(""));
        assert_eq!(parent_local(""), None);
    }

    #[test]
    fn remote_paths_collapse_without_rising_above_root() {
        assert_eq!(normalize_remote("/a/../../b/./c").unwrap(), "/b/c");
        assert_eq!(normalize_remote("/..").unwrap(), "/");
        assert!(normalize_remote("../x").is_err());
        assert!(normalize_remote("/a\0b").is_err());
        assert_eq!(join_remote("/", "x").unwrap(), "/x");
        assert!(join_remote("/", "..").is_err());
        assert_eq!(parent_remote("/a/b").as_deref(), Some("/a"));
        assert_eq!(parent_remote("/a").as_deref(), Some("/"));
        assert_eq!(parent_remote("/"), None);
        assert_eq!(file_name_remote("/a/b.txt").unwrap(), "b.txt");
    }
}
