use std::{
    collections::BTreeMap,
    sync::{mpsc, Arc},
    thread::JoinHandle,
};

use eframe::egui;
use festerm_document::{DocumentId, DocumentOrigin};
use festerm_markdown::{
    LocalMarkdownSource, MarkdownCancellation, MarkdownDocument, MarkdownLoader,
};
use festerm_mobile_docs::OpenDocument;

type Projection = Result<Arc<MarkdownDocument>, String>;

struct Pending {
    document: DocumentId,
    revision: u64,
    cancel: MarkdownCancellation,
    receiver: mpsc::Receiver<Projection>,
    worker: JoinHandle<()>,
}

/// One readonly parser worker for this host, including cancelled work.
#[derive(Default)]
pub(super) struct PreviewLoader {
    pending: Option<Pending>,
    cache: BTreeMap<DocumentId, (u64, Projection)>,
}

impl PreviewLoader {
    pub fn forget(&mut self, document: DocumentId) {
        self.cache.remove(&document);
        if let Some(pending) = &self.pending {
            if pending.document == document {
                pending.cancel.cancel();
            }
        }
    }

    pub fn document(
        &mut self,
        document: DocumentId,
        entry: &OpenDocument,
        ctx: &egui::Context,
    ) -> Result<Option<Arc<MarkdownDocument>>, String> {
        let revision = entry.text().revision();
        if let Some(pending) = &self.pending {
            if pending.document != document || pending.revision != revision {
                pending.cancel.cancel();
            }
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.worker.is_finished())
        {
            let pending = self.pending.take().expect("finished preview worker");
            let result = if pending.worker.join().is_err() {
                Err("Markdown preview stopped unexpectedly. Return to Edit and retry.".to_owned())
            } else {
                pending.receiver.try_recv().unwrap_or_else(|_| {
                    Err(
                        "Markdown preview ended without a result. Return to Edit and retry."
                            .to_owned(),
                    )
                })
            };
            if pending.document == document
                && pending.revision == revision
                && !pending.cancel.is_cancelled()
            {
                self.cache.insert(document, (revision, result));
            }
        }
        if let Some((cached_revision, result)) = self.cache.get(&document) {
            if *cached_revision == revision {
                return result.clone().map(Some);
            }
        }
        if self.pending.is_some() {
            return Ok(None);
        }
        let source = match entry.origin() {
            DocumentOrigin::Local(local) => {
                LocalMarkdownSource::new(local.path()).map_err(|error| error.to_string())?
            }
            _ => return Err("This origin has no authorized local preview adapter.".to_owned()),
        };
        let bytes = entry.text().text().as_bytes().to_vec();
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancel = MarkdownCancellation::new();
        let cancellation = cancel.clone();
        let context = ctx.clone();
        let worker = std::thread::Builder::new()
            .name("mobile-document-preview".to_owned())
            .spawn(move || {
                let result = MarkdownLoader::default()
                    .load(source.into(), bytes.len(), &bytes, &cancellation)
                    .map(Arc::new)
                    .map_err(|error| format!("Could not render Markdown: {error}"));
                // Closing the host intentionally drops the result receiver.
                let _ = sender.send(result);
                context.request_repaint();
            })
            .map_err(|error| format!("Could not start Markdown preview: {error}"));
        match worker {
            Ok(worker) => {
                self.pending = Some(Pending {
                    document,
                    revision,
                    cancel,
                    receiver,
                    worker,
                });
                Ok(None)
            }
            Err(error) => {
                self.cache.insert(document, (revision, Err(error.clone())));
                Err(error)
            }
        }
    }
}

