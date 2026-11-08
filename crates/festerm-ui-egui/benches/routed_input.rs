use std::{env, hint::black_box};

use festerm_core::{Dimensions, InputEvent, Key, Terminal};
use festerm_ui_egui::{route_input, EncodedInputSink};

#[derive(Default)]
struct Sink {
    count: usize,
    checksum: usize,
}

impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, bytes: &[u8]) {
        self.count += 1;
        for &byte in bytes {
            self.checksum = self.checksum.wrapping_add(usize::from(black_box(byte)));
        }
    }
}

fn main() {
    let mut args = env::args().skip(1);
    let case = args.next().expect("expected ascii or emoji");
    let iterations: usize = args.next().expect("expected iterations").parse().unwrap();
    assert!(args.next().is_none());
    let character = match case.as_str() {
        "ascii" => 'x',
        "emoji" => '🙂',
        _ => panic!("expected ascii or emoji"),
    };
    let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
    let mut sink = Sink::default();
    for _ in 0..100 {
        route_input(
            &mut terminal,
            InputEvent::Key(Key::Character(character)),
            &mut sink,
        );
    }
    sink.count = 0;
    sink.checksum = 0;
    for _ in 0..iterations {
        black_box(route_input(
            &mut terminal,
            InputEvent::Key(Key::Character(black_box(character))),
            &mut sink,
        ));
    }
    let mut encoded = [0; 4];
    let byte_sum: usize = character
        .encode_utf8(&mut encoded)
        .bytes()
        .map(usize::from)
        .sum();
    assert_eq!(sink.count, iterations);
    assert_eq!(sink.checksum, byte_sum.wrapping_mul(iterations));
    assert!(terminal.queued_input().is_empty());
    println!("{case}: {iterations} events, checksum {}", sink.checksum);
}
