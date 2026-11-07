use std::{
    fmt,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use festerm_core::Terminal;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    advanced_pacing::{AdvancedTimeline, EventTiming, SplitKind},
    ArtifactError, ArtifactEvent, ArtifactReader, FinishReason, RecoveryPrefix, RecoveryReader,
    StreamId, MAX_DURATION,
};

const FRAMES_PER_SECOND: u64 = 30;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// A playback multiplier applied to the shared timeline.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSpeed {
    Half,
    #[default]
    Normal,
    Double,
    Quadruple,
    Octuple,
}

impl PlaybackSpeed {
    pub(crate) const fn ratio(self) -> (u64, u64) {
        match self {
            Self::Half => (1, 2),
            Self::Normal => (1, 1),
            Self::Double => (2, 1),
            Self::Quadruple => (4, 1),
            Self::Octuple => (8, 1),
        }
    }
}

/// Compresses only gaps in which none of the recorded streams changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdleCompression {
    threshold: Duration,
    target: Duration,
}

impl IdleCompression {
    pub fn new(threshold: Duration, target: Duration) -> Result<Self, PlaybackError> {
        if threshold.is_zero() || threshold > MAX_DURATION || target.is_zero() || target > threshold
        {
            return Err(PlaybackError::new("invalid idle-compression interval"));
        }
        Ok(Self { threshold, target })
    }

    pub const fn threshold(self) -> Duration {
        self.threshold
    }

    pub const fn target(self) -> Duration {
        self.target
    }
}

/// Presentation speed inferred from echoed terminal output, never local input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TypingSpeed(u8);

impl TypingSpeed {
    pub fn new(characters_per_second: u8) -> Result<Self, PlaybackError> {
        if !(5..=60).contains(&characters_per_second) {
            return Err(PlaybackError::new(
                "typing speed must be between 5 and 60 characters per second",
            ));
        }
        Ok(Self(characters_per_second))
    }

    pub const fn characters_per_second(self) -> u8 {
        self.0
    }
}

/// Shortens sustained non-typing output on the shared timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LongOutputLimit {
    threshold: Duration,
    target: Duration,
}

impl LongOutputLimit {
    pub fn new(threshold: Duration, target: Duration) -> Result<Self, PlaybackError> {
        if threshold.is_zero() || threshold > MAX_DURATION || target.is_zero() || target > threshold
        {
            return Err(PlaybackError::new("invalid long-output interval"));
        }
        Ok(Self { threshold, target })
    }

    pub const fn threshold(self) -> Duration {
        self.threshold
    }

    pub const fn target(self) -> Duration {
        self.target
    }
}

/// Progressive presentation of large terminal-output updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputRevealRate(u16);

impl OutputRevealRate {
    pub fn new(lines_per_second: u16) -> Result<Self, PlaybackError> {
        if !(20..=200).contains(&lines_per_second) {
            return Err(PlaybackError::new(
                "output reveal rate must be between 20 and 200 lines per second",
            ));
        }
        Ok(Self(lines_per_second))
    }

    pub const fn lines_per_second(self) -> u16 {
        self.0
    }
}

/// Settings shared by every stream in an export.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pacing {
    pub speed: PlaybackSpeed,
    pub idle: Option<IdleCompression>,
    pub typing: Option<TypingSpeed>,
    pub long_output: Option<LongOutputLimit>,
    pub reveal: Option<OutputRevealRate>,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            speed: PlaybackSpeed::Normal,
            idle: Some(IdleCompression {
                threshold: Duration::from_secs(3),
                target: Duration::from_millis(500),
            }),
            typing: None,
            long_output: None,
            reveal: None,
        }
    }
}

/// An invalid source, playback plan, or replay.
#[derive(Debug)]
pub struct PlaybackError(String);

impl PlaybackError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PlaybackError {}

impl From<ArtifactError> for PlaybackError {
    fn from(error: ArtifactError) -> Self {
        Self::new(error.to_string())
    }
}

