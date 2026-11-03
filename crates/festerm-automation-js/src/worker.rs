use crate::{
    engine::{self, Input, Transport},
    platform,
    protocol::{encode_frame, Decoder, HostRequest, ParentMessage, WorkerMessage, CHUNK_BYTES},
    snapshot::{ApprovedBundle, SourceSnapshot, MAX_SNAPSHOT_BYTES},
    Error,
};
use std::{
    io::{Read, Write},
    time::{Duration, Instant},
};

/// Call only from the install-owned executable's early `--automation-worker`
/// branch, on its fresh main thread, before GUI/threads/credentials initialize.
/// The supplied streams MUST be inherited fd 0 and fd 1, respectively.
/// This function establishes native confinement before reading source or
/// constructing an interpreter. Unsupported platforms return Unavailable.
pub fn worker_entry(input: impl Read, output: impl Write) -> Result<(), Error> {
    platform::confine()?;
    let mut pipe = Pipe {
        input,
        output,
        decoder: Decoder::default(),
    };
    pipe.write(&WorkerMessage::Ready {
        profile: platform::LINUX_PROFILE.into(),
    })?;
    let load_deadline = Instant::now() + Duration::from_secs(2);
    let begin = pipe.wait(load_deadline)?;
    let ParentMessage::Begin {
        digest,
        snapshot_bytes,
        parameters,
        limits,
    } = begin
    else {
        return Err(Error::Protocol);
    };
    limits.validate()?;
    let size = snapshot_bytes as usize;
    if size == 0 || size > MAX_SNAPSHOT_BYTES {
        return Err(Error::Protocol);
    }
    let mut source = Vec::with_capacity(size);
    while source.len() < size {
        match pipe.wait(load_deadline)? {
            ParentMessage::SourceChunk { bytes }
                if !bytes.is_empty()
                    && bytes.len() <= CHUNK_BYTES
                    && source.len() + bytes.len() <= size =>
            {
                source.extend_from_slice(&bytes);
            }
            ParentMessage::Cancel {} => return pipe.finish(crate::protocol::Outcome::Cancelled {}),
            _ => return Err(Error::Protocol),
        }
    }
    if !matches!(pipe.wait(load_deadline)?, ParentMessage::Run {}) {
        return Err(Error::Protocol);
    }
    let snapshot: SourceSnapshot = serde_json::from_slice(&source).map_err(|_| Error::Snapshot)?;
    let bundle = ApprovedBundle::new(snapshot)?;
    if bundle.digest() != digest {
        return Err(Error::Snapshot);
    }
    bundle.validate_parameters(&parameters)?;
    drop(source);
    let outcome = engine::run(&bundle, &parameters, &limits, &mut pipe);
    pipe.finish(outcome)
}

struct Pipe<R, W> {
    input: R,
    output: W,
    decoder: Decoder,
}

impl<R: Read, W: Write> Pipe<R, W> {
    fn wait(&mut self, deadline: Instant) -> Result<ParentMessage, Error> {
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            if let Some(message) = self.decoder.read(&mut self.input)? {
                return Ok(message);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn finish(&mut self, outcome: crate::protocol::Outcome) -> Result<(), Error> {
        self.write(&WorkerMessage::Finished { outcome })
    }

    fn write(&mut self, message: &WorkerMessage) -> Result<(), Error> {
        let frame = encode_frame(message)?;
        let mut sent = 0;
        let deadline = Instant::now() + Duration::from_millis(250);
        while sent < frame.len() {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            match self.output.write(&frame[sent..]) {
                Ok(0) => return Err(Error::Protocol),
                Ok(n) => sent += n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            match self.output.flush() {
                Ok(()) => return Ok(()),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl<R: Read, W: Write> Transport for Pipe<R, W> {
    fn send(&mut self, id: u32, request: HostRequest) -> Result<(), Error> {
        self.write(&WorkerMessage::HostCall { id, request })
    }

    fn receive(&mut self) -> Result<Input, Error> {
        match self.decoder.read(&mut self.input)? {
            Some(ParentMessage::HostResult { id, result }) => Ok(Input::Result(id, result)),
            Some(ParentMessage::Cancel {}) => Ok(Input::Cancel),
            Some(_) => Err(Error::Protocol),
            None => Ok(Input::Idle),
        }
    }
}
