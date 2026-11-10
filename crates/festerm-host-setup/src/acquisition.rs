//! Acquisition only: pinned official bytes into protected, non-executable staging.
//! This is neither native publisher qualification nor installation permission.

use crate::staging;
use festerm_tunnel_bridge::cli_pins::{self, CliPin};
use reqwest::{header, redirect::Policy, Client, Url};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

const ORIGIN: &str = "tunnelsassetsprod.blob.core.windows.net";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

fn refused(reason: &'static str) -> io::Error {
    io::Error::other(reason)
}

/// Explicit opt-in; no URLs, hashes, credentials or trust roots from callers.
/// Run on a blocking worker, not a GUI/runtime thread. No automatic retries.
/// Returns private staged bytes only; native signer/admission checks are still
/// required before any future executable publication or launch.
pub fn acquire_cli(destination: &Path) -> io::Result<PathBuf> {
    acquire_for_target(std::env::consts::OS, std::env::consts::ARCH, destination)
}

fn acquire_for_target(
    platform: &str,
    architecture: &str,
    destination: &Path,
) -> io::Result<PathBuf> {
    let pin = cli_pins::for_target(platform, architecture)
        .map_err(|_| refused("host.cli_target_unavailable"))?;
    let url = approved_url(&pin)?;
    let client = client(true, REQUEST_TIMEOUT)?;
    acquire(&client, url, &pin, destination, cli_pins::DOWNLOAD_LIMIT)
}

fn approved_url(pin: &CliPin) -> io::Result<Url> {
    let asset = match (pin.platform.as_str(), pin.architecture.as_str()) {
        ("windows", "x86_64") => "devtunnel.exe",
        ("macos", "x86_64") => "osx-x64-devtunnel",
        ("macos", "aarch64") => "osx-arm64-devtunnel",
        ("linux", "x86_64") => "linux-x64-devtunnel",
        ("linux", "aarch64") => "linux-arm64-devtunnel",
        _ => return Err(refused("host.cli_target_unavailable")),
    };
    let expected = format!(
        "https://{ORIGIN}/cli/{}/{asset}",
        cli_pins::DOWNLOAD_VERSION
    );
    if pin.download != expected
        || pin.format != "executable"
        || pin.archive_sha256 != pin.executable_sha256
        || pin.executable_sha256.len() != 64
        || !pin
            .executable_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(refused("host.cli_download_policy"));
    }
    Url::parse(&expected).map_err(|_| refused("host.cli_download_policy"))
}

struct Downloader {
    client: Client,
    timeout: Duration,
}

fn client(https_only: bool, timeout: Duration) -> io::Result<Downloader> {
    let client = Client::builder()
        .https_only(https_only)
        // The exact versioned blob URL needs no redirect. Refuse all 3xx,
        // including same-origin, aka.ms, HTTPS downgrade and credential URLs.
        .redirect(Policy::none())
        .no_proxy()
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        // Async DNS is cancelled with the request; no detached blocking
        // getaddrinfo worker can outlive the overall deadline.
        .hickory_dns(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(timeout)
        .build()
        .map_err(|_| refused("host.cli_download_failed"))?;
    Ok(Downloader { client, timeout })
}

fn acquire(
    downloader: &Downloader,
    url: Url,
    pin: &CliPin,
    destination: &Path,
    limit: u64,
) -> io::Result<PathBuf> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(refused("host.cli_blocking_worker_required"));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| refused("host.cli_download_failed"))?;
    let bytes = runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + downloader.timeout;
        tokio::time::timeout_at(deadline, receive(&downloader.client, url, limit))
            .await
            .map_err(|_| refused("host.cli_download_failed"))?
    })?;
    // Dropping the bounded runtime cancels all remaining asynchronous IO.
    drop(runtime);
    staging::stage_cli_bytes(&bytes, destination, pin)
}

