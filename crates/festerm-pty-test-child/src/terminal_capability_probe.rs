//! Opt-in Windows console observations, never terminal reply injection.
//! Kept separate from the test child's arbitrary-output argv protocol.
#![cfg_attr(not(windows), allow(dead_code))]
use regex::bytes::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_BYTES: usize = 4096;
const MAX_FRAME: usize = 256;
const ENVIRONMENT: [&str; 6] = [
    "TERM",
    "TERM_PROGRAM",
    "COLORTERM",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];
const QUERIES: [(&str, &[u8]); 10] = [
    ("da1", b"\x1b[c"),
    ("da2", b"\x1b[>c"),
    ("dsr6", b"\x1b[6n"),
    ("osc4_0", b"\x1b]4;0;?\x1b\\"),
    ("osc4_7", b"\x1b]4;7;?\x1b\\"),
    ("osc4_15", b"\x1b]4;15;?\x1b\\"),
    ("osc4_255", b"\x1b]4;255;?\x1b\\"),
    ("osc10", b"\x1b]10;?\x1b\\"),
    ("osc11", b"\x1b]11;?\x1b\\"),
    ("osc12", b"\x1b]12;?\x1b\\"),
];

#[derive(Debug, PartialEq)]
struct Refused(&'static str, usize);

struct Replies {
    buffer: Vec<u8>,
    received: usize,
    values: BTreeMap<&'static str, Vec<Value>>,
    da: Regex,
    cursor: Regex,
    color: Regex,
}
impl Replies {
    fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(MAX_FRAME), received: 0,
            values: QUERIES.iter().map(|(name, _)| (*name, Vec::new())).collect(),
            da: Regex::new(r"\A\x1b\[([?>])([0-9;]{1,128})c\z").unwrap(),
            cursor: Regex::new(r"\A\x1b\[([0-9]{1,5});([0-9]{1,5})R\z").unwrap(),
            color: Regex::new(r"\A\x1b\](4;(0|7|15|255)|10|11|12);rgb:([0-9a-fA-F]{1,4})/([0-9a-fA-F]{1,4})/([0-9a-fA-F]{1,4})(\x07|\x1b\\)\z").unwrap(),
        }
    }
    fn refuse(&mut self, reason: &'static str) -> Refused {
        let count = self.buffer.len();
        self.buffer.clear();
        Refused(reason, count)
    }
    fn feed(&mut self, data: &[u8]) -> Result<(), Refused> {
        self.received += data.len();
        if self.received > MAX_BYTES {
            return Err(Refused("input_budget", data.len()));
        }
        self.buffer.extend_from_slice(data);
        while !self.buffer.is_empty() {
            if self.buffer[0] != 27 {
                return Err(self.refuse("unexpected_input"));
            }
            if self.buffer.len() < 2 {
                return Ok(());
            }
            let end = match self.buffer[1] {
                b'[' => self
                    .buffer
                    .iter()
                    .enumerate()
                    .skip(2)
                    .find(|(_, b)| (0x40..=0x7e).contains(*b))
                    .map(|(i, _)| i + 1),
                b']' => (2..self.buffer.len()).find_map(|i| {
                    if self.buffer[i] == 7 {
                        Some(i + 1)
                    } else if self.buffer[i..].starts_with(b"\x1b\\") {
                        Some(i + 2)
                    } else {
                        None
                    }
                }),
                _ => return Err(self.refuse("unexpected_input")),
            };
            let Some(end) = end else {
                if self.buffer.len() > MAX_FRAME {
                    return Err(self.refuse("frame_budget"));
                }
                return Ok(());
            };
            if end > MAX_FRAME {
                return Err(self.refuse("frame_budget"));
            }
            let recognized = self.recognize(&self.buffer[..end]);
            self.buffer.drain(..end);
            let Some((name, value)) = recognized else {
                return Err(Refused("unexpected_reply", end));
            };
            self.values.get_mut(name).unwrap().push(value);
        }
        Ok(())
    }
    fn recognize(&self, frame: &[u8]) -> Option<(&'static str, Value)> {
        if let Some(m) = self.da.captures(frame) {
            let parts: Vec<_> = m[2].split(|b| *b == b';').collect();
            if parts.len() > 32 || parts.iter().any(|p| p.is_empty() || p.len() > 5) {
                return None;
            }
            let values: Option<Vec<u16>> = parts
                .iter()
                .map(|p| std::str::from_utf8(p).ok()?.parse().ok())
                .collect();
            return Some((
                if &m[1] == b"?" { "da1" } else { "da2" },
                json!({"parameters": values?}),
            ));
        }
        if let Some(m) = self.cursor.captures(frame) {
            let row: u16 = std::str::from_utf8(&m[1]).ok()?.parse().ok()?;
            let column: u16 = std::str::from_utf8(&m[2]).ok()?.parse().ok()?;
            if row == 0 || column == 0 {
                return None;
            }
            return Some(("dsr6", json!({"row":row,"column":column})));
        }
        if let Some(m) = self.color.captures(frame) {
            let name = match &m[1] {
                b"4;0" => "osc4_0",
                b"4;7" => "osc4_7",
                b"4;15" => "osc4_15",
                b"4;255" => "osc4_255",
                b"10" => "osc10",
                b"11" => "osc11",
                b"12" => "osc12",
                _ => return None,
            };
            let rgb: Vec<_> = [3, 4, 5]
                .iter()
                .map(|i| std::str::from_utf8(&m[*i]).unwrap().to_ascii_lowercase())
                .collect();
            return Some((name, json!({"rgb":rgb})));
        }
        None
    }
    fn all_received(&self) -> bool {
        self.values.values().all(|v| !v.is_empty())
    }
    fn exactly_once(&self) -> bool {
        self.values.values().all(|v| v.len() == 1) && self.buffer.is_empty()
    }
    fn report(&self) -> Value {
        Value::Object(
            self.values
                .iter()
                .map(|(name, values)| {
                    (
                        name.to_string(),
                        json!({"count":values.len(),"replies":values}),
                    )
                })
                .collect(),
        )
    }
}
fn environment_value(value: Option<String>) -> Value {
    match value {
        None => json!({"set":false,"value":null}),
        Some(value)
            if value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.@:+/-".contains(&b)) =>
        {
            json!({"set":true,"value":value})
        }
        Some(_) => json!({"set":true,"omitted":"invalid_or_overlength"}),
    }
}
fn case_valid(case: &str) -> bool {
    !case.is_empty()
        && case.len() <= 64
        && case
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn qualified(record: &Value) -> bool {
    record["status"] == "complete"
        && record["grid_stable"] == true
        && record["terminal_restored"] == true
        && record["worker_drained"] == true
}
// Both handles are attempted and both observed even if an earlier call fails.
fn restore_console_modes(
    input: u32,
    output: u32,
    mut set: impl FnMut(bool, u32) -> bool,
    mut get: impl FnMut(bool) -> Option<u32>,
) -> bool {
    let input_set = set(true, input);
    let output_set = set(false, output);
    let input_after = get(true);
    let output_after = get(false);
    input_set && output_set && input_after == Some(input) && output_after == Some(output)
}
#[cfg(windows)]
#[path = "terminal_capability_windows.rs"]
mod windows;
fn main() {
    #[cfg(windows)]
    std::process::exit(windows::main());
    #[cfg(not(windows))]
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete() -> Vec<u8> {
        // Color replies deliberately precede identity/cursor replies, unlike
        // the write order, and the test feeds every byte separately.
        let mut bytes = Vec::new();
        for i in [0, 7, 15, 255] {
            bytes.extend(format!("\x1b]4;{i};rgb:0/aa/FFFF\x1b\\").bytes());
        }
        for i in [10, 11, 12] {
            bytes.extend(format!("\x1b]{i};rgb:1/2/3\x07").bytes());
        }
        bytes.extend_from_slice(b"\x1b[>0;1;0c\x1b[?1;0c\x1b[2;3R");
        bytes
    }
    #[test]
    fn fragmented_reordered_queries_and_duplicates_are_observed_not_invented() {
        let mut p = Replies::new();
        for b in complete() {
            p.feed(&[b]).unwrap();
        }
        assert!(p.exactly_once());
        assert_eq!(
            p.report()["osc4_0"]["replies"][0]["rgb"],
            json!(["0", "aa", "ffff"])
        );
        p.feed(b"\x1b[?1;0c").unwrap();
        assert!(p.all_received());
        assert!(!p.exactly_once());
        assert_eq!(p.report()["da1"]["count"], 2);
        let mut p = Replies::new();
        p.feed(b"\x1b[2;3R").unwrap();
        assert!(!p.all_received());
        assert!(!p.exactly_once());
    }
    #[test]
    fn unrecognized_input_is_counted_without_retaining_or_reporting_content() {
        let mut p = Replies::new();
        assert_eq!(p.feed(b"private"), Err(Refused("unexpected_input", 7)));
        assert!(p.buffer.is_empty());
        assert!(!p.report().to_string().contains("private"));
        for bytes in [
            b"\x1b[?65536c".as_slice(),
            b"\x1b[0;2R",
            b"\x1b]52;secret\x07",
            b"\x1b[?;c",
        ] {
            let mut p = Replies::new();
            assert_eq!(p.feed(bytes), Err(Refused("unexpected_reply", bytes.len())));
        }
    }
    #[test]
    fn bounded_frames_and_total_input_refuse_before_unbounded_retention() {
        let mut p = Replies::new();
        let mut bytes = b"\x1b]".to_vec();
        bytes.extend(vec![b'0'; MAX_FRAME]);
        assert_eq!(p.feed(&bytes), Err(Refused("frame_budget", MAX_FRAME + 2)));
        let mut p = Replies::new();
        assert_eq!(
            p.feed(&vec![b'x'; MAX_BYTES + 1]),
            Err(Refused("input_budget", MAX_BYTES + 1))
        );
        assert!(p.buffer.is_empty());
    }
    #[test]
    fn environment_and_case_identifiers_retain_only_the_fixed_safe_contract() {
        assert_eq!(ENVIRONMENT.len(), 6);
        assert_eq!(QUERIES.len(), 10);
        assert_eq!(environment_value(None), json!({"set":false,"value":null}));
        assert_eq!(
            environment_value(Some(String::new())),
            json!({"set":true,"value":""})
        );
        for v in ["secret\nvalue".to_owned(), "x".repeat(129), "é".to_owned()] {
            assert_eq!(
                environment_value(Some(v)),
                json!({"set":true,"omitted":"invalid_or_overlength"})
            );
        }
        assert!(case_valid("windows-local_1"));
        for v in ["", "bad/path", "bad\ncase"] {
            assert!(!case_valid(v));
        }
    }
    #[test]
    fn cleanup_attempts_and_verifies_both_modes_after_a_set_failure() {
        use std::cell::RefCell;
        let calls = RefCell::new(Vec::new());
        assert!(!restore_console_modes(
            7,
            11,
            |input, value| {
                calls.borrow_mut().push((input, value));
                !input
            },
            |input| Some(if input { 7 } else { 11 })
        ));
        assert_eq!(*calls.borrow(), vec![(true, 7), (false, 11)]);
        assert!(!restore_console_modes(
            7,
            11,
            |_, _| true,
            |input| Some(if input { 7 } else { 99 })
        ));
        assert!(!restore_console_modes(7, 11, |_, _| true, |_| None));
        assert!(restore_console_modes(
            7,
            11,
            |_, _| true,
            |input| Some(if input { 7 } else { 11 })
        ));
    }
    #[test]
    fn success_requires_every_observation_and_verified_cleanup_condition() {
        let base = json!({"status":"complete","grid_stable":true,"terminal_restored":true,"worker_drained":true});
        assert!(qualified(&base));
        for key in ["grid_stable", "terminal_restored", "worker_drained"] {
            let mut record = base.clone();
            record[key] = json!(false);
            assert!(!qualified(&record));
        }
        let mut record = base;
        record["status"] = json!("incomplete");
        assert!(!qualified(&record));
    }
}
