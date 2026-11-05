use std::{collections::VecDeque, io, panic::AssertUnwindSafe};

use festerm_automation_mcp::{
    secure_channel::ChannelError,
    secure_handshake_io::{Error, HandshakeIo, Interest, ReadProgress, WriteProgress},
    secure_record_io::{RecordIo, WriteProgress as RecordWriteProgress},
};

fn write_step(
    handshake: &mut HandshakeIo,
    wire: &mut VecDeque<u8>,
    limit: usize,
) -> Result<WriteProgress, Error> {
    handshake.write_pending(|pending| {
        let count = pending.len().min(limit);
        wire.extend(&pending[..count]);
        Ok(count)
    })
}

fn read_step(
    handshake: &mut HandshakeIo,
    wire: &mut VecDeque<u8>,
    limit: usize,
) -> Result<ReadProgress, Error> {
    handshake.read_pending(|buffer| {
        if wire.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = buffer.len().min(limit).min(wire.len());
        for byte in &mut buffer[..count] {
            *byte = wire.pop_front().unwrap();
        }
        Ok(count)
    })
}

fn flush(handshake: &mut HandshakeIo, wire: &mut VecDeque<u8>, limit: usize) {
    loop {
        match write_step(handshake, wire, limit).unwrap() {
            WriteProgress::Partial => {}
            WriteProgress::Complete => return,
            other => panic!("unexpected write progress: {other:?}"),
        }
    }
}

fn read_frame(handshake: &mut HandshakeIo, wire: &mut VecDeque<u8>, limit: usize) -> ReadProgress {
    loop {
        match read_step(handshake, wire, limit).unwrap() {
            ReadProgress::Partial => {}
            progress @ (ReadProgress::WriteReady | ReadProgress::Complete) => return progress,
            other => panic!("unexpected read progress: {other:?}"),
        }
    }
}

fn first_frame() -> VecDeque<u8> {
    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut wire = VecDeque::new();
    flush(&mut initiator, &mut wire, usize::MAX);
    wire
}

fn assert_closed(handshake: &mut HandshakeIo) {
    assert_eq!(handshake.interest(), Interest::Closed);
    assert!(matches!(
        handshake.take_unverified(),
        Err(Error::Channel(ChannelError::Closed))
    ));
    assert!(matches!(
        handshake.read_pending(|_| panic!("closed input must not be read")),
        Err(Error::Channel(ChannelError::Closed))
    ));
    assert!(matches!(
        handshake.write_pending(|_| panic!("closed output must not be written")),
        Err(Error::Channel(ChannelError::Closed))
    ));
}

fn complete_pair(
    chunk_size: usize,
) -> (
    festerm_automation_mcp::secure_channel::UnverifiedChannel,
    festerm_automation_mcp::secure_channel::UnverifiedChannel,
) {
    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut responder = HandshakeIo::responder();
    let mut to_responder = VecDeque::new();
    let mut to_initiator = VecDeque::new();

    for _ in 0..256 {
        for handshake in [&mut initiator, &mut responder] {
            if handshake.interest() != Interest::Complete {
                assert!(matches!(handshake.take_unverified(), Err(Error::NotReady)));
            }
        }
        if initiator.interest() == Interest::Write {
            write_step(&mut initiator, &mut to_responder, chunk_size).unwrap();
        }
        if responder.interest() == Interest::Read {
            read_step(&mut responder, &mut to_responder, chunk_size).unwrap();
        }
        if responder.interest() == Interest::Write {
            write_step(&mut responder, &mut to_initiator, chunk_size).unwrap();
        }
        if initiator.interest() == Interest::Read {
            read_step(&mut initiator, &mut to_initiator, chunk_size).unwrap();
        }
        if initiator.interest() == Interest::Complete && responder.interest() == Interest::Complete
        {
            assert!(to_initiator.is_empty());
            assert!(to_responder.is_empty());
            let left = initiator.take_unverified().unwrap();
            let right = responder.take_unverified().unwrap();
            assert_closed(&mut initiator);
            assert_closed(&mut responder);
            return (left, right);
        }
    }
    panic!("fragmented handshake did not finish");
}

