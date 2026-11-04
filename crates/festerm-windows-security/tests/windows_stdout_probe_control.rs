#[path = "support/stdout_probe_control.rs"]
mod stdout_probe_control;

use std::time::{Duration, Instant};

use stdout_probe_control::{
    mode_setup, ControlError, ControlParser, FillEvidence, ModeAttempt, ModeSetup, MAX_CONTROL,
    MAX_EMPTY_LINES, MAX_WRITES,
};

fn time_window() -> (Instant, Instant) {
    let now = Instant::now();
    (now, now + Duration::from_secs(5))
}

#[test]
fn stdout_fill_evidence_distinguishes_partial_from_full_then_zero() {
    let mut partial = FillEvidence::default();
    partial.observe(16, 16).unwrap();
    partial.observe(16, 7).unwrap();
    partial.observe(16, 0).unwrap();
    assert_eq!(
        partial,
        FillEvidence {
            total: 23,
            partial: true,
            full: true,
        }
    );

    let mut no_partial = FillEvidence::default();
    no_partial.observe(16, 16).unwrap();
    no_partial.observe(16, 0).unwrap();
    assert!(!no_partial.partial);
    assert!(no_partial.full);
    assert!(no_partial.observe(16, 1).is_err());
    assert!(FillEvidence::default().observe(16, 17).is_err());
    assert!(FillEvidence::default().observe(0, 0).is_err());
}

#[test]
fn stdout_control_accepts_bounded_fill_trace() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    let frame = format!(
        "FESTERM_STDOUT_PROBE FILL_WRITE 1 {} {} {}\n",
        u32::MAX,
        u32::MAX,
        u64::MAX
    );
    for _ in 0..MAX_WRITES {
        parser.push(frame.as_bytes(), now, deadline).unwrap();
        assert_eq!(
            parser.next_event("FILL_WRITE", now, deadline),
            Ok(Some(vec![
                1,
                u64::from(u32::MAX),
                u64::from(u32::MAX),
                u64::MAX
            ]))
        );
    }
    parser
        .push(b"FESTERM_STDOUT_PROBE FULL 1\n", now, deadline)
        .unwrap();
    assert_eq!(parser.next_event("FULL", now, deadline), Ok(Some(vec![1])));
}

#[test]
fn stdout_control_classifies_initial_retry_and_double_refusal() {
    let set = ModeAttempt::from_values(&[1, 0]).unwrap();
    let refused = ModeAttempt::from_values(&[0, 231]).unwrap();
    let refused_again = ModeAttempt::from_values(&[0, 5]).unwrap();

    assert_eq!(mode_setup(set, None), Ok(ModeSetup::SetInitially));
    assert_eq!(
        mode_setup(refused, Some(set)),
        Ok(ModeSetup::SetAfterRefusal(231))
    );
    assert_eq!(
        mode_setup(refused, Some(refused_again)),
        Ok(ModeSetup::Refused(231, 5))
    );
    assert!(mode_setup(set, Some(set)).is_err());
    assert!(mode_setup(refused, None).is_err());
}

#[test]
fn stdout_control_rejects_malformed_mode_results() {
    for values in [
        &[][..],
        &[1][..],
        &[1, 5][..],
        &[2, 0][..],
        &[0, u64::from(u32::MAX) + 1][..],
        &[1, 0, 0][..],
    ] {
        assert!(ModeAttempt::from_values(values).is_err());
    }
    assert_eq!(
        ModeAttempt::from_values(&[0, 0]),
        Ok(ModeAttempt::Refused(0))
    );
}

#[test]
fn stdout_control_handles_empty_lines_and_fragmented_frames() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    parser.push(b"\n\r", now, deadline).unwrap();
    assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    parser.push(b"\nFESTERM_STDOUT_", now, deadline).unwrap();
    assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    parser.push(b"PROBE READY 41 1", now, deadline).unwrap();
    assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    parser
        .push(b" 0\r\nFESTERM_STDOUT_PROBE FULL 99\n", now, deadline)
        .unwrap();
    assert_eq!(
        parser.next_event("READY", now, deadline),
        Ok(Some(vec![41, 1, 0]))
    );
    assert_eq!(parser.next_event("FULL", now, deadline), Ok(Some(vec![99])));
    assert_eq!(parser.next_event("FULL", now, deadline), Ok(None));
}

