//! The `tests` module of `src/livekit/room_service.rs`, which declares this
//! file by path, so private items are in scope through `super`.

use super::*;
use axum::{
    Router,
    http::{HeaderMap, StatusCode, Uri, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::{Arc, Mutex};

const STUB_API_KEY: &str = "devkey";
const STUB_API_SECRET: &str = "devsecret";
const NOW: u64 = 1_700_000_000;

/// One request as the stub saw it.
struct Seen {
    path: String,
    bearer: Option<String>,
    body: serde_json::Value,
}

/// A RoomService that gives every request the same answer and remembers what
/// it was asked. The URL is `ws://` because that is what an operator
/// configures, so `delete_room` has to map it to HTTP before the request can
/// arrive here.
struct RoomService {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl RoomService {
    async fn start(status: u16, reply: &'static str) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let app = Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: String| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded.lock().unwrap().push(Seen {
                    path: uri.path().to_string(),
                    bearer: headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.strip_prefix("Bearer "))
                        .map(str::to_string),
                    body: serde_json::from_str(&body).unwrap_or_default(),
                });
                (
                    StatusCode::from_u16(status).unwrap(),
                    [(header::CONTENT_TYPE, "application/json")],
                    reply,
                )
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, seen }
    }

    async fn delete(&self, room: &str) -> Result<(), String> {
        delete_room(&self.url, STUB_API_KEY, STUB_API_SECRET, room, NOW)
            .await
            .map_err(|error| error.to_string())
    }

    fn requests(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

/// The `video` grant of a token this project's secret signed, or `None` when
/// the issuer or the signature is wrong.
fn verified_video_grant(token: &str) -> Option<serde_json::Value> {
    if crate::token::livekit_token_issuer(token).as_deref() != Some(STUB_API_KEY) {
        return None;
    }
    let (signing_input, signature) = token.rsplit_once('.')?;
    if !crate::token::verify_hs256(STUB_API_SECRET, signing_input, signature) {
        return None;
    }
    let (_, payload) = signing_input.split_once('.')?;
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims.get("video").cloned()
}

/// LiveKit gates DeleteRoom on `roomCreate`, so that is the grant the call has
/// to carry. `roomAdmin` is pinned absent because it is the grant a caller
/// would reach for by habit, and it would let the cleanup path evict people.
#[tokio::test]
async fn deleting_a_room_posts_delete_room_under_a_room_create_token() {
    let service = RoomService::start(200, "{}").await;

    service.delete("interview-abc12345").await.unwrap();

    let requests = service.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.path, "/twirp/livekit.RoomService/DeleteRoom");
    assert_eq!(
        request.body,
        serde_json::json!({ "room": "interview-abc12345" })
    );

    let token = request.bearer.as_deref().expect("a bearer token");
    let video = verified_video_grant(token).expect("signed by this project");
    assert_eq!(video["roomCreate"], serde_json::json!(true));
    assert_eq!(video["roomAdmin"], serde_json::Value::Null);
}

/// Cleanup runs after the fact and LiveKit drops empty rooms on its own, so a
/// room that is already gone is the ordinary outcome, not a failure.
#[tokio::test]
async fn a_room_already_gone_counts_as_deleted() {
    let service = RoomService::start(
        404,
        r#"{"code":"not_found","msg":"requested room does not exist"}"#,
    )
    .await;

    assert_eq!(service.delete("interview-abc12345").await, Ok(()));
}

/// Anything else is an error that says which call failed, how, and what
/// LiveKit said. A 404 without the Twirp code is in this group: it is what a
/// wrong host or path answers, and forgetting the room then would leak it.
#[tokio::test]
async fn any_other_refusal_is_an_error_that_names_the_call() {
    let failing = RoomService::start(500, r#"{"code":"internal","msg":"boom"}"#).await;
    let error = failing.delete("interview-abc12345").await.unwrap_err();
    assert!(error.contains("DeleteRoom"), "{error}");
    assert!(error.contains("500"), "{error}");
    assert!(error.contains("boom"), "{error}");

    let wrong_route = RoomService::start(404, "404 page not found").await;
    let error = wrong_route.delete("interview-abc12345").await.unwrap_err();
    assert!(error.contains("404"), "{error}");
}

/// A proxy can answer 200 with an HTML page while the room is still there, so
/// a success that is not JSON is not a deleted room.
#[tokio::test]
async fn a_html_success_is_not_a_deleted_room() {
    let service = RoomService::start(200, "<html>ok</html>").await;
    let error = service.delete("interview-abc12345").await.unwrap_err();
    assert!(error.contains("DeleteRoom"), "{error}");
    assert!(error.contains("invalid JSON"), "{error}");
}

#[test]
fn a_room_is_gone_only_on_a_twirp_not_found() {
    let not_found = reqwest::StatusCode::NOT_FOUND;
    assert!(is_twirp_not_found(
        not_found,
        r#"{"code":"not_found","msg":"x"}"#
    ));
    assert!(!is_twirp_not_found(not_found, "not_found"));
    assert!(!is_twirp_not_found(not_found, r#"{"code":"internal"}"#));
    assert!(!is_twirp_not_found(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        r#"{"code":"not_found"}"#
    ));
}
