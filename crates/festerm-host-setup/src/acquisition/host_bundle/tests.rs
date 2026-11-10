use super::*;
use crate::artifacts::{
    host_acquisition_fixture, verify_staging_fixture as verify_acquisition_fixture,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

struct Response {
    head: Vec<u8>,
    body: Vec<u8>,
    before_headers: Duration,
    before_body: Duration,
}

impl Response {
    fn raw(head: impl Into<Vec<u8>>, body: Vec<u8>) -> Self {
        Self {
            head: head.into(),
            body,
            before_headers: Duration::ZERO,
            before_body: Duration::ZERO,
        }
    }

    fn ok(bytes: &[u8]) -> Self {
        Self::raw(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            ),
            bytes.to_vec(),
        )
    }
}

// Only this test module can replace the fixed origins or embedded verifier.
// The server owns/join-closes every fixture thread, including early refusals.
struct Server {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(responses: Vec<Response>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let observed = requests.clone();
        let stopped = stop.clone();
        let task = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for response in responses {
                let mut connection = loop {
                    if stopped.load(Ordering::SeqCst) || Instant::now() >= deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((connection, _)) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("fixture accept: {error}"),
                    }
                };
                connection.set_nonblocking(false).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                connection
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let request = read_request(&mut connection);
                observed.lock().unwrap().push(request);
                thread::sleep(response.before_headers);
                if connection.write_all(&response.head).is_ok() {
                    thread::sleep(response.before_body);
                    let _ = connection.write_all(&response.body);
                }
            }
        });
        Self {
            address,
            requests,
            stop,
            task: Some(task),
        }
    }

    fn urls(&self, expected: &ExpectedArtifact<'_>) -> [Url; 3] {
        release_urls(expected).unwrap().map(|mut url| {
            url.set_scheme("http").unwrap();
            url.set_host(Some("127.0.0.1")).unwrap();
            url.set_port(Some(self.address.port())).unwrap();
            url
        })
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.task.take().unwrap().join().unwrap();
    }
}

fn read_request(connection: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut byte = [0];
    while request.len() < 4096 && !request.ends_with(b"\r\n\r\n") {
        if connection.read(&mut byte).unwrap_or(0) == 0 {
            break;
        }
        request.push(byte[0]);
    }
    String::from_utf8(request).unwrap()
}

fn identity() -> ExpectedArtifact<'static> {
    current_identity("linux", "x86_64").unwrap()
}

fn responses(fixture: &VerifiedArtifact) -> Vec<Response> {
    [
        fixture.manifest_bytes(),
        fixture.signature_bytes(),
        fixture.archive_bytes(),
    ]
    .map(Response::ok)
    .into()
}

fn fixture_acquire(
    server: &Server,
    expected: &ExpectedArtifact<'_>,
) -> Result<VerifiedArtifact, HostBundleError> {
    acquire(
        &fixture_client(Duration::from_secs(2)),
        server.urls(expected),
        expected,
        &CancellationToken::new(),
        Instant::now() + Duration::from_secs(3),
        verify_acquisition_fixture,
    )
}

fn fixture_client(timeout: Duration) -> Downloader {
    let mut builder = host_client_builder(false, timeout);
    // Even a broken redirect oracle cannot use real release assets or DNS.
    for host in [
        "release-assets.githubusercontent.com",
        "release-assets.githubusercontent.com.evil.invalid",
        "evil.invalid",
        "github.com",
    ] {
        builder = builder.resolve(host, "127.0.0.1:1".parse().unwrap());
    }
    Downloader {
        client: builder.build().unwrap(),
        timeout,
    }
}

fn refused(result: Result<VerifiedArtifact, HostBundleError>) -> HostBundleError {
    match result {
        Ok(_) => panic!("unexpected authenticated capability"),
        Err(error) => error,
    }
}

