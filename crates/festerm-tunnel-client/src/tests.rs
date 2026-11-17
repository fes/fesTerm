use super::*;

#[test]
fn native_identity_diagnostic_classifies_only_typed_transport_flags_and_status() {
    for (flags, expected) in [
        ((true, true, true), IdentityHttpCause::SendTimeout),
        ((false, true, true), IdentityHttpCause::SendConnect),
        ((false, false, true), IdentityHttpCause::SendRequest),
        ((false, false, false), IdentityHttpCause::SendOther),
    ] {
        assert_eq!(
            IdentityHttpCause::send_flags(flags.0, flags.1, flags.2),
            expected
        );
    }
    for (oauth, status, expected) in [
        (true, 200, IdentityHttpCause::Body),
        (true, 400, IdentityHttpCause::Body),
        (false, 200, IdentityHttpCause::Body),
        (false, 400, IdentityHttpCause::HttpStatus),
        (true, 403, IdentityHttpCause::HttpStatus),
        (true, 429, IdentityHttpCause::HttpStatus),
        (true, 503, IdentityHttpCause::HttpStatus),
    ] {
        assert_eq!(IdentityHttpCause::after_headers(oauth, status), expected);
    }
}

#[test]
fn native_identity_diagnostic_fallback_preserves_stage_without_duplicate_delivery() {
    for (stage, expected) in [
        (IdentityHttpStage::DeviceGrant, "device-grant"),
        (IdentityHttpStage::DeviceToken, "device-token"),
        (IdentityHttpStage::Account, "github-account"),
        (IdentityHttpStage::Refresh, "github-refresh"),
    ] {
        let mut captured = String::new();
        identity_log_fallback(
            false,
            stage,
            IdentityHttpCause::HttpStatus,
            Some(503),
            |record| {
                use std::fmt::Write;
                write!(captured, "{record}").unwrap();
            },
        );
        assert_eq!(
            captured,
            format!(
            "Native GitHub request failed stage={expected} cause=http-status http_status=Some(503)"
        )
        );
        identity_log_fallback(true, stage, IdentityHttpCause::Body, None, |_| {
            panic!("a tracing subscriber already owns delivery")
        });
    }
}

#[tokio::test]
async fn native_identity_diagnostic_never_formats_private_request_error_details() {
    // HTTPS-only refusal occurs before any network request. The fixture error
    // carries an owned private-looking URL; production reads its type flags only.
    let error = Https::new()
        .unwrap()
        .0
        .get("http://fixture.invalid/PRIVATE_DEVICE?code=PRIVATE_CODE")
        .send()
        .await
        .unwrap_err();
    assert!(error.is_builder());
    assert!(error
        .url()
        .is_some_and(|url| url.as_str().contains("PRIVATE_DEVICE")));
    let cause =
        IdentityHttpCause::send_flags(error.is_timeout(), error.is_connect(), error.is_request());
    let mut captured = String::new();
    identity_log_fallback(
        false,
        IdentityHttpStage::DeviceToken,
        cause,
        None,
        |record| {
            use std::fmt::Write;
            write!(captured, "{record}").unwrap();
        },
    );
    assert_eq!(
        captured,
        "Native GitHub request failed stage=device-token cause=send-other http_status=None"
    );
    assert!(!captured.contains("PRIVATE") && !captured.contains("fixture.invalid"));
}

#[test]
fn native_http_requests_use_service_app_device_contracts_and_sensitive_account_headers() {
    let http = Https::new().unwrap();
    for (operation, endpoint, expected) in [
        (
            IdentityRequest::Device {
                client_id: identity::SERVICE_CLIENT_ID,
            },
            "https://github.com/login/device/code",
            vec![("client_id", identity::SERVICE_CLIENT_ID)],
        ),
        (
            IdentityRequest::Poll {
                client_id: identity::SERVICE_CLIENT_ID,
                device_code: "PRIVATE_DEVICE",
            },
            "https://github.com/login/oauth/access_token",
            vec![
                ("client_id", identity::SERVICE_CLIENT_ID),
                ("device_code", "PRIVATE_DEVICE"),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        ),
        (
            IdentityRequest::Refresh {
                client_id: identity::SERVICE_CLIENT_ID,
                refresh_token: "PRIVATE_REFRESH",
            },
            "https://github.com/login/oauth/access_token",
            vec![
                ("client_id", identity::SERVICE_CLIENT_ID),
                ("refresh_token", "PRIVATE_REFRESH"),
                ("grant_type", "refresh_token"),
            ],
        ),
    ] {
        let (oauth, request) = http.build_request(operation).unwrap();
        assert!(oauth);
        let request = request.build().unwrap();
        assert_eq!(request.url().as_str(), endpoint);
        assert_eq!(request.method(), reqwest::Method::POST);
        let fields: std::collections::BTreeMap<_, _> =
            url::form_urlencoded::parse(request.body().unwrap().as_bytes().unwrap())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
        assert!(!fields.contains_key("client_secret"));
        assert!(!fields.contains_key("scope"));
        assert_eq!(
            fields,
            expected
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect::<std::collections::BTreeMap<_, _>>()
        );
    }
    let (oauth, request) = http
        .build_request(IdentityRequest::Account {
            access_token: "PRIVATE_ACCESS",
        })
        .unwrap();
    assert!(!oauth);
    let request = request.build().unwrap();
    assert_eq!(request.url().as_str(), "https://api.github.com/user");
    let header = &request.headers()[reqwest::header::AUTHORIZATION];
    assert!(header.is_sensitive());
    assert!(header.as_bytes() == b"Bearer PRIVATE_ACCESS");
    assert!(!format!("{request:?}").contains("PRIVATE"));
}

#[tokio::test]
async fn native_http_body_bound_and_cancel_refuse_without_raw_service_details() {
    let response = reqwest::Response::from(
        http::Response::builder()
            .status(200)
            .body(vec![b'x'; RESPONSE_LIMIT + 1])
            .unwrap(),
    );
    assert_eq!(
        bounded_response(response, &CancellationToken::new())
            .await
            .unwrap_err(),
        Error::Response
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let response = reqwest::Response::from(
        http::Response::builder()
            .status(200)
            .body(b"PRIVATE_BODY".to_vec())
            .unwrap(),
    );
    assert_eq!(
        bounded_response(response, &cancel).await.unwrap_err(),
        Error::Cancelled
    );
    let response = reqwest::Response::from(
        http::Response::builder()
            .status(403)
            .body(b"PRIVATE_RAW_SERVICE_ERROR".to_vec())
            .unwrap(),
    );
    let error = bounded_response(response, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error, Error::Denied);
    assert!(!format!("{error:?} {error}").contains("PRIVATE"));
}
