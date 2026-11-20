use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use festerm_core::{
    kitty::{KittyConfig, KittyFormat, KittyGeometry, KittyPngValidator, KittySourceRect},
    Dimensions, Terminal, MAX_RECORDING_CHECKPOINT_BYTES, TRANSPORT_QUEUE_HIGH_WATERMARK,
};

const DA1: &str = "\x1b[?6c";
const RGB: &str = "f=24,s=1,v=1;AQID";
const ZLIB_RGB: &str = "eJxjZGIGAAANAAc=";
const ZLIB_RGBA: &str = "eJxjZGJmAQAAGAAL";
const VALID_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

fn terminal(columns: usize, rows: usize) -> Terminal {
    Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
}

fn enabled(columns: usize, rows: usize, config: KittyConfig) -> Terminal {
    let mut terminal = terminal(columns, rows);
    terminal.enable_kitty_graphics(config).unwrap();
    terminal
        .set_kitty_geometry(KittyGeometry::new(1, 1))
        .unwrap();
    terminal
}

fn apc(terminal: &mut Terminal, command: &str) {
    terminal.ingest(format!("\x1b_G{command}\x1b\\").as_bytes());
}

fn replies(terminal: &mut Terminal) -> String {
    String::from_utf8(terminal.drain_replies()).unwrap()
}

fn upload(terminal: &mut Terminal, controls: &str) {
    apc(terminal, &format!("{controls},{RGB}"));
}

#[test]
fn opt_in_requires_geometry_and_refuses_recording_and_recovery() {
    let mut terminal = terminal(12, 4);
    upload(&mut terminal, "a=q,i=9");
    terminal.ingest(b"\x1b[c");
    assert_eq!(replies(&mut terminal), DA1);
    assert!(terminal.kitty_images().is_empty());

    terminal
        .enable_kitty_graphics(KittyConfig::default())
        .unwrap();
    upload(&mut terminal, "a=T,i=9");
    terminal.ingest(b"\x1b[14t\x1b[16t\x1b[c");
    assert_eq!(replies(&mut terminal), DA1);
    assert!(terminal.kitty_images().is_empty());

    terminal
        .set_kitty_geometry(KittyGeometry::new(6, 9))
        .unwrap();
    upload(&mut terminal, "a=q,i=9");
    terminal.ingest(b"\x1b[c\x1b[14t\x1b[16t");
    assert_eq!(
        replies(&mut terminal),
        format!("\x1b_Gi=9;OK\x1b\\{DA1}\x1b[4;36;72t\x1b[6;9;6t")
    );
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal
        .recording_checkpoint(MAX_RECORDING_CHECKPOINT_BYTES)
        .is_err());
    assert!(terminal
        .recovery_clone()
        .validate_recovery_snapshot()
        .is_err());
    assert!(serde_json::to_value(terminal.recovery_clone()).is_err());
}

#[test]
fn chunked_upload_is_atomic_and_uses_latest_quiet_without_query_interference() {
    let mut terminal = enabled(9, 4, KittyConfig::default());
    apc(&mut terminal, "a=t,i=4,p=8,f=24,s=2,v=1,m=1,q=2;AQID");
    assert!(terminal.kitty_images().is_empty());
    assert!(replies(&mut terminal).is_empty());
    upload(&mut terminal, "a=q,i=99");
    assert_eq!(replies(&mut terminal), "\x1b_Gi=99;OK\x1b\\");
    terminal.ingest(b"\x1b_Gq=0,m=0;BAUG\x1b");
    assert!(terminal.kitty_images().is_empty());
    terminal.ingest(b"\\");
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3, 4, 5, 6]);
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Rgb);
    assert_eq!(replies(&mut terminal), "\x1b_Gi=4,p=8;OK\x1b\\");

    apc(&mut terminal, "i=5,f=24,s=2,v=1,m=1;AQI");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=5;EINVAL:invalid graphics command\x1b\\"
    );
    apc(&mut terminal, "i=5,f=24,s=2,v=1,m=1;AQID");
    apc(&mut terminal, "a=d,d=a");
    apc(&mut terminal, "m=0;BAUG");
    assert_eq!(terminal.kitty_images().len(), 1);
    assert!(replies(&mut terminal).is_empty());
}

#[test]
fn image_numbers_resolve_newest_and_placement_ids_replace_selectively() {
    let mut terminal = enabled(10, 4, KittyConfig::default());
    upload(&mut terminal, "a=q,I=73");
    assert_eq!(replies(&mut terminal), "\x1b_GI=73;OK\x1b\\");
    upload(&mut terminal, "a=t,I=73");
    upload(&mut terminal, "a=t,I=73");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=1,I=73;OK\x1b\\\x1b_Gi=2,I=73;OK\x1b\\"
    );
    apc(&mut terminal, "a=p,I=73,p=15,C=1,z=1");
    assert_eq!(replies(&mut terminal), "\x1b_Gi=2,I=73,p=15;OK\x1b\\");
    terminal.ingest(b"\x1b[2;3H");
    apc(&mut terminal, "a=p,I=73,p=15,C=1,z=4,q=1");
    apc(&mut terminal, "a=p,I=73,p=23,C=1,z=-4,q=1");
    assert!(replies(&mut terminal).is_empty());
    assert_eq!(terminal.kitty_placements().len(), 2);
    assert_eq!(terminal.kitty_placements()[0].placement_id, Some(23));
    assert_eq!(terminal.kitty_placements()[1].anchor.column(), 2);
    assert_eq!(terminal.kitty_placements()[1].z, 4);

    apc(&mut terminal, "a=d,d=N,I=73,p=23");
    assert_eq!(terminal.kitty_images().len(), 2);
    assert_eq!(terminal.kitty_placements()[0].placement_id, Some(15));
    apc(&mut terminal, "a=d,d=N,I=73,p=15");
    assert_eq!(terminal.kitty_images().len(), 1);
    assert!(terminal.kitty_placements().is_empty());
    apc(&mut terminal, "a=p,I=73,p=30,C=1");
    assert_eq!(terminal.kitty_placements()[0].image_id, 1);
    assert!(replies(&mut terminal).ends_with("\x1b_Gi=1,I=73,p=30;OK\x1b\\"));

    upload(&mut terminal, "a=t,i=1");
    assert!(terminal.kitty_placements().is_empty());
    assert!(terminal.kitty_images()[0].serial > 2);
    replies(&mut terminal);
    apc(&mut terminal, "a=p,i=999,p=31,q=1");
    apc(&mut terminal, "a=p,i=999,p=32,q=2");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=999,p=31;ENOENT:image not found\x1b\\"
    );
}

