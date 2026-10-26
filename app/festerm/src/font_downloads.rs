//! Composition-owned, bounded optional-font acquisition. The renderer only sees
//! verified face bytes; saved preferences never become availability state.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    time::{Duration, Instant},
};

use eframe::egui;
use festerm_config::TerminalFontPreference;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_FACE_BYTES: u64 = 32 * 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct FontLicenseNotice {
    pub(crate) family: &'static str,
    pub(crate) version: &'static str,
    pub(crate) notice: &'static str,
    pub(crate) text: &'static str,
}

pub(crate) const FONT_LICENSE_NOTICES: &[FontLicenseNotice] = &[
    FontLicenseNotice {
        family: "JetBrains Mono",
        version: "2.304",
        notice: "SIL Open Font License 1.1",
        text: include_str!("../../../assets/fonts/jetbrains-mono/OFL.txt"),
    },
    FontLicenseNotice {
        family: "JetBrains Mono",
        version: "2.304",
        notice: "Project authors",
        text: include_str!("../../../assets/fonts/jetbrains-mono/AUTHORS.txt"),
    },
    FontLicenseNotice {
        family: "Noto Emoji",
        version: "3.002",
        notice: "SIL Open Font License 1.1",
        text: include_str!("../../../assets/fonts/noto-emoji/OFL.txt"),
    },
    FontLicenseNotice {
        family: "Iosevka Term",
        version: "34.8.1",
        notice: "SIL Open Font License 1.1",
        text: include_str!("../../../assets/fonts/iosevka-term/LICENSE.md"),
    },
    FontLicenseNotice {
        family: "JuliaMono",
        version: "0.63.2",
        notice: "SIL Open Font License 1.1",
        text: include_str!("../../../assets/fonts/julia-mono/LICENSE"),
    },
    FontLicenseNotice {
        family: "Maple Mono",
        version: "7.9",
        notice: "SIL Open Font License 1.1",
        text: include_str!("../../../assets/fonts/maple-mono/LICENSE.txt"),
    },
];

#[derive(Clone, Deserialize)]
struct FontFile {
    path: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Deserialize)]
struct Manifest {
    files: Vec<FontFile>,
}

fn catalog(family: TerminalFontPreference) -> Result<[FontFile; 4], String> {
    let json = match family {
        TerminalFontPreference::JetBrainsMono => return Err("The default font is embedded.".into()),
        TerminalFontPreference::IosevkaTerm => {
            include_str!("../../../assets/fonts/iosevka-term/manifest.json")
        }
        TerminalFontPreference::JuliaMono => {
            include_str!("../../../assets/fonts/julia-mono/manifest.json")
        }
        TerminalFontPreference::MapleMono => {
            include_str!("../../../assets/fonts/maple-mono/manifest.json")
        }
    };
    let manifest: Manifest =
        serde_json::from_str(json).map_err(|_| "The font catalog is invalid.")?;
    let faces: [FontFile; 4] = manifest
        .files
        .into_iter()
        .filter(|file| file.path.ends_with(".ttf"))
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|_| "The font catalog needs four faces.")?;
    if faces.iter().any(|face| {
        face.size_bytes == 0
            || face.size_bytes > MAX_FACE_BYTES
            || face.sha256.len() != 64
            || !face.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Err("The font catalog has invalid size or checksum bounds.".into());
    }
    Ok(faces)
}

pub(crate) fn family_label(family: TerminalFontPreference) -> &'static str {
    match family {
        TerminalFontPreference::JetBrainsMono => "JetBrains Mono",
        TerminalFontPreference::IosevkaTerm => "Iosevka Term",
        TerminalFontPreference::JuliaMono => "JuliaMono",
        TerminalFontPreference::MapleMono => "Maple Mono",
    }
}

fn asset_name(face: &FontFile) -> String {
    format!("festerm-font-{}.ttf", face.sha256)
}

fn asset_url(face: &FontFile) -> String {
    format!(
        "https://github.com/fes/fesTerm/releases/download/v{}/{}",
        env!("CARGO_PKG_VERSION"),
        asset_name(face)
    )
}

fn verify(bytes: &[u8], face: &FontFile) -> bool {
    bytes.len() as u64 == face.size_bytes
        && Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == face.sha256
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Font download cancelled.".into())
    } else {
        Ok(())
    }
}

