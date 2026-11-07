use std::{
    fs::{self, File},
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use sha2::{Digest, Sha256};

use super::{platform, RecordingLease, Snapshot};
use crate::{
    ArtifactEvent, ArtifactReader, FinishReason, RecordingId, RecoveryPrefix, MAX_ARTIFACT_BYTES,
    MAX_STREAMS,
};
use festerm_core::{Parser, Terminal};

const MAX_SOURCES: usize = 4096;
const INSPECT_OUTPUT_CHUNK: usize = 16 * 1024;

/// Integrity state established by inspecting the entire bounded artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordingSourceState {
    Empty,
    Complete {
        duration: Duration,
        reason: FinishReason,
    },
    Incomplete {
        detail: String,
        prefix: Option<RecoveryPrefix>,
    },
}

/// A checked source name and content snapshot. Operations revalidate the
/// source under the same exclusive storage lease before changing it.
pub struct RecordingSource {
    directory_path: PathBuf,
    directory: File,
    name: String,
    id: RecordingId,
    partial: bool,
    length: u64,
    snapshot: Snapshot,
    state: RecordingSourceState,
}

impl RecordingSource {
    pub fn recover(directory_path: &Path, lease: &RecordingLease) -> io::Result<Vec<Self>> {
        Self::recover_with_cancellation(directory_path, lease, &AtomicBool::new(false))
    }

    pub fn recover_with_cancellation(
        directory_path: &Path,
        lease: &RecordingLease,
        cancelled: &AtomicBool,
    ) -> io::Result<Vec<Self>> {
        check_cancelled(cancelled)?;
        lease.verify_for(directory_path)?;
        let directory = platform::open_directory(directory_path)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(directory_path)? {
            check_cancelled(cancelled)?;
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with("recording-")
                && (name.ends_with(".partial") || name.ends_with(".fstrec"))
            {
                source_name(name)?;
                names.push(name.to_owned());
                if names.len() > MAX_SOURCES {
                    return Err(io::Error::other("too many recording sources"));
                }
            }
        }
        names.sort();
        platform::check_directory(directory_path, &directory)?;
        let result = names
            .into_iter()
            .map(|name| {
                check_cancelled(cancelled)?;
                read_source(
                    directory_path,
                    directory.try_clone()?,
                    name,
                    Some(cancelled),
                )
            })
            .collect();
        check_cancelled(cancelled)?;
        lease.verify_for(directory_path)?;
        result
    }

    pub const fn id(&self) -> RecordingId {
        self.id
    }

