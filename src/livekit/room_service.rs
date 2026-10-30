//! LiveKit's RoomService over Twirp, for any caller holding a project's URL,
//! API key and API secret.
//!
//! Split out of `rooms.rs`, where the transport read its credentials from
//! `AgentConfig`. The agent's duplicate handling still goes through it. Room
//! deletion for the web server, which has only a provider's credentials to
//! hand, is exercised by the tests and has no production caller yet.

use crate::config::livekit_http_base;
use crate::token::livekit_room_admin_token;

/// The Twirp POST every RoomService caller makes. Only the token and the base
/// differ, and both are strings; the body comes back with the status because a
/// refusal explains itself there and not in the code.
pub(super) async fn post_room_service(
    base: &str,
    token: &str,
    method: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, String), Box<dyn std::error::Error + Send + Sync>> {
    let response = crate::http_client()
        .post(format!("{base}/twirp/livekit.RoomService/{method}"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    Ok((status, response.text().await?))
}

/// A room-scoped call under a `roomAdmin` token, answered with LiveKit's parsed
/// reply.
pub(super) async fn livekit_room_service_request(
    url: &str,
    api_key: &str,
    api_secret: &str,
    room_name: &str,
    method: &str,
    body: serde_json::Value,
    now_seconds: u64,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    let token = livekit_room_admin_token(api_key, api_secret, room_name, now_seconds)?;
    let (status, text) = post_room_service(&livekit_http_base(url), &token, method, &body).await?;
    if !status.is_success() {
        return Err(format!("LiveKit RoomService {method} failed: {status} {text}").into());
    }
    parse_room_service_response(method, &text)
}

pub(super) fn parse_room_service_response(
    method: &str,
    text: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
    serde_json::from_str(text).map_err(|error| {
        format!("LiveKit RoomService {method} returned invalid JSON: {error}").into()
    })
}

/// Deletes a room, which disconnects everyone still in it.
///
/// A room that is already gone counts as deleted: cleanup runs after the fact,
/// and LiveKit drops an empty room on its own, so losing that race is the
/// ordinary case. A 2xx body still has to parse as JSON, because a reverse
/// proxy can answer 200 with an HTML page while the room is still there. Any
/// other refusal is an error carrying the method, the status and LiveKit's
/// body, the same shape as every other RoomService failure.
#[cfg(test)]
pub(crate) async fn delete_room(
    url: &str,
    api_key: &str,
    api_secret: &str,
    room_name: &str,
    now_seconds: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let token = crate::token::livekit_room_create_token(api_key, api_secret, now_seconds)?;
    let (status, text) = post_room_service(
        &livekit_http_base(url),
        &token,
        "DeleteRoom",
        &serde_json::json!({ "room": room_name }),
    )
    .await?;
    if is_twirp_not_found(status, &text) {
        return Ok(());
    }
    if !status.is_success() {
        return Err(format!("LiveKit RoomService DeleteRoom failed: {status} {text}").into());
    }
    parse_room_service_response("DeleteRoom", &text).map(|_| ())
}

/// Both the status and the Twirp code, because a bare 404 could also be a
/// wrong host or path answering with an HTML page, and that is a failure to
/// report rather than a room or participant to forget.
pub(super) fn is_twirp_not_found(status: reqwest::StatusCode, text: &str) -> bool {
    status == reqwest::StatusCode::NOT_FOUND
        && serde_json::from_str::<serde_json::Value>(text)
            .is_ok_and(|body| body["code"] == "not_found")
}

#[cfg(test)]
#[path = "../../tests/unit/livekit/room_service.rs"]
mod tests;
