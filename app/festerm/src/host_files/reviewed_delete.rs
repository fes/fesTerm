//! One bounded worker-owned review. The UI receives metadata and a batch ID,
//! never the protocol handles that authorize the exact selected objects.
use super::*;
use std::sync::{Arc, Mutex};

pub(super) struct PreparedDelete {
    id: u64,
    stop: CancellationToken,
    entries: Vec<(Handle, String)>,
}

#[derive(Clone, Default)]
pub(crate) struct DeleteObservation(Arc<Mutex<Option<DeleteProgress>>>);
struct DeleteProgress {
    path: String,
    confirmed: usize,
    total: usize,
}
impl DeleteObservation {
    pub(crate) fn interrupted(&self) -> Option<SftpSessionError> {
        self.0.lock().expect("Files delete observation lock").take().map(|value| {
            SftpSessionError::PublicationUncertain {
                path: value.path,
                reason: format!("The deletion result was not observed. {} of {} removals were confirmed; inspect the current target and refresh before reviewing untouched entries. Do not retry the unobserved deletion automatically.", value.confirmed, value.total),
            }
        })
    }
}

#[derive(Debug)]
pub(crate) struct DeleteOutcome {
    pub(crate) confirmed: usize,
    pub(crate) total: usize,
    /// A returned known refusal leaves this false. A missing mutation result
    /// or RecoveryRequired is explicitly unknown, never offered as retryable.
    pub(crate) failure: Option<(bool, String)>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> FilesEndpoint<S> {
    pub(crate) fn delete_review_stop(&self) -> Option<CancellationToken> {
        self.delete_review
            .as_ref()
            .map(|review| review.stop.clone())
    }
    pub(crate) async fn cancel_delete_review(&mut self) -> Result<(), SftpSessionError> {
        let Some(review) = self.delete_review.take() else {
            return Ok(());
        };
        self.close_delete_entries(review.entries).await
    }
    async fn close_delete_entries(
        &mut self,
        entries: Vec<(Handle, String)>,
    ) -> Result<(), SftpSessionError> {
        let mut failure = None;
        for (handle, path) in entries {
            if let Err(error) = self.client.close(&handle).await {
                failure = Some(Self::error(&path, "close deletion review", error));
            }
        }
        if let Some(error) = failure {
            self.client.cancel();
            Err(error)
        } else {
            Ok(())
        }
    }
    pub(crate) async fn prepare_delete(
        &mut self,
        paths: &[String],
        stop: CancellationToken,
    ) -> Result<(u64, Vec<SftpDirectoryItem>), SftpSessionError> {
        self.check_current()?;
        if self.delete_review.is_some()
            || !self.client.delete_advertised()
            || paths.is_empty()
            || paths.len() > 8
            || paths
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != paths.len()
        {
            return Err(Self::error(
                "/",
                "review deletion",
                file_client::Error::Invalid,
            ));
        }
        let id = self.next_id()?;
        let mut entries = Vec::with_capacity(paths.len());
        let mut items = Vec::with_capacity(paths.len());
        let prepared = async {
            for path in paths {
                self.check_current()?;
                if stop.is_cancelled() {
                    return Err(SftpSessionError::SessionClosed);
                }
                // Await the complete exchange before honoring a review cancellation;
                // dropping this exchange would strand an unobserved remote handle.
                let (handle, metadata) = self
                    .client
                    .review_entry(path)
                    .await
                    .map_err(|error| Self::error(path, "review deletion", error))?;
                entries.push((handle, path.clone()));
                let metadata = self.metadata_value(path, metadata)?;
                let name = metadata.path.file_name()?;
                items.push(SftpDirectoryItem {
                    name,
                    path: metadata.path,
                    file_type: metadata.file_type,
                    size: metadata.size,
                    modified_at: metadata.modified_at,
                    permissions: metadata.permissions,
                });
            }
            self.check_current()?;
            if stop.is_cancelled() {
                return Err(SftpSessionError::SessionClosed);
            }
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            // Known Busy/refusal closes partial reviews while preserving this
            // channel and the independent transfer stream if cleanup succeeds.
            self.close_delete_entries(entries).await?;
            return Err(error);
        }
        self.delete_review = Some(PreparedDelete { id, stop, entries });
        Ok((id, items))
    }
    pub(crate) async fn commit_delete(
        &mut self,
        id: u64,
        observation: &DeleteObservation,
    ) -> DeleteOutcome {
        if self
            .delete_review
            .as_ref()
            .is_none_or(|review| review.id != id)
        {
            return DeleteOutcome {
                confirmed: 0,
                total: 0,
                failure: Some((
                    false,
                    "The deletion review expired. Refresh and review again.".into(),
                )),
            };
        }
        let review = self.delete_review.take().expect("matched deletion review");
        let mut outcome = DeleteOutcome {
            confirmed: 0,
            total: review.entries.len(),
            failure: None,
        };
        let mut entries = review.entries.into_iter();
        while let Some((handle, path)) = entries.next() {
            if review.stop.is_cancelled() || self.check_current().is_err() {
                let mut remaining = vec![(handle, path)];
                remaining.extend(entries);
                let _ = self.close_delete_entries(remaining).await;
                outcome.failure = Some((
                    false,
                    "Deletion stopped before the next target was sent.".into(),
                ));
                return outcome;
            }
            *observation.0.lock().expect("Files delete observation lock") = Some(DeleteProgress {
                path: path.clone(),
                confirmed: outcome.confirmed,
                total: outcome.total,
            });
            let result = self.client.delete_reviewed(&handle).await;
            // Every returned result clears the owner-drop marker, including
            // known refusals and cancellation. Only a dropped future leaves it.
            observation
                .0
                .lock()
                .expect("Files delete observation lock")
                .take();
            match result {
                Ok(()) => outcome.confirmed += 1,
                Err(error) => {
                    let uncertain = matches!(error, file_client::Error::UncertainDelete(_));
                    let reason = match error {
                        file_client::Error::UncertainDelete(cause) => {
                            Self::error(&path, "delete", *cause).to_string()
                        }
                        other => Self::error(&path, "delete", other).to_string(),
                    };
                    outcome.failure = Some((uncertain, reason));
                    let _ = self.close_delete_entries(entries.collect()).await;
                    break;
                }
            }
        }
        outcome
    }
}