    pub const fn is_partial(&self) -> bool {
        self.partial
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub fn state(&self) -> &RecordingSourceState {
        &self.state
    }

    /// SHA-256 of the entire inspected source, used to bind recovery export
    /// to the exact bytes that were shown in the library.
    pub const fn fingerprint(&self) -> [u8; 32] {
        self.snapshot.digest
    }

    pub fn path(&self) -> PathBuf {
        self.directory_path.join(&self.name)
    }

    pub fn verify(&self, lease: &RecordingLease) -> io::Result<()> {
        lease.verify_for(&self.directory_path)?;
        drop(self.verify_current(None)?);
        lease.verify_for(&self.directory_path)
    }

    pub fn verify_with_cancellation(
        &self,
        lease: &RecordingLease,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        check_cancelled(cancelled)?;
        lease.verify_for(&self.directory_path)?;
        drop(self.verify_current(Some(cancelled))?);
        check_cancelled(cancelled)?;
        lease.verify_for(&self.directory_path)
    }

    /// Deletes the exact unchanged source only after explicit user action.
    pub fn delete(self, lease: &RecordingLease) -> io::Result<()> {
        lease.verify_for(&self.directory_path)?;
        let pinned = self.verify_current(None)?;
        lease.verify_for(&self.directory_path)?;
        platform::remove_file(&self.directory, &pinned, Path::new(&self.name))?;
        drop(pinned);
        platform::sync_directory(&self.directory).map_err(|error| {
            io::Error::other(format!(
                "recording source was removed, but storage could not be synced: {error}"
            ))
        })
    }

    /// Restores a fully validated partial whose final rename was interrupted.
    /// Truncated or incomplete sources are never promoted to complete.
    pub fn promote(self, lease: &RecordingLease) -> io::Result<Self> {
        if !self.partial
            || !matches!(
                self.state,
                RecordingSourceState::Complete { reason, .. } if reason.is_complete()
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only a complete partial recording can be recovered",
            ));
        }
        lease.verify_for(&self.directory_path)?;
        let pinned = self.verify_current(None)?;
        let target = format!("recording-{}.fstrec", self.id);
        lease.verify_for(&self.directory_path)?;
        platform::publish_file(
            &self.directory,
            &pinned,
            Path::new(&self.name),
            Path::new(&target),
            false,
        )?;
        drop(pinned);
        platform::sync_directory(&self.directory).map_err(|error| {
            io::Error::other(format!(
                "recording source was renamed, but storage could not be synced: {error}"
            ))
        })?;
        let recovered = read_source(
            &self.directory_path,
            self.directory.try_clone()?,
            target,
            None,
        )?;
        if recovered.snapshot != self.snapshot || recovered.length != self.length {
            return Err(io::Error::other(
                "recovered recording changed; it was retained for inspection",
            ));
        }
        lease.verify_for(&self.directory_path)?;
        Ok(recovered)
    }

    fn verify_current(&self, cancelled: Option<&AtomicBool>) -> io::Result<File> {
        platform::check_directory(&self.directory_path, &self.directory)?;
        let pinned = platform::open_file_for_removal(&self.directory, Path::new(&self.name))?;
        platform::require_private_file(&pinned)?;
        let current = read_source(
            &self.directory_path,
            self.directory.try_clone()?,
            self.name.clone(),
            cancelled,
        )?;
        if platform::identity(&pinned)? != self.snapshot.identity
            || current.snapshot != self.snapshot
            || current.length != self.length
        {
            return Err(io::Error::other(
                "recording source changed and was retained",
            ));
        }
        Ok(pinned)
    }
}