#[test]
fn stdout_control_rejects_nonempty_malformed_frames_with_safe_diagnostics() {
    let (now, deadline) = time_window();
    let malformed: &[&[u8]] = &[
        b" \n",
        b"\t\n",
        b"\r\r\n",
        b"unexpected\n",
        b"FESTERM_STDOUT_PROBE OTHER 1\n",
        b"FESTERM_STDOUT_PROBE READY\n",
        b"FESTERM_STDOUT_PROBE READY  1\n",
        b"FESTERM_STDOUT_PROBE READY 1 \n",
        b"FESTERM_STDOUT_PROBE READY +1\n",
        b"FESTERM_STDOUT_PROBE READY \xff\n",
        b"FESTERM_STDOUT_PROBE READY 18446744073709551616\n",
        b"FESTERM_STDOUT_PROBE READY 1 2 3 4 5\n",
    ];
    for frame in malformed {
        let mut parser = ControlParser::default();
        parser.push(frame, now, deadline).unwrap();
        assert!(matches!(
            parser.next_event("READY", now, deadline),
            Err(ControlError::UnexpectedFrame { .. })
        ));
    }

    let mut parser = ControlParser::default();
    let mut frame = b"\x1b[31m".to_vec();
    frame.extend_from_slice(&[b'x'; 400]);
    frame.extend_from_slice(b"\npossible panic detail\n");
    parser.push(&frame, now, deadline).unwrap();
    let error = parser.next_event("READY", now, deadline).unwrap_err();
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("\\x1b[31m"));
    assert!(!diagnostic.contains('\x1b'));
    assert!(diagnostic.contains("..."));
    assert!(diagnostic.len() < 800);
}

#[test]
fn stdout_control_surfaces_panic_after_empty_prefix_and_unfinished_output() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    parser.push(b"\nthread 'probe' pan", now, deadline).unwrap();
    assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    let unfinished = parser.unfinished().unwrap().to_string();
    assert!(unfinished.contains("thread \\'probe\\' pan"));
    parser
        .push(
            b"icked at probe.rs:42:\nWriteFile failed with 232\n",
            now,
            deadline,
        )
        .unwrap();
    let diagnostic = parser
        .next_event("READY", now, deadline)
        .unwrap_err()
        .to_string();
    assert!(diagnostic.contains("panicked at probe.rs:42:"));
    assert!(diagnostic.contains("WriteFile failed with 232"));
    assert!(!diagnostic.contains('\n'));
}

#[test]
fn stdout_control_bounds_empty_lines_and_total_bytes_across_events() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    for _ in 0..MAX_EMPTY_LINES {
        parser.push(b"\n", now, deadline).unwrap();
        assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    }
    parser.push(b"\r\n", now, deadline).unwrap();
    assert_eq!(
        parser.next_event("READY", now, deadline),
        Err(ControlError::TooManyEmptyLines)
    );

    let mut parser = ControlParser::default();
    let frame = b"FESTERM_STDOUT_PROBE FULL 1\n";
    let events = MAX_CONTROL / frame.len();
    for _ in 0..events {
        parser.push(frame, now, deadline).unwrap();
        assert_eq!(parser.next_event("FULL", now, deadline), Ok(Some(vec![1])));
    }
    let remaining = MAX_CONTROL - events * frame.len();
    let remaining_bytes = vec![b'x'; remaining];
    parser.push(&remaining_bytes, now, deadline).unwrap();
    assert_eq!(
        parser.push(b"x", now, deadline),
        Err(ControlError::Overflow)
    );

    let mut unterminated = ControlParser::default();
    unterminated
        .push(&[b'x'; MAX_CONTROL], now, deadline)
        .unwrap();
    assert_eq!(unterminated.next_event("READY", now, deadline), Ok(None));
    assert_eq!(
        unterminated.push(b"\n", now, deadline),
        Err(ControlError::Overflow)
    );
}

#[test]
fn stdout_control_deadline_applies_to_buffered_and_continuous_input() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    parser
        .push(b"FESTERM_STDOUT_PROBE READY 1\n", now, deadline)
        .unwrap();
    assert_eq!(
        parser.next_event("READY", deadline, deadline),
        Err(ControlError::Deadline)
    );

    let mut parser = ControlParser::default();
    parser.push(b"\n", now, deadline).unwrap();
    assert_eq!(parser.next_event("READY", now, deadline), Ok(None));
    assert_eq!(
        parser.push(b"FESTERM_STDOUT_PROBE READY 1\n", deadline, deadline),
        Err(ControlError::Deadline)
    );
    assert_eq!(
        parser.next_event("READY", deadline, deadline),
        Err(ControlError::Deadline)
    );
}