fn cached(root: &Path, face: &FontFile) -> Result<Option<Vec<u8>>, String> {
    let path = root.join(asset_name(face));
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if metadata.len() != face.size_bytes {
                return Ok(None);
            }
        }
        Ok(_) => return Err("The font cache contains an unsafe file. Remove it and Retry.".into()),
        Err(_) => {
            return Err("The font cache cannot be inspected. Check permissions and Retry.".into())
        }
    }
    let file = File::open(path)
        .map_err(|_| "The cached font cannot be read. Check permissions and Retry.")?;
    let mut bytes = Vec::with_capacity(face.size_bytes as usize);
    file.take(face.size_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The cached font could not be read completely. Retry.")?;
    Ok(verify(&bytes, face).then_some(bytes))
}

fn prepare_store(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|_| "The font store cannot be created. Check permissions and Retry.")?;
    let metadata =
        fs::symlink_metadata(root).map_err(|_| "The font store cannot be inspected. Retry.")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("The font store must be an application-owned directory, not a link.".into());
    }
    Ok(())
}

fn acquire_lock(root: &Path, cancel: &AtomicBool) -> Result<File, String> {
    let path = root.join("download.lock");
    if fs::symlink_metadata(&path)
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("The font download lock is unsafe. Remove it and Retry.".into());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| "The font store cannot be locked. Check permissions and Retry.")?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        cancelled(cancel)?;
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => return Ok(lock),
            Err(error)
                if error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => {
                return Err(
                    "Another font installation is busy or the store cannot be locked. Retry."
                        .into(),
                )
            }
        }
    }
}

fn publish(root: &Path, face: &FontFile, bytes: &[u8]) -> Result<(), String> {
    if !verify(bytes, face) {
        return Err("Font integrity verification failed. Retry.".into());
    }
    let mut staged = tempfile::NamedTempFile::new_in(root).map_err(|_| {
        "A font download cannot be staged. Check free space and permissions, then Retry."
    })?;
    staged
        .write_all(bytes)
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|_| "The font download could not be saved. Check free space, then Retry.")?;
    let path = root.join(asset_name(face));
    if fs::symlink_metadata(&path).is_ok() {
        if cached(root, face)?.is_some() {
            return Ok(());
        }
        fs::remove_file(&path).map_err(|_| "An invalid cached font cannot be replaced. Retry.")?;
    }
    staged
        .persist_noclobber(path)
        .map_err(|_| "The verified font could not be installed. Retry.")?;
    Ok(())
}

fn download(
    client: &reqwest::blocking::Client,
    face: &FontFile,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    context: &egui::Context,
) -> Result<Vec<u8>, String> {
    let mut response = client.get(asset_url(face)).send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| "The optional font could not be downloaded. Check your connection or release availability, then Retry.")?;
    if response
        .content_length()
        .is_some_and(|size| size != face.size_bytes)
    {
        return Err("The font download has an unexpected size. Retry.".into());
    }
    read_download(&mut response, face, cancel, progress, context)
}