fn source_name(name: &str) -> io::Result<(RecordingId, bool)> {
    let value = name
        .strip_prefix("recording-")
        .ok_or_else(|| io::Error::other("invalid recording source name"))?;
    let (value, partial) = if let Some(value) = value.strip_suffix(".partial") {
        (value, true)
    } else if let Some(value) = value.strip_suffix(".fstrec") {
        (value, false)
    } else {
        return Err(io::Error::other("invalid recording source name"));
    };
    let id = RecordingId::parse(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if value != id.to_string() {
        return Err(io::Error::other("invalid recording source name"));
    }
    Ok((id, partial))
}

fn read_source(
    directory_path: &Path,
    directory: File,
    name: String,
    cancelled: Option<&AtomicBool>,
) -> io::Result<RecordingSource> {
    let (id, partial) = source_name(&name)?;
    let file = platform::open_file_read(&directory, Path::new(&name))?;
    platform::require_private_file(&file)?;
    let before = file.metadata()?;
    if before.len() > MAX_ARTIFACT_BYTES {
        return Err(io::Error::other("recording source exceeds its size limit"));
    }
    let identity = platform::identity(&file)?;
    let mut input = HashingReader {
        inner: BufReader::new(file),
        digest: Sha256::new(),
        length: 0,
        cancelled,
    };
    let state = if before.len() == 0 {
        RecordingSourceState::Empty
    } else {
        inspect(&mut input, cancelled)?
    };
    io::copy(&mut input, &mut io::sink())?;
    let after = input.inner.get_ref().metadata()?;
    if before.len() != after.len()
        || before.modified()? != after.modified()?
        || input.length != before.len()
    {
        return Err(io::Error::other(
            "recording source changed during inspection",
        ));
    }
    let snapshot = Snapshot {
        identity,
        digest: input.digest.finalize().into(),
    };
    platform::check_directory(directory_path, &directory)?;
    Ok(RecordingSource {
        directory_path: directory_path.to_owned(),
        directory,
        name,
        id,
        partial,
        length: before.len(),
        snapshot,
        state,
    })
}

fn inspect(input: impl Read, cancelled: Option<&AtomicBool>) -> io::Result<RecordingSourceState> {
    let mut reader = match ArtifactReader::new(input) {
        Ok(reader) => reader,
        Err(error) => {
            return Ok(RecordingSourceState::Incomplete {
                detail: error.to_string(),
                prefix: None,
            });
        }
    };
    let mut terminals: [Option<Terminal>; MAX_STREAMS] = std::array::from_fn(|_| None);
    let mut parsers: [Parser; MAX_STREAMS] = std::array::from_fn(|_| Parser::new());
    let mut prefix = None;
    let mut required_checkpoint = None;
    loop {
        if let Some(cancelled) = cancelled {
            check_cancelled(cancelled)?;
        }
        let event = match reader.next_event() {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Ok(RecordingSourceState::Incomplete {
                    detail: "Recording has no finish event".to_owned(),
                    prefix,
                });
            }
            Err(error) => {
                return Ok(RecordingSourceState::Incomplete {
                    detail: error.to_string(),
                    prefix,
                });
            }
        };
        if let Some((expected_stream, expected_at)) = required_checkpoint {
            if !matches!(
                &event,
                ArtifactEvent::Checkpoint { stream, at, .. }
                    if *stream == expected_stream && *at == expected_at
            ) {
                return Ok(RecordingSourceState::Incomplete {
                    detail: "A required post-output checkpoint is missing".to_owned(),
                    prefix,
                });
            }
            required_checkpoint = None;
        }
        match event {
            ArtifactEvent::Started {
                stream, terminal, ..
            }
            | ArtifactEvent::Checkpoint {
                stream, terminal, ..
            }
            | ArtifactEvent::Resized {
                stream, terminal, ..
            } => {
                let slot = stream.index() - 1;
                terminals[slot] = Some(terminal);
                parsers[slot] = Parser::new();
            }
            ArtifactEvent::Output { at, stream, bytes } => {
                let slot = stream.index() - 1;
                let Some(terminal) = terminals[slot].as_mut() else {
                    return Ok(RecordingSourceState::Incomplete {
                        detail: "Recording output has no valid screen".to_owned(),
                        prefix,
                    });
                };
                let generation = terminal.screen_generation();
                let mut repeated = false;
                for chunk in bytes.chunks(INSPECT_OUTPUT_CHUNK) {
                    if let Some(cancelled) = cancelled {
                        check_cancelled(cancelled)?;
                    }
                    repeated |= parsers[slot].observe_recording_output(chunk);
                    terminal.ingest(chunk);
                }
                let _ = terminal.drain_replies();
                let _ = terminal.take_reply_queue_overflowed();
                if repeated || terminal.screen_generation() != generation {
                    required_checkpoint = Some((stream, at));
                }
            }
            ArtifactEvent::Ended { stream, .. } => {
                let slot = stream.index() - 1;
                terminals[slot] = None;
                parsers[slot] = Parser::new();
            }
            ArtifactEvent::Finished { at, reason } => {
                return Ok(if reason.is_complete() {
                    RecordingSourceState::Complete {
                        duration: at,
                        reason,
                    }
                } else {
                    RecordingSourceState::Incomplete {
                        detail: format!("Recording stopped before completion ({reason:?})"),
                        prefix,
                    }
                });
            }
        }
        if required_checkpoint.is_none() {
            prefix = reader.recovery_prefix();
        }
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::other("recording inspection was cancelled"))
    } else {
        Ok(())
    }
}

struct HashingReader<'a, R> {
    inner: R,
    digest: Sha256,
    length: u64,
    cancelled: Option<&'a AtomicBool>,
}