pub(crate) fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), PlaybackError> {
    if cancelled() {
        return Err(PlaybackError::new("recording planning was cancelled"));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FinishPolicy {
    Complete,
    Recovered,
}

impl FinishPolicy {
    pub(crate) const fn accepts(self, reason: FinishReason) -> bool {
        match self {
            Self::Complete => reason.is_complete(),
            Self::Recovered => matches!(reason, FinishReason::RecoveredPrefix),
        }
    }
}

/// A validated, shared frame schedule for all streams of one source.
#[derive(Clone, Debug)]
pub struct PlaybackPlan {
    pacing: Pacing,
    duration: Duration,
    frame_count: u64,
    stream_count: usize,
    fingerprint: [u8; 32],
    finish_policy: FinishPolicy,
    advanced: Option<Arc<AdvancedTimeline>>,
}

impl PlaybackPlan {
    /// Validates the whole source before any output is published.
    pub fn inspect<R: Read>(source: R, pacing: Pacing) -> Result<Self, PlaybackError> {
        Self::inspect_with_cancellation(source, pacing, FinishPolicy::Complete, &|| false)
    }

    /// Validates and plans a source while observing cancellation during both
    /// reads and CPU-only pacing work.
    pub fn inspect_cancellable<R: Read>(
        source: R,
        pacing: Pacing,
        cancelled: &AtomicBool,
    ) -> Result<Self, PlaybackError> {
        Self::inspect_with_cancellation(source, pacing, FinishPolicy::Complete, &|| {
            cancelled.load(Ordering::Acquire)
        })
    }

    /// Plans an explicitly selected prefix and binds it to its inspected bytes.
    pub fn inspect_recovered_cancellable<R: Read>(
        source: RecoveryReader<R>,
        prefix: RecoveryPrefix,
        pacing: Pacing,
        cancelled: &AtomicBool,
    ) -> Result<Self, PlaybackError> {
        let plan =
            Self::inspect_with_cancellation(source, pacing, FinishPolicy::Recovered, &|| {
                cancelled.load(Ordering::Acquire)
            })?;
        if plan.fingerprint != prefix.fingerprint() || plan.stream_count != prefix.stream_count() {
            return Err(PlaybackError::new(
                "the recovered recording prefix changed after inspection",
            ));
        }
        Ok(plan)
    }

    fn inspect_with_cancellation<R: Read>(
        source: R,
        pacing: Pacing,
        finish_policy: FinishPolicy,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Self, PlaybackError> {
        check_cancelled(cancelled)?;
        if pacing.typing.is_some() || pacing.long_output.is_some() || pacing.reveal.is_some() {
            let timeline = AdvancedTimeline::inspect(source, pacing, finish_policy, cancelled)?;
            let duration = timeline.duration();
            let frames = Self::count_frames(duration)?;
            return Ok(Self {
                pacing,
                duration,
                frame_count: frames,
                stream_count: timeline.stream_count(),
                fingerprint: timeline.fingerprint(),
                finish_policy,
                advanced: Some(Arc::new(timeline)),
            });
        }
        let mut reader = ArtifactReader::new(source)?;
        let mut warp = TimeWarp::new(pacing);
        let mut stream_count = 0;
        let duration = loop {
            check_cancelled(cancelled)?;
            let event = reader
                .next_event()?
                .ok_or_else(|| PlaybackError::new("recording ended without a finish frame"))?;
            let mapped = warp.advance(event.timestamp())?;
            match event {
                ArtifactEvent::Started { .. } => stream_count += 1,
                ArtifactEvent::Finished { reason, .. } => {
                    if !finish_policy.accepts(reason) {
                        return Err(PlaybackError::new(
                            "an incomplete recording requires explicit recovery",
                        ));
                    }
                    break Duration::from_nanos(mapped);
                }
                _ => {}
            }
        };
        check_cancelled(cancelled)?;
        Ok(Self {
            pacing,
            duration,
            frame_count: Self::count_frames(duration)?,
            stream_count,
            fingerprint: reader
                .fingerprint()
                .expect("a validated finish provides a source fingerprint"),
            finish_policy,
            advanced: None,
        })
    }

    fn count_frames(duration: Duration) -> Result<u64, PlaybackError> {
        (duration.as_nanos() * u128::from(FRAMES_PER_SECOND))
            .div_ceil(u128::from(NANOS_PER_SECOND))
            .checked_add(1)
            .and_then(|frames| u64::try_from(frames).ok())
            .ok_or_else(|| PlaybackError::new("recording frame count overflows"))
    }

    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Includes a final frame at or after the synchronized stop time.
    pub const fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub const fn stream_count(&self) -> usize {
        self.stream_count
    }

    /// Integrity fingerprint of the complete validated source.
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
}

/// A frame at a fixed 30 fps. A late-joining stream is neutral while `terminal`
/// is `None`; an ended stream retains its last terminal.
pub struct PlaybackFrame<'a> {
    pub index: u64,
    pub at: Duration,
    /// False when the previously rendered pixels can be reused.
    pub changed: bool,
    pub terminal: Option<&'a Terminal>,
}

/// Streams one session against the plan shared by every exported MP4.
pub struct Playback<R> {
    reader: ArtifactReader<R>,
    plan: PlaybackPlan,
    stream: StreamId,
    warp: TimeWarp,
    pending: Option<PendingEvent>,
    event_index: usize,
    terminal: Option<Terminal>,
    next_frame: u64,
    finished: bool,
}

#[derive(Clone, Copy)]
enum ScheduledTime {
    Simple(u64),
    Advanced(EventTiming),
}

impl ScheduledTime {
    fn at(self, piece: usize, speed: PlaybackSpeed) -> Result<u64, PlaybackError> {
        match self {
            Self::Simple(at) if piece == 0 => Ok(at),
            Self::Simple(_) => Err(PlaybackError::new("recording output split is invalid")),
            Self::Advanced(timing) => timing.at(piece, speed),
        }
    }

    fn split(self) -> Option<crate::advanced_pacing::Split> {
        match self {
            Self::Simple(_) => None,
            Self::Advanced(timing) => timing.split,
        }
    }
}

struct PendingEvent {
    timing: ScheduledTime,
    event: ArtifactEvent,
    offset: usize,
    piece: usize,
}

impl<R: Read> Playback<R> {
    pub fn new(source: R, stream: StreamId, plan: PlaybackPlan) -> Result<Self, PlaybackError> {
        if stream.index() > plan.stream_count {
            return Err(PlaybackError::new("recording stream is not in this plan"));
        }
        let reader = ArtifactReader::new(source)?;
        Ok(Self {
            reader,
            warp: TimeWarp::new(plan.pacing),
            plan,
            stream,
            pending: None,
            event_index: 0,
            terminal: None,
            next_frame: 0,
            finished: false,
        })
    }

    pub fn next_frame(&mut self) -> Result<Option<PlaybackFrame<'_>>, PlaybackError> {
        if self.next_frame == self.plan.frame_count {
            return Ok(None);
        }
        let target_ns = u64::try_from(
            u128::from(self.next_frame) * u128::from(NANOS_PER_SECOND)
                / u128::from(FRAMES_PER_SECOND),
        )
        .map_err(|_| PlaybackError::new("recording frame time overflows"))?;
        let mut changed = self.next_frame == 0;
        while !self.finished {
            if self.pending.is_none() {
                let event = self
                    .reader
                    .next_event()?
                    .ok_or_else(|| PlaybackError::new("recording ended without a finish frame"))?;
                let timing = if let Some(timeline) = &self.plan.advanced {
                    let timing = timeline.timing(self.event_index)?;
                    if let Some(split) = timing.split {
                        let valid = match (&event, split.kind) {
                            (ArtifactEvent::Output { bytes, .. }, SplitKind::Typing) => {
                                crate::advanced_pacing::echo_pieces(bytes) == Some(split.pieces)
                            }
                            (ArtifactEvent::Output { bytes, .. }, SplitKind::Lines) => {
                                crate::advanced_pacing::line_pieces(bytes) == split.pieces
                            }
                            _ => false,
                        };
                        if !valid {
                            return Err(PlaybackError::new(
                                "recording changed after pacing was planned",
                            ));
                        }
                    }
                    ScheduledTime::Advanced(timing)
                } else {
                    ScheduledTime::Simple(self.warp.advance(event.timestamp())?)
                };
                self.event_index += 1;
                self.pending = Some(PendingEvent {
                    timing,
                    event,
                    offset: 0,
                    piece: 0,
                });
            }
            let pending = self.pending.as_ref().expect("pending event was populated");
            let other_stream =
                event_stream(&pending.event).is_some_and(|stream| stream != self.stream);
            if other_stream && self.plan.advanced.is_some() {
                self.pending = None;
                continue;
            }
            let mapped = pending.timing.at(pending.piece, self.plan.pacing.speed)?;
            if mapped > target_ns {
                break;
            }
            let PendingEvent {
                timing,
                event,
                offset,
                piece,
            } = self.pending.take().expect("pending event was populated");
            match event {
                ArtifactEvent::Started {
                    stream, terminal, ..
                }
                | ArtifactEvent::Checkpoint {
                    stream, terminal, ..
                }
                | ArtifactEvent::Resized {
                    stream, terminal, ..
                } if stream == self.stream => {
                    self.terminal = Some(terminal);
                    changed = true;
                }
                ArtifactEvent::Output { at, stream, bytes } if stream == self.stream => {
                    let terminal = self.terminal.as_mut().ok_or_else(|| {
                        PlaybackError::new("recording output precedes its checkpoint")
                    })?;
                    let end = match timing.split().map(|split| split.kind) {
                        Some(SplitKind::Typing) => std::str::from_utf8(&bytes)
                            .expect("typing output was validated")
                            .grapheme_indices(true)
                            .nth(piece + 1)
                            .map_or(bytes.len(), |(position, _)| position),
                        Some(SplitKind::Lines) => bytes[offset..]
                            .iter()
                            .position(|byte| *byte == b'\n')
                            .map_or(bytes.len(), |position| offset + position + 1),
                        None => bytes.len(),
                    };
                    terminal.ingest(&bytes[offset..end]);
                    let _ = terminal.drain_replies();
                    let _ = terminal.take_reply_queue_overflowed();
                    changed = true;
                    if end < bytes.len() {
                        self.pending = Some(PendingEvent {
                            timing,
                            event: ArtifactEvent::Output { at, stream, bytes },
                            offset: end,
                            piece: piece + 1,
                        });
                    }
                }
                ArtifactEvent::Finished { reason, .. } => {
                    if !self.plan.finish_policy.accepts(reason)
                        || Duration::from_nanos(mapped) != self.plan.duration
                        || self.reader.fingerprint() != Some(self.plan.fingerprint)
                        || self
                            .plan
                            .advanced
                            .as_ref()
                            .is_some_and(|timeline| self.event_index != timeline.len())
                    {
                        return Err(PlaybackError::new(
                            "recording changed or became incomplete after planning",
                        ));
                    }
                    self.finished = true;
                }
                _ => {}
            }
        }
        let index = self.next_frame;
        self.next_frame += 1;
        if self.next_frame == self.plan.frame_count && !self.finished {
            return Err(PlaybackError::new(
                "recording did not reach its planned synchronized end",
            ));
        }
        Ok(Some(PlaybackFrame {
            index,
            at: Duration::from_nanos(target_ns),
            changed,
            terminal: self.terminal.as_ref(),
        }))
    }
}

fn event_stream(event: &ArtifactEvent) -> Option<StreamId> {
    match event {
        ArtifactEvent::Started { stream, .. }
        | ArtifactEvent::Output { stream, .. }
        | ArtifactEvent::Checkpoint { stream, .. }
        | ArtifactEvent::Resized { stream, .. }
        | ArtifactEvent::Ended { stream, .. } => Some(*stream),
        ArtifactEvent::Finished { .. } => None,
    }
}

struct TimeWarp {
    pacing: Pacing,
    last_source_ns: u64,
    mapped_ns: u64,
    remainder: u64,
}

impl TimeWarp {
    fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            last_source_ns: 0,
            mapped_ns: 0,
            remainder: 0,
        }
    }
    fn advance(&mut self, source: Duration) -> Result<u64, PlaybackError> {
        let source_ns = u64::try_from(source.as_nanos())
            .map_err(|_| PlaybackError::new("recording timestamp overflows"))?;
        let mut gap = source_ns
            .checked_sub(self.last_source_ns)
            .ok_or_else(|| PlaybackError::new("recording timestamps are not ordered"))?;
        self.last_source_ns = source_ns;
        if let Some(idle) = self.pacing.idle {
            if gap > idle.threshold.as_nanos() as u64 {
                gap = idle.target.as_nanos() as u64;
            }
        }
        let (numerator, denominator) = self.pacing.speed.ratio();
        let scaled = gap
            .checked_mul(denominator)
            .and_then(|value| value.checked_add(self.remainder))
            .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
        self.remainder = scaled % numerator;
        self.mapped_ns = self
            .mapped_ns
            .checked_add(scaled / numerator)
            .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
        Ok(self.mapped_ns)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use festerm_core::{Dimensions, Terminal, MAX_RECORDING_CHECKPOINT_BYTES};

    use super::*;
    use crate::artifact::{ArtifactWriter, CapturedData, CapturedEvent, FinishReason};

    fn source(final_output: &[u8], reason: FinishReason) -> Vec<u8> {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = ArtifactWriter::new(file.reopen().unwrap()).unwrap();
        let mut first = Terminal::new(Dimensions::new(12, 2).unwrap()).unwrap();
        first.ingest(b"s1:");
        let mut second = Terminal::new(Dimensions::new(12, 2).unwrap()).unwrap();
        second.ingest(b"s2:");
        let one = StreamId::from_index(1).unwrap();
        let two = StreamId::from_index(2).unwrap();
        let events = [
            CapturedEvent {
                at_ns: 0,
                stream: one,
                data: CapturedData::Start(
                    first
                        .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
                        .unwrap(),
                ),
            },
            CapturedEvent {
                at_ns: NANOS_PER_SECOND,
                stream: one,
                data: CapturedData::Output(b"A".to_vec()),
            },
            CapturedEvent {
                at_ns: NANOS_PER_SECOND,
                stream: two,
                data: CapturedData::Start(
                    second
                        .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
                        .unwrap(),
                ),
            },
            CapturedEvent {
                at_ns: 2 * NANOS_PER_SECOND,
                stream: two,
                data: CapturedData::Output(b"B\x1b[6n".to_vec()),
            },
            CapturedEvent {
                at_ns: 2 * NANOS_PER_SECOND,
                stream: two,
                data: CapturedData::End,
            },
            CapturedEvent {
                at_ns: 7 * NANOS_PER_SECOND,
                stream: one,
                data: CapturedData::Output(final_output.to_vec()),
            },
        ];
        for event in events {
            writer.write_event(&event).unwrap();
        }
        writer.finish(8 * NANOS_PER_SECOND, reason).unwrap();
        fs::read(file.path()).unwrap()
    }

    fn timed_source(updates: &[(u64, usize, &[u8])], finish_ms: u64) -> Vec<u8> {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = ArtifactWriter::new(file.reopen().unwrap()).unwrap();
        for index in 1..=2 {
            let terminal = Terminal::new(Dimensions::new(24, 8).unwrap()).unwrap();
            writer
                .write_event(&CapturedEvent {
                    at_ns: 0,
                    stream: StreamId::from_index(index).unwrap(),
                    data: CapturedData::Start(
                        terminal
                            .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
                            .unwrap(),
                    ),
                })
                .unwrap();
        }
        for &(at_ms, index, bytes) in updates {
            writer
                .write_event(&CapturedEvent {
                    at_ns: at_ms * 1_000_000,
                    stream: StreamId::from_index(index).unwrap(),
                    data: CapturedData::Output(bytes.to_vec()),
                })
                .unwrap();
        }
        writer
            .finish(finish_ms * 1_000_000, FinishReason::User)
            .unwrap();
        fs::read(file.path()).unwrap()
    }

    fn replay_rows(bytes: &[u8], plan: &PlaybackPlan, stream: usize) -> Vec<Vec<String>> {
        let mut playback = Playback::new(
            Cursor::new(bytes),
            StreamId::from_index(stream).unwrap(),
            plan.clone(),
        )
        .unwrap();
        let mut frames = Vec::new();
        while let Some(frame) = playback.next_frame().unwrap() {
            let rows = frame
                .terminal
                .map(|terminal| {
                    (0..terminal.dimensions().rows())
                        .map(|row| terminal.row_text(row).unwrap().trim_end().to_owned())
                        .collect()
                })
                .unwrap_or_default();
            frames.push(rows);
        }
        frames
    }

    #[test]
    fn streams_use_one_idle_transform_and_frame_schedule() {
        let bytes = source(b"C", FinishReason::User);
        let pacing = Pacing {
            speed: PlaybackSpeed::Double,
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        assert_eq!(plan.duration(), Duration::from_millis(1_750));
        assert_eq!(plan.frame_count(), 54);
        assert_eq!(plan.stream_count(), 2);

        let frames: Vec<_> = (1..=2)
            .map(|index| {
                let mut playback = Playback::new(
                    Cursor::new(&bytes),
                    StreamId::from_index(index).unwrap(),
                    plan.clone(),
                )
                .unwrap();
                let mut frames = Vec::new();
                while let Some(frame) = playback.next_frame().unwrap() {
                    if let Some(terminal) = frame.terminal {
                        assert!(terminal.queued_input().is_empty());
                        assert!(terminal.queued_replies().is_empty());
                    }
                    frames.push((
                        frame.index,
                        frame.at,
                        frame.terminal.and_then(|terminal| terminal.row_text(0)),
                        frame.changed,
                    ));
                }
                frames
            })
            .collect();
        assert_eq!(frames[0].len(), 54);
        assert_eq!(frames[1].len(), 54);
        assert_eq!(
            frames[0].iter().map(|frame| frame.1).collect::<Vec<_>>(),
            frames[1].iter().map(|frame| frame.1).collect::<Vec<_>>()
        );
        assert!(frames[1][14].2.is_none());
        assert!(frames[1][0].3);
        assert!(!frames[1][14].3);
        assert!(frames[1][15].3);
        assert!(frames[1][30].3);
        assert!(!frames[1][53].3);
        assert!(frames[1][15].2.as_deref().unwrap().starts_with("s2:"));
        assert!(frames[1][30].2.as_deref().unwrap().starts_with("s2:B"));
        assert!(frames[1][53].2.as_deref().unwrap().starts_with("s2:B"));
        assert!(frames[0][53].2.as_deref().unwrap().starts_with("s1:AC"));
    }

    #[test]
    fn changed_and_incomplete_sources_cannot_finish_a_planned_export() {
        let original = source(b"C", FinishReason::User);
        let changed = source(b"D", FinishReason::User);
        let plan = PlaybackPlan::inspect(Cursor::new(&original), Pacing::default()).unwrap();
        let mut playback =
            Playback::new(Cursor::new(changed), StreamId::from_index(1).unwrap(), plan).unwrap();
        let error = loop {
            match playback.next_frame() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("a changed source must fail before completion"),
                Err(error) => break error,
            }
        };
        assert!(error.to_string().contains("changed"));
        let incomplete = source(b"C", FinishReason::IntegrityFailure);
        assert!(PlaybackPlan::inspect(Cursor::new(incomplete), Pacing::default()).is_err());
    }

    #[test]
    fn echoed_typing_is_retimed_without_losing_shared_session_alignment() {
        let bytes = timed_source(
            &[
                (1_000, 1, b"he"),
                (1_400, 1, b"l"),
                (1_400, 2, b"X"),
                (2_000, 1, b"lo"),
            ],
            3_000,
        );
        let pacing = Pacing {
            idle: None,
            typing: Some(TypingSpeed::new(10).unwrap()),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        assert_eq!(plan.duration(), Duration::from_millis(2_400));
        let first = replay_rows(&bytes, &plan, 1);
        let second = replay_rows(&bytes, &plan, 2);
        assert_eq!(first.len(), second.len());
        assert_eq!(first[30][0], "h");
        assert_eq!(first[33][0], "he");
        assert_eq!(first[36][0], "hel");
        assert_eq!(first[39][0], "hell");
        assert_eq!(first[42][0], "hello");
        assert_eq!(second[35][0], "");
        assert_eq!(second[36][0], "X");
        assert_eq!(first.last().unwrap()[0], "hello");
        assert_eq!(second.last().unwrap()[0], "X");
    }

    #[test]
    fn typing_normalization_preserves_unicode_graphemes() {
        let chunks = ["你a", "é", "好β"];
        let bytes = timed_source(
            &[
                (1_000, 1, chunks[0].as_bytes()),
                (1_400, 1, chunks[1].as_bytes()),
                (1_800, 1, chunks[2].as_bytes()),
            ],
            2_200,
        );
        let pacing = Pacing {
            idle: None,
            typing: Some(TypingSpeed::new(10).unwrap()),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        let frames = replay_rows(&bytes, &plan, 1);
        assert!(frames[30][0].contains('你'));
        assert!(!frames[30][0].contains('a'));
        assert!(frames[33][0].contains('a'));
        assert!(frames[36][0].contains('é'));
        assert!(frames[39][0].contains('好'));
        assert!(!frames[39][0].contains('β'));
        assert!(frames[42][0].contains('β'));
        let mut original = Terminal::new(Dimensions::new(24, 8).unwrap()).unwrap();
        for chunk in chunks {
            original.ingest(chunk.as_bytes());
        }
        assert_eq!(
            frames.last().unwrap()[0],
            original.row_text(0).unwrap().trim_end()
        );
    }

    #[test]
    fn sustained_output_compression_retimes_all_streams_and_keeps_final_state() {
        let bytes = timed_source(
            &[
                (1_000, 1, b"one\r\n"),
                (4_000, 1, b"two\r\n"),
                (4_000, 2, b"X"),
                (7_000, 1, b"three\r\n"),
            ],
            8_000,
        );
        let pacing = Pacing {
            idle: None,
            long_output: Some(
                LongOutputLimit::new(Duration::from_secs(5), Duration::from_secs(2)).unwrap(),
            ),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        assert_eq!(plan.duration(), Duration::from_secs(4));
        let first = replay_rows(&bytes, &plan, 1);
        let second = replay_rows(&bytes, &plan, 2);
        assert_eq!(first[30][0], "one");
        assert_eq!(first[60][1], "two");
        assert_eq!(first[90][2], "three");
        assert_eq!(second[59][0], "");
        assert_eq!(second[60][0], "X");
        assert_eq!(&first.last().unwrap()[..3], ["one", "two", "three"]);
    }

    #[test]
    fn simultaneous_large_updates_reveal_together_and_preserve_final_screens() {
        let bytes = timed_source(
            &[
                (1_000, 1, b"a\r\nb\r\nc\r\nd\r\ne\r\nf"),
                (1_000, 1, b" tail"),
                (1_000, 2, b"1\r\n2\r\n3\r\n4\r\n5"),
            ],
            2_000,
        );
        let pacing = Pacing {
            idle: None,
            reveal: Some(OutputRevealRate::new(20).unwrap()),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        assert_eq!(plan.duration(), Duration::from_millis(2_250));
        let first = replay_rows(&bytes, &plan, 1);
        let second = replay_rows(&bytes, &plan, 2);
        assert_eq!(first[30][0], "a");
        assert_eq!(first[30][1], "");
        assert_eq!(second[30][0], "1");
        assert_eq!(first[33][2], "c");
        assert_eq!(second[33][2], "3");
        assert_eq!(first[37][5], "");
        assert_eq!(first[38][5], "f tail");
        assert_eq!(
            &first.last().unwrap()[..6],
            ["a", "b", "c", "d", "e", "f tail"]
        );
        assert_eq!(&second.last().unwrap()[..5], ["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn overlapping_pacing_uses_the_longest_required_interval() {
        let bytes = timed_source(
            &[
                (1_000, 1, b"one\r\n"),
                (2_000, 2, b"a"),
                (2_400, 2, b"b"),
                (2_800, 2, b"c"),
                (4_000, 1, b"two\r\n"),
                (7_000, 1, b"three\r\n"),
            ],
            8_000,
        );
        let pacing = Pacing {
            idle: None,
            typing: Some(TypingSpeed::new(5).unwrap()),
            long_output: Some(
                LongOutputLimit::new(Duration::from_secs(5), Duration::from_secs(2)).unwrap(),
            ),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        assert_eq!(plan.duration(), Duration::from_nanos(4_133_333_333));
        let first = replay_rows(&bytes, &plan, 1);
        let second = replay_rows(&bytes, &plan, 2);
        assert_eq!(second[40][0], "a");
        assert_eq!(second[46][0], "ab");
        assert_eq!(second[52][0], "abc");
        assert_eq!(first[64][1], "two");
        assert_eq!(first[94][2], "three");
    }

    #[test]
    fn reveal_keeps_split_control_sequences_hidden_and_preserves_unicode() {
        let output = b"A\r\n\x1b]2;not-visible\nsecret\x07B\r\nC \xcf\x80\r\nD\r\nE\r\nF";
        let bytes = timed_source(&[(1_000, 1, output)], 2_000);
        let pacing = Pacing {
            idle: None,
            reveal: Some(OutputRevealRate::new(20).unwrap()),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(&bytes), pacing).unwrap();
        let frames = replay_rows(&bytes, &plan, 1);
        assert_eq!(frames[32][0], "A");
        assert_eq!(frames[32][1], "");
        assert!(frames
            .iter()
            .flatten()
            .all(|row| !row.contains("not-visible") && !row.contains("secret")));
        let mut original = Terminal::new(Dimensions::new(24, 8).unwrap()).unwrap();
        original.ingest(output);
        let expected: Vec<_> = (0..8)
            .map(|row| original.row_text(row).unwrap().trim_end().to_owned())
            .collect();
        assert_eq!(frames.last().unwrap(), &expected);
    }

    #[test]
    fn advanced_pacing_rejects_a_source_changed_after_planning() {
        let original = timed_source(
            &[(1_000, 1, b"a"), (1_300, 1, b"b"), (1_600, 1, b"c")],
            2_000,
        );
        let changed = timed_source(
            &[(1_000, 1, b"a"), (1_300, 1, b"b"), (1_600, 1, b"d")],
            2_000,
        );
        let pacing = Pacing {
            typing: Some(TypingSpeed::new(15).unwrap()),
            ..Pacing::default()
        };
        let plan = PlaybackPlan::inspect(Cursor::new(original), pacing).unwrap();
        let mut playback =
            Playback::new(Cursor::new(changed), StreamId::from_index(1).unwrap(), plan).unwrap();
        let error = loop {
            match playback.next_frame() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("a changed source must fail before completion"),
                Err(error) => break error,
            }
        };
        assert!(error.to_string().contains("changed"));
    }

    #[test]
    fn excessive_reveal_duration_is_rejected_before_export() {
        let output = vec![b'\n'; 1024 * 1024];
        let bytes = timed_source(&[(1_000, 1, &output)], 2_000);
        let pacing = Pacing {
            idle: None,
            reveal: Some(OutputRevealRate::new(20).unwrap()),
            ..Pacing::default()
        };
        let error = PlaybackPlan::inspect(Cursor::new(bytes), pacing).unwrap_err();
        assert!(error.to_string().contains("eight-hour"));
    }

    #[test]
    fn advanced_pacing_observes_cancellation_after_the_last_source_read() {
        struct CancelAtEof<'a> {
            source: Cursor<Vec<u8>>,
            cancelled: &'a AtomicBool,
        }

        impl Read for CancelAtEof<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                let count = self.source.read(buffer)?;
                if count == 0 {
                    self.cancelled.store(true, Ordering::Release);
                }
                Ok(count)
            }
        }

        let cancelled = AtomicBool::new(false);
        let reader = CancelAtEof {
            source: Cursor::new(source(b"C", FinishReason::User)),
            cancelled: &cancelled,
        };
        let pacing = Pacing {
            typing: Some(TypingSpeed::new(15).unwrap()),
            ..Pacing::default()
        };
        let error = PlaybackPlan::inspect_cancellable(reader, pacing, &cancelled).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