#[test]
fn quota_eviction_is_ordered_and_failed_commands_leave_state_unchanged() {
    let mut terminal = enabled(
        8,
        4,
        KittyConfig {
            rgba_budget: 12,
            png_validator: None,
        },
    );
    upload(&mut terminal, "a=T,i=1,C=1");
    upload(&mut terminal, "a=t,i=2");
    upload(&mut terminal, "a=t,i=3,N=1");
    let cursor = terminal.cursor();
    upload(&mut terminal, "a=T,I=44,p=17,X=3");
    assert_eq!(terminal.cursor(), cursor);
    assert_eq!(terminal.kitty_images().len(), 3);
    assert_eq!(terminal.kitty_placements().len(), 1);
    assert!(
        replies(&mut terminal).ends_with("\x1b_GI=44,p=17;EINVAL:invalid graphics command\x1b\\")
    );

    upload(&mut terminal, "a=t,I=44");
    assert!(terminal.kitty_images().iter().all(|image| image.id != 3));
    assert_eq!(replies(&mut terminal), "\x1b_Gi=4,I=44;OK\x1b\\");
    apc(&mut terminal, "a=p,I=44,C=1,q=1");
    upload(&mut terminal, "a=t,i=5");
    assert!(terminal.kitty_images().iter().all(|image| image.id != 2));
    apc(&mut terminal, "a=p,i=5,C=1,q=1");
    upload(&mut terminal, "a=t,i=6");
    assert!(terminal.kitty_images().iter().all(|image| image.id != 1));
    assert!(terminal
        .kitty_placements()
        .iter()
        .all(|placement| placement.image_id != 1));
}

