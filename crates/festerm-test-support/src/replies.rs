//! Framing oracle shared by parser property tests and fuzz targets.

/// Checks each CSI, DCS, and OSC frame in the terminal's ASCII reply stream.
///
/// A drain can concatenate different reply kinds. In particular, a stream
/// beginning with DCS need not end with ST if a complete CSI reply follows it.
pub fn terminal_replies_are_complete(mut replies: &[u8]) -> bool {
    while !replies.is_empty() {
        let Some((&kind, body)) = replies
            .strip_prefix(b"\x1b")
            .and_then(|rest| rest.split_first())
        else {
            return false;
        };
        replies = match kind {
            b'[' => {
                let parameters = body
                    .iter()
                    .take_while(|&&byte| (0x30..=0x3f).contains(&byte))
                    .count();
                let intermediates = body[parameters..]
                    .iter()
                    .take_while(|&&byte| (0x20..=0x2f).contains(&byte))
                    .count();
                let Some((&final_byte, rest)) = body[parameters + intermediates..].split_first()
                else {
                    return false;
                };
                if !(0x40..=0x7e).contains(&final_byte) {
                    return false;
                }
                rest
            }
            b'P' | b']' => {
                let payload = body
                    .iter()
                    .take_while(|&&byte| (0x20..=0x7e).contains(&byte))
                    .count();
                let terminator = &body[payload..];
                if let Some(rest) = terminator.strip_prefix(b"\x1b\\") {
                    rest
                } else if let Some(rest) = terminator.strip_prefix(b"\x07").filter(|_| kind == b']')
                {
                    rest
                } else {
                    return false;
                }
            }
            _ => return false,
        };
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_complete_concatenated_reply_frames() {
        let streams: &[&[u8]] = &[
            b"",
            b"\x1b[0n",
            b"\x1b[?25;1$y",
            b"\x1bP1$r0m\x1b\\\x1b[0n",
            b"\x1bP0$r\x1b\\\x1bP1$r0m\x1b\\",
            b"\x1bP0$r\x1b\\\x1b[2;2R\x1b[3;1R\x1b[>0;0;0c",
            b"\x1b]10;rgb:ffff/ffff/ffff\x07\x1b[1;1R",
            b"\x1b[?6c\x1b]11;rgb:0000/0000/0000\x1b\\\x1bP0$r\x1b\\",
        ];
        for stream in streams {
            assert!(terminal_replies_are_complete(stream), "{stream:?}");
        }
    }

    #[test]
    fn rejects_incomplete_or_malformed_reply_frames_anywhere_in_the_stream() {
        let streams: &[&[u8]] = &[
            b"\x1b",
            b"\x1b[",
            b"\x1b[?25;1$",
            b"\x1b[1$2y",
            b"\x1bP1$r0m",
            b"\x1bP1$r0m\x07",
            b"\x1b]10;rgb:ffff/ffff/ffff",
            b"\x1bP1$r0m\x1b[0n\x1b\\",
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b[0n",
            b"\x1b[0n\x1bP1$r0m",
            b"\x1bP1$r0m\x1b\\\x1b[",
            b"\x1bP1$r\0m\x1b\\",
            b"\x1b[0ntrailing text",
        ];
        for stream in streams {
            assert!(!terminal_replies_are_complete(stream), "{stream:?}");
        }
    }
}
