//! Private metadata retention for a new, explicitly approved Desktop Enable.
//! Retained references remain unknown native items, never absence or deletion.
use super::{reference, refused, write, Record};
use crate::lifecycle::Origin;
use festerm_sessiond::private_storage as storage;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs::File, io, path::Path};

pub(super) const NAME: &str = "host-cloud-initial-authorization-history.json";
pub(super) const MAX_ENTRIES: usize = 8;
const MAX_RECORD_BYTES: usize = 16 * 1024;
pub(super) const MAX_BYTES: usize = 128 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct History {
    version: u16,
    // Preserve the exact original journal bytes, not a reconstructed record.
    records: Vec<String>,
}

pub(super) fn initial(record: &Record) -> bool {
    record.origin == Origin::Desktop
        && !record.authenticated
        && !record.submitted
        && record.resource.is_none()
        && record.creation_name.is_none()
        && record.receipt.is_none()
}

fn invalid() -> io::Error {
    refused("host.cloud_initial_authorization_history_requires_reconciliation")
}

fn same_identity(a: &Record, b: &Record) -> bool {
    a.version == 1
        && b.version == 1
        && a.installation == b.installation
        && a.origin == b.origin
        && a.os_user == b.os_user
        && a.account_id != 0
        && a.account_id == b.account_id
        && a.host_id == b.host_id
        && a.fingerprint == b.fingerprint
}

pub(super) fn preserve_then_replace(
    directory: &File,
    previous: &Record,
    next: &Record,
) -> io::Result<()> {
    preserve_then_replace_using(directory, previous, next, publish_archive, write)
}

pub(super) fn publish_archive(directory: &File, bytes: &[u8], existed: bool) -> io::Result<()> {
    if existed {
        storage::replace_private(directory, Path::new(NAME), bytes)
    } else {
        crate::protected_state::publish_new(directory, Path::new(NAME), bytes)
    }
}

// The narrow storage seam also exercises errors after actual publication. It
// cannot confer native authority; only foreground() returns a live handle.
pub(super) fn preserve_then_replace_using(
    directory: &File,
    previous: &Record,
    next: &Record,
    archive: impl FnOnce(&File, &[u8], bool) -> io::Result<()>,
    current: impl FnOnce(&File, &Record) -> io::Result<()>,
) -> io::Result<()> {
    if !initial(previous)
        || !initial(next)
        || !same_identity(previous, next)
        || previous.ticket.epoch() == 0
        || previous.ticket.epoch() >= next.ticket.epoch()
        || previous.credential == next.credential
    {
        return Err(invalid());
    }
    previous.ticket.approved_settings().validate()?;
    next.ticket.approved_settings().validate()?;
    reference(previous)?;
    reference(next)?;
    let original = storage::read_private(directory, Path::new(super::RECORD), MAX_RECORD_BYTES)?;
    let parsed: Record = serde_json::from_slice(&original).map_err(|_| invalid())?;
    if &parsed != previous {
        return Err(invalid());
    }
    let (mut history, existed) = match storage::read_private(directory, Path::new(NAME), MAX_BYTES)
    {
        Ok(bytes) => (
            serde_json::from_slice::<History>(&bytes).map_err(|_| invalid())?,
            true,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (
            History {
                version: 1,
                records: Vec::new(),
            },
            false,
        ),
        Err(error) => return Err(error),
    };
    if history.version != 1
        || history.records.len() >= MAX_ENTRIES
        || (existed && history.records.is_empty())
    {
        return Err(invalid());
    }
    let mut last_epoch = 0;
    let mut references = BTreeSet::new();
    for bytes in &history.records {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(invalid());
        }
        let old: Record = serde_json::from_str(bytes).map_err(|_| invalid())?;
        if !initial(&old)
            || !same_identity(&old, previous)
            || old.ticket.epoch() <= last_epoch
            || old.ticket.epoch() >= previous.ticket.epoch()
            || !references.insert(old.credential.clone())
        {
            return Err(invalid());
        }
        old.ticket.approved_settings().validate()?;
        reference(&old)?;
        last_epoch = old.ticket.epoch();
    }
    if !references.insert(previous.credential.clone()) || references.contains(&next.credential) {
        return Err(invalid());
    }
    history
        .records
        .push(String::from_utf8(original.to_vec()).map_err(|_| invalid())?);
    let bytes = serde_json::to_vec(&history).map_err(|_| invalid())?;
    if bytes.len() > MAX_BYTES {
        return Err(invalid());
    }
    // Use existing durable private-publication primitives (including directory
    // synchronization on Unix), never a new storage or recovery mechanism.
    // Failure/uncertain durability never yields a fresh in-memory grant. An
    // archive-only crash is a collision on the next attempt, not a replay.
    archive(directory, &bytes, existed)?;
    current(directory, next)
}