#[test]
fn placement_capacity_evicts_only_images_that_free_placements() {
    let mut terminal = enabled(8, 4, KittyConfig::default());
    upload(&mut terminal, "a=t,i=1,q=1");
    upload(&mut terminal, "a=t,i=2,q=1");
    upload(&mut terminal, "a=t,i=3,N=1,q=1");
    for p in 1..=256 {
        let id = if p <= 128 { 1 } else { 2 };
        apc(&mut terminal, &format!("a=p,i={id},p={p},C=1,q=1"));
    }

    apc(&mut terminal, "a=p,i=2,p=256,C=1,q=1,z=1");
    assert_eq!(terminal.kitty_images().len(), 3);
    assert_eq!(terminal.kitty_placements().len(), 256);
    assert_eq!(terminal.kitty_placements().last().unwrap().z, 1);

    let mut scrolled = terminal.clone();
    scrolled.ingest(b"\x1b[4;8H");
    apc(&mut scrolled, "a=p,i=2,p=257,q=1");
    assert_eq!(scrolled.kitty_images().len(), 3);
    assert_eq!(scrolled.kitty_placements().len(), 1);
    assert_eq!(scrolled.kitty_placements()[0].placement_id, Some(257));

    apc(&mut terminal, "a=p,i=2,p=257,C=1,q=1");
    assert_eq!(terminal.kitty_placements().len(), 129);
    assert_eq!(
        terminal
            .kitty_images()
            .iter()
            .map(|img| img.id)
            .collect::<Vec<_>>(),
        [2, 3]
    );

    for p in 1..=128 {
        apc(&mut terminal, &format!("a=p,i=2,p={p},C=1,q=1"));
    }
    let placements = terminal.kitty_placements();
    assert_eq!(placements.len(), 256);
    assert!(!placements
        .iter()
        .any(|placement| placement.placement_id == Some(129)));
    assert!(placements
        .iter()
        .any(|placement| placement.placement_id == Some(128)));
    assert_eq!(terminal.kitty_images().len(), 2);

    upload(&mut terminal, "a=T,i=4,C=1,q=1");
    assert_eq!(terminal.kitty_placements().len(), 1);
    assert_eq!(terminal.kitty_placements()[0].image_id, 4);
    assert_eq!(
        terminal
            .kitty_images()
            .iter()
            .map(|img| img.id)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    assert!(replies(&mut terminal).is_empty());
}

#[test]
fn addressed_mutation_requires_capacity_for_the_whole_reply() {
    let mut terminal = enabled(
        5,
        3,
        KittyConfig {
            rgba_budget: 4,
            png_validator: None,
        },
    );
    upload(&mut terminal, "a=T,i=1,C=1,q=1,p=9");
    terminal.ingest(b"three\x1b[3;5H");
    let cursor = terminal.cursor();
    let image = terminal.kitty_images()[0].clone();
    let placement = terminal.kitty_placements()[0].clone();

    let reply = "\x1b_Gi=2,I=70;OK\x1b\\";
    let prefix = vec![b'x'; TRANSPORT_QUEUE_HIGH_WATERMARK - reply.len() + 1];
    terminal.queue_reply(&prefix);
    upload(&mut terminal, "a=T,I=70");
    assert!(terminal.take_reply_queue_overflowed());
    assert_eq!(terminal.queued_replies(), prefix);
    terminal.queue_reply(&vec![b'x'; reply.len() - 1]);
    for command in ["a=p,i=1,p=9", "a=d,d=I,i=1,p=9"] {
        apc(&mut terminal, command);
        assert!(terminal.take_reply_queue_overflowed());
        assert_eq!(
            terminal.queued_replies().len(),
            TRANSPORT_QUEUE_HIGH_WATERMARK
        );
        assert!(terminal.queued_replies().iter().all(|&byte| byte == b'x'));
        assert_eq!(terminal.kitty_images(), std::slice::from_ref(&image));
        assert_eq!(
            terminal.kitty_placements(),
            std::slice::from_ref(&placement)
        );
        assert_eq!(terminal.cursor(), cursor);
        assert_eq!(terminal.scrollback_lines().len(), 0);
    }
    apc(&mut terminal, "a=p,i=1,p=9,q=1");
    assert!(!terminal.take_reply_queue_overflowed());
    assert_eq!(terminal.scrollback_lines().len(), 1);
    terminal.drain_replies();

    terminal.queue_reply(&prefix[..prefix.len() - 1]);
    upload(&mut terminal, "a=T,I=70");
    assert_eq!(
        terminal.queued_replies().len(),
        TRANSPORT_QUEUE_HIGH_WATERMARK
    );
    assert!(!terminal.take_reply_queue_overflowed());
    assert!(terminal.queued_replies().ends_with(reply.as_bytes()));
    assert_eq!(terminal.kitty_images()[0].id, 2);
    assert_eq!(terminal.kitty_images()[0].serial, image.serial + 1);
    assert_eq!(terminal.kitty_placements()[0].image_id, 2);
}

#[test]
fn reply_backpressure_resumes_before_graphics_commit_and_following_output() {
    let mut terminal = enabled(9, 4, KittyConfig::default());
    let first = format!("before\x1b_Ga=T,I=44,p=7,o=z,S=3,f=24,s=1,v=1;{ZLIB_RGB}\x1b");
    assert_eq!(
        terminal.ingest_with_reply_backpressure(first.as_bytes()),
        first.len()
    );
    let cursor = terminal.cursor();
    let full = vec![b'x'; TRANSPORT_QUEUE_HIGH_WATERMARK];
    terminal.queue_reply(&full);

    let rest = b"\\after\x1b[c";
    assert_eq!(terminal.ingest_with_reply_backpressure(rest), 0);
    assert_eq!(terminal.queued_replies(), full);
    assert!(!terminal.take_reply_queue_overflowed());
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(terminal.cursor(), cursor);
    assert_eq!(terminal.row_text(0).unwrap().trim(), "before");

    assert_eq!(terminal.drain_replies(), full);
    assert_eq!(terminal.ingest_with_reply_backpressure(rest), rest.len());
    assert_eq!(
        replies(&mut terminal),
        format!("\x1b_Gi=1,I=44,p=7;OK\x1b\\{DA1}")
    );
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3]);
    assert_eq!(terminal.kitty_placements()[0].anchor, cursor);
    assert_eq!(terminal.cell(7, 0).unwrap().character(), 'a');

    let query = format!("\x1b_Ga=q,i=99,{RGB}\x1b\\");
    let answer = "\x1b_Gi=99;OK\x1b\\";
    let burst = query.repeat(100);
    let filled = vec![b'x'; TRANSPORT_QUEUE_HIGH_WATERMARK - 1024];
    terminal.queue_reply(&filled);
    let consumed = terminal.ingest_with_reply_backpressure(burst.as_bytes());
    assert!(consumed > 0 && consumed < burst.len());
    assert_eq!(consumed % query.len(), 0);
    let queued = terminal.drain_replies();
    assert_eq!(&queued[..filled.len()], filled.as_slice());
    assert_eq!(
        &queued[filled.len()..],
        answer.repeat(consumed / query.len()).as_bytes()
    );
    let rest = &burst.as_bytes()[consumed..];
    assert_eq!(terminal.ingest_with_reply_backpressure(rest), rest.len());
    assert_eq!(
        replies(&mut terminal),
        answer.repeat(rest.len() / query.len())
    );
    assert!(!terminal.take_reply_queue_overflowed());
}