impl<R: Read> Read for HashingReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Some(cancelled) = self.cancelled {
            check_cancelled(cancelled)?;
        }
        let count = self.inner.read(buffer)?;
        self.length = self
            .length
            .checked_add(count as u64)
            .filter(|length| *length <= MAX_ARTIFACT_BYTES)
            .ok_or_else(|| io::Error::other("recording source exceeds its size limit"))?;
        self.digest.update(&buffer[..count]);
        Ok(count)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        Completion, Pacing, Playback, PlaybackPlan, PreparedRecording, Recorder, StreamId,
        TypingSpeed,
    };
    use festerm_core::{Dimensions, Terminal};
    use std::{io::Cursor, sync::mpsc, time::Duration};

    fn private_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        festerm_unix_security::make_private_directory(&File::open(root.path()).unwrap()).unwrap();
        root
    }

    fn complete_source(root: &Path) -> PathBuf {
        let terminal = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
        let (wake, ready) = mpsc::sync_channel(1);
        let (mut recorder, _) = Recorder::start(root, &[&terminal], move || {
            let _ = wake.send(());
        })
        .unwrap();
        recorder.stop(FinishReason::User).unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let Some(Completion::Complete { path, .. }) = recorder.poll_completion() else {
            panic!("recording did not finish");
        };
        path
    }

    fn source_with_output(root: &Path, outputs: &[&[u8]]) -> PathBuf {
        let mut terminal = Terminal::new(Dimensions::new(20, 3).unwrap()).unwrap();
        let (wake, ready) = mpsc::sync_channel(1);
        let (mut recorder, streams) = Recorder::start(root, &[&terminal], move || {
            let _ = wake.send(());
        })
        .unwrap();
        for bytes in outputs {
            let generation = terminal.screen_generation();
            terminal.ingest(bytes);
            recorder
                .observe_output(streams[0], bytes, generation, &terminal)
                .unwrap();
        }
        recorder.stop(FinishReason::User).unwrap();
        ready.recv_timeout(Duration::from_secs(10)).unwrap();
        let Some(Completion::Complete { path, .. }) = recorder.poll_completion() else {
            panic!("recording did not finish");
        };
        path
    }

    fn frame_offsets(bytes: &[u8]) -> Vec<usize> {
        let mut offsets = Vec::new();
        let mut offset = 8;
        while offset < bytes.len() {
            offsets.push(offset);
            let length = u32::from_le_bytes(bytes[offset + 10..offset + 14].try_into().unwrap());
            offset += 18 + length as usize;
        }
        assert_eq!(offset, bytes.len());
        offsets
    }

    fn recovered_prefix(source: &RecordingSource) -> RecoveryPrefix {
        match source.state() {
            RecordingSourceState::Incomplete {
                prefix: Some(prefix),
                ..
            } => *prefix,
            state => panic!("expected a recoverable prefix, got {state:?}"),
        }
    }

    #[test]
    fn recovery_stops_before_corrupt_truncated_or_semantically_invalid_frames() {
        let root = private_root();
        let path = source_with_output(root.path(), &[b"first", b"second"]);
        let complete = fs::read(&path).unwrap();
        let offsets = frame_offsets(&complete);
        assert_eq!(offsets.len(), 4);
        let lease = RecordingLease::acquire(root.path()).unwrap();
        for damage in 0..3 {
            let mut damaged = complete.clone();
            if damage == 0 {
                damaged.truncate(offsets[2] + 20);
            } else if damage == 1 {
                damaged[offsets[2] + 18] ^= 0x40;
            } else {
                let offset = offsets[2];
                damaged[offset + 1] = 2;
                let mut checksum = crc32fast::Hasher::new();
                checksum.update(&damaged[offset..offset + 14]);
                checksum.update(&damaged[offset + 18..offsets[3]]);
                damaged[offset + 14..offset + 18]
                    .copy_from_slice(&checksum.finalize().to_le_bytes());
            }
            fs::write(&path, &damaged).unwrap();
            let source = RecordingSource::recover(root.path(), &lease)
                .unwrap()
                .remove(0);
            let prefix = recovered_prefix(&source);
            assert_eq!(prefix.length(), offsets[2] as u64);
            assert_eq!(prefix.stream_count(), 1);
            let mut reader = ArtifactReader::new(prefix.reader(Cursor::new(&damaged))).unwrap();
            assert!(matches!(
                reader.next_event().unwrap(),
                Some(ArtifactEvent::Started { .. })
            ));
            assert!(matches!(
                reader.next_event().unwrap(),
                Some(ArtifactEvent::Output { bytes, .. }) if bytes == b"first"
            ));
            assert!(matches!(
                reader.next_event().unwrap(),
                Some(ArtifactEvent::Finished {
                    reason: FinishReason::RecoveredPrefix,
                    ..
                })
            ));
            assert_eq!(reader.fingerprint(), Some(prefix.fingerprint()));
            assert_eq!(fs::read(&path).unwrap(), damaged);

            for advanced in [false, true] {
                let pacing = Pacing {
                    typing: advanced.then(|| TypingSpeed::new(15).unwrap()),
                    ..Pacing::default()
                };
                let cancelled = AtomicBool::new(false);
                let plan = PlaybackPlan::inspect_recovered_cancellable(
                    prefix.reader(Cursor::new(&damaged)),
                    prefix,
                    pacing,
                    &cancelled,
                )
                .unwrap();
                let mut playback = Playback::new(
                    prefix.reader(Cursor::new(&damaged)),
                    StreamId::from_index(1).unwrap(),
                    plan,
                )
                .unwrap();
                while playback.next_frame().unwrap().is_some() {}
                assert!(
                    PlaybackPlan::inspect(prefix.reader(Cursor::new(&damaged)), pacing).is_err()
                );
            }

            let shortened = &damaged[..prefix.length() as usize - 1];
            let mut reader = ArtifactReader::new(prefix.reader(Cursor::new(shortened))).unwrap();
            assert!(reader.next_event().is_ok());
            assert!(reader.next_event().is_err());
        }
    }

    #[test]
    fn recovery_omits_output_without_its_required_post_mutation_checkpoint() {
        let root = private_root();
        let path = source_with_output(root.path(), &[b"first", b"\x1b[3b"]);
        let mut bytes = fs::read(&path).unwrap();
        let offsets = frame_offsets(&bytes);
        assert_eq!(offsets.len(), 5);
        bytes.truncate(offsets[3]);
        fs::write(&path, &bytes).unwrap();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let source = RecordingSource::recover(root.path(), &lease)
            .unwrap()
            .remove(0);
        let prefix = recovered_prefix(&source);
        assert_eq!(prefix.length(), offsets[2] as u64);
    }

    #[test]
    fn reports_orphans_and_incomplete_sources_and_refuses_changed_deletion() {
        let root = private_root();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let id = RecordingId::new();
        drop(PreparedRecording::create(root.path(), id).unwrap());
        let path = root.path().join(format!("recording-{id}.partial"));
        let mut sources = RecordingSource::recover(root.path(), &lease).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].state(), &RecordingSourceState::Empty);
        fs::write(&path, b"not a complete recording").unwrap();
        assert!(sources.remove(0).delete(&lease).is_err());
        assert!(path.exists());

        let mut sources = RecordingSource::recover(root.path(), &lease).unwrap();
        assert!(matches!(
            sources[0].state(),
            RecordingSourceState::Incomplete { .. }
        ));
        sources.remove(0).delete(&lease).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn recovers_only_validated_complete_partials_without_overwriting() {
        let root = private_root();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let complete = complete_source(root.path());
        let partial = complete.with_extension("partial");
        fs::rename(&complete, &partial).unwrap();
        let mut sources = RecordingSource::recover(root.path(), &lease).unwrap();
        assert!(sources[0].is_partial());
        assert!(matches!(
            sources[0].state(),
            RecordingSourceState::Complete { .. }
        ));
        fs::copy(&partial, &complete).unwrap();
        assert!(sources.remove(0).promote(&lease).is_err());
        assert!(complete.exists() && partial.exists());
        fs::remove_file(&complete).unwrap();
        let mut sources = RecordingSource::recover(root.path(), &lease).unwrap();
        let recovered = sources.remove(0).promote(&lease).unwrap();
        assert!(!recovered.is_partial());
        assert_eq!(recovered.path(), complete);
        assert!(!partial.exists());
    }

    #[test]
    fn refuses_unsafe_or_foreign_storage() {
        let root = private_root();
        let other = private_root();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        assert!(RecordingSource::recover(other.path(), &lease).is_err());
        let name = format!("recording-{}.partial", RecordingId::new());
        std::os::unix::fs::symlink(other.path(), root.path().join(name)).unwrap();
        assert!(RecordingSource::recover(root.path(), &lease).is_err());
    }

    #[test]
    fn cancelled_inspection_does_not_consume_or_remove_a_source() {
        let root = private_root();
        let lease = RecordingLease::acquire(root.path()).unwrap();
        let path = complete_source(root.path());
        let cancelled = AtomicBool::new(true);
        assert!(
            RecordingSource::recover_with_cancellation(root.path(), &lease, &cancelled).is_err()
        );
        assert!(path.exists());
    }
}
