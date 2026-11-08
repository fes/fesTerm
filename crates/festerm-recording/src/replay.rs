use std::{fmt, io};

use festerm_core::{ColorScheme, Dimensions, Terminal, MAX_RECORDING_SCROLLBACK_BYTES};
use sha2::{Digest, Sha256};

use crate::{
    artifact::{CapturedData, CapturedEvent},
    RepeatHint, MAX_STREAMS,
};

#[derive(Debug)]
pub(crate) enum ReplayError {
    Invalid(String),
    ResourceLimit(String),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Invalid(message) | Self::ResourceLimit(message)) = self;
        formatter.write_str(message)
    }
}

pub(crate) fn visible_fingerprint(terminal: &Terminal) -> Result<[u8; 32], ReplayError> {
    struct DigestWriter(Sha256);

    impl io::Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut writer = DigestWriter(Sha256::new());
    bincode::serialize_into(&mut writer, &terminal.recording_visible_state()).map_err(|error| {
        ReplayError::Invalid(format!(
            "visible recording state could not be checked: {error}"
        ))
    })?;
    Ok(writer.0.finalize().into())
}

pub(crate) struct VisibleCheck {
    pub(crate) end: usize,
    pub(crate) fingerprint: [u8; 32],
}

pub(crate) fn verify_visible(terminal: &Terminal, expected: [u8; 32]) -> Result<(), ReplayError> {
    if visible_fingerprint(terminal)? != expected {
        return Err(ReplayError::Invalid(
            "recording cannot faithfully reproduce the visible terminal state without private pre-consent content".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) struct ReplayTerminal {
    terminal: Terminal,
    history_capped: bool,
    history_lost: bool,
    evicted_lines: u64,
    oversize_lines: u64,
}

impl ReplayTerminal {
    pub(crate) fn new(terminal: Terminal, history_capped: bool) -> Self {
        let stats = terminal.scrollback_stats();
        Self {
            terminal,
            history_capped,
            history_lost: false,
            evicted_lines: stats.evicted_lines(),
            oversize_lines: stats.oversize_lines(),
        }
    }

    pub(crate) fn apply_captured_event(
        terminals: &mut [Option<ReplayTerminal>; MAX_STREAMS],
        event: &CapturedEvent,
        checks: &[VisibleCheck],
    ) -> Result<(), ReplayError> {
        let slot = &mut terminals[event.stream.index() - 1];
        match &event.data {
            CapturedData::Start(checkpoint) => {
                let terminal = checkpoint.clone().into_terminal().map_err(|error| {
                    ReplayError::Invalid(format!("recording checkpoint is invalid: {error}"))
                })?;
                *slot = Some(ReplayTerminal::new(terminal, checkpoint.history_capped()));
            }
            CapturedData::Output { bytes, repeats } => {
                let terminal = slot.as_mut().ok_or_else(|| {
                    ReplayError::Invalid("recording output has no screen".to_owned())
                })?;
                let mut start = 0;
                for check in checks {
                    if check.end <= start || check.end > bytes.len() {
                        return Err(ReplayError::Invalid(
                            "recording screen checks are out of order".to_owned(),
                        ));
                    }
                    terminal.ingest(bytes, repeats, start, check.end)?;
                    verify_visible(terminal.terminal(), check.fingerprint)?;
                    start = check.end;
                }
                terminal.ingest(bytes, repeats, start, bytes.len())?;
                terminal.finish_output()?;
            }
            CapturedData::Resize(dimensions) => {
                slot.as_mut()
                    .ok_or_else(|| {
                        ReplayError::Invalid("recording resize has no screen".to_owned())
                    })?
                    .resize(*dimensions)?;
            }
            CapturedData::Reset => {
                slot.as_mut()
                    .ok_or_else(|| {
                        ReplayError::Invalid("recording reset has no screen".to_owned())
                    })?
                    .reset();
            }
            CapturedData::Configure {
                color_scheme,
                scrollback_limit_bytes,
                history_capped,
            } => {
                slot.as_mut()
                    .ok_or_else(|| {
                        ReplayError::Invalid("recording configuration has no screen".to_owned())
                    })?
                    .configure(*color_scheme, *scrollback_limit_bytes, *history_capped)?;
            }
            CapturedData::End => *slot = None,
        }
        Ok(())
    }

    pub(crate) fn terminal(&self) -> &Terminal {
        &self.terminal
    }

    pub(crate) fn ingest(
        &mut self,
        bytes: &[u8],
        repeats: &[RepeatHint],
        start: usize,
        end: usize,
    ) -> Result<(), ReplayError> {
        let first = repeats.partition_point(|(offset, _)| *offset < start);
        let last = repeats.partition_point(|(offset, _)| *offset < end);
        let hints = repeats[first..last]
            .iter()
            .map(|(offset, character)| (offset - start, *character));
        self.terminal
            .ingest_recorded_output(&bytes[start..end], hints)
            .map_err(|error| {
                ReplayError::Invalid(format!("recorded output is invalid: {error}"))
            })?;
        let _ = self.terminal.drain_replies();
        let _ = self.terminal.take_reply_queue_overflowed();
        self.update_history_loss();
        Ok(())
    }

    pub(crate) fn finish_output(&self) -> Result<(), ReplayError> {
        self.terminal.validate_recording_screen().map_err(|error| {
            ReplayError::ResourceLimit(format!("recorded screen exceeds its limit: {error}"))
        })
    }

    pub(crate) fn resize(&mut self, dimensions: Dimensions) -> Result<(), ReplayError> {
        self.update_history_loss();
        if self.history_lost {
            return Err(ReplayError::ResourceLimit(
                "recorded history exceeded its limit before a resize".to_owned(),
            ));
        }
        self.terminal
            .resize(dimensions)
            .map_err(|error| ReplayError::Invalid(format!("recording resize failed: {error}")))?;
        self.update_history_loss();
        if self.history_lost {
            return Err(ReplayError::ResourceLimit(
                "recorded history exceeded its limit during a resize".to_owned(),
            ));
        }
        self.finish_output()
    }

    pub(crate) fn reset(&mut self) {
        self.terminal.reset_to_initial_state();
        self.update_history_loss();
    }

    pub(crate) fn configure(
        &mut self,
        color_scheme: ColorScheme,
        scrollback_limit_bytes: usize,
        history_capped: bool,
    ) -> Result<(), ReplayError> {
        if scrollback_limit_bytes > MAX_RECORDING_SCROLLBACK_BYTES {
            return Err(ReplayError::Invalid(
                "recorded history limit exceeds its bound".to_owned(),
            ));
        }
        self.update_history_loss();
        self.history_capped = history_capped;
        self.terminal.set_color_scheme(color_scheme);
        self.terminal.set_scrollback_limit(scrollback_limit_bytes);
        self.update_history_loss();
        Ok(())
    }

    fn update_history_loss(&mut self) {
        let stats = self.terminal.scrollback_stats();
        if self.history_capped
            && (stats.evicted_lines() > self.evicted_lines
                || stats.oversize_lines() > self.oversize_lines)
        {
            self.history_lost = true;
        }
        self.evicted_lines = stats.evicted_lines();
        self.oversize_lines = stats.oversize_lines();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resize_refuses_history_lost_to_the_recording_cap() {
        let dimensions = Dimensions::new(20, 2).unwrap();
        for capped in [false, true] {
            let terminal = Terminal::with_scrollback_limit(dimensions, 256).unwrap();
            let mut replay = ReplayTerminal::new(terminal, capped);
            let output = b"post-consent line\r\n".repeat(100);
            replay.ingest(&output, &[], 0, output.len()).unwrap();
            replay.finish_output().unwrap();
            assert!(replay.terminal().scrollback_stats().evicted_lines() > 0);
            let resized = replay.resize(Dimensions::new(20, 4).unwrap());
            assert_eq!(resized.is_err(), capped);
        }
    }
}