fn transfer(sender: &mut RecordIo, receiver: &mut RecordIo, message: &[u8]) {
    sender.prepare_send(message).unwrap();
    let mut wire = Vec::new();
    loop {
        let progress = sender
            .write_pending(|pending| {
                let count = pending.len().min(3);
                wire.extend_from_slice(&pending[..count]);
                Ok(count)
            })
            .unwrap();
        if progress == RecordWriteProgress::Complete {
            break;
        }
        assert_eq!(progress, RecordWriteProgress::Partial);
    }
    let mut received = None;
    for bytes in wire.chunks(5) {
        let (used, frame) = receiver.receive(bytes).unwrap();
        assert_eq!(used, bytes.len());
        if frame.is_some() {
            assert!(received.is_none());
            received = frame;
        }
    }
    assert_eq!(received.as_deref(), Some(message));
}

#[test]
fn fragmented_handshakes_interoperate_after_independent_comparison() {
    for chunk_size in [1, 2, 7, usize::MAX] {
        let (initiator, responder) = complete_pair(chunk_size);
        let initiator_fingerprint = initiator.fingerprint().to_string();
        let responder_fingerprint = responder.fingerprint().to_string();
        assert_eq!(initiator_fingerprint.len(), 64);
        assert_eq!(initiator_fingerprint, responder_fingerprint);

        // Simulate the independent human comparison outside the handshake.
        let mut left = RecordIo::new(initiator.confirm(&responder_fingerprint).unwrap()).unwrap();
        let mut right = RecordIo::new(responder.confirm(&initiator_fingerprint).unwrap()).unwrap();
        transfer(&mut left, &mut right, b"adapter to application");
        transfer(&mut right, &mut left, b"application to adapter");
    }
}

#[test]
fn direct_and_adapter_handshakes_are_cryptographically_separated() {
    let mut direct_client = HandshakeIo::direct_initiator().unwrap();
    let mut direct_app = HandshakeIo::direct_responder();
    let mut first = VecDeque::new();
    let mut reply = VecDeque::new();
    flush(&mut direct_client, &mut first, 3);
    assert_eq!(
        read_frame(&mut direct_app, &mut first, 5),
        ReadProgress::WriteReady
    );
    flush(&mut direct_app, &mut reply, 3);
    assert_eq!(
        read_frame(&mut direct_client, &mut reply, 5),
        ReadProgress::Complete
    );
    let client = direct_client.take_unverified().unwrap();
    let app = direct_app.take_unverified().unwrap();
    let client_fingerprint = client.fingerprint().to_string();
    let app_fingerprint = app.fingerprint().to_string();
    let mut client = RecordIo::new(client.confirm(&app_fingerprint).unwrap()).unwrap();
    let mut app = RecordIo::new(app.confirm(&client_fingerprint).unwrap()).unwrap();
    transfer(&mut client, &mut app, b"direct request");
    transfer(&mut app, &mut client, b"direct response");

    for (mut initiator, mut responder) in [
        (
            HandshakeIo::direct_initiator().unwrap(),
            HandshakeIo::responder(),
        ),
        (
            HandshakeIo::initiator().unwrap(),
            HandshakeIo::direct_responder(),
        ),
    ] {
        let mut first = VecDeque::new();
        let mut reply = VecDeque::new();
        flush(&mut initiator, &mut first, usize::MAX);
        assert_eq!(
            read_frame(&mut responder, &mut first, usize::MAX),
            ReadProgress::WriteReady
        );
        flush(&mut responder, &mut reply, usize::MAX);
        assert_eq!(
            read_step(&mut initiator, &mut reply, usize::MAX).unwrap(),
            ReadProgress::Partial
        );
        assert!(matches!(
            read_step(&mut initiator, &mut reply, usize::MAX),
            Err(Error::Channel(ChannelError::InvalidMessage))
        ));
        assert_closed(&mut initiator);
    }
}

