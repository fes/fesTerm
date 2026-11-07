use std::{cmp::Ordering, collections::BTreeMap, io::Read, time::Duration};

use festerm_core::Terminal;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    ArtifactEvent, ArtifactReader, Pacing, PlaybackError, PlaybackSpeed, StreamId, MAX_STREAMS,
};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const MIN_TYPING_GAP_NS: u64 = 40_000_000;
const MAX_TYPING_GAP_NS: u64 = NANOS_PER_SECOND;
const DEFAULT_OUTPUT_GAP_NS: u64 = 3 * NANOS_PER_SECOND;
const MIN_TYPING_UPDATES: usize = 3;
const MIN_REVEAL_PIECES: usize = 5;
const MAX_ANALYZED_EVENTS: usize = 1_000_000;
const MAX_EXPORT_DURATION_NS: u64 = 8 * 60 * 60 * NANOS_PER_SECOND;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitKind {
    Typing,
    Lines,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Split {
    pub(crate) kind: SplitKind,
    pub(crate) pieces: usize,
    rate: u16,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct EventTiming {
    start_ns: u64,
    pub(crate) split: Option<Split>,
}

impl EventTiming {
    pub(crate) fn at(self, piece: usize, speed: PlaybackSpeed) -> Result<u64, PlaybackError> {
        let offset = match self.split {
            Some(split) => {
                if piece >= split.pieces {
                    return Err(PlaybackError::new("recording output split is invalid"));
                }
                (piece as u128 * u128::from(NANOS_PER_SECOND)) / u128::from(split.rate)
            }
            None if piece == 0 => 0,
            None => return Err(PlaybackError::new("recording output split is invalid")),
        };
        let unscaled = u128::from(self.start_ns) + offset;
        let (numerator, denominator) = speed.ratio();
        u64::try_from(unscaled * u128::from(denominator) / u128::from(numerator))
            .map_err(|_| PlaybackError::new("recording time transform overflows"))
    }
}

#[derive(Debug)]
pub(crate) struct AdvancedTimeline {
    timings: Vec<EventTiming>,
    duration: Duration,
    stream_count: usize,
    fingerprint: [u8; 32],
}

impl AdvancedTimeline {
    pub(crate) fn inspect<R: Read>(source: R, pacing: Pacing) -> Result<Self, PlaybackError> {
        let mut reader = ArtifactReader::new(source)?;
        let mut entries = Vec::new();
        let mut terminals: [Option<Terminal>; MAX_STREAMS] = std::array::from_fn(|_| None);
        let mut stream_count = 0;
        loop {
            let event = reader
                .next_event()?
                .ok_or_else(|| PlaybackError::new("recording ended without a finish frame"))?;
            if entries.len() == MAX_ANALYZED_EVENTS {
                return Err(PlaybackError::new(
                    "this recording has too many events for output pacing",
                ));
            }
            entries.try_reserve(1).map_err(|error| {
                PlaybackError::new(format!("could not allocate the pacing plan: {error}"))
            })?;
            let at = nanos(event.timestamp())?;
            let (stream, kind) = match event {
                ArtifactEvent::Started {
                    stream, terminal, ..
                } => {
                    terminals[stream.index() - 1] = Some(terminal);
                    stream_count += 1;
                    (Some(stream), EntryKind::Boundary)
                }
                ArtifactEvent::Checkpoint {
                    stream, terminal, ..
                }
                | ArtifactEvent::Resized {
                    stream, terminal, ..
                } => {
                    terminals[stream.index() - 1] = Some(terminal);
                    (Some(stream), EntryKind::Boundary)
                }
                ArtifactEvent::Output { stream, bytes, .. } => {
                    let terminal = terminals[stream.index() - 1]
                        .as_mut()
                        .ok_or_else(|| PlaybackError::new("recording output has no screen"))?;
                    let before = terminal.cursor();
                    let pieces = echo_pieces(&bytes);
                    terminal.ingest(&bytes);
                    let _ = terminal.drain_replies();
                    let _ = terminal.take_reply_queue_overflowed();
                    let after = terminal.cursor();
                    let simple_pieces = pieces.filter(|pieces| {
                        before.row() == after.row()
                            && after
                                .column()
                                .checked_sub(before.column())
                                .is_some_and(|advance| advance > 0 && advance <= pieces * 2)
                    });
                    (
                        Some(stream),
                        EntryKind::Output {
                            simple_pieces: simple_pieces.unwrap_or(0) as u8,
                            line_pieces: line_pieces(&bytes),
                            typing: false,
                        },
                    )
                }
                ArtifactEvent::Ended { stream, .. } => {
                    terminals[stream.index() - 1] = None;
                    (Some(stream), EntryKind::Boundary)
                }
                ArtifactEvent::Finished { reason, .. } => {
                    if !reason.is_complete() {
                        return Err(PlaybackError::new(
                            "an incomplete recording requires explicit recovery",
                        ));
                    }
                    (None, EntryKind::Finish)
                }
            };
            let finished = matches!(kind, EntryKind::Finish);
            entries.push(Entry { at, stream, kind });
            if finished {
                break;
            }
        }

        let mut spans = Vec::new();
        classify_typing(&mut entries, pacing, &mut spans)?;
        if let Some(limit) = pacing.long_output {
            classify_long_output(
                &entries,
                pacing.idle.map_or(DEFAULT_OUTPUT_GAP_NS, |idle| {
                    idle.threshold().as_nanos() as u64
                }),
                nanos(limit.threshold())?,
                nanos(limit.target())?,
                &mut spans,
            )?;
        }
        let (timings, duration) = schedule(&entries, &spans, pacing)?;
        Ok(Self {
            timings,
            duration,
            stream_count,
            fingerprint: reader
                .fingerprint()
                .expect("a validated finish provides a source fingerprint"),
        })
    }

    pub(crate) const fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) const fn stream_count(&self) -> usize {
        self.stream_count
    }

    pub(crate) const fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    pub(crate) fn timing(&self, index: usize) -> Result<EventTiming, PlaybackError> {
        self.timings
            .get(index)
            .copied()
            .ok_or_else(|| PlaybackError::new("recording changed after pacing was planned"))
    }

    pub(crate) fn len(&self) -> usize {
        self.timings.len()
    }
}

fn nanos(duration: Duration) -> Result<u64, PlaybackError> {
    u64::try_from(duration.as_nanos())
        .map_err(|_| PlaybackError::new("recording timestamp overflows"))
}

pub(crate) fn echo_pieces(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || bytes.len() > 64 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text.chars().any(char::is_control) {
        return None;
    }
    let pieces = text.graphemes(true).take(5).count();
    (1..=4).contains(&pieces).then_some(pieces)
}

pub(crate) fn line_pieces(bytes: &[u8]) -> usize {
    let lines = bytes.iter().filter(|byte| **byte == b'\n').count();
    lines + usize::from(bytes.last().is_some_and(|byte| *byte != b'\n'))
}

#[derive(Clone, Copy)]
struct Entry {
    at: u64,
    stream: Option<StreamId>,
    kind: EntryKind,
}

#[derive(Clone, Copy)]
enum EntryKind {
    Boundary,
    Output {
        simple_pieces: u8,
        line_pieces: usize,
        typing: bool,
    },
    Finish,
}

fn classify_typing(
    entries: &mut [Entry],
    pacing: Pacing,
    spans: &mut Vec<Span>,
) -> Result<(), PlaybackError> {
    let mut runs: [Vec<usize>; MAX_STREAMS] = std::array::from_fn(|_| Vec::new());
    for index in 0..entries.len() {
        let Some(stream) = entries[index].stream else {
            continue;
        };
        let run = &mut runs[stream.index() - 1];
        let simple = matches!(
            entries[index].kind,
            EntryKind::Output {
                simple_pieces: 1..,
                ..
            }
        );
        if simple {
            if let Some(previous) = run.last() {
                let gap = entries[index].at - entries[*previous].at;
                if !(MIN_TYPING_GAP_NS..=MAX_TYPING_GAP_NS).contains(&gap) {
                    finish_typing_run(run, entries, pacing, spans)?;
                }
            }
            run.push(index);
        } else {
            finish_typing_run(run, entries, pacing, spans)?;
        }
    }
    for run in &mut runs {
        finish_typing_run(run, entries, pacing, spans)?;
    }
    Ok(())
}

fn finish_typing_run(
    run: &mut Vec<usize>,
    entries: &mut [Entry],
    pacing: Pacing,
    spans: &mut Vec<Span>,
) -> Result<(), PlaybackError> {
    if run.len() >= MIN_TYPING_UPDATES {
        for &index in run.iter() {
            if let EntryKind::Output { typing, .. } = &mut entries[index].kind {
                *typing = true;
            }
        }
        if let Some(speed) = pacing.typing {
            let target = NANOS_PER_SECOND / u64::from(speed.characters_per_second());
            for pair in run.windows(2) {
                push_span(spans, entries[pair[0]].at, entries[pair[1]].at, target)?;
            }
        }
    }
    run.clear();
    Ok(())
}

#[derive(Clone, Copy)]
struct OutputRun {
    first: u64,
    last: u64,
}

fn classify_long_output(
    entries: &[Entry],
    max_gap: u64,
    threshold: u64,
    target: u64,
    spans: &mut Vec<Span>,
) -> Result<(), PlaybackError> {
    let mut runs: [Option<OutputRun>; MAX_STREAMS] = [None; MAX_STREAMS];
    for entry in entries {
        let Some(stream) = entry.stream else {
            continue;
        };
        let run = &mut runs[stream.index() - 1];
        let output = matches!(entry.kind, EntryKind::Output { typing: false, .. });
        if output {
            if run.is_some_and(|active| entry.at - active.last > max_gap) {
                finish_output_run(run, threshold, target, spans)?;
            }
            if let Some(active) = run {
                active.last = entry.at;
            } else {
                *run = Some(OutputRun {
                    first: entry.at,
                    last: entry.at,
                });
            }
        } else {
            finish_output_run(run, threshold, target, spans)?;
        }
    }
    for run in &mut runs {
        finish_output_run(run, threshold, target, spans)?;
    }
    Ok(())
}

fn finish_output_run(
    run: &mut Option<OutputRun>,
    threshold: u64,
    target: u64,
    spans: &mut Vec<Span>,
) -> Result<(), PlaybackError> {
    if let Some(active) = run.take() {
        if active.last - active.first > threshold {
            push_span(spans, active.first, active.last, target)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct Ratio {
    numerator: u64,
    denominator: u64,
}

impl PartialEq for Ratio {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Ratio {}

impl PartialOrd for Ratio {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ratio {
    fn cmp(&self, other: &Self) -> Ordering {
        (u128::from(self.numerator) * u128::from(other.denominator))
            .cmp(&(u128::from(other.numerator) * u128::from(self.denominator)))
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: u64,
    end: u64,
    ratio: Ratio,
}

fn push_span(
    spans: &mut Vec<Span>,
    start: u64,
    end: u64,
    target: u64,
) -> Result<(), PlaybackError> {
    if start >= end || target == 0 {
        return Err(PlaybackError::new("invalid recording pacing interval"));
    }
    spans.try_reserve(1).map_err(|error| {
        PlaybackError::new(format!("could not allocate the pacing plan: {error}"))
    })?;
    spans.push(Span {
        start,
        end,
        ratio: Ratio {
            numerator: target,
            denominator: end - start,
        },
    });
    Ok(())
}

fn schedule(
    entries: &[Entry],
    spans: &[Span],
    pacing: Pacing,
) -> Result<(Vec<EventTiming>, Duration), PlaybackError> {
    let mut endpoints = Vec::new();
    endpoints
        .try_reserve(spans.len().saturating_mul(2))
        .map_err(|error| {
            PlaybackError::new(format!("could not allocate the pacing plan: {error}"))
        })?;
    for span in spans {
        endpoints.push((span.start, true, span.ratio));
        endpoints.push((span.end, false, span.ratio));
    }
    endpoints.sort_unstable_by_key(|(at, start, _)| (*at, *start));

    let mut active: BTreeMap<Ratio, usize> = BTreeMap::new();
    let mut endpoint_index = 0;
    let mut previous = 0;
    let mut elapsed = 0u64;
    let mut timings = Vec::new();
    timings.try_reserve_exact(entries.len()).map_err(|error| {
        PlaybackError::new(format!("could not allocate the pacing plan: {error}"))
    })?;
    let mut index = 0;
    while index < entries.len() {
        let at = entries[index].at;
        let gap = at - previous;
        let transformed = if let Some((ratio, _)) = active.last_key_value() {
            u64::try_from(
                u128::from(gap) * u128::from(ratio.numerator) / u128::from(ratio.denominator),
            )
            .map_err(|_| PlaybackError::new("recording time transform overflows"))?
        } else if let Some(idle) = pacing
            .idle
            .filter(|idle| gap > idle.threshold().as_nanos() as u64)
        {
            nanos(idle.target())?
        } else {
            gap
        };
        elapsed = elapsed
            .checked_add(transformed)
            .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
        while endpoint_index < endpoints.len() && endpoints[endpoint_index].0 == at {
            let (_, start, ratio) = endpoints[endpoint_index];
            if start {
                *active.entry(ratio).or_default() += 1;
            } else {
                let count = active
                    .get_mut(&ratio)
                    .ok_or_else(|| PlaybackError::new("invalid recording pacing interval"))?;
                *count -= 1;
                if *count == 0 {
                    active.remove(&ratio);
                }
            }
            endpoint_index += 1;
        }
        let mut offsets = [0u64; MAX_STREAMS];
        while index < entries.len() && entries[index].at == at {
            let entry = entries[index];
            let offset = entry.stream.map_or_else(
                || offsets.iter().copied().max().unwrap_or(0),
                |stream| offsets[stream.index() - 1],
            );
            let split = split_for(entry, pacing);
            let start_ns = elapsed
                .checked_add(offset)
                .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
            timings.push(EventTiming { start_ns, split });
            if let (Some(stream), Some(split)) = (entry.stream, split) {
                let duration = u64::try_from(
                    (split.pieces as u128 - 1) * u128::from(NANOS_PER_SECOND)
                        / u128::from(split.rate),
                )
                .map_err(|_| PlaybackError::new("recording time transform overflows"))?;
                offsets[stream.index() - 1] = offset
                    .checked_add(duration)
                    .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
            }
            index += 1;
        }
        elapsed = elapsed
            .checked_add(offsets.into_iter().max().unwrap_or(0))
            .ok_or_else(|| PlaybackError::new("recording time transform overflows"))?;
        if scale(elapsed, pacing.speed)? > MAX_EXPORT_DURATION_NS {
            return Err(PlaybackError::new(
                "the selected output pacing would exceed the eight-hour export limit",
            ));
        }
        previous = at;
    }
    Ok((timings, Duration::from_nanos(scale(elapsed, pacing.speed)?)))
}

fn split_for(entry: Entry, pacing: Pacing) -> Option<Split> {
    let EntryKind::Output {
        simple_pieces,
        line_pieces,
        typing,
    } = entry.kind
    else {
        return None;
    };
    if typing && simple_pieces > 1 {
        return pacing.typing.map(|speed| Split {
            kind: SplitKind::Typing,
            pieces: usize::from(simple_pieces),
            rate: u16::from(speed.characters_per_second()),
        });
    }
    if line_pieces >= MIN_REVEAL_PIECES {
        return pacing.reveal.map(|rate| Split {
            kind: SplitKind::Lines,
            pieces: line_pieces,
            rate: rate.lines_per_second(),
        });
    }
    None
}

fn scale(unscaled: u64, speed: PlaybackSpeed) -> Result<u64, PlaybackError> {
    let (numerator, denominator) = speed.ratio();
    u64::try_from(u128::from(unscaled) * u128::from(denominator) / u128::from(numerator))
        .map_err(|_| PlaybackError::new("recording time transform overflows"))
}