#[test]
fn worker_validates_png_before_owner_commits_cursor_and_ordered_replies() {
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = std::thread::current().id();
    let validator = KittyPngValidator::new({
        let calls = Arc::clone(&calls);
        move |data, width, height| {
            assert_ne!(std::thread::current().id(), owner);
            assert_eq!((data.len(), width, height), (70, 1, 1));
            calls.fetch_add(1, Ordering::Relaxed);
            true
        }
    });
    let mut terminal = enabled(
        9,
        4,
        KittyConfig {
            rgba_budget: 4,
            png_validator: Some(validator),
        },
    );
    terminal.enable_kitty_worker_handoff();

    let prefix = format!("A\x1b[c\x1b_Ga=T,I=44,p=17,f=100;{VALID_PNG}\x1b\\");
    let suffix = b"B\x1b[c";
    let mut input = prefix.as_bytes().to_vec();
    input.extend_from_slice(suffix);
    assert_eq!(
        terminal.ingest_with_reply_backpressure(&input),
        prefix.len()
    );
    assert!(terminal.kitty_validation_pending());
    assert_eq!(terminal.queued_replies(), DA1.as_bytes());
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    let cursor = terminal.cursor();
    assert_eq!(cursor.column(), 1);
    assert_eq!(terminal.ingest_with_reply_backpressure(suffix), 0);
    assert!(terminal.resize(Dimensions::new(8, 4).unwrap()).is_err());
    assert!(terminal
        .set_kitty_geometry(KittyGeometry::new(3, 5))
        .is_err());
    assert!(terminal.queue_reply(b"later").overflowed());

    let job = terminal.take_kitty_validation().unwrap();
    assert!(terminal.take_kitty_validation().is_none());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let result = std::thread::spawn(|| job.validate()).join().unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(terminal.kitty_images().is_empty());
    assert_eq!(terminal.queued_replies(), DA1.as_bytes());
    assert!(terminal.complete_kitty_validation(result));
    assert!(!terminal.kitty_validation_pending());
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Png);
    assert_eq!(terminal.kitty_placements()[0].anchor, cursor);
    assert_eq!(terminal.cursor().column(), 2);

    assert_eq!(
        terminal.ingest_with_reply_backpressure(suffix),
        suffix.len()
    );
    assert_eq!(terminal.row_text(0).unwrap().trim(), "A B");
    assert_eq!(
        replies(&mut terminal),
        format!("{DA1}\x1b_Gi=1,I=44,p=17;OK\x1b\\{DA1}")
    );
}

#[test]
fn worker_error_returns_staging_without_spending_an_id_or_accepting_foreign_results() {
    let config = KittyConfig {
        rgba_budget: 8,
        png_validator: None,
    };
    let mut terminal = enabled(9, 4, config);
    terminal.enable_kitty_worker_handoff();
    let bad = format!("\x1b_Ga=T,I=44,p=17,f=24,s=1,v=1,o=z,S=3;{ZLIB_RGBA}\x1b\\");
    assert_eq!(
        terminal.ingest_with_reply_backpressure(bad.as_bytes()),
        bad.len()
    );
    assert!(terminal.kitty_validation_pending());
    let mut copy = terminal.clone();
    assert!(!copy.kitty_validation_pending());
    let good = format!("\x1b_Ga=T,I=22,{RGB}\x1b\\");
    assert_eq!(
        copy.ingest_with_reply_backpressure(good.as_bytes()),
        good.len()
    );
    let foreign = copy.take_kitty_validation().unwrap().validate();
    assert!(!terminal.complete_kitty_validation(foreign));
    assert!(terminal.kitty_validation_pending());

    let cursor = terminal.cursor();
    let bad = terminal.take_kitty_validation().unwrap().validate();
    assert!(terminal.complete_kitty_validation(bad));
    assert_eq!(terminal.cursor(), cursor);
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(
        replies(&mut terminal),
        "\x1b_GI=44,p=17;EINVAL:invalid graphics command\x1b\\"
    );

    assert_eq!(
        terminal.ingest_with_reply_backpressure(good.as_bytes()),
        good.len()
    );
    let good = terminal.take_kitty_validation().unwrap().validate();
    assert!(terminal.complete_kitty_validation(good));
    assert_eq!(replies(&mut terminal), "\x1b_Gi=1,I=22;OK\x1b\\");
}

#[test]
fn owner_reset_waits_for_the_pending_reply_and_restores_worker_staging() {
    let mut terminal = enabled(9, 4, KittyConfig::default());
    terminal.enable_kitty_worker_handoff();
    let packet = format!("A\x1b_Ga=T,i=3,{RGB}\x1b\\");
    assert_eq!(
        terminal.ingest_with_reply_backpressure(packet.as_bytes()),
        packet.len()
    );
    terminal.reset_to_initial_state();
    assert_eq!(terminal.row_text(0).unwrap().trim(), "A");
    let result = terminal.take_kitty_validation().unwrap().validate();
    assert!(terminal.complete_kitty_validation(result));
    assert_eq!(replies(&mut terminal), "\x1b_Gi=3;OK\x1b\\");
    assert!(terminal.row_text(0).unwrap().trim().is_empty());
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(
        terminal.ingest_with_reply_backpressure(packet.as_bytes()),
        packet.len()
    );
    let result = terminal.take_kitty_validation().unwrap().validate();
    assert!(terminal.complete_kitty_validation(result));
    assert_eq!(terminal.kitty_images()[0].id, 3);
}

