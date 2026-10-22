use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn mock(responses: Vec<String>) -> (Api, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = Api::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let len: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")?
                                .parse()
                                .ok()
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    (api, task)
}

fn response(status: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn retries_503_retry_after_preserving_form_and_authorization() {
    let (api, task) = mock(vec![
        response(503, "Retry-After: 1\r\n", "provisioning"),
        response(
            503,
            "Retry-After: 0\r\n",
            r#"{"error":"temporarily_unavailable"}"#,
        ),
        response(200, "", r#"{"ok":true}"#),
    ])
    .await;
    let start = tokio::time::Instant::now();
    let result = api
        .request(
            Method::POST,
            "/device/token",
            Some("secret"),
            json!({"grant_type":"refresh_token","refresh_token":"private"}),
        )
        .await
        .unwrap();
    assert_eq!(result, json!({"ok":true}));
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "Retry-After was ignored"
    );
    let requests = task.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r == &requests[0]));
    assert!(requests[0].contains("application/x-www-form-urlencoded"));
    assert!(requests[0].contains("authorization: Bearer secret"));
    assert!(requests[0].contains("grant_type=refresh_token"));
}

#[tokio::test]
async fn protocol_errors_are_safe_and_meaningful() {
    for code in [
        "temporarily_unavailable",
        "rate_limited",
        "quota_exceeded",
        "key_in_use",
        "revocation_list_full",
        "unsupported_grant_type",
        "secret_untrusted_text",
    ] {
        let (api, task) = mock(vec![response(
            400,
            "",
            &json!({"error":code,"details":"private"}).to_string(),
        )])
        .await;
        let error = api
            .request(Method::POST, "/device/token", None, json!({}))
            .await
            .unwrap_err()
            .to_string();
        let expected = if code == "secret_untrusted_text" {
            "request_failed"
        } else {
            code
        };
        assert_eq!(error, format!("HTTP 400 {expected}"));
        assert!(!error.contains("private"));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn does_not_retry_other_statuses_or_invalid_retry_after() {
    for (status, header) in [
        (503, ""),
        (503, "Retry-After: garbage\r\n"),
        (400, "Retry-After: 0\r\n"),
        (429, "Retry-After: 0\r\n"),
    ] {
        let (api, task) = mock(vec![response(
            status,
            header,
            r#"{"error":"temporarily_unavailable"}"#,
        )])
        .await;
        let error = api
            .request(
                Method::POST,
                "/v1/agents",
                Some("secret"),
                json!({"id":"bot"}),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("HTTP {status} temporarily_unavailable")
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[test]
fn retry_after_supports_dates_and_caps_each_attempt() {
    assert_eq!(retry_delay("600"), Some(Duration::from_secs(10)));
    assert_eq!(retry_delay("0"), Some(Duration::ZERO));
    let future = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(600));
    assert_eq!(retry_delay(&future), Some(Duration::from_secs(10)));
    assert_eq!(
        retry_delay("Thu, 01 Jan 1970 00:00:00 GMT"),
        Some(Duration::ZERO)
    );
    assert_eq!(retry_delay("invalid"), None);
}

#[tokio::test]
async fn retry_wait_cannot_exceed_total_deadline() {
    let (api, task) = mock(vec![response(
        503,
        "Retry-After: 600\r\n",
        r#"{"error":"temporarily_unavailable"}"#,
    )])
    .await;
    let deadline = Instant::now() + Duration::from_secs(1);
    let error = api
        .request_until(Method::POST, "/v1/agents", None, json!({}), deadline)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "HTTP 503 temporarily_unavailable");
    assert!(Instant::now() < deadline);
    assert_eq!(task.await.unwrap().len(), 1);
}
