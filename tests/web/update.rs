//! `/api/update`, through the router, against a stub GitHub.
//!
//! Split out of `tests/web.rs`, which is still the test target: cargo
//! discovers only `tests/*.rs`, so this compiles as a module of that one
//! binary rather than relinking the crate for a file of its own.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const LATEST: &str = "0123456789abcdef0123456789abcdef01234567";

/// A GitHub whose `latest` release names [`LATEST`], counting how often it is
/// asked.
async fn spawn_release_stub() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let app = axum::Router::new().route(
        "/repos/sysprog21/codetrial/releases/tags/latest",
        axum::routing::get(move || {
            let counted = Arc::clone(&counted);
            async move {
                counted.fetch_add(1, Ordering::Relaxed);
                axum::Json(serde_json::json!({ "target_commitish": LATEST }))
            }
        }),
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), asked, server)
}

async fn update_answer(base: &str) -> (reqwest::StatusCode, Option<String>, Value) {
    let response = http_client()
        .get(format!("{base}/api/update"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let cache = response
        .headers()
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, cache, response.json().await.unwrap())
}

/// Off is the default, and off asks GitHub nothing: the stub is a real
/// address the server could reach, so a request it made would be counted.
#[tokio::test]
async fn a_server_with_the_check_off_asks_github_nothing() {
    let (github, asked, github_server) = spawn_release_stub().await;
    let mut config = web_config();
    config.github_api_base_url = Some(github);
    assert_eq!(config.update_check_commit, None);
    let (base, server) = spawn_web_server(config).await;

    let (status, _, body) = update_answer(&base).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "enabled": false }));
    assert_eq!(asked.load(Ordering::Relaxed), 0);

    server.shutdown().await;
    github_server.abort();
}

#[tokio::test]
async fn a_server_with_the_check_on_offers_a_newer_release() {
    let (github, asked, github_server) = spawn_release_stub().await;
    let mut config = web_config();
    config.github_api_base_url = Some(github);
    config.update_check_commit = Some("abc1234".to_string());
    let (base, server) = spawn_web_server(config).await;

    let (status, cache, body) = update_answer(&base).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(
        body,
        serde_json::json!({
            "enabled": true,
            "available": true,
            "current": "abc1234",
            "latest": "0123456",
            "url": "https://github.com/sysprog21/codetrial/releases/tag/latest",
        })
    );

    // A second tab is answered from the same lookup.
    update_answer(&base).await;
    assert_eq!(asked.load(Ordering::Relaxed), 1);

    server.shutdown().await;
    github_server.abort();
}