async fn receive(client: &Client, url: Url, limit: u64) -> io::Result<Vec<u8>> {
    let mut response = client
        .get(url)
        .header(header::ACCEPT_ENCODING, "identity")
        .send()
        .await
        .map_err(|_| refused("host.cli_download_failed"))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(refused("host.cli_download_status"));
    }
    if response
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
    {
        return Err(refused("host.cli_download_encoding"));
    }
    let length = response.content_length();
    if length.is_some_and(|length| length == 0 || length > limit) {
        return Err(refused("host.cli_download_limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| refused("host.cli_download_failed"))?
    {
        if chunk.len() as u64 > limit.saturating_sub(bytes.len() as u64) {
            return Err(refused("host.cli_download_limit"));
        }
        let required = bytes.len() + chunk.len();
        if required > bytes.capacity() {
            let capacity = required
                .max(bytes.capacity().saturating_mul(2))
                .min(limit as usize);
            bytes
                .try_reserve_exact(capacity - bytes.len())
                .map_err(|_| refused("host.cli_download_limit"))?;
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() || bytes.len() as u64 > limit {
        return Err(refused("host.cli_download_limit"));
    }
    if length.is_some_and(|length| length != bytes.len() as u64) {
        return Err(refused("host.cli_download_failed"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn fixture_pin(bytes: &[u8]) -> CliPin {
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        CliPin {
            platform: "linux".into(),
            architecture: "x86_64".into(),
            download: format!(
                "https://{ORIGIN}/cli/{}/linux-x64-devtunnel",
                cli_pins::DOWNLOAD_VERSION
            ),
            format: "executable".into(),
            archive_sha256: hash.clone(),
            executable_sha256: hash,
        }
    }

    // The HTTP URL and synthetic hash seam is private to unit tests. Production
    // exposes only acquire_cli, with embedded pins and HTTPS-only client.
    fn server(response: Vec<u8>, delay: Duration) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/fixture",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let _ = connection.read(&mut request);
            thread::sleep(delay);
            let _ = connection.write_all(&response);
        });
        (url, task)
    }

    #[test]
    fn bounded_http_acquisition_stages_only_pinned_bytes_and_preserves_cache() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let destination = root.path().join("cache");
        let bytes = b"synthetic inert CLI";
        let pin = fixture_pin(bytes);
        let client = client(false, Duration::from_secs(2)).unwrap();
        let response = [
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".as_slice(),
            bytes,
        ]
        .concat();
        for _ in 0..2 {
            let (url, task) = server(response.clone(), Duration::ZERO);
            let path = acquire(&client, url, &pin, &destination, 64).unwrap();
            task.join().unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            let (url, task) = server(
                [
                    format!(
                        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                    bytes,
                    b"\r\n0\r\n\r\n",
                ]
                .concat(),
                Duration::ZERO,
            );
            acquire(&client, url, &pin, &destination, 64).unwrap();
            task.join().unwrap();
        }
        let path = destination.join(format!("devtunnel{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"preserve changed cache").unwrap();
        let (url, task) = server(response.clone(), Duration::ZERO);
        assert_eq!(
            acquire(&client, url, &pin, &destination, 64)
                .unwrap_err()
                .to_string(),
            "host.cached_cli_mismatch"
        );
        task.join().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve changed cache");
        #[cfg(unix)]
        {
            let outside = root.path().join("not-owned-by-cache");
            std::fs::write(&outside, b"preserve symlink target").unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            let (url, task) = server(response, Duration::ZERO);
            assert!(acquire(&client, url, &pin, &destination, 64).is_err());
            task.join().unwrap();
            assert_eq!(std::fs::read(outside).unwrap(), b"preserve symlink target");
            assert!(std::fs::symlink_metadata(path).unwrap().is_symlink());
        }
    }

    #[test]
    fn bounded_http_acquisition_rejects_redirects_status_size_hash_and_truncation() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let pin = fixture_pin(b"good");
        let client = client(false, Duration::from_secs(2)).unwrap();
        for response in [
            "HTTP/1.1 302 Found\r\nLocation: https://aka.ms/ignored\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: /fixture\r\nContent-Length: 0\r\n\r\n",
            "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngood",
            "HTTP/1.1 200 OK\r\nContent-Length: 65\r\n\r\n",
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n01234567890123456789012345678901234567890123456789012345678901234",
            "HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\ngood",
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbad!",
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\n\r\ngood",
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ] {
            let destination = root.path().join("must-not-publish");
            let (url, task) = server(response.as_bytes().to_vec(), Duration::ZERO);
            assert!(acquire(&client, url, &pin, &destination, 64).is_err());
            task.join().unwrap();
            assert!(!destination.exists());
        }
    }

    #[test]
    fn bounded_http_acquisition_deadline_and_production_origin_fail_closed() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let pin = fixture_pin(b"good");
        let (url, task) = server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ngood".to_vec(),
            Duration::from_millis(150),
        );
        let client = client(false, Duration::from_millis(40)).unwrap();
        let destination = root.path().join("cache");
        assert!(acquire(&client, url, &pin, &destination, 64).is_err());
        task.join().unwrap();
        assert!(!destination.exists());
        let original = fixture_pin(b"good");
        approved_url(&original).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/must-not-connect", listener.local_addr().unwrap());
        assert!(acquire(
            &super::client(true, Duration::from_secs(1)).unwrap(),
            Url::parse(&url).unwrap(),
            &pin,
            &destination,
            64
        )
        .is_err());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        for download in [
            "http://tunnelsassetsprod.blob.core.windows.net/cli/1.0.2115+e16833f284/linux-x64-devtunnel",
            "https://aka.ms/TunnelsCliDownload/linux-x64",
            "https://tunnelsassetsprod.blob.core.windows.net.evil.example/cli/binary",
            "https://user:password@tunnelsassetsprod.blob.core.windows.net/cli/binary",
            "https://tunnelsassetsprod.blob.core.windows.net:444/cli/binary",
        ] {
            let mut pin = original.clone();
            pin.download = download.into();
            assert!(approved_url(&pin).is_err());
        }
        let mut arm_windows = original;
        arm_windows.platform = "windows".into();
        arm_windows.architecture = "aarch64".into();
        assert_eq!(
            approved_url(&arm_windows).unwrap_err().to_string(),
            "host.cli_target_unavailable"
        );
    }

    #[test]
    fn new_acquisition_refuses_windows_arm_before_network_or_staging() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let destination = root.path().join("must-not-stage");
        assert_eq!(
            acquire_for_target("windows", "aarch64", &destination)
                .unwrap_err()
                .to_string(),
            "host.cli_target_unavailable"
        );
        assert!(!destination.exists());
        #[cfg(all(windows, target_arch = "aarch64"))]
        assert_eq!(
            acquire_cli(&destination).unwrap_err().to_string(),
            "host.cli_target_unavailable"
        );
    }

    #[test]
    fn slow_body_cannot_extend_the_absolute_acquisition_deadline() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let destination = root.path().join("cache");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/fixture",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let _ = connection.read(&mut request);
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
                .unwrap();
            for byte in b"good" {
                thread::sleep(Duration::from_millis(70));
                if connection.write_all(&[*byte]).is_err() {
                    break;
                }
            }
        });
        let started = std::time::Instant::now();
        assert!(acquire(
            &client(false, Duration::from_millis(170)).unwrap(),
            url,
            &fixture_pin(b"good"),
            &destination,
            64
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!destination.exists());
        task.join().unwrap();
    }
}
