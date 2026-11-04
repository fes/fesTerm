use std::{fmt, time::Instant};

pub(crate) const MARKER: &str = "FESTERM_STDOUT_PROBE";
pub(crate) const MAX_CONTROL: usize = 4096;
pub(crate) const MAX_EMPTY_LINES: usize = 16;
const MAX_VALUES: usize = 4;
const MAX_DIAGNOSTIC_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ControlError {
    Deadline,
    Overflow,
    TooManyEmptyLines,
    UnexpectedFrame {
        line_bytes: usize,
        escaped_prefix: String,
    },
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline => formatter.write_str("control deadline expired"),
            Self::Overflow => formatter.write_str("control byte limit exceeded"),
            Self::TooManyEmptyLines => formatter.write_str("control empty-line limit exceeded"),
            Self::UnexpectedFrame {
                line_bytes,
                escaped_prefix,
            } => write!(
                formatter,
                "unexpected control frame ({line_bytes} line bytes; escaped buffered prefix: \"{escaped_prefix}\")"
            ),
        }
    }
}

#[derive(Default)]
pub(crate) struct ControlParser {
    buffered: Vec<u8>,
    received: usize,
    empty_lines: usize,
}

impl ControlParser {
    pub(crate) fn push(
        &mut self,
        bytes: &[u8],
        now: Instant,
        deadline: Instant,
    ) -> Result<(), ControlError> {
        if now >= deadline {
            return Err(ControlError::Deadline);
        }
        let received = self
            .received
            .checked_add(bytes.len())
            .filter(|&count| count <= MAX_CONTROL)
            .ok_or(ControlError::Overflow)?;
        self.received = received;
        self.buffered.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn next_event(
        &mut self,
        expected: &str,
        now: Instant,
        deadline: Instant,
    ) -> Result<Option<Vec<u64>>, ControlError> {
        if now >= deadline {
            return Err(ControlError::Deadline);
        }
        loop {
            let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') else {
                return Ok(None);
            };
            if end == 0 || (end == 1 && self.buffered[0] == b'\r') {
                self.consume_empty(end + 1)?;
                continue;
            }

            let line = &self.buffered[..end];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let mut fields = line.split(|byte| *byte == b' ');
            if fields.next() != Some(MARKER.as_bytes())
                || fields.next() != Some(expected.as_bytes())
            {
                return Err(self.unexpected(end));
            }
            let mut values = Vec::new();
            for field in fields {
                if field.is_empty() || values.len() == MAX_VALUES {
                    return Err(self.unexpected(end));
                }
                let Some(value) = field.iter().try_fold(0_u64, |value, &byte| {
                    if byte.is_ascii_digit() {
                        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
                    } else {
                        None
                    }
                }) else {
                    return Err(self.unexpected(end));
                };
                values.push(value);
            }
            if values.is_empty() {
                return Err(self.unexpected(end));
            }
            drop(self.buffered.drain(..=end));
            return Ok(Some(values));
        }
    }

    pub(crate) fn trailing(
        &mut self,
        eof: bool,
        now: Instant,
        deadline: Instant,
    ) -> Result<(), ControlError> {
        if now >= deadline {
            return Err(ControlError::Deadline);
        }
        loop {
            match self.buffered.as_slice() {
                [] => return Ok(()),
                [b'\n', ..] => self.consume_empty(1)?,
                [b'\r', b'\n', ..] => self.consume_empty(2)?,
                [b'\r'] if !eof => return Ok(()),
                _ => {
                    let end = self
                        .buffered
                        .iter()
                        .position(|byte| *byte == b'\n')
                        .unwrap_or(self.buffered.len());
                    return Err(self.unexpected(end));
                }
            }
        }
    }

    pub(crate) fn unfinished(&self) -> Option<ControlError> {
        (!self.buffered.is_empty()).then(|| self.unexpected(self.buffered.len()))
    }

    pub(crate) fn rejected(bytes: &[u8]) -> ControlError {
        let line_bytes = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(bytes.len());
        Self::describe(bytes, line_bytes)
    }

    fn consume_empty(&mut self, length: usize) -> Result<(), ControlError> {
        if self.empty_lines == MAX_EMPTY_LINES {
            return Err(ControlError::TooManyEmptyLines);
        }
        self.empty_lines += 1;
        drop(self.buffered.drain(..length));
        Ok(())
    }

    fn unexpected(&self, line_bytes: usize) -> ControlError {
        Self::describe(&self.buffered, line_bytes)
    }

    fn describe(bytes: &[u8], line_bytes: usize) -> ControlError {
        let limit = bytes.len().min(MAX_DIAGNOSTIC_BYTES);
        let mut escaped_prefix: String = bytes[..limit]
            .iter()
            .flat_map(|&byte| std::ascii::escape_default(byte))
            .map(char::from)
            .collect();
        if bytes.len() > limit {
            escaped_prefix.push_str("...");
        }
        ControlError::UnexpectedFrame {
            line_bytes,
            escaped_prefix,
        }
    }
}