impl Drop for PreviewLoader {
    fn drop(&mut self) {
        if let Some(pending) = &self.pending {
            pending.cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_document::LocalOrigin;
    use festerm_mobile_docs::{Documents, SourceVersion};
    use std::time::{Duration, Instant};

    fn ready(
        loader: &mut PreviewLoader,
        id: DocumentId,
        registry: &Documents,
    ) -> Arc<MarkdownDocument> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(document) = loader
                .document(id, registry.get(id).unwrap(), &egui::Context::default())
                .unwrap()
            {
                return document;
            }
            assert!(
                Instant::now() < deadline,
                "bounded preview worker must finish"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn mobile_preview_keeps_one_worker_and_rejects_cancelled_old_revisions() {
        let mut registry = Documents::default();
        let view = registry
            .open(
                LocalOrigin::new("/authorized/notes.md").unwrap().into(),
                b"# Initial\n",
                Some(SourceVersion([1; 32])),
                true,
            )
            .unwrap();
        let id = registry.document_for(view).unwrap();
        let revision = registry.view(view).unwrap().text().revision();
        let (release, gate) = mpsc::sync_channel(0);
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            gate.recv().unwrap();
            let _ = sender.send(Err("cancelled fixture result".to_owned()));
        });
        let worker_id = worker.thread().id();
        let mut loader = PreviewLoader {
            pending: Some(Pending {
                document: id,
                revision,
                cancel: MarkdownCancellation::new(),
                receiver,
                worker,
            }),
            cache: BTreeMap::new(),
        };
        let target = registry.target(view, 0..0).unwrap();
        registry.sync(&target, "# Current\n").unwrap();
        assert!(loader
            .document(id, registry.get(id).unwrap(), &egui::Context::default())
            .unwrap()
            .is_none());
        let pending = loader.pending.as_ref().unwrap();
        assert_eq!(pending.worker.thread().id(), worker_id);
        assert!(pending.cancel.is_cancelled());
        release.send(()).unwrap();
        let parsed = ready(&mut loader, id, &registry);
        assert_eq!(parsed.source_text(), "# Current\n");
        let same = ready(&mut loader, id, &registry);
        assert!(Arc::ptr_eq(&parsed, &same));
        assert!(loader.pending.is_none());
        assert_eq!(registry.view(view).unwrap().text().text(), "# Current\n");
        assert!(registry.undo(view).unwrap());
        assert_eq!(registry.view(view).unwrap().text().text(), "# Initial\n");
        loader.forget(id);
        assert!(loader.cache.is_empty());
    }

    #[test]
    fn mobile_preview_releases_closed_document_cache_without_adopting_late_results() {
        let mut registry = Documents::default();
        let view = registry
            .open(
                LocalOrigin::new("/authorized/notes.md").unwrap().into(),
                b"# Notes\n",
                Some(SourceVersion([1; 32])),
                true,
            )
            .unwrap();
        let id = registry.document_for(view).unwrap();
        let mut loader = PreviewLoader::default();
        let parsed = ready(&mut loader, id, &registry);
        let revision = registry.get(id).unwrap().text().revision();
        let (release, gate) = mpsc::sync_channel(0);
        let (sender, receiver) = mpsc::sync_channel(1);
        loader.pending = Some(Pending {
            document: id,
            revision,
            cancel: MarkdownCancellation::new(),
            receiver,
            worker: std::thread::spawn(move || {
                gate.recv().unwrap();
                let _ = sender.send(Ok(parsed));
            }),
        });
        loader.forget(id);
        assert!(loader.pending.as_ref().unwrap().cancel.is_cancelled());
        registry.close(view, false).unwrap();
        assert!(loader.cache.is_empty());
        assert!(registry.get(id).is_err());
        let next = registry
            .open(
                LocalOrigin::new("/authorized/next.md").unwrap().into(),
                b"# Next\n",
                Some(SourceVersion([2; 32])),
                true,
            )
            .unwrap();
        release.send(()).unwrap();
        let next_id = registry.document_for(next).unwrap();
        assert_eq!(
            ready(&mut loader, next_id, &registry).source_text(),
            "# Next\n"
        );
        assert!(!loader.cache.contains_key(&id));
        assert_eq!(loader.cache.len(), 1);
    }

    #[test]
    fn mobile_preview_failed_projection_retries_without_mutating_source() {
        let mut registry = Documents::default();
        let view = registry
            .open(
                LocalOrigin::new("/authorized/notes.md").unwrap().into(),
                b"# Notes\n",
                Some(SourceVersion([1; 32])),
                true,
            )
            .unwrap();
        let id = registry.document_for(view).unwrap();
        let revision = registry.get(id).unwrap().text().revision();
        let mut loader = PreviewLoader::default();
        loader
            .cache
            .insert(id, (revision, Err("fixture parser failure".to_owned())));
        assert_eq!(
            loader
                .document(id, registry.get(id).unwrap(), &egui::Context::default())
                .unwrap_err(),
            "fixture parser failure"
        );
        loader.forget(id);
        assert_eq!(ready(&mut loader, id, &registry).source_text(), "# Notes\n");
        assert!(!registry.view(view).unwrap().dirty());
        assert!(!registry.undo(view).unwrap());
    }
}
