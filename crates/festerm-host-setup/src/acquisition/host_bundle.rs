//! Official schema-2 acquisition only; no staging, execution or host authority.

use super::{
    client_builder, receive_response, send_request, Downloader, ReceiveError, REQUEST_TIMEOUT,
};
use crate::artifacts::{
    self, Architecture, ArtifactError, ExpectedArtifact, Package, Platform, VerifiedArtifact,
};
use reqwest::{header, redirect::Policy, StatusCode, Url};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

const RELEASE_ROOT: &str = "https://github.com/fes/fesTerm/releases/download";
const MAX_REDIRECTS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BundleAsset {
    Manifest,
    Signature,
    Archive,
}

impl BundleAsset {
    fn limit(self) -> u64 {
        match self {
            Self::Manifest => artifacts::MAX_MANIFEST_BYTES as u64,
            Self::Signature => artifacts::MAX_SIGNATURE_BYTES as u64,
            Self::Archive => artifacts::MAX_ARCHIVE_BYTES as u64,
        }
    }
}

/// Redacted: no request URL, signed redirect query, body or transport diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostBundleError {
    UnsupportedTarget,
    Identity,
    BlockingWorkerRequired,
    Cancelled,
    Deadline,
    Client,
    NotPublished(BundleAsset),
    Status(BundleAsset, u16),
    Redirect(BundleAsset),
    Transport(BundleAsset),
    Encoding(BundleAsset),
    Bounds(BundleAsset),
    Verification(ArtifactError),
}

impl std::fmt::Display for HostBundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "host bundle acquisition refused: {self:?}")
    }
}

impl std::error::Error for HostBundleError {}

/// Fetch only this trusted build's exact version and native compilation target.
/// Call explicitly on a blocking worker, outside any Tokio runtime. No caller
/// URLs, versions, keys, automatic latest/older retry or schema-1 fallback.
///
/// The shared updater root authenticates the returned owned bytes, not native
/// signing, trusted first-verifier delivery, installation or permission to run.
/// A 404 is `NotPublished`, never successful absence. Cancellation and the
/// overall deadline fence every receive and acceptance after bounded verification.
pub fn acquire_current_host_bundle(
    cancel: &CancellationToken,
) -> Result<VerifiedArtifact, HostBundleError> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    check_active(cancel, deadline)?;
    require_worker()?;
    let expected = current_identity(std::env::consts::OS, std::env::consts::ARCH)?;
    let urls = release_urls(&expected)?;
    let downloader = host_client(true, REQUEST_TIMEOUT)?;
    acquire(
        &downloader,
        urls,
        &expected,
        cancel,
        deadline,
        artifacts::verify,
    )
}

fn current_identity(
    platform: &str,
    architecture: &str,
) -> Result<ExpectedArtifact<'static>, HostBundleError> {
    let platform = match platform {
        "linux" => Platform::Linux,
        "macos" => Platform::Macos,
        "windows" => Platform::Windows,
        _ => return Err(HostBundleError::UnsupportedTarget),
    };
    let architecture = match architecture {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        _ => return Err(HostBundleError::UnsupportedTarget),
    };
    Ok(ExpectedArtifact {
        version: crate::VERSION,
        platform,
        architecture,
        package: Package::Host,
    })
}

fn release_urls(expected: &ExpectedArtifact<'_>) -> Result<[Url; 3], HostBundleError> {
    if expected.package != Package::Host || !artifacts::stable_version(expected.version) {
        return Err(HostBundleError::Identity);
    }
    let archive = format!(
        "{RELEASE_ROOT}/v{}/{}",
        expected.version,
        artifacts::filename(expected)
    );
    let parse = |url: &str| Url::parse(url).map_err(|_| HostBundleError::Identity);
    Ok([
        parse(&format!("{archive}.manifest.json"))?,
        parse(&format!("{archive}.manifest.json.sig"))?,
        parse(&archive)?,
    ])
}