#[test]
fn official_host_bundle_identity_is_exact_current_native_host_only() {
    for platform in ["linux", "macos", "windows"] {
        for architecture in ["x86_64", "aarch64"] {
            let expected = current_identity(platform, architecture).unwrap();
            assert_eq!(expected.version, crate::VERSION);
            assert_eq!(expected.platform.name(), platform);
            assert_eq!(expected.architecture.name(), architecture);
            assert_eq!(expected.package, Package::Host);
            let urls = release_urls(&expected).unwrap();
            let archive = format!(
                "{RELEASE_ROOT}/v{}/fesTerm-host-{}-{platform}-{architecture}.{}",
                crate::VERSION,
                crate::VERSION,
                if platform == "windows" {
                    "zip"
                } else {
                    "tar.gz"
                }
            );
            assert_eq!(urls[0].as_str(), format!("{archive}.manifest.json"));
            assert_eq!(urls[1].as_str(), format!("{archive}.manifest.json.sig"));
            assert_eq!(urls[2].as_str(), archive);
        }
    }
    for (platform, architecture) in [("unknown", "aarch64"), ("windows", "unknown"), ("", "")] {
        assert!(matches!(
            current_identity(platform, architecture),
            Err(HostBundleError::UnsupportedTarget)
        ));
    }
    for version in [
        "",
        "latest",
        "../1.2.3",
        "1.2.3?token=redact",
        "v1.2.3",
        "01.2.3",
        "1.2.3-rc",
    ] {
        let mut expected = identity();
        expected.version = version;
        assert_eq!(release_urls(&expected), Err(HostBundleError::Identity));
    }
    let mut expected = identity();
    expected.package = Package::Helpers;
    assert_eq!(release_urls(&expected), Err(HostBundleError::Identity));
}

#[test]
fn official_host_bundle_six_signed_targets_return_owned_authenticated_bytes() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        for architecture in [Architecture::X86_64, Architecture::Aarch64] {
            let expected = ExpectedArtifact {
                platform,
                architecture,
                ..identity()
            };
            let fixture = host_acquisition_fixture(&expected);
            let mut response = responses(&fixture);
            if architecture == Architecture::Aarch64 {
                for response in &mut response {
                    if platform == Platform::Macos {
                        response.head =
                            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
                        response.body = [
                            format!("{:x}\r\n", response.body.len()).as_bytes(),
                            &response.body,
                            b"\r\n0\r\n\r\n",
                        ]
                        .concat();
                    } else {
                        response.head = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
                    }
                }
            }
            let server = Server::new(response);
            let verified = fixture_acquire(&server, &expected).unwrap();
            assert_eq!(verified.manifest(), fixture.manifest());
            assert_eq!(verified.manifest_bytes(), fixture.manifest_bytes());
            assert_eq!(verified.signature_bytes(), fixture.signature_bytes());
            assert_eq!(verified.archive_bytes(), fixture.archive_bytes());
            assert_eq!(
                verified.entries().len(),
                if platform == Platform::Windows { 8 } else { 7 }
            );
            for (entry, original) in verified.entries().iter().zip(fixture.entries()) {
                assert_eq!(entry.name(), original.name());
                assert_eq!(entry.bytes(), original.bytes());
                assert_eq!(entry.executable(), original.executable());
            }
            let requests = server.requests();
            assert_eq!(requests.len(), 3);
            for (request, url) in requests.iter().zip(release_urls(&expected).unwrap()) {
                assert!(request.starts_with(&format!("GET {} HTTP/1.1\r\n", url.path())));
                let headers = request.to_ascii_lowercase();
                assert!(headers.contains("\r\naccept-encoding: identity\r\n"));
                for forbidden in [
                    "authorization:",
                    "proxy-authorization:",
                    "cookie:",
                    "referer:",
                ] {
                    assert!(!headers.contains(forbidden));
                }
            }
            drop(server);
            assert_eq!(verified.archive_bytes(), fixture.archive_bytes());
        }
    }
}

