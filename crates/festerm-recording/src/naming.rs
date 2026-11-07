use std::fmt;

use uuid::Uuid;

use crate::StreamId;

const DEFAULT_DIRECTORY: &str = "recording-{recording_id}";
const DEFAULT_SESSION: &str = "session-{session_index}.mp4";
const MAX_COMPONENT_UNITS: usize = 255;
const MAX_TEMPLATE_BYTES: usize = 2048;

/// A random, recording-local identifier that does not contain session metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RecordingId(Uuid);

impl RecordingId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Parses the identifier from a previously created private source name.
    pub fn parse(value: &str) -> Result<Self, NameError> {
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(NameError::new("invalid recording identifier"));
        }
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| NameError::new("invalid recording identifier"))
    }
}

impl Default for RecordingId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RecordingId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0.simple())
    }
}

/// Local wall-clock values captured once at the start of a recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalStart {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl LocalStart {
    pub fn new(
        year: u16,
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, NameError> {
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => 0,
        };
        if year == 0
            || year > 9999
            || day == 0
            || day > days
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(NameError::new("invalid local recording start time"));
        }
        Ok(Self {
            year,
            month,
            day,
            hour,
            minute,
            second,
        })
    }
}

/// An invalid filename template or recording start time.
#[derive(Debug)]
pub struct NameError(String);

impl NameError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for NameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for NameError {}

/// Validated templates for a recording directory and its per-session MP4s.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NameTemplates {
    directory: String,
    session: String,
}

impl NameTemplates {
    pub fn new(directory: &str, session: &str) -> Result<Self, NameError> {
        if directory.len() > MAX_TEMPLATE_BYTES || session.len() > MAX_TEMPLATE_BYTES {
            return Err(NameError::new("recording filename template is too long"));
        }
        if !session.contains("{session_index}") {
            return Err(NameError::new(
                "the MP4 filename must contain {session_index}",
            ));
        }
        if !session.ends_with(".mp4") {
            return Err(NameError::new("the MP4 filename must end in .mp4"));
        }
        let templates = Self {
            directory: directory.to_owned(),
            session: session.to_owned(),
        };
        let sample_id = RecordingId(Uuid::nil());
        let sample_start = LocalStart::new(2000, 1, 1, 0, 0, 0)?;
        templates.directory(sample_id, sample_start)?;
        templates.session(sample_id, sample_start, StreamId::from_index(1).unwrap())?;
        Ok(templates)
    }

    pub fn directory_template(&self) -> &str {
        &self.directory
    }

    pub fn session_template(&self) -> &str {
        &self.session
    }

    pub fn directory(&self, id: RecordingId, start: LocalStart) -> Result<String, NameError> {
        expand(&self.directory, id, start, None)
    }

    pub fn session(
        &self,
        id: RecordingId,
        start: LocalStart,
        stream: StreamId,
    ) -> Result<String, NameError> {
        expand(&self.session, id, start, Some(stream))
    }
}

impl Default for NameTemplates {
    fn default() -> Self {
        Self::new(DEFAULT_DIRECTORY, DEFAULT_SESSION).expect("default recording names are valid")
    }
}

fn expand(
    template: &str,
    id: RecordingId,
    start: LocalStart,
    stream: Option<StreamId>,
) -> Result<String, NameError> {
    let mut name = String::new();
    let mut remaining = template;
    while let Some(open) = remaining.find('{') {
        let literal = &remaining[..open];
        if literal.contains('}') {
            return Err(NameError::new("unmatched brace in recording filename"));
        }
        name.push_str(literal);
        remaining = &remaining[open + 1..];
        let close = remaining
            .find('}')
            .ok_or_else(|| NameError::new("unmatched brace in recording filename"))?;
        let token = &remaining[..close];
        match token {
            "recording_id" => name.push_str(&id.to_string()),
            "session_index" => {
                let stream = stream.ok_or_else(|| {
                    NameError::new("{session_index} is only valid in MP4 filenames")
                })?;
                name.push_str(&format!("{:02}", stream.index()));
            }
            "date" => name.push_str(&format!(
                "{:04}{:02}{:02}",
                start.year, start.month, start.day
            )),
            "time" => name.push_str(&format!(
                "{:02}{:02}{:02}",
                start.hour, start.minute, start.second
            )),
            _ => {
                return Err(NameError::new(format!(
                    "unknown recording filename token {{{token}}}"
                )))
            }
        }
        remaining = &remaining[close + 1..];
    }
    if remaining.contains('}') {
        return Err(NameError::new("unmatched brace in recording filename"));
    }
    name.push_str(remaining);
    validate_component(&name)?;
    Ok(name)
}

