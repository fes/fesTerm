use crate::{
    platform,
    program::{
        encode_program_worker_frame, ProgramBundleLoader, ProgramBundleMessage, ProgramHostMessage,
        ProgramOutcome, ProgramWorkerFailure, ProgramWorkerMessage, WriteAcceptance,
        PROGRAM_API_VERSION, PROGRAM_PROTOCOL_VERSION,
    },
    protocol::Decoder,
    quickjs_program_runtime::{run_inline_program, InlineProgramTransport},
    snapshot::MAX_SNAPSHOT_BYTES,
    Error,
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

const LOAD_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);

/// Program-mode worker entry. Native confinement completes before `Ready` is
/// written or any source byte is read.
pub fn program_worker_entry(input: impl Read, output: impl Write) -> Result<(), Error> {
    let profile = platform::availability()?;
    platform::confine()?;
    let mut pipe = ProgramPipe {
        input,
        output,
        decoder: Decoder::default(),
    };
    pipe.write(&ProgramWorkerMessage::Ready {
        profile: profile.into(),
    })?;
    let result = run_worker(&mut pipe);
    if let Err(error) = &result {
        let code = match error {
            Error::Unavailable(_) => ProgramWorkerFailure::Unavailable,
            Error::Snapshot => ProgramWorkerFailure::InvalidSnapshot,
            Error::Protocol => ProgramWorkerFailure::Protocol,
            Error::Deadline => ProgramWorkerFailure::Deadline,
            Error::Io(_) => ProgramWorkerFailure::Io,
            _ => ProgramWorkerFailure::Runtime,
        };
        let _ = pipe.write(&ProgramWorkerMessage::Failed { code });
    }
    result
}

fn run_worker<R: Read, W: Write>(pipe: &mut ProgramPipe<R, W>) -> Result<(), Error> {
    let begin = pipe.wait::<ProgramBundleMessage>(Instant::now() + LOAD_TIMEOUT)?;
    begin.validate()?;
    let ProgramBundleMessage::Begin {
        generation,
        bundle_digest,
        snapshot_bytes,
    } = begin
    else {
        return Err(Error::Protocol);
    };
    let size = snapshot_bytes as usize;
    if size == 0 || size > MAX_SNAPSHOT_BYTES {
        return Err(Error::Protocol);
    }
    let deadline = Instant::now() + LOAD_TIMEOUT;
    let mut loader = ProgramBundleLoader::begin(bundle_digest, size)?;
    while loader.remaining() > 0 {
        let message = pipe.wait::<ProgramBundleMessage>(deadline)?;
        message.validate()?;
        match message {
            ProgramBundleMessage::SourceChunk {
                generation: source_generation,
                bytes,
            } if source_generation == generation => loader.push(&bytes)?,
            _ => return Err(Error::Protocol),
        }
    }
    let program = loader.finish()?;
    let launch = pipe.wait::<ProgramHostMessage>(deadline)?;
    launch.validate()?;
    let ProgramHostMessage::Launch {
        version,
        api_version,
        generation: launch_generation,
        bundle_digest,
        initial_size,
    } = launch
    else {
        return Err(Error::Protocol);
    };
    if version != PROGRAM_PROTOCOL_VERSION
        || api_version != PROGRAM_API_VERSION
        || launch_generation != generation
        || bundle_digest != program.digest()
    {
        return Err(Error::Protocol);
    }
    let mut transport = InlinePipe {
        pipe,
        generation,
        next_write_id: 1,
    };
    run_inline_program(program, generation, initial_size, &mut transport)
}

fn publish_output<R: Read, W: Write>(
    pipe: &mut ProgramPipe<R, W>,
    generation: u64,
    output: Vec<Vec<u8>>,
    next_write_id: &mut u64,
) -> Result<(), Error> {
    for bytes in output {
        let write_id = *next_write_id;
        *next_write_id = (*next_write_id).checked_add(1).ok_or(Error::Protocol)?;
        pipe.write(&ProgramWorkerMessage::Write {
            generation,
            write_id,
            bytes,
        })?;
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let result = pipe.wait::<ProgramHostMessage>(deadline)?;
        result.validate()?;
        match result {
            ProgramHostMessage::WriteResult {
                generation: result_generation,
                write_id: result_id,
                acceptance: WriteAcceptance::Accepted,
            } if result_generation == generation && result_id == write_id => {}
            _ => return Err(Error::Protocol),
        }
    }
    Ok(())
}