#[test]
fn responder_handoff_waits_for_every_reply_byte_to_be_written() {
    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut responder = HandshakeIo::responder();
    let mut first = VecDeque::new();
    flush(&mut initiator, &mut first, usize::MAX);
    assert_eq!(
        read_frame(&mut responder, &mut first, 3),
        ReadProgress::WriteReady
    );
    assert_eq!(responder.interest(), Interest::Write);
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));

    let mut expected = Vec::new();
    assert_eq!(
        responder
            .write_pending(|pending| {
                expected.extend_from_slice(pending);
                Err(io::ErrorKind::WouldBlock.into())
            })
            .unwrap(),
        WriteProgress::Blocked
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));
    assert_eq!(
        responder
            .write_pending(|pending| {
                assert_eq!(pending, expected);
                Err(io::ErrorKind::Interrupted.into())
            })
            .unwrap(),
        WriteProgress::Retry
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));

    let mut reply = VecDeque::new();
    assert_eq!(
        responder
            .write_pending(|pending| {
                assert_eq!(pending, expected);
                let count = pending.len() - 1;
                reply.extend(&pending[..count]);
                Ok(count)
            })
            .unwrap(),
        WriteProgress::Partial
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));
    assert_eq!(reply.len() + 1, expected.len());
    assert_eq!(
        responder
            .write_pending(|pending| {
                assert_eq!(pending, &expected[expected.len() - 1..]);
                Err(io::ErrorKind::WouldBlock.into())
            })
            .unwrap(),
        WriteProgress::Blocked
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));
    assert_eq!(
        responder
            .write_pending(|pending| {
                assert_eq!(pending, &expected[expected.len() - 1..]);
                Err(io::ErrorKind::Interrupted.into())
            })
            .unwrap(),
        WriteProgress::Retry
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));

    assert_eq!(
        responder
            .write_pending(|pending| {
                assert_eq!(pending, &expected[expected.len() - 1..]);
                reply.extend(pending);
                Ok(pending.len())
            })
            .unwrap(),
        WriteProgress::Complete
    );
    assert_eq!(responder.interest(), Interest::Complete);
    let responder = responder.take_unverified().unwrap();
    assert_eq!(
        read_frame(&mut initiator, &mut reply, 5),
        ReadProgress::Complete
    );
    let initiator = initiator.take_unverified().unwrap();
    assert_eq!(
        initiator.fingerprint().to_string(),
        responder.fingerprint().to_string()
    );
}

#[test]
fn initiator_does_not_read_or_handoff_before_its_first_write_completes() {
    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut first = VecDeque::new();
    assert_eq!(initiator.interest(), Interest::Write);
    assert_eq!(
        initiator
            .read_pending(|_| panic!("the first frame is still unsent"))
            .unwrap(),
        ReadProgress::Idle
    );
    assert_eq!(
        write_step(&mut initiator, &mut first, 1).unwrap(),
        WriteProgress::Partial
    );
    assert!(matches!(initiator.take_unverified(), Err(Error::NotReady)));
    assert_eq!(
        initiator
            .read_pending(|_| panic!("the first frame is still partial"))
            .unwrap(),
        ReadProgress::Idle
    );
    flush(&mut initiator, &mut first, usize::MAX);
    assert_eq!(first.len(), 34);
    assert_eq!(initiator.interest(), Interest::Read);
    assert!(matches!(initiator.take_unverified(), Err(Error::NotReady)));
}

