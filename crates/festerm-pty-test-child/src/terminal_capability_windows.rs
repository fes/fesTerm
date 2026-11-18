//! Native console I/O only; no ConPTY creation or tunnel/session authority.
// This nonshipping test boundary alone permits Win32 FFI. Other helper code
// retains deny(unsafe_code); no product or workspace lint policy changes.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use std::{
    fs::File,
    io::Write,
    os::windows::io::AsRawHandle,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{Console::*, IO::CancelSynchronousIo},
};

static CANCELLED: AtomicBool = AtomicBool::new(false);
unsafe extern "system" fn control(event: u32) -> i32 {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            CANCELLED.store(true, Ordering::Release);
            1
        }
        _ => 0,
    }
}
fn mode(handle: HANDLE) -> Result<u32, Refused> {
    let mut value = 0;
    // SAFETY: borrowed standard console handle; value is valid writable storage.
    if unsafe { GetConsoleMode(handle, &mut value) } == 0 {
        Err(Refused("console_required", 0))
    } else {
        Ok(value)
    }
}
fn grid(handle: HANDLE) -> Result<Value, Refused> {
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: borrowed console handle and initialized output structure.
    if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
        return Err(Refused("console_geometry", 0));
    }
    let columns = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    let rows = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
    if columns <= 0 || rows <= 0 {
        return Err(Refused("console_geometry", 0));
    }
    Ok(json!({"columns":columns,"rows":rows}))
}
fn idle(handle: HANDLE) -> Result<(), Refused> {
    let mut count = 0;
    // SAFETY: observes the borrowed input handle without consuming any records.
    if unsafe { GetNumberOfConsoleInputEvents(handle, &mut count) } == 0 {
        return Err(Refused("console_input", 0));
    }
    if count != 0 {
        return Err(Refused("pending_input", count as usize));
    }
    Ok(())
}
struct Modes {
    input: HANDLE,
    output: HANDLE,
    input_mode: u32,
    output_mode: u32,
    handler: bool,
    changed: bool,
}
impl Modes {
    fn restore(&mut self) -> bool {
        // Attempt BOTH restorations even after one fails. No flush or input read.
        let restored = if self.changed {
            restore_console_modes(
                self.input_mode,
                self.output_mode,
                |input, value| {
                    // SAFETY: both borrowed handles remain live until helper exit.
                    (unsafe { SetConsoleMode(if input { self.input } else { self.output }, value) })
                        != 0
                },
                |input| mode(if input { self.input } else { self.output }).ok(),
            )
        } else {
            true
        };
        if restored {
            self.changed = false;
        }
        let handler = if self.handler {
            // SAFETY: removes the exact process-local handler registered below.
            let removed = unsafe { SetConsoleCtrlHandler(Some(control), 0) } != 0;
            if removed {
                self.handler = false;
            }
            removed
        } else {
            true
        };
        restored && handler
    }
}
impl Drop for Modes {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

// Only native query characters may enter the portable parser. Never expose
// unrelated event codes, characters, scan codes or control-key state in JSON.
fn reply_record(record: &INPUT_RECORD) -> Result<Vec<u8>, Refused> {
    if u32::from(record.EventType) != KEY_EVENT {
        return Err(Refused("unexpected_console_event", 1));
    }
    // SAFETY: union arm is selected only after verifying EventType == KEY_EVENT.
    let key = unsafe { record.Event.KeyEvent };
    // SAFETY: ReadConsoleInputW populates the Unicode arm of KEY_EVENT_RECORD.
    let character = unsafe { key.uChar.UnicodeChar };
    if key.bKeyDown == 0 || character == 0 || character > 127 || key.wRepeatCount == 0 {
        return Err(Refused("unexpected_console_event", 1));
    }
    let repeat = usize::from(key.wRepeatCount);
    if repeat > MAX_BYTES {
        return Err(Refused("input_budget", repeat));
    }
    Ok(vec![character as u8; repeat])
}
enum Chunk {
    Input(Vec<u8>),
    Failed(Refused),
}

fn worker(
    input: usize,
    output: usize,
    stop: Arc<AtomicBool>,
    written: Arc<AtomicBool>,
    sender: mpsc::SyncSender<Chunk>,
) {
    let input = input as HANDLE;
    let output = output as HANDLE;
    let bytes: Vec<u16> = QUERIES
        .iter()
        .flat_map(|(_, q)| q.iter().map(|b| u16::from(*b)))
        .collect();
    let mut offset = 0;
    while offset < bytes.len() && !stop.load(Ordering::Acquire) {
        let mut count = 0;
        // SAFETY: bounded UTF-16 query slice, valid count output, borrowed handle.
        if unsafe {
            WriteConsoleW(
                output,
                bytes[offset..].as_ptr().cast(),
                (bytes.len() - offset) as u32,
                &mut count,
                std::ptr::null(),
            )
        } == 0
            || count == 0
        {
            let _ = sender.try_send(Chunk::Failed(Refused("query_write", 0)));
            return;
        }
        offset += count as usize;
    }
    if offset != bytes.len() {
        return;
    }
    written.store(true, Ordering::Release);
    while !stop.load(Ordering::Acquire) {
        let mut records = [INPUT_RECORD::default(); 32];
        let mut count = 0;
        // SAFETY: fixed initialized record array; Windows writes at most its length.
        if unsafe {
            ReadConsoleInputW(
                input,
                records.as_mut_ptr(),
                records.len() as u32,
                &mut count,
            )
        } == 0
            || count == 0
        {
            if !stop.load(Ordering::Acquire) {
                let _ = sender.try_send(Chunk::Failed(Refused("console_read", 0)));
            }
            return;
        }
        let mut bytes = Vec::new();
        for record in &records[..count as usize] {
            match reply_record(record) {
                Ok(part) if bytes.len() + part.len() <= MAX_BYTES => bytes.extend(part),
                Ok(_) => {
                    let _ = sender.try_send(Chunk::Failed(Refused("input_budget", 1)));
                    return;
                }
                Err(error) => {
                    let _ = sender.try_send(Chunk::Failed(error));
                    return;
                }
            }
        }
        // Finite handoff; a stalled collector cannot grow memory indefinitely.
        if sender.try_send(Chunk::Input(bytes)).is_err() {
            return;
        }
    }
}

fn observe() -> Result<Value, Refused> {
    // SAFETY: obtains process-borrowed handles, never closes or replaces them.
    let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    // SAFETY: same borrowed-handle contract; validity is checked by mode/grid.
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let mut modes = Modes {
        input,
        output,
        input_mode: mode(input)?,
        output_mode: mode(output)?,
        handler: false,
        changed: false,
    };
    let before = grid(output)?;
    idle(input)?;
    CANCELLED.store(false, Ordering::Release);
    // SAFETY: static callback only writes an atomic; removed before normal return.
    if unsafe { SetConsoleCtrlHandler(Some(control), 1) } == 0 {
        return Err(Refused("control_handler", 0));
    }
    modes.handler = true;
    let start = Instant::now();
    let mut replies = Replies::new();
    let mut result = json!({"grid_before":before,"status":"incomplete","worker_drained":true});
    let outcome = (|| -> Result<(), Refused> {
        let raw =
            modes.input_mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
        modes.changed = true;
        // SAFETY: mode snapshot precedes mutation and the guard restores both handles.
        if unsafe { SetConsoleMode(input, raw | ENABLE_VIRTUAL_TERMINAL_INPUT) } == 0
            || unsafe {
                SetConsoleMode(
                    output,
                    modes.output_mode
                        | ENABLE_PROCESSED_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                )
            } == 0
        {
            return Err(Refused("console_mode", 0));
        }
        idle(input)?; // Refuse typeahead exposed by changing modes, without reading.
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(8);
        let stop_worker = stop.clone();
        let written_worker = written.clone();
        let input = input as usize;
        let output = output as usize;
        let handle = thread::Builder::new()
            .name("terminal-probe-io".into())
            .spawn(move || worker(input, output, stop_worker, written_worker, sender))
            .map_err(|_| Refused("worker_start", 0))?;
        let deadline = start + Duration::from_secs(12);
        let mut quiet = None;
        let collected = (|| -> Result<(), Refused> {
            while Instant::now() < deadline {
                if CANCELLED.load(Ordering::Acquire) {
                    return Err(Refused("interrupted", 0));
                }
                if replies.all_received()
                    && quiet.is_some_and(|t: Instant| t.elapsed() >= Duration::from_millis(250))
                {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(25)) {
                    Ok(Chunk::Input(bytes)) => {
                        replies.feed(&bytes)?;
                        quiet = Some(Instant::now());
                    }
                    Ok(Chunk::Failed(error)) => return Err(error),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(Refused("worker_stopped", 0))
                    }
                }
            }
            if !replies.buffer.is_empty() {
                return Err(Refused("incomplete_reply", replies.buffer.len()));
            }
            if !written.load(Ordering::Acquire) {
                return Err(Refused("query_write_timeout", 0));
            }
            Ok(())
        })();
        stop.store(true, Ordering::Release);
        let drain_deadline = Instant::now() + Duration::from_millis(500);
        while !handle.is_finished() && Instant::now() < drain_deadline {
            // The worker can pass its stop check before this thread sets stop,
            // then enter ReadConsoleInputW after a cancellation found no I/O.
            // Reissue within the same drain bound to cover that handoff race.
            // SAFETY: retained JoinHandle owns the live thread handle through join.
            // A cancellation request does NOT prove I/O or worker completion.
            unsafe { CancelSynchronousIo(handle.as_raw_handle().cast()) };
            thread::sleep(Duration::from_millis(5));
        }
        if !handle.is_finished() {
            // Cancellation is not completion. Do not leave a reader running in
            // the foreground console or publish a success. Terminating this
            // standalone helper retires its worker; the supervisor sees failure.
            let _ = modes.restore();
            std::process::exit(1);
        }
        handle.join().map_err(|_| Refused("worker_panicked", 0))?;
        // A late batch after the quiet/deadline boundary cannot be silently
        // discarded and converted into a complete observation.
        for chunk in receiver.try_iter() {
            match chunk {
                Chunk::Input(bytes) => replies.feed(&bytes)?,
                Chunk::Failed(error) => return Err(error),
            }
        }
        collected
    })();
    match outcome {
        Ok(()) if replies.exactly_once() => result["status"] = json!("complete"),
        Ok(()) => {}
        Err(Refused(reason, count)) => {
            result["status"] = json!("aborted");
            result["reason"] = json!(reason);
            result["unknown_bytes"] = json!(count);
            result["unknown_count_unit"] = json!(if matches!(
                reason,
                "pending_input" | "unexpected_console_event"
            ) {
                "console_records"
            } else {
                "bytes"
            });
        }
    }
    let after = grid(output);
    result["terminal_restored"] = json!(modes.restore());
    result["grid_stable"] = json!(after.as_ref() == Ok(&before));
    result["grid_after"] = after.unwrap_or(Value::Null);
    result["input_bytes"] = json!(replies.received);
    result["elapsed_ms"] = json!(start.elapsed().as_millis() as u64);
    result["observations"] = replies.report();
    if CANCELLED.load(Ordering::Acquire) {
        result["status"] = json!("aborted");
        result["reason"] = json!("interrupted");
    }
    Ok(result)
}
fn destination(path: &Path) -> Result<File, ()> {
    use festerm_windows_security::*;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(())?;
    let name = path.file_name().ok_or(())?;
    let directory = lock_directory_path_without_delete_sharing(parent).map_err(|_| ())?;
    if !is_current_user_only(&directory).map_err(|_| ())? {
        return Err(());
    }
    create_current_user_only_file_exclusive(&directory, Path::new(name)).map_err(|_| ())
}
fn refusal_record(Refused(reason, count): Refused) -> Value {
    json!({"status":"aborted","reason":reason,"unknown_bytes":count,
        "unknown_count_unit": if matches!(reason,"pending_input"|"unexpected_console_event") {"console_records"} else {"bytes"}})
}
pub(super) fn main() -> i32 {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 || args[0] != "--case" || args[2] != "--output" {
        return 2;
    }
    let Some(case) = args[1].to_str().filter(|s| case_valid(s)) else {
        return 2;
    };
    let Ok(mut file) = destination(Path::new(&args[3])) else {
        return 2;
    };
    let result = match observe() {
        Ok(value) => value,
        Err(error) => refusal_record(error),
    };
    let success = qualified(&result);
    let mut record = json!({"schema":1,"case":case,"platform":"Windows","reply_scope":"application_visible_console","environment":{}});
    for name in ENVIRONMENT {
        // Distinguish unset from non-Unicode/invalid values without printing them.
        let value = match std::env::var_os(name) {
            None => environment_value(None),
            Some(value) => match value.into_string() {
                Ok(value) => environment_value(Some(value)),
                Err(_) => json!({"set":true,"omitted":"invalid_or_overlength"}),
            },
        };
        record["environment"][name] = value;
    }
    record
        .as_object_mut()
        .unwrap()
        .extend(result.as_object().unwrap().clone());
    if serde_json::to_writer(&mut file, &record).is_err() || file.write_all(b"\n").is_err() {
        return 1;
    }
    if success {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_records_refuse_unrelated_events_and_bound_repeats_without_leaking_codes() {
        let mut record = INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            ..Default::default()
        };
        record.Event.KeyEvent = KEY_EVENT_RECORD {
            bKeyDown: 1,
            wRepeatCount: 1,
            uChar: KEY_EVENT_RECORD_0 { UnicodeChar: 27 },
            ..Default::default()
        };
        assert_eq!(reply_record(&record), Ok(vec![27]));
        record.Event.KeyEvent.wRepeatCount = u16::MAX;
        assert_eq!(
            reply_record(&record),
            Err(Refused("input_budget", usize::from(u16::MAX)))
        );
        record.Event.KeyEvent.wRepeatCount = 1;
        record.Event.KeyEvent.bKeyDown = 0;
        assert_eq!(
            reply_record(&record),
            Err(Refused("unexpected_console_event", 1))
        );
        record.Event.KeyEvent.bKeyDown = 1;
        record.Event.KeyEvent.uChar.UnicodeChar = 0x00e9;
        assert_eq!(
            reply_record(&record),
            Err(Refused("unexpected_console_event", 1))
        );
        record.EventType = MOUSE_EVENT as u16;
        assert_eq!(
            reply_record(&record),
            Err(Refused("unexpected_console_event", 1))
        );
    }
    #[test]
    fn private_result_file_is_current_user_only_and_never_replaces_existing() {
        use festerm_windows_security::{
            create_current_user_only_directory, is_current_user_only,
            lock_directory_path_without_delete_sharing,
        };
        let temporary = tempfile::tempdir().unwrap();
        let parent = lock_directory_path_without_delete_sharing(temporary.path()).unwrap();
        // The retained parent is read-only, so it cannot mutate its DACL.
        // Create a private child through the existing capability-based helper.
        let private = create_current_user_only_directory(&parent, Path::new("private")).unwrap();
        assert!(is_current_user_only(&private).unwrap());
        // The creation handle has DELETE access; retire it before destination
        // pins the same directory without delete sharing.
        drop(private);
        let path = temporary.path().join("private").join("result.json");
        let mut file = destination(&path).unwrap();
        assert!(is_current_user_only(&file).unwrap());
        file.write_all(b"retained").unwrap();
        drop(file);
        assert!(destination(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"retained");
    }
    #[test]
    fn redirected_handles_cannot_be_admitted_as_console() {
        assert_eq!(
            mode(std::ptr::null_mut()),
            Err(Refused("console_required", 0))
        );
    }
}
