use std::fmt;
use std::ops::Deref;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::redirect::Policy;
use reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode};
use tokio::runtime::{Builder, Runtime};
use tokio::time::Instant as TokioInstant;
use zeroize::Zeroizing;

use crate::auth::OperationControl;

#[cfg(test)]
use reqwest::dns::Resolve;
#[cfg(test)]
use std::sync::{Arc, Mutex};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub(crate) const DEFAULT_MAX_ERROR_BYTES: usize = 16 * 1024;

pub(crate) struct HttpClient {
    client: Client,
    runtime: Runtime,
}

pub(crate) struct HttpBody(Zeroizing<Vec<u8>>);

impl Deref for HttpBody {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

impl fmt::Debug for HttpBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HttpBody(<redacted>)")
    }
}

pub(crate) struct HttpResponse {
    status: StatusCode,
    body: HttpBody,
    claims_challenge: bool,
}

impl HttpResponse {
    pub(crate) const fn status(&self) -> StatusCode {
        self.status
    }

    pub(crate) fn into_bytes(self) -> HttpBody {
        self.body
    }

    pub(crate) fn into_body(self) -> HttpBody {
        self.body
    }

    pub(crate) const fn claims_challenge(&self) -> bool {
        self.claims_challenge
    }
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body", &"<redacted>")
            .field("claims_challenge", &self.claims_challenge)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HttpErrorKind {
    Cancelled,
    TimedOut,
    Network,
    Redirected,
    ResponseTooLarge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HttpError {
    kind: HttpErrorKind,
}

impl HttpError {
    pub(crate) const fn new(kind: HttpErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn with_status(kind: HttpErrorKind, _status: StatusCode) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(self) -> HttpErrorKind {
        self.kind
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            HttpErrorKind::Cancelled => formatter.write_str("operation cancelled"),
            HttpErrorKind::TimedOut => formatter.write_str("operation timed out"),
            HttpErrorKind::Network => formatter.write_str("network request failed"),
            HttpErrorKind::Redirected => {
                formatter.write_str("redirected responses are not accepted")
            }
            HttpErrorKind::ResponseTooLarge => {
                formatter.write_str("HTTP response exceeded the configured byte limit")
            }
        }
    }
}

impl std::error::Error for HttpError {}

impl HttpClient {
    pub(crate) fn new() -> Result<Self, HttpError> {
        Self::build(false)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test() -> Result<Self, HttpError> {
        Self::build(true)
    }

    #[cfg(test)]
    fn new_for_test_with_resolver<R>(resolver: R) -> Result<Self, HttpError>
    where
        R: Resolve + 'static,
    {
        Self::with_builder(
            Client::builder()
                .redirect(Policy::none())
                .connect_timeout(CONNECT_TIMEOUT)
                .pool_max_idle_per_host(0)
                .https_only(false)
                .no_proxy()
                .dns_resolver(Arc::new(resolver)),
        )
    }

    fn build(allow_http: bool) -> Result<Self, HttpError> {
        let builder = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .pool_max_idle_per_host(0)
            .hickory_dns(true)
            .https_only(!allow_http);
        #[cfg(test)]
        let builder = builder.no_proxy();
        Self::with_builder(builder)
    }

    fn with_builder(builder: ClientBuilder) -> Result<Self, HttpError> {
        // DNS state and its IO resources must keep the same runtime across requests.
        // Async resolution also keeps runtime shutdown independent of getaddrinfo.
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| HttpError::new(HttpErrorKind::Network))?;
        let client = {
            let _entered = runtime.enter();
            builder
                .build()
                .map_err(|_| HttpError::new(HttpErrorKind::Network))?
        };
        Ok(Self { client, runtime })
    }

    pub(crate) fn post_form(
        &self,
        url: &str,
        body: &str,
        control: &OperationControl,
        max_success_bytes: usize,
        max_error_bytes: usize,
    ) -> Result<HttpResponse, HttpError> {
        let request = self
            .client
            .post(url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.to_owned());
        self.execute(request, control, max_success_bytes, max_error_bytes)
    }

    pub(crate) fn get(
        &self,
        url: &str,
        bearer_token: &str,
        control: &OperationControl,
        max_success_bytes: usize,
        max_error_bytes: usize,
    ) -> Result<HttpResponse, HttpError> {
        let authorization = Zeroizing::new(format!("Bearer {bearer_token}"));
        let mut header_value = HeaderValue::from_str(&authorization)
            .map_err(|_| HttpError::new(HttpErrorKind::Network))?;
        header_value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, header_value);
        let request = self.client.get(url).headers(headers);
        self.execute(request, control, max_success_bytes, max_error_bytes)
    }

    fn execute(
        &self,
        request: RequestBuilder,
        control: &OperationControl,
        max_success_bytes: usize,
        max_error_bytes: usize,
    ) -> Result<HttpResponse, HttpError> {
        self.runtime.block_on(async {
            execute_async(request, control, max_success_bytes, max_error_bytes).await
        })
    }
}

async fn execute_async(
    request: RequestBuilder,
    control: &OperationControl,
    max_success_bytes: usize,
    max_error_bytes: usize,
) -> Result<HttpResponse, HttpError> {
    ensure_active(control)?;
    let mut response = select_with_control(control, request.send()).await?;
    if response.status().is_redirection() {
        return Err(HttpError::with_status(
            HttpErrorKind::Redirected,
            response.status(),
        ));
    }
    let status = response.status();
    let claims_challenge = has_claims_challenge(response.headers());
    let max_bytes = if status.is_success() {
        max_success_bytes
    } else {
        max_error_bytes
    };
    let body = read_bounded_body(&mut response, control, max_bytes).await?;
    ensure_active(control)?;
    Ok(HttpResponse {
        status,
        body,
        claims_challenge,
    })
}

fn ensure_active(control: &OperationControl) -> Result<(), HttpError> {
    if control.is_cancelled() {
        return Err(HttpError::new(HttpErrorKind::Cancelled));
    }
    if Instant::now() >= control.deadline() {
        return Err(HttpError::new(HttpErrorKind::TimedOut));
    }
    Ok(())
}

async fn select_with_control<T>(
    control: &OperationControl,
    future: impl std::future::Future<Output = Result<T, reqwest::Error>>,
) -> Result<T, HttpError> {
    tokio::pin!(future);
    let deadline = tokio::time::sleep_until(TokioInstant::from_std(control.deadline()));
    tokio::pin!(deadline);
    let cancelled = control.cancelled();
    tokio::pin!(cancelled);

    tokio::select! {
        biased;
        _ = &mut cancelled => Err(HttpError::new(HttpErrorKind::Cancelled)),
        _ = &mut deadline => Err(HttpError::new(HttpErrorKind::TimedOut)),
        result = &mut future => result.map_err(map_reqwest_error),
    }
}

async fn read_bounded_body(
    response: &mut Response,
    control: &OperationControl,
    max_bytes: usize,
) -> Result<HttpBody, HttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(HttpError::new(HttpErrorKind::ResponseTooLarge));
    }

    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let next = select_with_control(control, response.chunk()).await?;
        match next {
            Some(chunk) => {
                if bytes.len().saturating_add(chunk.len()) > max_bytes {
                    return Err(HttpError::new(HttpErrorKind::ResponseTooLarge));
                }
                bytes.extend_from_slice(&chunk);
            }
            None => break,
        }
    }
    Ok(HttpBody(bytes))
}