#[test]
fn read_readiness_distinguishes_interruption_progress_and_backpressure() {
    let mut responder = HandshakeIo::responder();
    assert_eq!(
        responder
            .write_pending(|_| panic!("no output is ready"))
            .unwrap(),
        WriteProgress::Idle
    );
    assert_eq!(
        responder
            .read_pending(|_| Err(io::ErrorKind::WouldBlock.into()))
            .unwrap(),
        ReadProgress::Blocked
    );
    assert_eq!(
        responder
            .read_pending(|_| Err(io::ErrorKind::Interrupted.into()))
            .unwrap(),
        ReadProgress::Retry
    );
    let mut wire = first_frame();
    assert_eq!(
        read_step(&mut responder, &mut wire, 1).unwrap(),
        ReadProgress::Partial
    );
    assert_eq!(
        responder
            .read_pending(|_| Err(io::ErrorKind::WouldBlock.into()))
            .unwrap(),
        ReadProgress::Blocked
    );
    assert_eq!(
        responder
            .read_pending(|_| Err(io::ErrorKind::Interrupted.into()))
            .unwrap(),
        ReadProgress::Retry
    );
    assert_eq!(
        read_step(&mut responder, &mut wire, 1).unwrap(),
        ReadProgress::Partial
    );
    assert_eq!(
        read_frame(&mut responder, &mut wire, usize::MAX),
        ReadProgress::WriteReady
    );
    assert_eq!(responder.interest(), Interest::Write);
    assert_eq!(
        responder
            .read_pending(|_| panic!("do not consume later stream bytes"))
            .unwrap(),
        ReadProgress::Idle
    );
}

#[test]
fn wrong_lengths_and_malformed_noise_messages_fail_closed() {
    for length in [0u16, 1, 31, 33, 48, u16::MAX] {
        let mut responder = HandshakeIo::responder();
        let mut wire = VecDeque::from(length.to_be_bytes());
        wire.extend([42; 4]);
        assert!(matches!(
            read_step(&mut responder, &mut wire, usize::MAX),
            Err(Error::InvalidFrame)
        ));
        assert_eq!(wire, VecDeque::from([42; 4]));
        assert_closed(&mut responder);
    }
    for length in [0u16, 32, 47, 49, u16::MAX] {
        let mut initiator = HandshakeIo::initiator().unwrap();
        flush(&mut initiator, &mut VecDeque::new(), usize::MAX);
        let mut wire = VecDeque::from(length.to_be_bytes());
        wire.extend([42; 4]);
        assert!(matches!(
            read_step(&mut initiator, &mut wire, usize::MAX),
            Err(Error::InvalidFrame)
        ));
        assert_eq!(wire, VecDeque::from([42; 4]));
        assert_closed(&mut initiator);
    }
    for first_byte in [0, 1] {
        let mut responder = HandshakeIo::responder();
        let mut wire = VecDeque::from(vec![0, 32]);
        wire.push_back(first_byte);
        wire.extend([0; 31]);
        assert_eq!(
            read_step(&mut responder, &mut wire, usize::MAX).unwrap(),
            ReadProgress::Partial
        );
        assert!(matches!(
            read_step(&mut responder, &mut wire, usize::MAX),
            Err(Error::Channel(ChannelError::InvalidMessage))
        ));
        assert_closed(&mut responder);
    }

    let mut initiator = HandshakeIo::initiator().unwrap();
    flush(&mut initiator, &mut VecDeque::new(), usize::MAX);
    let mut wire = VecDeque::from(vec![0, 48]);
    wire.extend([0; 48]);
    assert_eq!(
        read_step(&mut initiator, &mut wire, usize::MAX).unwrap(),
        ReadProgress::Partial
    );
    assert!(matches!(
        read_step(&mut initiator, &mut wire, usize::MAX),
        Err(Error::Channel(ChannelError::InvalidMessage))
    ));
    assert_closed(&mut initiator);

    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut responder = HandshakeIo::responder();
    let mut first = VecDeque::new();
    flush(&mut initiator, &mut first, usize::MAX);
    read_frame(&mut responder, &mut first, usize::MAX);
    let mut reply = VecDeque::new();
    flush(&mut responder, &mut reply, usize::MAX);
    *reply.back_mut().unwrap() ^= 1;
    assert_eq!(
        read_step(&mut initiator, &mut reply, usize::MAX).unwrap(),
        ReadProgress::Partial
    );
    assert!(matches!(
        read_step(&mut initiator, &mut reply, usize::MAX),
        Err(Error::Channel(ChannelError::InvalidMessage))
    ));
    assert_closed(&mut initiator);
}

