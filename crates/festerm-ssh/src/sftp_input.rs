pub(super) const MAX_SFTP_COMMAND_BYTES: usize = 256 * 1024;
const RETAINED_CAPACITY_BYTES: usize = 4 * 1024;
const RECLAIM_CAPACITY_THRESHOLD_BYTES: usize = 32 * 1024;

#[derive(Default)]
pub(super) struct SftpInputBuffer {
    bytes: Vec<u8>,
    refusing: bool,
    skip_line_feed: bool,
}

#[derive(Debug, PartialEq)]
pub(super) enum SftpInputAction {
    Echo(u8),
    Submit(Vec<u8>),
    Erase,
    Cancel,
    Refuse,
    RefusedLineEnded,
    Ignore,
}

impl SftpInputBuffer {
    pub(super) fn push(&mut self, byte: u8) -> SftpInputAction {
        if self.skip_line_feed {
            self.skip_line_feed = false;
            if byte == b'\n' {
                return SftpInputAction::Ignore;
            }
        }
        if byte == 0x03 {
            self.bytes = Vec::new();
            self.refusing = false;
            return SftpInputAction::Cancel;
        }
        if self.refusing {
            return match byte {
                b'\r' | b'\n' => {
                    self.refusing = false;
                    self.skip_line_feed = byte == b'\r';
                    SftpInputAction::RefusedLineEnded
                }
                _ => SftpInputAction::Ignore,
            };
        }
        match byte {
            b'\r' | b'\n' => SftpInputAction::Submit(std::mem::take(&mut self.bytes)),
            0x08 | 0x7f if !self.bytes.is_empty() => {
                self.pop_scalar();
                if self.bytes.is_empty() {
                    self.bytes = Vec::new();
                } else if self.bytes.capacity() >= RECLAIM_CAPACITY_THRESHOLD_BYTES
                    && self.bytes.len() <= RETAINED_CAPACITY_BYTES
                {
                    self.bytes.shrink_to(RETAINED_CAPACITY_BYTES);
                }
                SftpInputAction::Erase
            }
            byte if !byte.is_ascii_control() => {
                if self.bytes.len() == MAX_SFTP_COMMAND_BYTES {
                    self.bytes = Vec::new();
                    self.refusing = true;
                    SftpInputAction::Refuse
                } else {
                    self.bytes.push(byte);
                    SftpInputAction::Echo(byte)
                }
            }
            _ => SftpInputAction::Ignore,
        }
    }