#[test]
fn official_host_bundle_rejects_signed_identity_mismatch_tamper_and_untrusted_key() {
    let expected = identity();
    for wrong in [
        ExpectedArtifact {
            version: "1.2.4",
            ..identity()
        },
        ExpectedArtifact {
            platform: Platform::Macos,
            ..identity()
        },
        ExpectedArtifact {
            architecture: Architecture::Aarch64,
            ..identity()
        },
        ExpectedArtifact {
            package: Package::Helpers,
            ..identity()
        },
    ] {
        let fixture = host_acquisition_fixture(&wrong);
        let server = Server::new(responses(&fixture));
        assert_eq!(
            refused(fixture_acquire(&server, &expected)),
            HostBundleError::Verification(ArtifactError::Target)
        );
        assert_eq!(server.requests().len(), 3);
    }
    let fixture = host_acquisition_fixture(&expected);
    for index in 0..3 {
        let mut response = responses(&fixture);
        if index == 1 {
            let mut signature = STANDARD.decode(&response[index].body).unwrap();
            let packet = signature.iter().position(|byte| *byte == b'\n').unwrap() + 1;
            signature[packet + 16] = if signature[packet + 16] == b'A' {
                b'B'
            } else {
                b'A'
            };
            response[index].body = STANDARD.encode(signature).into_bytes();
        } else {
            response[index].body[0] ^= 1;
        }
        let server = Server::new(response);
        let error = refused(fixture_acquire(&server, &expected));
        assert_eq!(
            error,
            HostBundleError::Verification(if index == 2 {
                ArtifactError::Hash
            } else {
                ArtifactError::Signature
            })
        );
    }
    let server = Server::new(responses(&fixture));
    assert_eq!(
        refused(acquire(
            &fixture_client(Duration::from_secs(2)),
            server.urls(&expected),
            &expected,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(3),
            artifacts::verify,
        )),
        HostBundleError::Verification(ArtifactError::Signature)
    );
}

#[test]
fn official_host_bundle_absent_assets_are_blocking_without_legacy_retry() {
    let expected = identity();
    let fixture = host_acquisition_fixture(&expected);
    for (index, asset) in [
        BundleAsset::Manifest,
        BundleAsset::Signature,
        BundleAsset::Archive,
    ]
    .into_iter()
    .enumerate()
    {
        let mut response = responses(&fixture);
        response[index] = Response::raw(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            Vec::new(),
        );
        let server = Server::new(response);
        let error = refused(fixture_acquire(&server, &expected));
        assert_eq!(error, HostBundleError::NotPublished(asset));
        assert_eq!(server.requests().len(), index + 1);
        let diagnostic = format!("{error} {error:?}");
        for secret in [
            "http",
            "127.0.0.1",
            crate::VERSION,
            "fesTerm-host",
            "token=",
        ] {
            assert!(!diagnostic.contains(secret));
        }
    }
}