const FINAL_FRAME: &[u8] = b"FESTERM_STDOUT_PROBE CLOSED_WRITE 0 0 109 1\n";

#[test]
fn stdout_control_rejects_buffered_and_late_trailing_output() {
    let (now, deadline) = time_window();
    for late in [false, true] {
        for tail in [
            &b"garbage\n"[..],
            &b"partial"[..],
            &b"\x1b"[..],
            &b"\rX"[..],
            &b"\r"[..],
            &b"FESTERM_STDOUT_PROBE CLOSED_WRITE 0 0 109 1\n"[..],
        ] {
            let mut parser = ControlParser::default();
            let mut first = FINAL_FRAME.to_vec();
            if !late {
                first.extend_from_slice(tail);
            }
            parser.push(&first, now, deadline).unwrap();
            assert_eq!(
                parser.next_event("CLOSED_WRITE", now, deadline),
                Ok(Some(vec![0, 0, 109, 1]))
            );
            if late {
                assert_eq!(parser.trailing(false, now, deadline), Ok(()));
                parser.push(tail, now, deadline).unwrap();
            }
            if tail == b"\r" {
                assert_eq!(parser.trailing(false, now, deadline), Ok(()));
            } else {
                assert!(matches!(
                    parser.trailing(false, now, deadline),
                    Err(ControlError::UnexpectedFrame { .. })
                ));
            }
            let error = parser.trailing(true, now, deadline).unwrap_err();
            assert!(matches!(&error, ControlError::UnexpectedFrame { .. }));
            let diagnostic = error.to_string();
            assert!(!diagnostic.contains('\n'));
            if tail == b"\x1b" {
                assert!(diagnostic.contains("\\x1b"));
                assert!(!diagnostic.contains('\x1b'));
            }
        }
    }
}

#[test]
fn stdout_control_requires_clean_eof_after_bounded_empty_lines() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    parser.push(b"\n", now, deadline).unwrap();
    parser.push(FINAL_FRAME, now, deadline).unwrap();
    parser.push(b"\n\r", now, deadline).unwrap();
    assert!(parser
        .next_event("CLOSED_WRITE", now, deadline)
        .unwrap()
        .is_some());
    assert_eq!(parser.trailing(false, now, deadline), Ok(()));
    assert!(parser.unfinished().is_some());
    parser.push(b"\n", now, deadline).unwrap();
    assert_eq!(parser.trailing(true, now, deadline), Ok(()));
    assert!(parser.unfinished().is_none());

    let mut flooded = ControlParser::default();
    flooded.push(b"\n", now, deadline).unwrap();
    flooded.push(FINAL_FRAME, now, deadline).unwrap();
    assert!(flooded
        .next_event("CLOSED_WRITE", now, deadline)
        .unwrap()
        .is_some());
    flooded
        .push(&[b'\n'; MAX_EMPTY_LINES], now, deadline)
        .unwrap();
    assert_eq!(
        flooded.trailing(true, now, deadline),
        Err(ControlError::TooManyEmptyLines)
    );
}

#[test]
fn stdout_control_trailing_drain_obeys_byte_limit_and_deadline() {
    let (now, deadline) = time_window();
    let mut parser = ControlParser::default();
    parser.push(FINAL_FRAME, now, deadline).unwrap();
    assert!(parser
        .next_event("CLOSED_WRITE", now, deadline)
        .unwrap()
        .is_some());
    let remaining = MAX_CONTROL - FINAL_FRAME.len();
    let bytes = vec![b'\n'; remaining];
    parser.push(&bytes, now, deadline).unwrap();
    assert_eq!(
        parser.push(b"\n", now, deadline),
        Err(ControlError::Overflow)
    );
    let diagnostic = ControlParser::rejected(&[b'\x1b'; 512]).to_string();
    assert!(diagnostic.contains("\\x1b"));
    assert!(diagnostic.contains("..."));
    assert!(!diagnostic.contains('\x1b'));
    assert!(diagnostic.len() < 1200);

    let mut parser = ControlParser::default();
    parser.push(FINAL_FRAME, now, deadline).unwrap();
    assert!(parser
        .next_event("CLOSED_WRITE", now, deadline)
        .unwrap()
        .is_some());
    parser.push(b"\r", now, deadline).unwrap();
    assert_eq!(parser.trailing(false, now, deadline), Ok(()));
    assert_eq!(
        parser.trailing(false, deadline, deadline),
        Err(ControlError::Deadline)
    );
    assert_eq!(
        parser.push(b"\n", deadline, deadline),
        Err(ControlError::Deadline)
    );
}