    fn pop_scalar(&mut self) {
        let mut start = self.bytes.len() - 1;
        let earliest = self.bytes.len().saturating_sub(4);
        while start > earliest && self.bytes[start] & 0xc0 == 0x80 {
            start -= 1;
        }
        let suffix = &self.bytes[start..];
        // A valid unfinished UTF-8 prefix can span transport chunks.
        match std::str::from_utf8(suffix) {
            Ok(_) => self.bytes.truncate(start),
            Err(error) if error.error_len().is_none() => self.bytes.truncate(start),
            Err(_) => {
                self.bytes.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(input: &mut SftpInputBuffer, bytes: usize) {
        for _ in 0..bytes {
            assert_eq!(input.push(b'x'), SftpInputAction::Echo(b'x'));
        }
    }

    #[test]
    fn sftp_input_accepts_exactly_256_kib_and_releases_submitted_capacity() {
        let mut input = SftpInputBuffer::default();
        fill(&mut input, MAX_SFTP_COMMAND_BYTES);
        assert!(input.bytes.capacity() <= MAX_SFTP_COMMAND_BYTES);
        let SftpInputAction::Submit(line) = input.push(b'\n') else {
            panic!("the exact byte boundary must be accepted");
        };
        assert_eq!(line.len(), MAX_SFTP_COMMAND_BYTES);
        assert_eq!(input.bytes.capacity(), 0);
    }

    #[test]
    fn sftp_input_refuses_the_whole_overlong_line_across_transport_chunks() {
        let mut input = SftpInputBuffer::default();
        for _ in 0..4 {
            fill(&mut input, festerm_session::MAX_IO_CHUNK_BYTES);
        }
        assert_eq!(input.push(b'x'), SftpInputAction::Refuse);
        assert_eq!(input.bytes.capacity(), 0);
        for &byte in b"mkdir /trailing-fragment\x08\x7f" {
            assert_eq!(input.push(byte), SftpInputAction::Ignore);
        }
        assert_eq!(input.push(b'\r'), SftpInputAction::RefusedLineEnded);
        assert_eq!(input.push(b'\n'), SftpInputAction::Ignore);
        for &byte in b"pwd" {
            assert_eq!(input.push(byte), SftpInputAction::Echo(byte));
        }
        assert_eq!(input.push(b'\n'), SftpInputAction::Submit(b"pwd".to_vec()));
    }

    #[test]
    fn sftp_input_cancel_recovers_from_refusal_and_releases_large_capacity() {
        let mut input = SftpInputBuffer::default();
        fill(&mut input, MAX_SFTP_COMMAND_BYTES);
        assert_eq!(input.push(0x03), SftpInputAction::Cancel);
        assert_eq!(input.bytes.capacity(), 0);
        fill(&mut input, MAX_SFTP_COMMAND_BYTES);
        assert_eq!(input.push(b'x'), SftpInputAction::Refuse);
        assert_eq!(input.push(0x03), SftpInputAction::Cancel);
        assert_eq!(input.push(b'p'), SftpInputAction::Echo(b'p'));
        assert_eq!(input.push(b'\r'), SftpInputAction::Submit(b"p".to_vec()));
    }

    #[test]
    fn sftp_input_backspace_removes_complete_and_chunk_split_utf8_scalars() {
        for text in ["a\u{03bb}", "a\u{4e2d}", "a\u{1f642}"] {
            let mut input = SftpInputBuffer::default();
            for byte in text.bytes() {
                input.push(byte);
            }
            assert_eq!(input.push(0x7f), SftpInputAction::Erase);
            assert_eq!(input.push(b'\n'), SftpInputAction::Submit(b"a".to_vec()));
        }
        for prefix in [&b"a\xf0"[..], &b"a\xf0\x9f"[..], &b"a\xf0\x9f\x99"[..]] {
            let mut input = SftpInputBuffer::default();
            for &byte in prefix {
                input.push(byte);
            }
            assert_eq!(input.push(0x08), SftpInputAction::Erase);
            assert_eq!(input.push(b'\n'), SftpInputAction::Submit(b"a".to_vec()));
        }
    }

    #[test]
    fn sftp_input_invalid_continuation_backspace_preserves_preceding_ascii() {
        let mut input = SftpInputBuffer::default();
        input.push(b'a');
        input.push(0x80);
        assert_eq!(input.push(0x08), SftpInputAction::Erase);
        assert_eq!(input.push(b'\n'), SftpInputAction::Submit(b"a".to_vec()));
    }

    #[test]
    fn sftp_input_backspace_handles_long_invalid_runs_without_a_full_tail_scan() {
        let mut input = SftpInputBuffer {
            bytes: vec![0x80; MAX_SFTP_COMMAND_BYTES],
            ..Default::default()
        };
        for remaining in (0..MAX_SFTP_COMMAND_BYTES).rev() {
            assert_eq!(input.push(0x7f), SftpInputAction::Erase);
            assert_eq!(input.bytes.len(), remaining);
        }
        assert_eq!(input.bytes.capacity(), 0);
    }

    #[test]
    fn sftp_input_counts_utf8_bytes_not_characters() {
        let mut input = SftpInputBuffer::default();
        fill(&mut input, MAX_SFTP_COMMAND_BYTES - 4);
        for byte in "\u{1f642}".bytes() {
            assert_eq!(input.push(byte), SftpInputAction::Echo(byte));
        }
        assert_eq!(input.bytes.len(), MAX_SFTP_COMMAND_BYTES);
        assert_eq!(input.push(b'x'), SftpInputAction::Refuse);
        assert_eq!(input.bytes.capacity(), 0);
    }

    #[test]
    fn sftp_input_backspace_reclaims_exceptional_capacity_with_hysteresis() {
        let mut input = SftpInputBuffer::default();
        fill(&mut input, MAX_SFTP_COMMAND_BYTES);
        for _ in RETAINED_CAPACITY_BYTES..MAX_SFTP_COMMAND_BYTES {
            assert_eq!(input.push(0x7f), SftpInputAction::Erase);
        }
        assert_eq!(input.bytes.len(), RETAINED_CAPACITY_BYTES);
        assert!(input.bytes.capacity() <= RETAINED_CAPACITY_BYTES);
        input.push(b'x');
        input.push(0x7f);
        assert!(input.bytes.capacity() < RECLAIM_CAPACITY_THRESHOLD_BYTES);
    }
}
