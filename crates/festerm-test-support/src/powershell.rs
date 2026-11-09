//! Controlled out-of-process PSRP startup fixture.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use psrp_rs::fragment::{encode_message, Reassembler};
use psrp_rs::message::{Destination, MessageType, PsrpMessage};
use psrp_rs::{PipelineState, PsObject, PsValue, RunspacePoolState};

#[derive(Clone, Copy, Debug)]
pub enum HeldStartupAck {
    Command,
    PipelineData,
    Neither,
}

#[derive(Default)]
struct StartupState {
    inbound: VecDeque<u8>,
    writes: Vec<String>,
    command_id: Option<String>,
    pipeline: Option<PsrpMessage>,
    pipeline_data_seen: bool,
    dropped: bool,
}

pub struct StalledStartupPeer {
    state: Arc<(Mutex<StartupState>, Condvar)>,
    held: HeldStartupAck,
}

pub struct StalledStartupStream {
    state: Arc<(Mutex<StartupState>, Condvar)>,
    held: HeldStartupAck,
}

impl StalledStartupPeer {
    pub fn pair(held: HeldStartupAck) -> (Self, StalledStartupStream) {
        let state = Arc::new((Mutex::new(StartupState::default()), Condvar::new()));
        (
            Self {
                state: Arc::clone(&state),
                held,
            },
            StalledStartupStream { state, held },
        )
    }

    pub fn wait_until_stalled(&self, timeout: Duration) -> bool {
        let (lock, changed) = &*self.state;
        let state = lock.lock().unwrap();
        let (state, _) = changed
            .wait_timeout_while(state, timeout, |state| match self.held {
                HeldStartupAck::Command => state.command_id.is_none(),
                HeldStartupAck::PipelineData | HeldStartupAck::Neither => !state.pipeline_data_seen,
            })
            .unwrap();
        match self.held {
            HeldStartupAck::Command => state.command_id.is_some(),
            HeldStartupAck::PipelineData | HeldStartupAck::Neither => state.pipeline_data_seen,
        }
    }

    pub fn stream_disposed(&self) -> bool {
        self.state.0.lock().unwrap().dropped
    }

    pub fn command_seen(&self) -> bool {
        self.state.0.lock().unwrap().command_id.is_some()
    }

    pub fn write_count(&self) -> usize {
        self.state.0.lock().unwrap().writes.len()
    }
}

impl Read for StalledStartupStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let (lock, changed) = &*self.state;
        let state = lock.lock().unwrap();
        let (mut state, _) = changed
            .wait_timeout_while(state, Duration::from_millis(20), |state| {
                state.inbound.is_empty()
            })
            .unwrap();
        if state.inbound.is_empty() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = buffer.len().min(state.inbound.len());
        for byte in &mut buffer[..count] {
            *byte = state.inbound.pop_front().unwrap();
        }
        Ok(count)
    }
}

impl Write for StalledStartupStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let frame = std::str::from_utf8(buffer).unwrap();
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.writes.push(frame.to_owned());
        if frame.starts_with("<Data ") && state.command_id.is_none() {
            let payload = frame
                .split_once('>')
                .unwrap()
                .1
                .split_once("</Data>")
                .unwrap()
                .0;
            let bytes = psrp_rs::internal::base64::decode(payload).unwrap();
            let messages = Reassembler::new().feed(&bytes).unwrap();
            let rpid = PsrpMessage::decode(&messages[0]).unwrap().rpid;
            let opened = PsrpMessage {
                destination: Destination::Client,
                message_type: MessageType::RunspacePoolState,
                rpid,
                pid: Default::default(),
                data: psrp_rs::to_clixml(&PsValue::Object(PsObject::new().with(
                    "RunspaceState",
                    PsValue::I32(RunspacePoolState::Opened as i32),
                ))),
            };
            let data = psrp_rs::internal::base64::encode(&encode_message(1, &opened.encode()));
            let response = format!(
                "<DataAck PSGuid='00000000-0000-0000-0000-000000000000' />\n\
                 <Data PSGuid='00000000-0000-0000-0000-000000000000'>{data}</Data>\n"
            );
            state.inbound.extend(response.bytes());
        } else if frame.starts_with("<Command ") {
            let id = frame
                .split_once("PSGuid='")
                .unwrap()
                .1
                .split_once('\'')
                .unwrap()
                .0
                .to_owned();
            state.command_id = Some(id.clone());
            if !matches!(self.held, HeldStartupAck::Command) {
                state
                    .inbound
                    .extend(format!("<CommandAck PSGuid='{id}' />\n").bytes());
            }
        } else if frame.starts_with("<Data ") {
            let id = frame
                .split_once("PSGuid='")
                .unwrap()
                .1
                .split_once('\'')
                .unwrap()
                .0;
            if id != "00000000-0000-0000-0000-000000000000" {
                state.pipeline_data_seen = true;
                if matches!(self.held, HeldStartupAck::Neither) {
                    let payload = frame
                        .split_once('>')
                        .unwrap()
                        .1
                        .split_once("</Data>")
                        .unwrap()
                        .0;
                    let bytes = psrp_rs::internal::base64::decode(payload).unwrap();
                    let messages = Reassembler::new().feed(&bytes).unwrap();
                    state.pipeline = Some(PsrpMessage::decode(&messages[0]).unwrap());
                }
            }
            if matches!(self.held, HeldStartupAck::Neither) {
                let ack = format!("<DataAck PSGuid='{id}' />\n");
                state.inbound.extend(ack.bytes());
            }
        } else if frame.starts_with("<Signal ") && matches!(self.held, HeldStartupAck::Neither) {
            let pipeline = state.pipeline.as_ref().unwrap();
            let stopped = PsrpMessage {
                destination: Destination::Client,
                message_type: MessageType::PipelineState,
                rpid: pipeline.rpid,
                pid: pipeline.pid,
                data: psrp_rs::to_clixml(&PsValue::Object(
                    PsObject::new()
                        .with("PipelineState", PsValue::I32(PipelineState::Stopped as i32)),
                )),
            };
            let data = psrp_rs::internal::base64::encode(&encode_message(2, &stopped.encode()));
            let id = state.command_id.as_ref().unwrap();
            let response =
                format!("<SignalAck PSGuid='{id}' />\n<Data PSGuid='{id}'>{data}</Data>\n");
            state.inbound.extend(response.bytes());
        } else if frame.starts_with("<Close ") && matches!(self.held, HeldStartupAck::Neither) {
            let id = frame
                .split_once("PSGuid='")
                .unwrap()
                .1
                .split_once('\'')
                .unwrap()
                .0;
            state
                .inbound
                .extend(format!("<CloseAck PSGuid='{id}' />\n").bytes());
        }
        changed.notify_all();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for StalledStartupStream {
    fn drop(&mut self) {
        let (lock, changed) = &*self.state;
        lock.lock().unwrap().dropped = true;
        changed.notify_all();
    }
}