#[test]
fn feature_queries_report_only_supported_fields_and_preserve_multipart_upload() {
    let mut terminal = enabled(9, 4, KittyConfig::default());
    terminal.enable_kitty_worker_handoff();
    let input = concat!(
        "\x1b_Gi=4,f=24,s=2,v=1,m=1;AQID\x1b\\",
        "\x1b_Ga=q,i=42,e=T,f=24,t=f,U=1,C=1,N=3,o=z,W=1;not base64!\x1b\\",
        "\x1b_Ga=q,i=43,e=a;ignored\x1b\\",
        "\x1b_Ga=q,i=44,e=t,f=100\x1b\\",
        "\x1b_Ga=q,i=45,e=t,q=1\x1b\\",
        "\x1b_Ga=q,I=10,e=t\x1b\\",
        "\x1b[c",
        "\x1b_Gm=0;BAUG\x1b\\",
    );
    assert_eq!(
        terminal.ingest_with_reply_backpressure(input.as_bytes()),
        input.len()
    );
    assert!(terminal.kitty_validation_pending());
    assert!(terminal.kitty_images().is_empty());
    assert_eq!(
        replies(&mut terminal),
        format!(
            "\x1b_Gi=42,e=T,f=24,o=z,C=1;OK\x1b\\\x1b_Gi=43;OK\x1b\\\x1b_Gi=44,e=t;OK\x1b\\{DA1}"
        )
    );
    let upload = terminal.take_kitty_validation().unwrap().validate();
    assert!(terminal.complete_kitty_validation(upload));
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3, 4, 5, 6]);
    assert_eq!(replies(&mut terminal), "\x1b_Gi=4;OK\x1b\\");
}

#[test]
fn direct_transport_errors_are_fixed_and_failed_uploads_do_not_publish() {
    let mut terminal = enabled(9, 4, KittyConfig::default());
    for medium in ['f', 't', 's'] {
        apc(&mut terminal, &format!("a=T,t={medium},i=1,p=15;PRIVATE"));
        assert_eq!(
            replies(&mut terminal),
            "\x1b_Gi=1,p=15;ENOTSUP:unsupported graphics command\x1b\\"
        );
    }
    apc(&mut terminal, "a=T,o=x,i=1,p=15;AAAA");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=1,p=15;ENOTSUP:unsupported graphics command\x1b\\"
    );
    apc(&mut terminal, "a=T,i=1,f=32,s=1,v=1;AQID");
    apc(&mut terminal, "a=T,i=1,I=7,f=24,s=1,v=1;AQID");
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(terminal.cursor().column(), 0);
    assert_eq!(replies(&mut terminal).matches("EINVAL:").count(), 2);
    apc(&mut terminal, "a=T,f=32,s=1,v=1;AQIDBA==");
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Rgba);
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3, 4]);
    assert_eq!(terminal.cursor().column(), 1);
    assert!(replies(&mut terminal).is_empty());
}

#[test]
fn zlib_query_precedes_da1_and_leaves_chunked_rgba_upload_intact() {
    let mut terminal = enabled(
        9,
        4,
        KittyConfig {
            rgba_budget: 4,
            png_validator: None,
        },
    );
    let (first, last) = ZLIB_RGBA.split_at(4);
    apc(
        &mut terminal,
        &format!("a=T,I=42,p=7,q=2,o=z,S=4,f=32,s=1,v=1,m=1;{first}"),
    );
    let cursor = terminal.cursor();
    terminal
        .ingest(format!("\x1b_Ga=q,i=9,o=z,S=3,f=24,s=1,v=1;{ZLIB_RGB}\x1b\\\x1b[c").as_bytes());
    assert_eq!(replies(&mut terminal), format!("\x1b_Gi=9;OK\x1b\\{DA1}"));
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    assert_eq!(terminal.cursor(), cursor);

    apc(&mut terminal, &format!("m=0,q=0;{last}"));
    assert_eq!(replies(&mut terminal), "\x1b_Gi=1,I=42,p=7;OK\x1b\\");
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Rgba);
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3, 4]);
    assert_eq!(terminal.kitty_placements()[0].anchor, cursor);
}

#[test]
fn zlib_requires_exact_pre_admitted_size_and_one_complete_checked_stream() {
    const EXACT: &str = "f=24,s=1,v=1,S=3";
    const INVALID: &str = "EINVAL:invalid graphics command";
    const LIMIT: &str = "ENOSPC:graphics limit exceeded";
    let mut terminal = enabled(
        9,
        4,
        KittyConfig {
            rgba_budget: 4,
            png_validator: None,
        },
    );
    upload(&mut terminal, "a=T,i=10,q=2,C=1");
    terminal.ingest(b"\x1b[4;9H");
    let cursor = terminal.cursor();
    let image = terminal.kitty_images()[0].clone();
    let placement = terminal.kitty_placements()[0].clone();
    for (controls, stream, error) in [
        ("f=24,s=1,v=1", ZLIB_RGB, INVALID),
        ("f=24,s=1,v=1,S=0", ZLIB_RGB, INVALID),
        ("f=24,s=1,v=1,S=2", ZLIB_RGB, INVALID),
        ("f=24,s=1,v=1,S=5", ZLIB_RGB, LIMIT),
        ("f=24,s=2,v=1,S=3", ZLIB_RGB, LIMIT),
        ("f=100,S=4194305", ZLIB_RGB, LIMIT),
        (EXACT, "eJxjZAIAAAYABA==", INVALID), // Too little output.
        (EXACT, ZLIB_RGBA, INVALID),          // Too much output.
        (EXACT, "eJxjZGIGAAANAAY=", INVALID), // Bad Adler-32.
        (EXACT, "eJxjZGIGAAANAA==", INVALID), // Truncated trailer.
        (EXACT, "eJxjZGIGAAANAAcA", INVALID), // Trailing byte.
        (EXACT, "eJxjZGIGAAANAAd4nAMAAAAAAQ==", INVALID), // Second zlib stream.
        (EXACT, "Y2RiBgA=", INVALID),         // Deflate without a zlib wrapper.
    ] {
        apc(
            &mut terminal,
            &format!("a=T,I=24,p=8,q=1,o=z,{controls};{stream}"),
        );
        assert_eq!(
            replies(&mut terminal),
            format!("\x1b_GI=24,p=8;{error}\x1b\\"),
            "{controls};{stream}"
        );
        assert_eq!(terminal.kitty_images(), std::slice::from_ref(&image));
        assert_eq!(
            terminal.kitty_placements(),
            std::slice::from_ref(&placement)
        );
        assert_eq!(terminal.cursor(), cursor);
        assert_eq!(terminal.scrollback_lines().len(), 0);
    }

    let valid = format!("a=T,I=24,p=8,o=z,{EXACT};{ZLIB_RGB}");
    let full = vec![b'x'; TRANSPORT_QUEUE_HIGH_WATERMARK];
    terminal.queue_reply(&full);
    apc(&mut terminal, &valid);
    assert!(terminal.take_reply_queue_overflowed());
    assert_eq!(terminal.drain_replies(), full);
    assert_eq!(terminal.kitty_images(), std::slice::from_ref(&image));
    assert_eq!(
        terminal.kitty_placements(),
        std::slice::from_ref(&placement)
    );
    assert_eq!(terminal.cursor(), cursor);

    apc(&mut terminal, &valid);
    assert_eq!(replies(&mut terminal), "\x1b_Gi=1,I=24,p=8;OK\x1b\\");
    assert_eq!(terminal.kitty_images()[0].serial, image.serial + 1);
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3]);
    assert_eq!(terminal.kitty_placements()[0].image_id, 1);
}

