//! Same-source relative path resolution. Pure string work: no I/O, no
//! canonicalisation, so it behaves identically for sandbox and SFTP paths.

const MAX_TARGET_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathRejection {
    TooLong,
    Empty,
    /// Escapes the source root (or the filesystem root).
    EscapesRoot,
    /// Bad percent-escape, encoded separator/NUL, or backslash.
    Malformed,
}

/// A target split into its path and optional fragment.
pub struct Target<'a> {
    pub path: &'a str,
    pub fragment: Option<&'a str>,
}

pub fn split_target(target: &str) -> Target<'_> {
    let (before_fragment, fragment) = match target.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (target, None),
    };
    let path = before_fragment
        .split_once('?')
        .map_or(before_fragment, |(p, _)| p);
    Target { path, fragment }
}

fn percent_decode(segment: &str) -> Result<String, PathRejection> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or(PathRejection::Malformed)?;
            let hex = std::str::from_utf8(hex).map_err(|_| PathRejection::Malformed)?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| PathRejection::Malformed)?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(out).map_err(|_| PathRejection::Malformed)?;
    if decoded.contains(['/', '\\', '\0']) || decoded.chars().any(char::is_control) {
        return Err(PathRejection::Malformed);
    }
    Ok(decoded)
}

/// Resolves `relative` against the directory of `source_path`.
/// `root`, when given, is a directory the result must stay inside.
pub fn resolve_relative(
    source_path: &str,
    root: Option<&str>,
    relative: &str,
) -> Result<String, PathRejection> {
    if relative.len() > MAX_TARGET_BYTES {
        return Err(PathRejection::TooLong);
    }
    if relative.is_empty() {
        return Err(PathRejection::Empty);
    }
    if relative.contains('\\') {
        return Err(PathRejection::Malformed);
    }
    let absolute = source_path.starts_with('/');
    let mut parts: Vec<String> = source_path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_owned)
        .collect();
    parts.pop(); // the document file itself
    for raw in relative.split('/') {
        match raw {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or(PathRejection::EscapesRoot)?;
            }
            _ => {
                let segment = percent_decode(raw)?;
                if segment == ".." || segment == "." {
                    return Err(PathRejection::Malformed);
                }
                parts.push(segment);
            }
        }
    }
    if parts.is_empty() {
        return Err(PathRejection::Empty);
    }
    if let Some(root) = root {
        let root_parts: Vec<&str> = root.split('/').filter(|s| !s.is_empty()).collect();
        let inside =
            parts.len() > root_parts.len() && root_parts.iter().zip(&parts).all(|(r, p)| r == p);
        if !inside {
            return Err(PathRejection::EscapesRoot);
        }
    }
    let joined = parts.join("/");
    Ok(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}
