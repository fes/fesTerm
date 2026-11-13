use festerm_recording::ExportDestination;
use festerm_recording_export::{
    ExportBackend, ExportPublication, PublicationRequest, StagedRecording,
};

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod headless {
    use super::*;
    pub(crate) struct RefusingEncoder;
    impl festerm_recording_export::VideoEncoder for RefusingEncoder {
        type Error = &'static str;
        fn append(&mut self, _: &[u8]) -> Result<(), Self::Error> {
            Err("Native mobile encoding is unavailable in the headless gallery")
        }
        fn finish(&mut self) -> Result<(), Self::Error> {
            Err("Native mobile encoding is unavailable in the headless gallery")
        }
    }
    pub(crate) struct RefusingFactory;
    impl festerm_recording_export::EncoderFactory for RefusingFactory {
        type Encoder = RefusingEncoder;
        type Error = &'static str;
        fn create(
            &mut self,
            _: &std::path::Path,
            _: festerm_ui_egui::RecordingFrameSize,
        ) -> Result<Self::Encoder, Self::Error> {
            Err("Native mobile encoding is unavailable in the headless gallery")
        }
    }
    pub(crate) struct Publication;
    pub(crate) fn encoder_factory() -> RefusingFactory {
        RefusingFactory
    }
    pub(crate) fn preflight(_: &ExportDestination) -> Result<(), String> {
        Err("Native mobile publication is unavailable in the headless gallery".to_owned())
    }
    pub(crate) fn publish(_: PublicationRequest<'_>) -> Result<Publication, String> {
        Err("Native mobile publication is unavailable in the headless gallery".to_owned())
    }
}
#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
use headless::publish;
#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
pub(super) use headless::{encoder_factory, preflight, Publication};

pub(super) struct MobileBackend;

impl ExportBackend for MobileBackend {
    type Publication = Publication;

    fn preflight(&self, destination: &ExportDestination) -> Result<(), String> {
        preflight(destination)
    }

    fn publish(&self, request: PublicationRequest<'_>) -> Result<Self::Publication, String> {
        publish(request)
    }
}

#[cfg(target_os = "android")]
pub(super) fn encoder_factory() -> festerm_recording_export::AndroidEncoderFactory {
    festerm_recording_export::AndroidEncoderFactory
}

#[cfg(target_os = "ios")]
pub(super) fn encoder_factory() -> festerm_recording_export::AppleEncoderFactory {
    festerm_recording_export::AppleEncoderFactory
}

#[cfg(target_os = "android")]
pub(super) fn preflight(destination: &ExportDestination) -> Result<(), String> {
    let ExportDestination::AndroidTree(uri) = destination else {
        return Err("Choose an Android Files folder for recording exports".to_owned());
    };
    festerm_recording_export::AndroidRecordingProvider::new(uri)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "ios")]
pub(super) fn preflight(destination: &ExportDestination) -> Result<(), String> {
    let ExportDestination::IosBookmark { bookmark, .. } = destination else {
        return Err("Choose an iOS Files folder for recording exports".to_owned());
    };
    festerm_ios_window::with_recording_folder(bookmark, |_| Ok(()))
}

#[cfg(target_os = "android")]
pub(super) struct Publication {
    published: festerm_recording_export::ProviderPublication,
    journal: festerm_recording_export::ProviderJournal,
}

#[cfg(target_os = "ios")]
pub(super) struct Publication(festerm_recording_export::PublishedRecording);

#[cfg(target_os = "android")]
fn publish(request: PublicationRequest<'_>) -> Result<Publication, String> {
    use festerm_recording_export::{
        publish_to_provider, AndroidRecordingProvider, ProviderJournal,
    };

    let ExportDestination::AndroidTree(uri) = request.destination else {
        return Err("Choose an Android Files folder for recording exports".to_owned());
    };
    let mut provider = AndroidRecordingProvider::new(uri).map_err(|error| error.to_string())?;
    let mut journal = ProviderJournal::begin(
        request.root,
        request.staged,
        uri,
        request.templates,
        request.start,
    )
    .map_err(|error| format!("Could not save export recovery information: {error}"))?;
    let published = publish_to_provider(
        request.staged,
        &mut provider,
        request.templates,
        request.id,
        request.start,
        request.cancelled,
        |directory| journal.remember_directory(directory),
    )
    .map_err(|error| error.to_string())?;
    journal.mark_published(&published).map_err(|error| {
        format!("Videos were published but recovery information could not be saved: {error}")
    })?;
    Ok(Publication { published, journal })
}

#[cfg(target_os = "ios")]
fn publish(request: PublicationRequest<'_>) -> Result<Publication, String> {
    let ExportDestination::IosBookmark { bookmark, .. } = request.destination else {
        return Err("Choose an iOS Files folder for recording exports".to_owned());
    };
    festerm_ios_window::with_recording_folder(bookmark, |directory| {
        festerm_recording_export::publish_to_directory(
            request.staged,
            directory,
            request.templates,
            request.id,
            request.start,
            request.cancelled,
        )
        .map(Publication)
        .map_err(|error| error.to_string())
    })
}

impl ExportPublication for Publication {
    #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
    fn names(&self) -> (String, Vec<String>) {
        unreachable!("headless publication always refuses")
    }
    #[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
    fn finish(self) -> Result<(), String> {
        Err("Native mobile publication is unavailable in the headless gallery".to_owned())
    }
    #[cfg(target_os = "android")]
    fn names(&self) -> (String, Vec<String>) {
        (
            self.published.directory.display_name.clone(),
            self.published
                .videos
                .iter()
                .map(|video| video.display_name.clone())
                .collect(),
        )
    }

    #[cfg(target_os = "ios")]
    fn names(&self) -> (String, Vec<String>) {
        (self.0.directory_name.clone(), self.0.video_names.clone())
    }

    fn delete_source(&mut self, _staged: &StagedRecording) -> Result<(), String> {
        Err("Mobile recording sources require explicit deletion from the library".to_owned())
    }

    #[cfg(target_os = "android")]
    fn finish(self) -> Result<(), String> {
        self.journal.remove().map_err(|error| {
            format!("Videos were published, but export recovery information could not be cleared completely: {error}")
        })
    }

    #[cfg(target_os = "ios")]
    fn finish(self) -> Result<(), String> {
        Ok(())
    }
}
