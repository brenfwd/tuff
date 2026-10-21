use super::*;

/// Rebinds the main HTTP listener whenever `tuff.http.host`/`tuff.http.port`
/// change, so `/tuff config` takes effect without a restart. Ends on its own
/// once the HTTP actor is gone (shutdown dropped its channel).
pub(super) async fn rebind_http_on_config_change(
    shared: Arc<Shared>,
    serve: bool,
    endpoints: Option<Vec<crate::public_ip::Endpoint>>,
) {
    let mut changes = shared.config.subscribe();
    loop {
        if changes.changed().await.is_err() {
            return;
        }
        let key = changes
            .borrow_and_update()
            .as_ref()
            .map(|change| change.key.clone());
        let Some(key) = key else { continue };
        if crate::http::is_bind_key(&key) {
            if !serve {
                continue;
            }
            let config = crate::http::config_from_store(&shared.config);
            if shared
                .http
                .bind(tuff_http::MAIN_LISTENER, config)
                .await
                .is_err()
            {
                return;
            }
            reconfigure_base_url(&shared, &endpoints).await;
            tuff_web_ui::announce(&*shared).await;
        } else if key == tuff_web_ui::ENABLED
            || key == tuff_web_ui::ALLOW_REMOTE
            || key == tuff_web_ui::TLS_TERMINATED
            || key == tuff_web_ui::ACK_PLAINTEXT
        {
            tuff_web_ui::announce(&*shared).await;
        } else if key == crate::config::HTTP_RESOURCE_BASE_URL
            || key == crate::config::HTTP_DETECT_PUBLIC_IP
        {
            reconfigure_base_url(&shared, &endpoints).await;
        }
    }
}

pub(super) async fn reconfigure_base_url(
    shared: &Shared,
    endpoints: &Option<Vec<crate::public_ip::Endpoint>>,
) {
    let Some(bound) = shared
        .http
        .addrs(tuff_http::MAIN_LISTENER)
        .await
        .into_iter()
        .next()
    else {
        return;
    };
    let config = crate::public_ip::config_from_store(&shared.config, endpoints.as_deref());
    shared.public_base_url.configure(config, bound);
}