#[test]
fn eof_and_cancellation_discard_pending_and_completed_handshakes() {
    let valid: Vec<u8> = first_frame().into_iter().collect();
    for cut in [0, 1, 2, 3, valid.len() - 1] {
        let mut responder = HandshakeIo::responder();
        let mut wire = VecDeque::from(valid[..cut].to_vec());
        while !wire.is_empty() {
            assert_eq!(
                read_step(&mut responder, &mut wire, usize::MAX).unwrap(),
                ReadProgress::Partial
            );
        }
        assert!(matches!(responder.end_input(), Err(Error::Truncated)));
        assert_closed(&mut responder);
    }

    let mut reply = vec![0, 48];
    reply.extend([42; 48]);
    for cut in [0, 1, 2, 3, reply.len() - 1] {
        let mut initiator = HandshakeIo::initiator().unwrap();
        flush(&mut initiator, &mut VecDeque::new(), usize::MAX);
        let mut wire = VecDeque::from(reply[..cut].to_vec());
        while !wire.is_empty() {
            assert_eq!(
                read_step(&mut initiator, &mut wire, usize::MAX).unwrap(),
                ReadProgress::Partial
            );
        }
        assert!(matches!(initiator.end_input(), Err(Error::Truncated)));
        assert_closed(&mut initiator);
    }

    let mut responder = HandshakeIo::responder();
    assert!(matches!(
        responder.read_pending(|_| Ok(0)),
        Err(Error::Truncated)
    ));
    assert_closed(&mut responder);

    let mut responder = HandshakeIo::responder();
    let mut first = first_frame();
    assert_eq!(
        read_frame(&mut responder, &mut first, usize::MAX),
        ReadProgress::WriteReady
    );
    assert!(matches!(responder.end_input(), Err(Error::Truncated)));
    assert_closed(&mut responder);

    let mut responder = HandshakeIo::responder();
    let mut first = first_frame();
    read_frame(&mut responder, &mut first, usize::MAX);
    flush(&mut responder, &mut VecDeque::new(), usize::MAX);
    assert!(matches!(responder.end_input(), Err(Error::PeerClosed)));
    assert_closed(&mut responder);

    let mut initiator = HandshakeIo::initiator().unwrap();
    assert_eq!(
        initiator
            .write_pending(|_| Err(io::ErrorKind::WouldBlock.into()))
            .unwrap(),
        WriteProgress::Blocked
    );
    initiator.close();
    initiator.close();
    assert_closed(&mut initiator);

    let mut responder = HandshakeIo::responder();
    let mut first = first_frame();
    read_frame(&mut responder, &mut first, usize::MAX);
    let mut reply = VecDeque::new();
    assert_eq!(
        write_step(&mut responder, &mut reply, 1).unwrap(),
        WriteProgress::Partial
    );
    responder.close();
    assert_closed(&mut responder);
}

#[test]
fn io_errors_and_impossible_counts_are_terminal() {
    for kind in [
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::PermissionDenied,
    ] {
        let mut responder = HandshakeIo::responder();
        assert!(matches!(
            responder.read_pending(|_| Err(kind.into())),
            Err(Error::Io(error)) if error.kind() == kind
        ));
        assert_closed(&mut responder);
    }

    let mut responder = HandshakeIo::responder();
    assert!(matches!(
        responder.read_pending(|buffer| Ok(buffer.len() + 1)),
        Err(Error::InvalidFrame)
    ));
    assert_closed(&mut responder);

    let mut initiator = HandshakeIo::initiator().unwrap();
    assert_eq!(
        initiator.write_pending(|_| Ok(1)).unwrap(),
        WriteProgress::Partial
    );
    assert!(matches!(
        initiator.write_pending(|_| Err(io::ErrorKind::BrokenPipe.into())),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
    ));
    assert_closed(&mut initiator);

    let mut responder = HandshakeIo::responder();
    let mut first = first_frame();
    read_frame(&mut responder, &mut first, usize::MAX);
    let mut partial_reply = VecDeque::new();
    assert_eq!(
        write_step(&mut responder, &mut partial_reply, 1).unwrap(),
        WriteProgress::Partial
    );
    assert!(matches!(responder.take_unverified(), Err(Error::NotReady)));
    assert!(matches!(
        responder.write_pending(|_| Err(io::ErrorKind::BrokenPipe.into())),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe
    ));
    assert_closed(&mut responder);

    let mut initiator = HandshakeIo::initiator().unwrap();
    assert!(matches!(
        initiator.write_pending(|_| Ok(0)),
        Err(Error::Io(error)) if error.kind() == io::ErrorKind::WriteZero
    ));
    assert_closed(&mut initiator);

    let mut initiator = HandshakeIo::initiator().unwrap();
    assert!(matches!(
        initiator.write_pending(|pending| Ok(pending.len() + 1)),
        Err(Error::InvalidFrame)
    ));
    assert_closed(&mut initiator);
}