#[cfg(test)]
fn message_generation(message: &ProgramHostMessage) -> u64 {
    match message {
        ProgramHostMessage::Launch { generation, .. }
        | ProgramHostMessage::Input { generation, .. }
        | ProgramHostMessage::Resize { generation, .. }
        | ProgramHostMessage::Close { generation, .. }
        | ProgramHostMessage::WriteResult { generation, .. }
        | ProgramHostMessage::Cancel { generation } => *generation,
    }
}

struct ProgramPipe<R, W> {
    input: R,
    output: W,
    decoder: Decoder,
}

struct InlinePipe<'a, R, W> {
    pipe: &'a mut ProgramPipe<R, W>,
    generation: u64,
    next_write_id: u64,
}

impl<R: Read, W: Write> InlineProgramTransport for InlinePipe<'_, R, W> {
    fn started(&mut self, generation: u64) -> rquickjs::Result<()> {
        self.pipe
            .write(&ProgramWorkerMessage::Started { generation })
            .map_err(|_| rquickjs::Error::Exception)
    }

    fn receive(&mut self) -> rquickjs::Result<ProgramHostMessage> {
        let message = self
            .pipe
            .wait_idle::<ProgramHostMessage>()
            .map_err(|_| rquickjs::Error::Exception)?;
        message.validate().map_err(|_| rquickjs::Error::Exception)?;
        Ok(message)
    }

    fn output(&mut self, output: Vec<Vec<u8>>) -> rquickjs::Result<()> {
        publish_output(self.pipe, self.generation, output, &mut self.next_write_id)
            .map_err(|_| rquickjs::Error::Exception)
    }

    fn completed(&mut self, generation: u64, sequence: u64) -> rquickjs::Result<()> {
        self.pipe
            .write(&ProgramWorkerMessage::EventCompleted {
                generation,
                sequence,
            })
            .map_err(|_| rquickjs::Error::Exception)
    }

    fn finished(&mut self, generation: u64, outcome: ProgramOutcome) -> rquickjs::Result<()> {
        self.pipe
            .write(&ProgramWorkerMessage::Finished {
                generation,
                outcome,
            })
            .map_err(|_| rquickjs::Error::Exception)
    }
}

impl<R: Read, W: Write> ProgramPipe<R, W> {
    fn wait<T: serde::de::DeserializeOwned>(&mut self, deadline: Instant) -> Result<T, Error> {
        loop {
            if let Some(value) = self.decoder.read(&mut self.input)? {
                return Ok(value);
            }

            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_idle<T: serde::de::DeserializeOwned>(&mut self) -> Result<T, Error> {
        loop {
            if let Some(value) = self.decoder.read(&mut self.input)? {
                return Ok(value);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn write(&mut self, message: &ProgramWorkerMessage) -> Result<(), Error> {
        let frame = encode_program_worker_frame(message)?;
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut sent = 0;
        while sent < frame.len() {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            match self.output.write(&frame[sent..]) {
                Ok(0) => return Err(Error::Protocol),
                Ok(count) => sent += count,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
            std::thread::yield_now();
        }
        self.output.flush().map_err(Error::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{developer_echo_program, TerminalSize};

    #[test]
    fn worker_protocol_rejects_unknown_and_cross_generation_frames() {
        assert!(serde_json::from_str::<ProgramBundleMessage>(
            r#"{"type":"begin","generation":1,"bundleDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","snapshotBytes":1,"extra":true}"#
        )
        .is_err());
        let mismatch = ProgramHostMessage::Close {
            generation: 2,
            sequence: 1,
        };
        assert_ne!(message_generation(&mismatch), 1);
        assert!(ProgramWorkerMessage::Started { generation: 0 }
            .validate()
            .is_err());
        let program = developer_echo_program().unwrap();
        assert_eq!(program.manifest().name, "developer-echo");
        assert!(TerminalSize {
            columns: 80,
            rows: 24
        }
        .validate()
        .is_ok());
    }
}