fn redirect_allowed(url: &Url, previous: usize) -> bool {
    previous <= MAX_REDIRECTS
        && url.scheme() == "https"
        && url.host_str() == Some("release-assets.githubusercontent.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.path().starts_with("/github-production-release-asset/")
}

fn host_client(
    https_only: bool,
    timeout: std::time::Duration,
) -> Result<Downloader, HostBundleError> {
    let client = host_client_builder(https_only, timeout)
        .build()
        .map_err(|_| HostBundleError::Client)?;
    Ok(Downloader { client, timeout })
}

fn host_client_builder(https_only: bool, timeout: std::time::Duration) -> reqwest::ClientBuilder {
    client_builder(https_only, timeout)
        .referer(false)
        .redirect(Policy::none())
}

async fn receive_asset(
    client: &reqwest::Client,
    mut url: Url,
    limit: u64,
) -> Result<Vec<u8>, ReceiveError> {
    for previous in 0..=MAX_REDIRECTS {
        let response = send_request(client, url).await?;
        if !matches!(
            response.status(),
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            return receive_response(response, limit).await;
        }
        let mut locations = response.headers().get_all(header::LOCATION).iter();
        let location = locations
            .next()
            .and_then(|location| location.to_str().ok())
            .ok_or(ReceiveError::Redirect)?;
        if locations.next().is_some() {
            return Err(ReceiveError::Redirect);
        }
        // Inspect Location ourselves, before reqwest can normalize away userinfo
        // or fragments or turn redirect credentials into Authorization headers.
        let next = Url::parse(location).map_err(|_| ReceiveError::Redirect)?;
        if !redirect_allowed(&next, previous + 1) {
            return Err(ReceiveError::Redirect);
        }
        url = next;
    }
    Err(ReceiveError::Redirect)
}

fn require_worker() -> Result<(), HostBundleError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        Err(HostBundleError::BlockingWorkerRequired)
    } else {
        Ok(())
    }
}

fn check_active(cancel: &CancellationToken, deadline: Instant) -> Result<(), HostBundleError> {
    if cancel.is_cancelled() {
        Err(HostBundleError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(HostBundleError::Deadline)
    } else {
        Ok(())
    }
}

fn download_error(asset: BundleAsset, error: ReceiveError) -> HostBundleError {
    match error {
        ReceiveError::Status(404) => HostBundleError::NotPublished(asset),
        ReceiveError::Status(status) => HostBundleError::Status(asset, status),
        ReceiveError::Redirect => HostBundleError::Redirect(asset),
        ReceiveError::Timeout => HostBundleError::Deadline,
        ReceiveError::Encoding => HostBundleError::Encoding(asset),
        ReceiveError::Limit => HostBundleError::Bounds(asset),
        ReceiveError::Transport => HostBundleError::Transport(asset),
    }
}

fn acquire(
    downloader: &Downloader,
    urls: [Url; 3],
    expected: &ExpectedArtifact<'_>,
    cancel: &CancellationToken,
    deadline: Instant,
    verify: impl FnOnce(
        &[u8],
        &[u8],
        &[u8],
        &ExpectedArtifact<'_>,
    ) -> Result<VerifiedArtifact, ArtifactError>,
) -> Result<VerifiedArtifact, HostBundleError> {
    check_active(cancel, deadline)?;
    require_worker()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| HostBundleError::Client)?;
    let fetched = runtime.block_on(async {
        cancel
            .run_until_cancelled(tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                async {
                    let mut bytes = [Vec::new(), Vec::new(), Vec::new()];
                    for ((asset, url), destination) in [
                        BundleAsset::Manifest,
                        BundleAsset::Signature,
                        BundleAsset::Archive,
                    ]
                    .into_iter()
                    .zip(urls)
                    .zip(&mut bytes)
                    {
                        check_active(cancel, deadline)?;
                        *destination = receive_asset(&downloader.client, url, asset.limit())
                            .await
                            .map_err(|error| download_error(asset, error))?;
                    }
                    Ok(bytes)
                },
            ))
            .await
    });
    // Cancel/drop all asynchronous IO before CPU-bounded verification. It cannot
    // publish a late capability: check cancellation/deadline again afterwards.
    drop(runtime);
    let [manifest, signature, archive] = fetched
        .ok_or(HostBundleError::Cancelled)?
        .map_err(|_| HostBundleError::Deadline)??;
    check_active(cancel, deadline)?;
    let verified = verify(&manifest, &signature, &archive, expected);
    check_active(cancel, deadline)?;
    verified.map_err(HostBundleError::Verification)
}

#[cfg(test)]
mod tests;
