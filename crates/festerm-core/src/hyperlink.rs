/// Normalizes passive OSC 8 metadata, not permission to open a target.
pub fn normalize_terminal_hyperlink(target: &str) -> Option<String> {
    crate::normalize_external_web_url(target)
        .or_else(|| local_file_uri(target).map(|uri| uri.to_string()))
}

/// Decodes a syntactically local, absolute file URI without filesystem access.
/// The application must separately authorize the originating session and path.
pub fn local_file_hyperlink_path(target: &str) -> Option<String> {
    let uri = local_file_uri(target)?;
    decode_path(uri.path())
}

fn local_file_uri(target: &str) -> Option<url::Url> {
    if target.len() > 2_048
        || !target.is_ascii()
        || target
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || !target.get(..7)?.eq_ignore_ascii_case("file://")
        || target.contains(['\\', '?', '#'])
    {
        return None;
    }
    let (authority, path) = target[7..].split_once('/')?;
    if (!authority.is_empty() && !authority.eq_ignore_ascii_case("localhost"))
        || path.is_empty()
        || path.starts_with('/')
    {
        return None;
    }
    // Validate before URL normalization can hide encoded separators or dot segments.
    let decoded = decode_path(&format!("/{path}"))?;
    if !safe_file_path(&decoded) {
        return None;
    }
    let uri = url::Url::parse(target).ok()?;
    safe_file_path(&decode_path(uri.path())?).then_some(uri)
}

fn decode_path(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(decoded).ok()
}

fn safe_file_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.chars().any(|ch| {
            ch.is_control()
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        && path.split('/').skip(1).enumerate().all(|(index, part)| {
            if index == 0
                && part.len() == 2
                && part.as_bytes()[0].is_ascii_alphabetic()
                && part.ends_with(':')
            {
                return true;
            }
            let stem = part.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
            !part.contains([':', '*', '?', '"', '<', '>', '|'])
                && !part.ends_with([' ', '.'])
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
                && !["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|suffix| {
                        matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{00b9}" | "\u{00b2}" | "\u{00b3}")
                    })
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hyperlinks_decode_local_absolute_paths_without_widening_web_policy() {
        for (uri, path) in [
            ("file:///tmp/report%20one.md", "/tmp/report one.md"),
            ("FILE://LOCALHOST/Q:/src/report.md", "/Q:/src/report.md"),
            ("file:///tmp/%E7%95%8C.md", "/tmp/\u{754c}.md"),
            ("file:///tmp/literal%252F.md", "/tmp/literal%2F.md"),
        ] {
            let normalized = normalize_terminal_hyperlink(uri).unwrap();
            assert_eq!(
                local_file_hyperlink_path(&normalized).as_deref(),
                Some(path)
            );
            assert!(crate::normalize_external_web_url(&normalized).is_none());
            let mut terminal = crate::Terminal::new(crate::Dimensions::new(8, 2).unwrap()).unwrap();
            terminal.ingest(format!("\x1b]8;;{uri}\x07LINK\x1b]8;;\x07").as_bytes());
            assert_eq!(
                terminal.cell(0, 0).unwrap().hyperlink(),
                Some(normalized.as_str())
            );
            assert!(terminal.cell(4, 0).unwrap().hyperlink().is_none());
        }
    }

    #[test]
    fn file_hyperlinks_reject_ambiguous_network_device_and_encoded_targets() {
        for uri in [
            "file:relative.md",
            "file://host/tmp/report.md",
            "file://localhost:80/tmp/report.md",
            "file://user@localhost/tmp/report.md",
            "file:////host/share/report.md",
            "file:///%2Fhost/share/report.md",
            "file://localhost/%5Chost/share/report.md",
            "file:///C:/a%5Cb.md",
            "file:///C:/file.md:stream",
            "file:///C:/CON.txt",
            "file:///C:/CON%20.txt",
            "file:///C:/a/../NUL",
            "file:///C:/LPT1.txt",
            "file:///C:/COM%C2%B9",
            "file:///C:/name.",
            "file:///C:/name%20",
            "file:///C:/name%00.md",
            "file:///tmp/%1b.md",
            "file:///tmp/%E2%80%AE.md",
            "file:///tmp/%FF.md",
            "file:///tmp/%",
            "file:///tmp/%2",
            "file:///tmp/%GG",
            "file:///tmp/x.md?query",
            "file:///tmp/x.md#fragment",
            "Q:\\src\\report.md",
            "docs/report.md",
        ] {
            assert!(normalize_terminal_hyperlink(uri).is_none(), "{uri}");
        }
        assert!(normalize_terminal_hyperlink(&format!("file:///{}", "a".repeat(2_048))).is_none());
    }
}