#[test]
fn one_handshake_frame_never_consumes_the_following_stream() {
    let mut responder = HandshakeIo::responder();
    let mut wire = first_frame();
    wire.extend([0x99, 0x88]);
    assert_eq!(
        read_frame(&mut responder, &mut wire, usize::MAX),
        ReadProgress::WriteReady
    );
    assert_eq!(wire, VecDeque::from([0x99, 0x88]));
    assert_eq!(
        responder
            .read_pending(|_| panic!("later bytes belong to the caller"))
            .unwrap(),
        ReadProgress::Idle
    );

    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut responder = HandshakeIo::responder();
    let mut first = VecDeque::new();
    flush(&mut initiator, &mut first, usize::MAX);
    read_frame(&mut responder, &mut first, usize::MAX);
    let mut reply = VecDeque::new();
    flush(&mut responder, &mut reply, usize::MAX);
    reply.extend([0x77, 0x66]);
    assert_eq!(
        read_frame(&mut initiator, &mut reply, usize::MAX),
        ReadProgress::Complete
    );
    assert_eq!(reply, VecDeque::from([0x77, 0x66]));
}

#[test]
fn failed_or_incomplete_fingerprint_comparison_never_confirms() {
    let (initiator, responder) = complete_pair(3);
    let full = responder.fingerprint().to_string();
    assert!(matches!(
        initiator.confirm(&full[..full.len() - 1]),
        Err(ChannelError::InvalidMessage)
    ));
    let (initiator, responder) = complete_pair(5);
    let mut wrong = responder.fingerprint().to_string().into_bytes();
    wrong[0] = if wrong[0] == b'0' { b'1' } else { b'0' };
    assert!(matches!(
        initiator.confirm(std::str::from_utf8(&wrong).unwrap()),
        Err(ChannelError::InvalidMessage)
    ));

    let (initiator, _) = complete_pair(2);
    let (_, unrelated_responder) = complete_pair(7);
    let local = initiator.fingerprint().to_string();
    let unrelated = unrelated_responder.fingerprint().to_string();
    assert_ne!(local, unrelated);
    assert!(matches!(
        initiator.confirm(&unrelated),
        Err(ChannelError::InvalidMessage)
    ));
}

#[test]
fn panicking_io_callbacks_cannot_leave_a_reusable_handshake() {
    let mut initiator = HandshakeIo::initiator().unwrap();
    let mut wire = Vec::new();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = initiator.write_pending(|pending| {
            wire.push(pending[0]);
            panic!("write outcome is unknown")
        });
    }))
    .is_err());
    assert_eq!(wire.len(), 1);
    assert_closed(&mut initiator);

    let mut responder = HandshakeIo::responder();
    assert!(std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = responder.read_pending(|buffer| {
            buffer[0] = 0;
            panic!("read outcome is unknown")
        });
    }))
    .is_err());
    assert_closed(&mut responder);

    let mut responder = HandshakeIo::responder();
    let mut first = first_frame();
    read_frame(&mut responder, &mut first, usize::MAX);
    assert!(std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = responder.write_pending(|_| panic!("reply outcome is unknown"));
    }))
    .is_err());
    assert_closed(&mut responder);
}