#[test]
fn zlib_publishes_all_pixels_across_output_windows_at_the_reservation_limit() {
    const ZLIB: &str = "eJztwwENAAAIA6AMT2soQ94gwkZmG1VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVfX1A9OyoUQ=";
    let mut terminal = enabled(
        9,
        4,
        KittyConfig {
            rgba_budget: 66_048,
            png_validator: None,
        },
    );
    apc(
        &mut terminal,
        &format!("a=t,i=1,o=z,f=32,s=128,v=129,S=66048;{ZLIB}"),
    );
    assert_eq!(replies(&mut terminal), "\x1b_Gi=1;OK\x1b\\");
    let data = terminal.kitty_images()[0].data();
    assert_eq!(data.len(), 66_048);
    assert!(data
        .as_chunks::<4>()
        .0
        .iter()
        .all(|&pixel| pixel == [0x11, 0x77, 0xab, 0xff]));
}

#[test]
fn kitty_triggered_scroll_moves_existing_placements_and_text_exactly_once() {
    let mut terminal = enabled(5, 4, KittyConfig::default());
    terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\x1b[3;1H");
    upload(&mut terminal, "a=T,i=1,C=1");
    terminal.ingest(b"\x1b[4;5H");
    upload(&mut terminal, "a=T,i=2");
    let placements = terminal.kitty_placements();
    assert_eq!(placements[0].anchor.row(), 1);
    assert_eq!(placements[1].anchor.row(), 2);
    assert_eq!(terminal.row_text(1).unwrap().trim(), "three");
    assert_eq!(terminal.scrollback_lines().len(), 1);
    assert_eq!(
        (terminal.cursor().column(), terminal.cursor().row()),
        (0, 3)
    );
}

#[test]
fn placement_pixel_extent_preserves_aspect_and_excludes_unused_cells() {
    let mut terminal = enabled(6, 4, KittyConfig::default());
    terminal
        .set_kitty_geometry(KittyGeometry::new(8, 12))
        .unwrap();
    apc(&mut terminal, "a=T,i=1,p=1,C=1,f=24,s=2,v=1;AAAAAAAA");
    apc(&mut terminal, "a=p,i=1,p=2,C=1,c=3");
    apc(&mut terminal, "a=p,i=1,p=3,C=1,c=2,r=2");
    apc(&mut terminal, "a=p,i=1,p=4,C=1,r=1,X=2,Y=1");
    let placements = terminal.kitty_placements();
    for (placement, span, extent, offset) in [
        (&placements[0], (1, 1), (2, 1), (0, 0)),
        (&placements[1], (3, 1), (24, 12), (0, 0)),
        (&placements[2], (2, 2), (16, 8), (0, 0)),
        (&placements[3], (3, 1), (22, 11), (2, 1)),
    ] {
        assert_eq!((placement.columns, placement.rows), span);
        assert_eq!(placement.pixel_extent(), extent);
        assert_eq!(placement.pixel_offset, offset);
    }

    terminal.ingest(b"\x1b[1S");
    assert!(terminal.kitty_placements().is_empty());
}

#[test]
fn placement_scroll_wrap_crop_and_buffer_lifecycle_are_ordered() {
    let mut terminal = enabled(5, 4, KittyConfig::default());
    terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\x1b[4;4H");
    apc(
        &mut terminal,
        "a=T,i=1,p=7,c=2,r=3,f=24,s=1,v=4;AAAAAAAAAAAAAAAA",
    );
    assert_eq!(terminal.cursor().column(), 0);
    assert_eq!(terminal.cursor().row(), 3);
    assert_eq!(terminal.scrollback_lines().len(), 3);
    assert_eq!(terminal.kitty_placements()[0].anchor.row(), 0);
    terminal.ingest(b"Z\x1b[1S");
    let placement = &terminal.kitty_placements()[0];
    assert_eq!(placement.rows, 2);
    assert_eq!(
        placement.source(),
        KittySourceRect {
            x: 0,
            y: 1,
            width: 1,
            height: 3
        }
    );
    terminal.ingest(b"\x1b[1T\x1b[0J\x1b[1J\x1b[3J");
    assert_eq!(terminal.kitty_placements()[0].anchor.row(), 1);
    terminal.ingest(b"\x1b[?47h");
    assert!(terminal.kitty_images().is_empty());
    upload(&mut terminal, "a=T,i=2,C=1");
    terminal.ingest(b"\x1b[?47l");
    assert_eq!(terminal.kitty_images()[0].id, 1);
    terminal.ingest(b"\x1b[2J\x1b[?47h");
    assert_eq!(terminal.kitty_placements()[0].image_id, 2);
    terminal.ingest(b"\x1b[?1047l");
    assert!(terminal.kitty_placements().is_empty());
    terminal.ingest(b"\x1b[?47h");
    assert!(terminal.kitty_images().is_empty());
    terminal.ingest(b"\x1bc");
    assert!(terminal.kitty_images().is_empty());
    assert!(terminal.kitty_placements().is_empty());
    upload(&mut terminal, "a=T,i=3,C=1");
    terminal.resize(Dimensions::new(6, 4).unwrap()).unwrap();
    assert_eq!(terminal.kitty_images()[0].id, 3);
    assert!(terminal.kitty_placements().is_empty());
    replies(&mut terminal);
    apc(&mut terminal, "a=p,i=3,C=1");
    assert!(replies(&mut terminal).is_empty());
}

#[test]
fn scroll_margins_move_only_wholly_contained_placements() {
    let mut terminal = enabled(8, 5, KittyConfig::default());
    apc(&mut terminal, "a=t,i=1,f=24,s=1,v=2;AAAAAAAA");
    terminal.ingest(b"\x1b[?69h\x1b[3;6s\x1b[2;4r\x1b[2;3H");
    apc(&mut terminal, "a=p,i=1,p=10,c=2,r=2,C=1");
    terminal.ingest(b"\x1b[2;2H");
    apc(&mut terminal, "a=p,i=1,p=20,c=2,r=2,C=1");
    terminal.ingest(b"\x1b[2;3H\x1b[1L\x1b[1M");
    assert!(terminal
        .kitty_placements()
        .iter()
        .all(|placement| placement.rows == 2));
    terminal.ingest(b"\x1b[1S");
    let placements = terminal.kitty_placements();
    assert_eq!(placements[0].placement_id, Some(10));
    assert_eq!((placements[0].anchor.row(), placements[0].rows), (1, 1));
    assert_eq!(
        (placements[0].source().y, placements[0].source().height),
        (1, 1)
    );
    assert_eq!(placements[1].placement_id, Some(20));
    assert_eq!((placements[1].anchor.row(), placements[1].rows), (1, 2));
}

#[test]
fn enabled_framing_swallows_c0_and_discards_oversize_or_interrupted_packets() {
    let mut terminal = enabled(15, 3, KittyConfig::default());
    terminal.ingest(b"before\x1b_Ga=T,i=1,f=24,s=1,v=1;AQ\r\n\tI\x0bD\x1b\\");
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3]);
    assert_eq!(terminal.cursor().row(), 0);
    assert_eq!(terminal.cursor().column(), 7);
    replies(&mut terminal);

    terminal.ingest(b"\x1b_Ga=T,i=2,f=24,s=1,v=1;");
    let overrun = vec![b'A'; 1024 * 1024];
    for _ in 0..6 {
        terminal.ingest(&overrun);
    }
    terminal.ingest(b"\x1b#\r\n9hidden\x1b[2;3HZ");
    assert_eq!(terminal.cell(2, 1).unwrap().character(), 'Z');
    assert_eq!(terminal.row_text(0).unwrap().trim(), "before");
    assert_eq!(terminal.kitty_images().len(), 1);
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=2;ENOSPC:graphics limit exceeded\x1b\\"
    );

    for end in [b"\x18".as_slice(), b"\x1a", b"\x1b\x1b\\", b"\x1b[H"] {
        terminal.ingest(b"\x1b_Ga=t,i=3,f=24,s=1,v=1;AQID");
        terminal.ingest(end);
        assert_eq!(terminal.kitty_images().len(), 1);
        assert_eq!(
            replies(&mut terminal),
            "\x1b_Gi=3;EINVAL:invalid graphics command\x1b\\"
        );
    }
    terminal.ingest(b"\x1bPqignored\r\n\x1b#9hidden\x1b\\X");
    assert_eq!(terminal.cell(0, 0).unwrap().character(), 'X');
}