fn has_claims_challenge(headers: &HeaderMap) -> bool {
    headers.get_all(WWW_AUTHENTICATE).iter().any(|value| {
        value.to_str().ok().is_some_and(|text| {
            let lowered = text.to_ascii_lowercase();
            lowered.contains("insufficient_claims") || lowered.contains("claims=")
        })
    })
}

fn map_reqwest_error(error: reqwest::Error) -> HttpError {
    if error.is_timeout() {
        return HttpError::new(HttpErrorKind::TimedOut);
    }
    HttpError::new(HttpErrorKind::Network)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OperationControl;
    use reqwest::dns::{Addrs, Name, Resolving};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;

    fn spawn_server(
        responder: impl Fn(TcpStream) + Send + Sync + 'static,
    ) -> (String, mpsc::Sender<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let responder = Arc::new(responder);
        let responder_clone = Arc::clone(&responder);
        let (stop_tx, stop_rx) = mpsc::channel();
        thread::spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    responder_clone(stream);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        });
        (format!("http://{address}"), stop_tx)
    }

    fn read_request(stream: &mut TcpStream) {
        let mut bytes = [0_u8; 2048];
        let _ = stream.read(&mut bytes);
    }

    struct StalledResolver {
        notify: Arc<tokio::sync::Notify>,
        started: mpsc::Sender<()>,
    }

    impl Resolve for StalledResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let notify = Arc::clone(&self.notify);
            let started = self.started.clone();
            Box::pin(async move {
                let _ = started.send(());
                notify.notified().await;
                let addrs: Addrs = Box::new([SocketAddr::from(([127, 0, 0, 1], 9))].into_iter());
                Ok(addrs)
            })
        }
    }

    #[test]
    fn http_client_cancels_during_async_dns_resolution_without_hanging_runtime_drop() {
        let notify = Arc::new(tokio::sync::Notify::new());
        let (started_tx, started_rx) = mpsc::channel();
        let client = HttpClient::new_for_test_with_resolver(StalledResolver {
            notify: Arc::clone(&notify),
            started: started_tx,
        })
        .unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(2)).unwrap();
        let cancel = control.clone();
        let started = Instant::now();
        let cancellation = thread::spawn(move || {
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            cancel.cancel();
        });
        let error = client
            .get(
                "http://stalled-resolution.invalid/",
                "token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap_err();
        drop(client);
        cancellation.join().unwrap();
        assert_eq!(error.kind(), HttpErrorKind::Cancelled);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    struct RuntimeBoundResolver {
        address: SocketAddr,
        background: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        wake: Arc<tokio::sync::Notify>,
    }

    impl Resolve for RuntimeBoundResolver {
        fn resolve(&self, _name: Name) -> Resolving {
            let address = self.address;
            let mut background = self.background.lock().unwrap();
            if let Some(completion) = background.take() {
                self.wake.notify_one();
                return Box::pin(async move {
                    completion.await?;
                    let addrs: Addrs = Box::new([address].into_iter());
                    Ok(addrs)
                });
            }
            let (finished, completion) = tokio::sync::oneshot::channel();
            *background = Some(completion);
            let wake = Arc::clone(&self.wake);
            tokio::spawn(async move {
                wake.notified().await;
                let _ = finished.send(());
            });
            Box::pin(async move {
                let addrs: Addrs = Box::new([address].into_iter());
                Ok(addrs)
            })
        }
    }

    #[test]
    fn http_client_reuses_runtime_for_dns_state_across_requests() {
        let (base, stop) = spawn_server(|mut stream| {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            read_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let address: SocketAddr = base.strip_prefix("http://").unwrap().parse().unwrap();
        let client = HttpClient::new_for_test_with_resolver(RuntimeBoundResolver {
            address,
            background: Mutex::new(None),
            wake: Arc::new(tokio::sync::Notify::new()),
        })
        .unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(5)).unwrap();
        for host in ["first.invalid", "second.invalid"] {
            let response = client.get(
                &format!("http://{host}:{}/", address.port()),
                "fixture-token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            );
            if let Err(error) = response {
                stop.send(()).unwrap();
                panic!("runtime-bound DNS request failed: {error}");
            }
        }
        stop.send(()).unwrap();
    }

    #[test]
    fn http_client_cancels_while_waiting_for_headers() {
        let (base, stop) = spawn_server(|mut stream| {
            read_request(&mut stream);
            thread::sleep(Duration::from_millis(250));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });
        let client = HttpClient::new_for_test().unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(2)).unwrap();
        let cancel = control.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let error = client
            .get(
                &format!("{base}/headers"),
                "token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap_err();
        assert_eq!(error.kind(), HttpErrorKind::Cancelled);
        stop.send(()).unwrap();
    }

    #[test]
    fn http_client_enforces_subsecond_deadlines() {
        let (base, stop) = spawn_server(|mut stream| {
            read_request(&mut stream);
            thread::sleep(Duration::from_millis(150));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });
        let client = HttpClient::new_for_test().unwrap();
        let control = OperationControl::with_timeout(Duration::from_millis(50)).unwrap();
        let error = client
            .get(
                &format!("{base}/deadline"),
                "token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap_err();
        assert_eq!(error.kind(), HttpErrorKind::TimedOut);
        stop.send(()).unwrap();
    }

    #[test]
    fn http_client_cancels_during_chunked_body_reads() {
        let (base, stop) = spawn_server(|mut stream| {
            read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            write!(stream, "5\r\nhello\r\n").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(250));
            let _ = write!(stream, "0\r\n\r\n");
        });
        let client = HttpClient::new_for_test().unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(2)).unwrap();
        let cancel = control.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        let error = client
            .get(
                &format!("{base}/chunked"),
                "token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap_err();
        assert_eq!(error.kind(), HttpErrorKind::Cancelled);
        stop.send(()).unwrap();
    }

    #[test]
    fn http_client_rejects_large_chunked_bodies_before_unbounded_allocation() {
        let (base, stop) = spawn_server(|mut stream| {
            read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            write!(stream, "6\r\nabcdef\r\n0\r\n\r\n").unwrap();
            stream.flush().unwrap();
        });
        let client = HttpClient::new_for_test().unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(2)).unwrap();
        let error = client
            .get(
                &format!("{base}/large"),
                "token",
                &control,
                5,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap_err();
        assert_eq!(error.kind(), HttpErrorKind::ResponseTooLarge);
        stop.send(()).unwrap();
    }

    #[test]
    fn http_client_marks_claims_challenges_from_www_authenticate() {
        let (base, stop) = spawn_server(|mut stream| {
            read_request(&mut stream);
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer error=\"insufficient_claims\", claims=\"opaque\"\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
            stream.flush().unwrap();
        });
        let client = HttpClient::new_for_test().unwrap();
        let control = OperationControl::with_timeout(Duration::from_secs(2)).unwrap();
        let response = client
            .get(
                &format!("{base}/claims"),
                "token",
                &control,
                DEFAULT_MAX_RESPONSE_BYTES,
                DEFAULT_MAX_ERROR_BYTES,
            )
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.claims_challenge());
        assert!(!format!("{response:?}").contains("opaque"));
        stop.send(()).unwrap();
    }
}
