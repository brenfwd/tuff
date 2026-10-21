use super::*;

fn ip(last: u8) -> IpAddr {
    IpAddr::from([10, 0, 0, last])
}

fn op() -> Principal {
    Principal::Operator {
        uuid: 7,
        name: "Alex".into(),
    }
}

#[tuff_core::watchdog]
#[test]
fn codes_group_and_normalize() {
    let digits = new_code_digits();
    assert_eq!(digits.len(), 16);
    let grouped = group_code(&digits);
    assert_eq!(grouped.len(), 19);
    assert_eq!(normalize_code(&grouped).as_deref(), Some(digits.as_str()));
    assert_eq!(
        normalize_code("1234 5678 9012 3456").as_deref(),
        Some("1234567890123456")
    );
    assert_eq!(normalize_code("1234-5678-9012-345"), None);
    assert_eq!(normalize_code("1234-5678-9012-345x"), None);
    assert!(constant_eq("abc", "abc") && !constant_eq("abc", "abd") && !constant_eq("a", "ab"));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn codes_are_single_use_and_expire() {
    let limits = Limits {
        code_ttl: Duration::from_millis(100),
        ..Limits::default()
    };
    let auth = Auth::spawn(None, limits).await;
    let code = auth.new_code(op()).await.unwrap();
    let session = auth.exchange(&code, ip(1)).await.unwrap();
    assert_eq!(session.principal, op());
    assert_eq!(
        auth.exchange(&code, ip(2)).await.err(),
        Some(ExchangeError::Invalid)
    );
    match auth.authenticate(&session.id).await {
        Authenticated::Ok {
            principal, csrf, ..
        } => {
            assert_eq!(principal, op());
            assert_eq!(csrf, session.csrf);
        }
        other => panic!("{other:?}"),
    }
    let late = auth.new_code(Principal::Console).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        auth.exchange(&late, ip(3)).await.err(),
        Some(ExchangeError::Invalid)
    );
    assert_eq!(
        auth.authenticate(&"0".repeat(64)).await,
        Authenticated::Unknown
    );
    assert_eq!(auth.authenticate("../etc").await, Authenticated::Unknown);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn failures_back_off_and_rates_are_limited() {
    let auth = Auth::spawn(None, Limits::default()).await;
    assert_eq!(
        auth.exchange("0000-0000-0000-0000", ip(1)).await.err(),
        Some(ExchangeError::Invalid)
    );
    let code = auth.new_code(op()).await.unwrap();
    assert!(matches!(
        auth.exchange(&code, ip(1)).await,
        Err(ExchangeError::RateLimited(_))
    ));
    assert!(auth.exchange(&code, ip(2)).await.is_ok());
    for i in 0..10 {
        let _ = auth.exchange("bad", IpAddr::from([10, 1, 0, i])).await;
    }
    let limited = Limits {
        exchanges_per_ip: 2.0,
        ..Limits::default()
    };
    let auth = Auth::spawn(None, limited).await;
    let code = auth.new_code(op()).await.unwrap();
    let first = auth.new_code(op()).await.unwrap();
    assert!(auth.exchange(&first, ip(4)).await.is_ok());
    let second = auth.new_code(op()).await.unwrap();
    assert!(auth.exchange(&second, ip(4)).await.is_ok());
    assert!(matches!(
        auth.exchange(&code, ip(4)).await,
        Err(ExchangeError::RateLimited(_))
    ));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn sessions_persist_by_hash_in_a_folder() {
    let dir = tuff_storage::test_dir::TestDir::new();
    let files: Arc<dyn ConfigStore> = Arc::new(tuff_storage::vanilla::config::VanillaConfig::new(
        dir.path(),
    ));
    persists_and_logs_out(files).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn sessions_persist_by_hash_in_a_server_file() {
    use tuff_storage::backend::WorldBackend;
    let dir = tuff_storage::test_dir::TestDir::new();
    let backend =
        tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap();
    persists_and_logs_out(backend.config().unwrap()).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn actor_panic_restarts_and_keeps_sessions() {
    let auth = Auth::spawn(None, Limits::default()).await;
    let a = auth
        .exchange(&auth.new_code(op()).await.unwrap(), ip(1))
        .await
        .unwrap();
    auth.tx.send(Request::Panic).await.unwrap();
    auth.flush().await;
    assert_eq!(auth.session_count().await, 1);
    assert!(matches!(
        auth.authenticate(&a.id).await,
        Authenticated::Ok { .. }
    ));
    assert!(auth.new_code(op()).await.is_some());
}

async fn persists_and_logs_out(files: Arc<dyn ConfigStore>) {
    let auth = Auth::spawn(Some(files.clone()), Limits::default()).await;
    let a = auth
        .exchange(&auth.new_code(op()).await.unwrap(), ip(1))
        .await
        .unwrap();
    let b = auth
        .exchange(&auth.new_code(Principal::Console).await.unwrap(), ip(2))
        .await
        .unwrap();
    auth.flush().await;
    let mut stored = String::new();
    for _ in 0..100 {
        stored = files
            .read_file(SESSIONS_FILE, 1 << 20)
            .unwrap()
            .map(|b| String::from_utf8(b).unwrap())
            .unwrap_or_default();
        if stored.contains(&b.session) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        stored.contains(&a.session) && stored.contains(&b.session),
        "{stored}"
    );
    assert!(!stored.contains(&a.id) && !stored.contains(&a.csrf));

    let restarted = Auth::spawn(Some(files.clone()), Limits::default()).await;
    match restarted.authenticate(&a.id).await {
        Authenticated::Ok {
            principal, csrf, ..
        } => {
            assert_eq!(principal, op());
            assert_eq!(csrf, a.csrf);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(restarted.logout_principal(op()).await, 1);
    assert_eq!(restarted.authenticate(&a.id).await, Authenticated::Unknown);
    assert!(matches!(
        restarted.authenticate(&b.id).await,
        Authenticated::Ok { .. }
    ));
    assert_eq!(restarted.logout_all().await, 1);
    assert_eq!(restarted.session_count().await, 0);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tokens_persist_by_hash_and_revoke_their_sessions() {
    let dir = tuff_storage::test_dir::TestDir::new();
    let files: Arc<dyn ConfigStore> = Arc::new(tuff_storage::vanilla::config::VanillaConfig::new(
        dir.path(),
    ));
    let auth = Auth::spawn(Some(files.clone()), Limits::default()).await;
    let made = auth
        .create_token("ci".into(), PermissionLevel::Admins, None, "console".into())
        .await
        .unwrap();
    assert!(split_token(&made.secret).is_some());
    assert!(matches!(
        auth.authenticate_token(&made.secret, ip(1)).await,
        Authenticated::Ok { .. }
    ));
    assert_eq!(
        refusal(auth.authenticate_token("tuff_x_y", ip(1)).await),
        Some("malformed token")
    );
    let session = auth
        .exchange(&auth.new_code(made.principal.clone()).await.unwrap(), ip(1))
        .await
        .unwrap();
    auth.flush().await;
    let mut stored = String::new();
    for _ in 0..100 {
        stored = files
            .read_file(TOKENS_FILE, 1 << 20)
            .unwrap()
            .map(|b| String::from_utf8(b).unwrap())
            .unwrap_or_default();
        if stored.contains("lastUsed\":1") || stored.contains("\"ci\"") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let secret = made.secret.rsplit('_').next().unwrap();
    assert!(
        stored.contains("\"ci\"") && !stored.contains(secret),
        "{stored}"
    );

    let restarted = Auth::spawn(Some(files), Limits::default()).await;
    assert_eq!(restarted.tokens().await.len(), 1);
    let Principal::Token { id, .. } = &made.principal else {
        panic!("not a token");
    };
    assert!(restarted.revoke_token(id).await);
    assert_eq!(
        refusal(restarted.authenticate_token(&made.secret, ip(1)).await),
        Some("unknown token")
    );
    assert_eq!(
        restarted.authenticate(&session.id).await,
        Authenticated::Unknown
    );
    assert!(valid_token_name("a.b-c_1") && !valid_token_name("") && !valid_token_name("a b"));
}

#[tuff_core::watchdog]
#[test]
fn token_secrets_are_masked() {
    let token = format!("tuff_{}_{}", "a".repeat(16), "b".repeat(64));
    assert_eq!(
        mask_tokens(&format!("Token: {token}. tuff_x")),
        format!("Token: tuff_{}_<redacted>. tuff_x", "a".repeat(16))
    );
    assert!(matches!(
        mask_tokens("no tokens"),
        std::borrow::Cow::Borrowed(_)
    ));
    assert_eq!(mask_tokens("tuff_é_tuff_"), "tuff_é_tuff_");
}

fn refusal(found: Authenticated) -> Option<&'static str> {
    match found {
        Authenticated::Refused(refused) => Some(refused.reason),
        _ => None,
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn expired_tokens_and_their_sessions_stop_working() {
    let auth = Auth::spawn(None, Limits::default()).await;
    let past = unix_millis() - 1;
    let made = auth
        .create_token(
            "old".into(),
            PermissionLevel::Admins,
            Some(past),
            "c".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        refusal(auth.authenticate_token(&made.secret, ip(1)).await),
        Some("expired")
    );
    let session = auth
        .exchange(&auth.new_code(made.principal.clone()).await.unwrap(), ip(1))
        .await
        .unwrap();
    assert_eq!(auth.authenticate(&session.id).await, Authenticated::Unknown);
    assert_eq!(auth.session_count().await, 0);
    let listed = auth.tokens().await;
    assert!(listed[0].expired() && listed[0].expires == Some(past));

    let later = unix_millis() + 3_600_000;
    let fresh = auth
        .create_token("new".into(), PermissionLevel::All, Some(later), "c".into())
        .await
        .unwrap();
    assert!(matches!(
        auth.authenticate_token(&fresh.secret, ip(1)).await,
        Authenticated::Ok { .. }
    ));
    let mut wrong = fresh.secret.clone();
    let last = if wrong.ends_with('0') { "1" } else { "0" };
    wrong.replace_range(wrong.len() - 1.., last);
    let Authenticated::Refused(refused) = auth.authenticate_token(&wrong, ip(1)).await else {
        panic!("wrong secret accepted");
    };
    assert_eq!(refused.reason, "wrong secret");
    assert_eq!(refused.token, Some(fresh.principal.clone()));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn token_refusals_are_audited_at_a_bounded_rate() {
    let auth = Auth::spawn(None, Limits::default()).await;
    let mut audited = 0;
    for _ in 0..50 {
        let Authenticated::Refused(refused) = auth.authenticate_token("nope", ip(1)).await else {
            panic!("not refused");
        };
        audited += usize::from(refused.audit.is_some());
    }
    assert_eq!(audited, REFUSALS_AUDITED as usize);
    let Authenticated::Refused(other) = auth.authenticate_token("nope", ip(2)).await else {
        panic!("not refused");
    };
    assert_eq!(other.audit, Some(0));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn idle_sessions_expire_and_requests_are_throttled() {
    let limits = Limits {
        idle: Duration::from_millis(100),
        requests_per_sec: 0.0,
        request_burst: 2.0,
        ..Limits::default()
    };
    let auth = Auth::spawn(None, limits).await;
    let s = auth
        .exchange(&auth.new_code(op()).await.unwrap(), ip(1))
        .await
        .unwrap();
    assert!(matches!(
        auth.authenticate(&s.id).await,
        Authenticated::Ok { .. }
    ));
    assert!(matches!(
        auth.authenticate(&s.id).await,
        Authenticated::Ok { .. }
    ));
    assert_eq!(auth.authenticate(&s.id).await, Authenticated::RateLimited);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(auth.authenticate(&s.id).await, Authenticated::Unknown);
}