fn read_download(
    mut response: impl Read,
    face: &FontFile,
    cancel: &AtomicBool,
    progress: &AtomicU64,
    context: &egui::Context,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(face.size_bytes as usize);
    let mut chunk = [0; 64 * 1024];
    let mut last_wake = Instant::now();
    loop {
        cancelled(cancel)?;
        let count = response
            .read(&mut chunk)
            .map_err(|_| "The font download was interrupted. Check your connection, then Retry.")?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > face.size_bytes as usize {
            return Err("The font download exceeded its size limit. Retry.".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        progress.fetch_add(count as u64, Ordering::Relaxed);
        if last_wake.elapsed() >= Duration::from_millis(100) {
            context.request_repaint();
            last_wake = Instant::now();
        }
    }
    if !verify(&bytes, face) {
        return Err("Font integrity verification failed. Retry.".into());
    }
    Ok(bytes)
}

fn reconcile(
    root: &Path,
    faces: &[FontFile; 4],
    cancel: &AtomicBool,
    mut fetch: impl FnMut(&FontFile) -> Result<Vec<u8>, String>,
) -> Result<[Vec<u8>; 4], String> {
    if fs::symlink_metadata(root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("The font store must not be a link.".into());
    }
    let mut local = Vec::with_capacity(4);
    for face in faces {
        cancelled(cancel)?;
        match cached(root, face)? {
            Some(bytes) => local.push(bytes),
            None => break,
        }
    }
    if local.len() == 4 {
        return local
            .try_into()
            .map_err(|_| "The cached font is incomplete.".into());
    }
    drop(local);
    prepare_store(root)?;
    let _lock = acquire_lock(root, cancel)?;
    let mut loaded = Vec::with_capacity(4);
    for face in faces {
        cancelled(cancel)?;
        let bytes = match cached(root, face)? {
            Some(bytes) => bytes,
            None => {
                let bytes = fetch(face)?;
                cancelled(cancel)?;
                publish(root, face, &bytes)?;
                bytes
            }
        };
        loaded.push(bytes);
    }
    loaded
        .try_into()
        .map_err(|_| "The font installation is incomplete.".into())
}

struct Job {
    family: TerminalFontPreference,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
    total: u64,
    receiver: Receiver<Result<[Vec<u8>; 4], String>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct FontDownloads {
    root: Option<PathBuf>,
    desired: Option<TerminalFontPreference>,
    attempted: bool,
    job: Option<Job>,
    active: TerminalFontPreference,
    error: Option<String>,
}

pub(crate) enum ReadyFont {
    Default,
    Optional(TerminalFontPreference, [Vec<u8>; 4]),
}

#[cfg(test)]
type TestCompletion = mpsc::SyncSender<Result<[Vec<u8>; 4], String>>;

impl FontDownloads {
    pub(crate) fn native() -> Self {
        Self::with_root(
            directories::ProjectDirs::from("com", "fes", "fesTerm")
                .map(|directories| directories.data_local_dir().join("fonts").join("v1")),
        )
    }

    pub(crate) fn with_root(root: Option<PathBuf>) -> Self {
        Self {
            root,
            desired: None,
            attempted: false,
            job: None,
            active: TerminalFontPreference::JetBrainsMono,
            error: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_test() -> Self {
        Self::with_root(None)
    }

    #[cfg(test)]
    pub(crate) fn cached_for_test(root: &Path, family: TerminalFontPreference) -> Self {
        prepare_store(root).unwrap();
        for face in catalog(family).unwrap() {
            let bytes = fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join(&face.path),
            )
            .unwrap();
            publish(root, &face, &bytes).unwrap();
        }
        Self::with_root(Some(root.to_owned()))
    }

    #[cfg(test)]
    pub(crate) fn pending_for_test(family: TerminalFontPreference) -> (Self, TestCompletion) {
        let mut controller = Self::unavailable_for_test();
        let (sender, receiver) = mpsc::sync_channel(1);
        controller.desired = Some(family);
        controller.attempted = true;
        controller.job = Some(Job {
            family,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU64::new(0)),
            total: 128,
        });
        (controller, sender)
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> TerminalFontPreference {
        self.active
    }

    pub(crate) fn retry(&mut self) {
        self.attempted = false;
        self.error = None;
    }

    pub(crate) fn installation_failed(&mut self, error: &str) {
        self.active = TerminalFontPreference::JetBrainsMono;
        self.error = Some(format!("{error} Retry the preferred font download."));
    }

    pub(crate) fn installation_superseded(&mut self) {
        self.active = TerminalFontPreference::JetBrainsMono;
        self.error = Some(
            "A newer terminal font preference superseded this installation. Retry if this family is still preferred."
                .into(),
        );
    }

    pub(crate) fn status(&self) -> Option<String> {
        let desired = self.desired?;
        if desired == TerminalFontPreference::JetBrainsMono {
            return None;
        }
        let name = family_label(desired);
        if let Some(error) = &self.error {
            Some(format!("Using JetBrains Mono temporarily. {name}: {error}"))
        } else if let Some(job) = &self.job {
            Some(format!("Using JetBrains Mono temporarily. Preparing {name}: {} / {} KiB downloaded. Cached faces need no download.",
                job.progress.load(Ordering::Relaxed) / 1024, job.total / 1024))
        } else {
            Some(format!(
                "{name} is downloaded and verified; available offline."
            ))
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.error.is_some()
    }

    pub(crate) fn update(
        &mut self,
        desired: TerminalFontPreference,
        context: &egui::Context,
    ) -> Option<ReadyFont> {
        let changed = self.desired != Some(desired);
        if changed {
            self.desired = Some(desired);
            self.attempted = false;
            self.error = None;
            if let Some(job) = &self.job {
                job.cancel.store(true, Ordering::Relaxed);
            }
            self.active = TerminalFontPreference::JetBrainsMono;
        }
        let result = self.job.as_ref().map(|job| job.receiver.try_recv());
        if let Some(result) = result {
            match result {
                Err(TryRecvError::Empty) => {}
                result => {
                    let job = self.job.take().expect("polled job exists");
                    if job.family == desired && !job.cancel.load(Ordering::Relaxed) {
                        match result {
                            Ok(Ok(faces)) => {
                                self.active = desired;
                                return Some(ReadyFont::Optional(desired, faces));
                            }
                            Ok(Err(error)) => self.error = Some(error),
                            Err(_) => {
                                self.error =
                                    Some("The font worker stopped unexpectedly. Retry.".into())
                            }
                        }
                    }
                }
            }
        }
        if desired != TerminalFontPreference::JetBrainsMono && !self.attempted && self.job.is_none()
        {
            self.attempted = true;
            let result = self.begin(desired, context.clone());
            if let Err(error) = result {
                self.error = Some(error);
            }
        }
        changed.then_some(ReadyFont::Default)
    }

    fn begin(
        &mut self,
        family: TerminalFontPreference,
        context: egui::Context,
    ) -> Result<(), String> {
        let root = self.root.clone().ok_or("The native font data directory is unavailable. Retry after fixing your application data location.")?;
        let faces = catalog(family)?;
        let total = faces.iter().map(|face| face.size_bytes).sum();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        std::thread::Builder::new()
            .name("optional-font-download".into())
            .spawn(move || {
                let mut client = None;
                let result = reconcile(&root, &faces, &worker_cancel, |face| {
                    if client.is_none() {
                        client = Some(
                            reqwest::blocking::Client::builder()
                                .https_only(true)
                                .connect_timeout(Duration::from_secs(10))
                                .timeout(Duration::from_secs(60))
                                .redirect(reqwest::redirect::Policy::limited(5))
                                .user_agent(concat!(
                                    "fesTerm/",
                                    env!("CARGO_PKG_VERSION"),
                                    " optional-fonts"
                                ))
                                .build()
                                .map_err(|_| "The font download client could not start. Retry.")?,
                        );
                    }
                    download(
                        client.as_ref().expect("initialized for missing faces"),
                        face,
                        &worker_cancel,
                        &worker_progress,
                        &context,
                    )
                });
                let _ = sender.send(result);
                context.request_repaint();
            })
            .map_err(|_| "The font download worker could not start. Retry.")?;
        self.job = Some(Job {
            family,
            cancel,
            progress,
            total,
            receiver,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn fixtures() -> ([FontFile; 4], [Vec<u8>; 4]) {
        let bytes: [Vec<u8>; 4] = std::array::from_fn(|index| vec![index as u8 + 1; 32 + index]);
        let faces = std::array::from_fn(|index| FontFile {
            path: format!("face-{index}.ttf"),
            sha256: Sha256::digest(&bytes[index])
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            size_bytes: bytes[index].len() as u64,
        });
        (faces, bytes)
    }

    fn fetch_fixture(face: &FontFile, faces: &[FontFile; 4], bytes: &[Vec<u8>; 4]) -> Vec<u8> {
        bytes[faces
            .iter()
            .position(|candidate| candidate.sha256 == face.sha256)
            .unwrap()]
        .clone()
    }

    #[test]
    fn optional_font_catalog_is_pinned_to_twelve_bounded_release_faces() {
        let mut total = 0;
        for family in [
            TerminalFontPreference::IosevkaTerm,
            TerminalFontPreference::JuliaMono,
            TerminalFontPreference::MapleMono,
        ] {
            let faces = catalog(family).unwrap();
            for face in &faces {
                assert!(asset_url(face).starts_with(&format!(
                    "https://github.com/fes/fesTerm/releases/download/v{}/festerm-font-",
                    env!("CARGO_PKG_VERSION")
                )));
                total += face.size_bytes;
            }
            assert!(faces[0].path.ends_with("-Regular.ttf"));
            assert!(faces[1].path.ends_with("-Bold.ttf"));
            assert!(faces[2].path.ends_with("Italic.ttf"));
            assert!(faces[3].path.ends_with("-BoldItalic.ttf"));
        }
        assert_eq!(total, 46_363_464);
        assert!(catalog(TerminalFontPreference::JetBrainsMono).is_err());
    }

    #[test]
    fn font_reconciliation_downloads_once_then_works_offline() {
        let directory = tempfile::tempdir().unwrap();
        let (faces, bytes) = fixtures();
        let calls = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let load = || {
            reconcile(directory.path(), &faces, &cancel, |face| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(fetch_fixture(face, &faces, &bytes))
            })
            .unwrap()
        };
        assert_eq!(load(), bytes);
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert_eq!(
            reconcile(directory.path(), &faces, &cancel, |_| Err("offline".into())).unwrap(),
            bytes
        );
        assert_eq!(load(), bytes);
        assert_eq!(calls.load(Ordering::Relaxed), 4);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 5);
    }

    #[test]
    fn font_reconciliation_repairs_corruption_and_fetches_only_missing_pins() {
        let directory = tempfile::tempdir().unwrap();
        let (faces, bytes) = fixtures();
        let cancel = AtomicBool::new(false);
        reconcile(directory.path(), &faces, &cancel, |face| {
            Ok(fetch_fixture(face, &faces, &bytes))
        })
        .unwrap();
        fs::write(
            directory.path().join(asset_name(&faces[1])),
            vec![0; bytes[1].len()],
        )
        .unwrap();
        fs::remove_file(directory.path().join(asset_name(&faces[2]))).unwrap();
        let calls = AtomicUsize::new(0);
        let result = reconcile(directory.path(), &faces, &cancel, |face| {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(fetch_fixture(face, &faces, &bytes))
        })
        .unwrap();
        assert_eq!(result, bytes);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let mut upgraded = faces.clone();
        upgraded[0].sha256 = "f".repeat(64);
        assert!(reconcile(directory.path(), &upgraded, &cancel, |_| Err(
            "offline".into()
        ))
        .is_err());
        assert_eq!(
            cached(directory.path(), &faces[0]).unwrap(),
            Some(bytes[0].clone())
        );
    }

    #[test]
    fn failed_interrupted_or_invalid_fonts_never_publish_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let (faces, bytes) = fixtures();
        let cancel = AtomicBool::new(false);
        assert!(reconcile(directory.path(), &faces, &cancel, |_| Err(
            "interrupted".into()
        ))
        .is_err());
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "only the lock is retained"
        );
        assert!(publish(directory.path(), &faces[0], b"wrong").is_err());
        assert!(!directory.path().join(asset_name(&faces[0])).exists());
        assert!(reconcile(directory.path(), &faces, &cancel, |face| {
            cancel.store(true, Ordering::Relaxed);
            Ok(fetch_fixture(face, &faces, &bytes))
        })
        .is_err());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        cancel.store(false, Ordering::Relaxed);
        assert_eq!(
            reconcile(directory.path(), &faces, &cancel, |face| Ok(fetch_fixture(
                face, &faces, &bytes
            )))
            .unwrap(),
            bytes
        );
    }

    #[test]
    fn font_download_reader_enforces_exact_size_hash_and_cancellation() {
        let (faces, bytes) = fixtures();
        let cancel = AtomicBool::new(false);
        let progress = AtomicU64::new(0);
        let context = egui::Context::default();
        assert_eq!(
            read_download(bytes[0].as_slice(), &faces[0], &cancel, &progress, &context).unwrap(),
            bytes[0]
        );
        for invalid in [
            vec![0; bytes[0].len()],
            vec![1; bytes[0].len() - 1],
            vec![1; bytes[0].len() + 1],
        ] {
            assert!(
                read_download(invalid.as_slice(), &faces[0], &cancel, &progress, &context).is_err()
            );
        }
        cancel.store(true, Ordering::Relaxed);
        assert!(
            read_download(bytes[0].as_slice(), &faces[0], &cancel, &progress, &context).is_err()
        );
        struct BrokenReader;
        impl Read for BrokenReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "fixture",
                ))
            }
        }
        cancel.store(false, Ordering::Relaxed);
        assert!(read_download(BrokenReader, &faces[0], &cancel, &progress, &context).is_err());
    }

    #[test]
    fn concurrent_font_reconciliation_coalesces_under_the_store_lock() {
        let directory = tempfile::tempdir().unwrap();
        let (faces, bytes) = fixtures();
        let calls = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..3 {
                scope.spawn(|| {
                    let cancel = AtomicBool::new(false);
                    assert_eq!(
                        reconcile(directory.path(), &faces, &cancel, |face| {
                            calls.fetch_add(1, Ordering::Relaxed);
                            Ok(fetch_fixture(face, &faces, &bytes))
                        })
                        .unwrap(),
                        bytes
                    );
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn unavailable_font_store_reports_failure_without_automatic_retry() {
        let context = egui::Context::default();
        let mut controller = FontDownloads::unavailable_for_test();
        assert!(matches!(
            controller.update(TerminalFontPreference::JetBrainsMono, &context),
            Some(ReadyFont::Default)
        ));
        assert!(!controller.failed());
        assert!(controller.job.is_none());
        assert!(matches!(
            controller.update(TerminalFontPreference::JuliaMono, &context),
            Some(ReadyFont::Default)
        ));
        assert!(controller.failed());
        assert!(controller.status().unwrap().contains("temporarily"));
        assert_eq!(controller.active(), TerminalFontPreference::JetBrainsMono);
        assert!(controller
            .update(TerminalFontPreference::JuliaMono, &context)
            .is_none());
        assert!(controller.attempted);
        controller.retry();
        assert!(!controller.attempted);
        controller.update(TerminalFontPreference::JuliaMono, &context);
        assert!(controller.failed());
        assert!(controller.attempted);
    }

    fn pending(
        controller: &mut FontDownloads,
        family: TerminalFontPreference,
    ) -> mpsc::SyncSender<Result<[Vec<u8>; 4], String>> {
        let (sender, receiver) = mpsc::sync_channel(1);
        controller.desired = Some(family);
        controller.attempted = true;
        controller.job = Some(Job {
            family,
            receiver,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU64::new(0)),
            total: 128,
        });
        sender
    }

    #[test]
    fn completed_font_download_never_overrides_a_newer_preference() {
        let context = egui::Context::default();
        for desired in [
            TerminalFontPreference::JetBrainsMono,
            TerminalFontPreference::MapleMono,
        ] {
            let mut controller = FontDownloads::unavailable_for_test();
            let sender = pending(&mut controller, TerminalFontPreference::JuliaMono);
            sender.send(Ok(fixtures().1)).unwrap();
            assert!(matches!(
                controller.update(desired, &context),
                Some(ReadyFont::Default)
            ));
            assert_eq!(controller.active(), TerminalFontPreference::JetBrainsMono);
            assert!(controller.job.is_none());
        }
        let mut controller = FontDownloads::unavailable_for_test();
        let sender = pending(&mut controller, TerminalFontPreference::JuliaMono);
        sender.send(Ok(fixtures().1)).unwrap();
        assert!(matches!(
            controller.update(TerminalFontPreference::JuliaMono, &context),
            Some(ReadyFont::Optional(TerminalFontPreference::JuliaMono, _))
        ));
        assert_eq!(controller.active(), TerminalFontPreference::JuliaMono);
    }

    #[test]
    fn superseded_font_installation_is_quiescent_retryable_and_reconciles_changed_preference() {
        let context = egui::Context::default();
        let mut controller = FontDownloads::unavailable_for_test();
        let sender = pending(&mut controller, TerminalFontPreference::JuliaMono);
        sender.send(Ok(fixtures().1)).unwrap();
        assert!(matches!(
            controller.update(TerminalFontPreference::JuliaMono, &context),
            Some(ReadyFont::Optional(TerminalFontPreference::JuliaMono, _))
        ));

        controller.installation_superseded();
        assert_eq!(controller.active(), TerminalFontPreference::JetBrainsMono);
        assert!(controller.failed());
        assert!(controller.job.is_none());
        assert!(controller.attempted);
        let status = controller.status().unwrap();
        assert!(status.contains("superseded"));
        assert!(status.contains("Retry"));
        assert!(!status.contains("downloaded and verified"));

        for _ in 0..4 {
            assert!(controller
                .update(TerminalFontPreference::JuliaMono, &context)
                .is_none());
            assert!(controller.job.is_none(), "supersession must not auto-loop");
        }

        controller.retry();
        assert!(!controller.attempted);
        controller.update(TerminalFontPreference::JuliaMono, &context);
        assert!(
            controller.failed(),
            "an explicit retry reaches a fresh attempt even when this test has no native store"
        );
        assert!(controller.attempted);

        assert!(matches!(
            controller.update(TerminalFontPreference::JetBrainsMono, &context),
            Some(ReadyFont::Default)
        ));
        assert!(!controller.failed());
        assert!(controller.status().is_none());
    }

    #[test]
    fn font_cache_write_failure_is_actionable_and_not_a_success() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("not-a-directory");
        fs::write(&file, b"fixture").unwrap();
        let (faces, _) = fixtures();
        assert!(
            reconcile(&file, &faces, &AtomicBool::new(false), |_| panic!(
                "must not fetch into a file"
            ))
            .unwrap_err()
            .contains("font")
        );
    }
}
