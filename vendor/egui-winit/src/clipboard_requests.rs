//! Identified clipboard reads, separate from unowned widget Paste events.
//! One request/response per viewport; superseded or cancelled reads cannot
//! complete a newer request. Payloads are never logged or persisted.
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Mailbox {
    next: u64,
    expected: Option<u64>,
    pending: Option<u64>,
    response: Option<(u64, Option<String>)>,
}

fn mailbox(context: &egui::Context, viewport: egui::ViewportId) -> Arc<Mutex<Mailbox>> {
    context.data_mut(|data| {
        data.get_temp_mut_or_default::<Arc<Mutex<Mailbox>>>(egui::Id::new((
            "identified-clipboard-read",
            viewport,
        )))
        .clone()
    })
}

pub fn request(context: &egui::Context, viewport: egui::ViewportId) -> Option<u64> {
    let shared = mailbox(context, viewport);
    let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
    let token = state.next.checked_add(1)?;
    state.next = token;
    state.expected = Some(token);
    state.pending = Some(token);
    state.response = None;
    drop(state);
    context.request_repaint_of(viewport);
    Some(token)
}

pub fn cancel(context: &egui::Context, viewport: egui::ViewportId, token: u64) {
    let shared = mailbox(context, viewport);
    let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
    if state.expected == Some(token) {
        state.expected = None;
        state.pending = None;
        state.response = None;
    }
}

/// Native adapter producer seam; useful for deterministic fake readers too.
pub fn take_request(context: &egui::Context, viewport: egui::ViewportId) -> Option<u64> {
    mailbox(context, viewport)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .pending
        .take()
}

pub fn complete(
    context: &egui::Context,
    viewport: egui::ViewportId,
    token: u64,
    text: Option<String>,
) {
    let shared = mailbox(context, viewport);
    let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
    if state.expected == Some(token) && state.response.is_none() {
        state.response = Some((token, text));
        state.pending = None;
        drop(state);
        context.request_repaint_of(viewport);
    }
}

pub fn take_response(
    context: &egui::Context,
    viewport: egui::ViewportId,
) -> Option<(u64, Option<String>)> {
    let shared = mailbox(context, viewport);
    let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
    let response = state.response.take()?;
    state.expected = None;
    Some(response)
}
