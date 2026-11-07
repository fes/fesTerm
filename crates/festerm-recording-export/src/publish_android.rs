use std::fs::File;

use festerm_android_platform::{
    AndroidRecordingDestination, RecordingDestinationError, RecordingDocument,
};

use crate::{ProviderEntry, RecordingProvider};

/// A persistently authorized Android Files destination.
pub struct AndroidRecordingProvider(AndroidRecordingDestination);

impl AndroidRecordingProvider {
    pub fn new(tree_uri: &str) -> Result<Self, RecordingDestinationError> {
        AndroidRecordingDestination::new(tree_uri).map(Self)
    }
}

impl RecordingProvider for AndroidRecordingProvider {
    type Error = RecordingDestinationError;
    type Writer = File;
    type Reader = File;

    fn create_directory(&mut self, name: &str) -> Result<ProviderEntry, Self::Error> {
        self.0.create_directory(name).map(provider_entry)
    }

    fn create_video(
        &mut self,
        directory: &ProviderEntry,
        name: &str,
    ) -> Result<ProviderEntry, Self::Error> {
        self.0.create_video(&directory.id, name).map(provider_entry)
    }

    fn create_appearance(
        &mut self,
        directory: &ProviderEntry,
        name: &str,
    ) -> Result<ProviderEntry, Self::Error> {
        self.0
            .create_appearance(&directory.id, name)
            .map(provider_entry)
    }

    fn open_for_write(&mut self, video: &ProviderEntry) -> Result<Self::Writer, Self::Error> {
        self.0.open_for_write(&video.id)
    }

    fn open_for_read(&mut self, video: &ProviderEntry) -> Result<Self::Reader, Self::Error> {
        self.0.open_for_read(&video.id)
    }
}

fn provider_entry(document: RecordingDocument) -> ProviderEntry {
    ProviderEntry {
        id: document.uri,
        display_name: document.display_name,
    }
}