#[test]
fn parser_abort_keeps_address_and_quiet_and_does_not_publish_a_continuation() {
    let mut terminal = enabled(10, 4, KittyConfig::default());
    upload(&mut terminal, "a=t,I=42,q=1");
    apc(&mut terminal, "I=42,p=7,q=2,f=24,s=2,v=1,m=1;AQID");
    terminal.ingest(b"\x1b_Ga=q,i=99,q=2;AA\x18");
    terminal.ingest(b"\x1b_Gq=1,m=0;BAUG\x18");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=1,I=42,p=7;EINVAL:invalid graphics command\x1b\\"
    );
    apc(&mut terminal, "m=0;BAUG");
    assert_eq!(terminal.kitty_images().len(), 1);
    assert_eq!(terminal.kitty_images()[0].data(), &[1, 2, 3]);
    assert!(replies(&mut terminal).is_empty());

    terminal.ingest(b"\x1b_Gi=9,p=2,q=2;AAA\x1a");
    terminal.ingest(b"\x1b_Gi=8,p=3,");
    terminal.ingest(&[b'X'; 1024]);
    terminal.ingest(b"hidden\x1b\\");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=8,p=3;ENOSPC:graphics limit exceeded\x1b\\"
    );
    assert!(terminal.row_text(0).unwrap().trim().is_empty());

    terminal.ingest(b"\x1b_Gi=8,p=3;AB\x1bc");
    upload(&mut terminal, "a=t,i=8");
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=8,p=3;EINVAL:invalid graphics command\x1b\\\x1b_Gi=8;OK\x1b\\"
    );
    assert_eq!(terminal.kitty_images().len(), 1);
}

