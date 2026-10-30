use super::*;

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
