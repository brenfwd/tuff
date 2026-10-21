use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

fn profile() -> GameProfile {
    GameProfile {
        id: 0x1234,
        name: "Alex".into(),
        properties: Vec::new(),
    }
}

/// Serves each connection one canned response and records the request text.
async fn serve(response: Vec<u8>, hang: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    serve_with(Arc::new(move |_: &str| response.clone()), hang).await
}

type Route = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

async fn serve_with(route: Route, hang: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let route = route.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                let mut len = 0;
                let response: Vec<u8>;
                loop {
                    let n = stream.read(&mut buf[len..]).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    len += n;
                    let text = String::from_utf8_lossy(&buf[..len]).into_owned();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let want = head
                            .to_ascii_lowercase()
                            .split("content-length: ")
                            .nth(1)
                            .and_then(|r| r.split("\r\n").next()?.parse::<usize>().ok())
                            .unwrap_or(0);
                        if body.len() >= want {
                            response = route(&text);
                            log.lock().unwrap().push(text);
                            break;
                        }
                    }
                }
                if hang {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                }
                let _ = stream.write_all(&response).await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn ok(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn filter(server: &str, extra: &str) -> TextFilter {
    let config = format!(r#"{{"apiServer":"{server}","apiKey":"secret"{extra}}}"#);
    TextFilter::from_properties(&config, 0).unwrap().unwrap()
}

#[tuff_core::watchdog]
#[test]
fn an_empty_config_is_off_and_bad_configs_say_why() {
    assert!(TextFilter::from_properties("  ", 0).unwrap().is_none());
    assert!(TextFilter::from_properties("{", 0).is_err());
    assert!(TextFilter::from_properties(r#"{"apiServer":"http://x","apiKey":""}"#, 0).is_err());
    assert!(TextFilter::from_properties(r#"{"apiServer":"ftp://x","apiKey":"k"}"#, 0).is_err());
    assert!(TextFilter::from_properties(r#"{"apiServer":"http://x","apiKey":"k"}"#, 1).is_err());
    assert!(
        TextFilter::from_properties(&safety_config("http://x", "\"nope.p12\"", ""), 1).is_err()
    );
    assert!(TextFilter::from_properties(r#"{"apiServer":"http://x","apiKey":"k"}"#, 7).is_err());
}

#[tuff_core::watchdog]
#[test]
fn masks_hide_by_utf16_unit_and_fully_filtered_hides_all() {
    let mask = FilterMask::Partial(vec![false, true, true, false, false, true]);
    assert_eq!(mask.apply("abcdef").as_deref(), Some("a##de#"));
    assert_eq!(mask.apply("a").as_deref(), Some("a"));
    let astral = FilterMask::Partial(vec![false, false, true, false]);
    assert_eq!(
        astral.apply("\u{1F600}b\u{1F600}").as_deref(),
        Some("\u{1F600}#\u{1F600}")
    );
    assert_eq!(FilterMask::FullyFiltered.apply("x"), None);
    assert_eq!(FilterMask::PassThrough.apply("x").as_deref(), Some("x"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_clean_answer_passes_and_the_request_carries_auth_and_the_message() {
    let (url, seen) = serve(ok(r#"{"response":true}"#), false).await;
    let mask = filter(&url, "").filter(&profile(), "hello").await;
    assert_eq!(mask, FilterMask::PassThrough);
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.starts_with("POST /v1/chat "), "{request}");
    assert!(request.contains("Basic c2VjcmV0") || request.contains("basic c2VjcmV0"));
    assert!(request.contains(r#""text":"hello""#));
    assert!(request.contains("00000000-0000-0000-0000-000000001234"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn hashes_become_a_partial_mask_and_out_of_range_indexes_are_ignored() {
    let (url, _) = serve(
        ok(r##"{"response":false,"hashed":"h###o","hashes":[1,2,3,99]}"##),
        false,
    )
    .await;
    let mask = filter(&url, "").filter(&profile(), "hello").await;
    assert_eq!(mask.apply("hello").as_deref(), Some("h###o"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn hashes_to_drop_hides_the_whole_message_over_the_threshold() {
    let (url, _) = serve(ok(r#"{"hashed":"x","hashes":[0,1]}"#), false).await;
    let mask = filter(&url, r#","hashesToDrop":2"#)
        .filter(&profile(), "hello")
        .await;
    assert_eq!(mask, FilterMask::FullyFiltered);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_missing_hashed_field_hides_everything() {
    let (url, _) = serve(ok(r#"{"response":false}"#), false).await;
    assert_eq!(
        filter(&url, "").filter(&profile(), "hi").await,
        FilterMask::FullyFiltered
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn http_errors_garbage_negative_indexes_and_oversize_bodies_hide_everything() {
    let error = b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
    let (url, _) = serve(error, false).await;
    assert_eq!(
        filter(&url, "").filter(&profile(), "hi").await,
        FilterMask::FullyFiltered
    );
    let (url, _) = serve(ok("not json"), false).await;
    assert_eq!(
        filter(&url, "").filter(&profile(), "hi").await,
        FilterMask::FullyFiltered
    );
    let (url, _) = serve(ok(r#"{"hashed":"x","hashes":[-1]}"#), false).await;
    assert_eq!(
        filter(&url, "").filter(&profile(), "hi").await,
        FilterMask::FullyFiltered
    );
    let huge = format!(
        r#"{{"response":true,"pad":"{}"}}"#,
        "a".repeat(MAX_RESPONSE_BYTES)
    );
    let (url, _) = serve(ok(&huge), false).await;
    assert_eq!(
        filter(&url, "").filter(&profile(), "hi").await,
        FilterMask::FullyFiltered
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_silent_service_times_out_and_hides_the_message() {
    let (url, _) = serve(ok("{}"), true).await;
    let started = std::time::Instant::now();
    let mask = filter(&url, r#","connectionReadTimeoutMs":300"#)
        .filter(&profile(), "hi")
        .await;
    assert_eq!(mask, FilterMask::FullyFiltered);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn an_unreachable_service_hides_the_message_and_empty_text_skips_the_call() {
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", free.local_addr().unwrap());
    drop(free);
    let f = filter(&url, "");
    assert_eq!(f.filter(&profile(), "hi").await, FilterMask::FullyFiltered);
    assert_eq!(f.filter(&profile(), "").await, FilterMask::PassThrough);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn join_and_leave_post_to_their_endpoints() {
    let (url, seen) = serve(ok(""), false).await;
    let f = filter(&url, r#","endpoints":{"join":"j","chat":"c/new"}"#);
    f.player_joined(&profile()).await;
    f.player_left(&profile()).await;
    let seen = seen.lock().unwrap();
    assert!(seen[0].starts_with("POST /j "));
    assert!(seen[1].starts_with("POST /v1/leave "));
    assert!(seen[0].contains(r#""user_display_name":"Alex""#));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_non_legacy_chat_endpoint_uses_the_newer_body() {
    let (url, seen) = serve(ok(r#"{"response":true}"#), false).await;
    filter(&url, r#","endpoints":{"chat":"c/new"},"ruleId":4"#)
        .filter(&profile(), "hi")
        .await;
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.starts_with("POST /c/new "));
    assert!(request.contains(r#""rule_id":"4""#));
}

fn testdata(file: &str) -> String {
    format!(
        "{}/src/text_filter/testdata/{file}",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn safety_config(server: &str, certificate: &str, extra: &str) -> String {
    format!(
        r#"{{"apiServer":"{server}","apiPath":"/safety/chat","scope":"api://filter/.default",
        "applicationId":"app-1","tenantId":"tenant-1","certificatePath":{certificate},
        "fullyFilteredEvents":["slur"]{extra}}}"#
    )
}

fn safety_filter(server: &str, certificate: &str) -> TextFilter {
    TextFilter::from_safety_config(
        &safety_config(server, certificate, ""),
        &format!("{server}/"),
    )
    .unwrap()
}

async fn safety_server(chat: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    serve_with(
        Arc::new(move |request: &str| {
            if request.starts_with("POST /tenant-1/oauth2/v2.0/token ") {
                ok(r#"{"token_type":"Bearer","expires_in":3599,"access_token":"tok123"}"#)
            } else {
                ok(chat)
            }
        }),
        false,
    )
    .await
}

fn jwt_part(jwt: &str, index: usize) -> Value {
    let part = jwt.split('.').nth(index).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(part)
            .unwrap(),
    )
    .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn version_one_signs_in_with_a_certificate_and_sends_the_bearer_token() {
    use rsa::signature::Verifier;
    let (url, seen) = safety_server(r#"{"result":{"filtered":false}}"#).await;
    let f = safety_filter(&url, &format!("{:?}", testdata("client.pem")));
    assert_eq!(f.filter(&profile(), "hello").await, FilterMask::PassThrough);
    assert_eq!(f.filter(&profile(), "again").await, FilterMask::PassThrough);
    f.player_joined(&profile()).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "one sign-in, two chats, no join call");
    let token_request = &seen[0];
    assert!(token_request.contains("grant_type=client_credentials"));
    assert!(token_request.contains("client_id=app-1"));
    assert!(token_request.contains("scope=api%3A%2F%2Ffilter%2F.default"));
    let assertion = token_request
        .split("client_assertion=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let header = jwt_part(assertion, 0);
    let claims = jwt_part(assertion, 1);
    assert_eq!(header["alg"], "RS256");
    assert_eq!(header["x5c"].as_array().unwrap().len(), 1);
    assert_eq!(claims["iss"], "app-1");
    assert_eq!(claims["aud"], format!("{url}/tenant-1/oauth2/v2.0/token"));
    let Service::Safety(safety) = &f.service else {
        panic!("not version 1")
    };
    let (input, signature) = assertion.rsplit_once('.').unwrap();
    let verifier = rsa::pkcs1v15::VerifyingKey::<sha2::Sha256>::new(safety.credential_public_key());
    let signature = rsa::pkcs1v15::Signature::try_from(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(signature)
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    verifier.verify(input.as_bytes(), &signature).unwrap();
    for chat in &seen[1..] {
        assert!(chat.starts_with("POST /safety/chat "), "{chat}");
        assert!(
            chat.to_ascii_lowercase()
                .contains("authorization: bearer tok123")
        );
        assert!(chat.contains(r#""area":"JavaChatRealms""#));
        assert!(chat.contains(r#""userDisplayName":"Alex""#));
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn version_one_reads_redactions_and_fully_filtered_events() {
    let pem = format!("{:?}", testdata("client.pem"));
    let (url, _) = safety_server(r#"{"result":{"filtered":true,"redactedTextIndex":[1,2]}}"#).await;
    let mask = safety_filter(&url, &pem).filter(&profile(), "hello").await;
    assert_eq!(mask.apply("hello").as_deref(), Some("h##lo"));
    let (url, _) = safety_server(
        r#"{"result":{"filtered":true,"events":[{"id":"slur"}],"redactedTextIndex":[1]}}"#,
    )
    .await;
    let mask = safety_filter(&url, &pem).filter(&profile(), "hello").await;
    assert_eq!(mask, FilterMask::FullyFiltered);
    let (url, _) = safety_server(r#"{"nothing":1}"#).await;
    let mask = safety_filter(&url, &pem).filter(&profile(), "hello").await;
    assert_eq!(mask, FilterMask::FullyFiltered);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_failed_sign_in_hides_the_message() {
    let (url, seen) = serve(
        b"HTTP/1.1 401 No\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        false,
    )
    .await;
    let f = safety_filter(&url, &format!("{:?}", testdata("client.pem")));
    assert_eq!(f.filter(&profile(), "hi").await, FilterMask::FullyFiltered);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "no chat call without a token"
    );
}

#[tuff_core::watchdog]
#[test]
fn pkcs12_archives_load_with_their_password_only() {
    let path = std::path::PathBuf::from(testdata("client.p12"));
    let credential = safety::load_credential(&path, "hunter2").unwrap();
    let pem = safety::load_credential(std::path::Path::new(&testdata("client.pem")), "").unwrap();
    assert_eq!(credential, pem);
    assert!(safety::load_credential(&path, "wrong").is_err());
    assert!(
        safety::credential_from_pem(
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
        )
        .is_err()
    );
}