fn validate_component(name: &str) -> Result<(), NameError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err(NameError::new(
            "recording names must be a single portable filename component",
        ));
    }
    if name.len() > MAX_COMPONENT_UNITS || name.encode_utf16().count() > MAX_COMPONENT_UNITS {
        return Err(NameError::new("recording filename is too long"));
    }
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered_device
    {
        return Err(NameError::new(
            "recording filename is reserved by the platform",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (RecordingId, LocalStart) {
        (
            RecordingId::parse("00112233445566778899aabbccddeeff").unwrap(),
            LocalStart::new(2026, 2, 28, 9, 6, 5).unwrap(),
        )
    }

    #[test]
    fn names_are_opaque_and_distinct_for_every_enrolled_stream() {
        let (id, start) = sample();
        let templates = NameTemplates::default();
        assert_eq!(
            templates.directory(id, start).unwrap(),
            "recording-00112233445566778899aabbccddeeff"
        );
        assert_eq!(
            templates
                .session(id, start, StreamId::from_index(1).unwrap())
                .unwrap(),
            "session-01.mp4"
        );
        assert_eq!(
            templates
                .session(id, start, StreamId::from_index(16).unwrap())
                .unwrap(),
            "session-16.mp4"
        );
        let custom = NameTemplates::new(
            "demo-{date}-{time}",
            "{recording_id}-{session_index}-{date}.mp4",
        )
        .unwrap();
        assert_eq!(custom.directory(id, start).unwrap(), "demo-20260228-090605");
        assert_eq!(
            custom
                .session(id, start, StreamId::from_index(2).unwrap())
                .unwrap(),
            "00112233445566778899aabbccddeeff-02-20260228.mp4"
        );
    }

    #[test]
    fn rejects_paths_reserved_names_and_invalid_tokens() {
        for directory in [
            "",
            ".",
            "..",
            "../demo",
            "demo\\folder",
            "CON",
            "con.txt",
            "LPT².log",
            "trailing.",
            "trailing ",
            "a:b",
            "{session_index}",
            "{host}",
            "{date",
            "date}",
            "a\nb",
        ] {
            assert!(
                NameTemplates::new(directory, DEFAULT_SESSION).is_err(),
                "{directory:?}"
            );
        }
        for session in [
            "session.mp4",
            "{session_index}.avi",
            "{session_index}.MP4",
            "../{session_index}.mp4",
            "{session_index}-{host}.mp4",
            "con.{session_index}.mp4",
        ] {
            assert!(
                NameTemplates::new(DEFAULT_DIRECTORY, session).is_err(),
                "{session:?}"
            );
        }
        assert!(NameTemplates::new(&"x".repeat(256), DEFAULT_SESSION).is_err());
    }

    #[test]
    fn validates_local_calendar_and_identifier() {
        assert!(LocalStart::new(2024, 2, 29, 23, 59, 59).is_ok());
        assert!(LocalStart::new(2023, 2, 29, 0, 0, 0).is_err());
        assert!(LocalStart::new(2026, 4, 31, 0, 0, 0).is_err());
        assert!(LocalStart::new(2026, 1, 1, 24, 0, 0).is_err());
        assert!(RecordingId::parse("../not-a-recording").is_err());
        let id = RecordingId::new();
        assert_eq!(RecordingId::parse(&id.to_string()).unwrap(), id);
    }
}