#[test]
fn png_requires_structure_dimensions_and_the_trusted_stream_validator() {
    const ZLIB_PNG: &str = "eJzrDPBz5+WS4mJgYOD19HAJAtKMIMzBBiTlRY90giVcHEMq5iT/OH/ggzwDKwPj/86ZtrJACQZPVz+XdU4JTQDMxBAe";
    const BAD_STREAM: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACElEQVRub3QtemxpYq351UYAAAAASUVORK5CYII=";
    const BAD_SIZE: &str =
        "iVBORw0KGgoAAAANSUhEUgAAIAEAAAABCAYAAACZiUteAAAACElEQVRub3QtemxpYq351UYAAAAASUVORK5CYII=";
    let mut no_decoder = enabled(10, 3, KittyConfig::default());
    apc(&mut no_decoder, &format!("a=q,i=4,f=100;{VALID_PNG}"));
    assert_eq!(
        replies(&mut no_decoder),
        "\x1b_Gi=4;ENOTSUP:unsupported graphics command\x1b\\"
    );

    let calls = Arc::new(AtomicUsize::new(0));
    let validator = KittyPngValidator::new({
        let calls = Arc::clone(&calls);
        move |data, width, height| {
            calls.fetch_add(1, Ordering::Relaxed);
            assert_eq!((width, height), (1, 1));
            data.len() == 70
        }
    });
    let mut terminal = enabled(
        10,
        3,
        KittyConfig {
            png_validator: Some(validator),
            ..KittyConfig::default()
        },
    );
    let corrupt_crc = VALID_PNG.replacen("FcSJ", "FcSK", 1);
    for bad in [&corrupt_crc, BAD_SIZE] {
        apc(&mut terminal, &format!("a=t,i=4,f=100;{bad}"));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(terminal.kitty_images().is_empty());
    replies(&mut terminal);
    apc(&mut terminal, &format!("a=t,i=4,f=100;{BAD_STREAM}"));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert!(terminal.kitty_images().is_empty());
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=4;EINVAL:invalid graphics command\x1b\\"
    );
    apc(&mut terminal, &format!("a=t,i=4,f=100;{VALID_PNG}"));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Png);
    assert_eq!(replies(&mut terminal), "\x1b_Gi=4;OK\x1b\\");

    let original = terminal.kitty_images()[0].clone();
    apc(&mut terminal, &format!("a=t,i=4,f=100,o=z,S=69;{ZLIB_PNG}"));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(terminal.kitty_images(), std::slice::from_ref(&original));
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=4;EINVAL:invalid graphics command\x1b\\"
    );
    apc(&mut terminal, &format!("a=t,i=4,f=100,o=z,S=70;{ZLIB_PNG}"));
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(terminal.kitty_images()[0].format, KittyFormat::Png);
    assert_eq!(terminal.kitty_images()[0].data(), original.data());
    assert_eq!(replies(&mut terminal), "\x1b_Gi=4;OK\x1b\\");
}

#[test]
fn retained_png_source_budget_is_shared_by_the_primary_and_alternate_buffers() {
    // A one-pixel PNG with a valid 2 MiB tEXt chunk, wrapped in zlib.
    let zlib_png = format!(
        "eNrtyTFqwgAYBeA/g1LEjF2EYi/ioETaLEHEwbEgHVxU2gzObjmAQ8UzeAYHnb2JOLq18RRdvg/ee8OrRsVb2uq0IiLN37NxvckjT826u8+nKl6jXQ6nZfb5Pfuar8r5chF9{}gH923l23EZHmWX+y3s/ul+OtG41Ifquf3kt9RD4sssPgY/MH3+WTFQ==",
        "A".repeat(2708)
    );
    let mut terminal = enabled(
        10,
        3,
        KittyConfig {
            rgba_budget: 8,
            png_validator: Some(KittyPngValidator::new(|_, w, h| (w, h) == (1, 1))),
        },
    );
    apc(
        &mut terminal,
        &format!("a=t,i=1,f=100,o=z,S=2097246;{zlib_png}"),
    );
    assert_eq!(terminal.kitty_images()[0].data().len(), 2_097_246);
    terminal.ingest(b"\x1b[?47h");
    apc(
        &mut terminal,
        &format!("a=t,i=2,f=100,o=z,S=2097246;{zlib_png}"),
    );
    assert_eq!(terminal.kitty_images()[0].id, 2);
    terminal.ingest(b"\x1b[?47l");
    assert!(
        terminal.kitty_images().is_empty(),
        "only source bytes exceeded quota"
    );
    assert_eq!(
        replies(&mut terminal),
        "\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\"
    );
}