#[test]
fn official_host_bundle_refuses_unsafe_redirects_before_following() {
    let approved = "https://release-assets.githubusercontent.com/github-production-release-asset/1/fixture?sig=redact";
    assert!(redirect_allowed(
        &Url::parse(approved).unwrap(),
        MAX_REDIRECTS
    ));
    assert!(!redirect_allowed(
        &Url::parse(approved).unwrap(),
        MAX_REDIRECTS + 1
    ));
    for destination in [
        "http://release-assets.githubusercontent.com/github-production-release-asset/1/fixture",
        "https://release-assets.githubusercontent.com.evil.invalid/github-production-release-asset/1/fixture",
        "https://release-assets.githubusercontent.com@evil.invalid/github-production-release-asset/1/fixture",
        "https://user:secret@release-assets.githubusercontent.com/github-production-release-asset/1/fixture",
        "https://release-assets.githubusercontent.com:444/github-production-release-asset/1/fixture",
        "https://release-assets.githubusercontent.com/github-production-release-asset/1/fixture#secret",
        "https://release-assets.githubusercontent.com/other/fixture",
        "https://github.com/fes/fesTerm/releases/latest",
        "http://127.0.0.1:1/secret",
        "/relative/secret",
        "not a URL",
    ] {
        if let Ok(url) = Url::parse(destination) {
            assert!(!redirect_allowed(&url, 1));
        }
        let response = Response::raw(
            format!("HTTP/1.1 302 Found\r\nLocation: {destination}\r\nContent-Length: 0\r\n\r\n"),
            Vec::new(),
        );
        let server = Server::new(vec![response]);
        let error = refused(fixture_acquire(&server, &identity()));
        assert_eq!(error, HostBundleError::Redirect(BundleAsset::Manifest));
        assert_eq!(server.requests().len(), 1);
        assert!(!format!("{error} {error:?}").contains("secret"));
        assert!(!format!("{error} {error:?}").contains(destination));
    }
    for location in [
        "",
        "Location: https://release-assets.githubusercontent.com/github-production-release-asset/1/fixture\r\nLocation: https://evil.invalid/secret\r\n",
    ] {
        let server = Server::new(vec![Response::raw(
            format!("HTTP/1.1 302 Found\r\n{location}Content-Length: 0\r\n\r\n"),
            Vec::new(),
        )]);
        assert_eq!(
            refused(fixture_acquire(&server, &identity())),
            HostBundleError::Redirect(BundleAsset::Manifest)
        );
    }
}

#[test]
fn official_host_bundle_rejects_zero_oversize_encoded_truncated_and_stream_overflow() {
    let expected = identity();
    let fixture = host_acquisition_fixture(&expected);
    assert_eq!(BundleAsset::Manifest.limit(), 4096);
    assert_eq!(BundleAsset::Signature.limit(), 4096);
    assert_eq!(BundleAsset::Archive.limit(), 256 * 1024 * 1024);
    for (index, asset) in [
        BundleAsset::Manifest,
        BundleAsset::Signature,
        BundleAsset::Archive,
    ]
    .into_iter()
    .enumerate()
    {
        let cases = [
            (Response::ok(&[]), HostBundleError::Bounds(asset)),
            (
                Response::raw(
                    b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(),
                    Vec::new(),
                ),
                HostBundleError::Bounds(asset),
            ),
            (
                Response::raw(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        asset.limit() + 1
                    ),
                    Vec::new(),
                ),
                HostBundleError::Bounds(asset),
            ),
            (
                Response::raw(
                    b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 1\r\n\r\n"
                        .to_vec(),
                    vec![1],
                ),
                HostBundleError::Encoding(asset),
            ),
            (
                Response::raw(
                    b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\nContent-Encoding: gzip\r\nContent-Length: 1\r\n\r\n"
                        .to_vec(),
                    vec![1],
                ),
                HostBundleError::Encoding(asset),
            ),
            (
                Response::raw(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n".to_vec(),
                    vec![1],
                ),
                HostBundleError::Transport(asset),
            ),
            (
                Response::raw(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    Vec::new(),
                ),
                HostBundleError::Status(asset, 503),
            ),
            (
                Response::raw(
                    b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    Vec::new(),
                ),
                HostBundleError::Status(asset, 304),
            ),
        ];
        for (invalid, error) in cases {
            let mut response = responses(&fixture);
            response[index] = invalid;
            let server = Server::new(response);
            assert_eq!(refused(fixture_acquire(&server, &expected)), error);
            assert_eq!(server.requests().len(), index + 1);
        }
        if asset != BundleAsset::Archive {
            for chunked in [false, true] {
                let mut response = responses(&fixture);
                response[index] = if chunked {
                    Response::raw(
                        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
                        [b"1001\r\n".as_slice(), &vec![b'x'; 4097], b"\r\n0\r\n\r\n"].concat(),
                    )
                } else {
                    Response::raw(
                        b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(),
                        vec![b'x'; 4097],
                    )
                };
                let server = Server::new(response);
                assert_eq!(
                    refused(fixture_acquire(&server, &expected)),
                    HostBundleError::Bounds(asset)
                );
            }
        }
    }
}

#[test]
fn official_host_bundle_cancellation_and_deadline_cover_headers_body_and_total_budget() {
    let expected = identity();
    let fixture = host_acquisition_fixture(&expected);
    let downloader = fixture_client(Duration::from_secs(2));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        refused(acquire_current_host_bundle(&cancel)),
        HostBundleError::Cancelled
    );
    for before_headers in [false, true] {
        for cancelled in [false, true] {
            let mut response = responses(&fixture);
            if before_headers {
                response[0].before_headers = Duration::from_millis(150);
            } else {
                response[0].before_body = Duration::from_millis(150);
            }
            let server = Server::new(response);
            let cancel = CancellationToken::new();
            let cancelling = if cancelled {
                let cancel = cancel.clone();
                let requests = server.requests.clone();
                Some(thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while requests.lock().unwrap().is_empty() && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(1));
                    }
                    cancel.cancel();
                }))
            } else {
                None
            };
            let started = Instant::now();
            let error = refused(acquire(
                &downloader,
                server.urls(&expected),
                &expected,
                &cancel,
                started
                    + if cancelled {
                        Duration::from_secs(2)
                    } else {
                        Duration::from_millis(40)
                    },
                verify_acquisition_fixture,
            ));
            if let Some(task) = cancelling {
                task.join().unwrap();
            }
            assert_eq!(
                error,
                if cancelled {
                    HostBundleError::Cancelled
                } else {
                    HostBundleError::Deadline
                }
            );
            assert!(started.elapsed() < Duration::from_secs(1));
            assert_eq!(server.requests().len(), 1);
        }
    }
    let mut response = responses(&fixture);
    for response in &mut response {
        response.before_body = Duration::from_millis(60);
    }
    let server = Server::new(response);
    assert_eq!(
        refused(acquire(
            &downloader,
            server.urls(&expected),
            &expected,
            &CancellationToken::new(),
            Instant::now() + Duration::from_millis(150),
            verify_acquisition_fixture,
        )),
        HostBundleError::Deadline
    );
    assert!((2..=3).contains(&server.requests().len()));

    let mut response = responses(&fixture);
    response[0].before_headers = Duration::from_millis(150);
    let server = Server::new(response);
    assert_eq!(
        refused(acquire(
            &fixture_client(Duration::from_millis(40)),
            server.urls(&expected),
            &expected,
            &CancellationToken::new(),
            Instant::now() + Duration::from_secs(2),
            verify_acquisition_fixture,
        )),
        HostBundleError::Deadline
    );
}

#[test]
fn official_host_bundle_late_verification_never_returns_capability() {
    let expected = identity();
    let fixture = host_acquisition_fixture(&expected);
    let downloader = fixture_client(Duration::from_secs(2));
    for cancelled in [false, true] {
        let server = Server::new(responses(&fixture));
        let cancel = CancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = acquire(
            &downloader,
            server.urls(&expected),
            &expected,
            &cancel,
            deadline,
            |manifest, signature, archive, expected| {
                let verified = verify_acquisition_fixture(manifest, signature, archive, expected)?;
                if cancelled {
                    cancel.cancel();
                } else {
                    // Make the acceptance deadline expire after verification began.
                    thread::sleep(deadline.saturating_duration_since(Instant::now()));
                }
                Ok(verified)
            },
        );
        assert_eq!(
            refused(result),
            if cancelled {
                HostBundleError::Cancelled
            } else {
                HostBundleError::Deadline
            }
        );
    }
}

#[tokio::test]
async fn official_host_bundle_requires_owned_blocking_worker() {
    assert_eq!(
        refused(acquire_current_host_bundle(&CancellationToken::new())),
        HostBundleError::BlockingWorkerRequired
    );
}
