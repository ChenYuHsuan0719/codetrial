//! Choosing a LiveKit project, and passing over one that cannot take the
//! interview.
//!
//! Split out of `tests/web.rs`, which is still the test target: cargo
//! discovers only `tests/*.rs`, so this compiles as a module of that one
//! binary rather than relinking the crate for a file of its own.

use super::*;

/// The property that matters is not that the web process picks a provider, it
/// is
/// that the agent process, which only ever receives the room name, resolves the
/// same one. Asserting that by calling the selector the handler itself uses
/// would pass for any implementation that is merely self-consistent, so this
/// walks the path the agent walks: read the id out of the room name, look it
/// up,
/// and check the token was signed with that provider's secret.
#[tokio::test]
async fn a_minted_room_name_routes_the_agent_to_the_provider_that_signed_it() {
    let (mut config, cookie, db_path) = signed_in_web_config("provider-routing");
    config.room_prefix = "interview".to_string();
    config.fixed_room_name = None;
    config.pool = codetrial::config::ProviderPool {
        providers: vec![
            provider(codetrial::config::PRIMARY_PROVIDER_ID, "primary"),
            provider("eu", "eu"),
            provider("us", "us"),
        ],
    };
    let pool = config.pool.clone();
    let (base, server) = spawn_web_server(config).await;
    let client = http_client();

    let mut served = Vec::new();
    for _ in 0..6 {
        let response = client
            .post(format!("{base}/api/token"))
            .header("cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.json::<Value>().await.unwrap();
        let room_name = body["roomName"].as_str().unwrap().to_string();

        // The same call the agent makes, given nothing but the room name.
        let expected = pool.for_room(&room_name, "interview").unwrap();

        assert_eq!(body["serverUrl"], expected.url);

        // A URL from one project and a signature from another would still look
        // right in the response body and fail at the SFU.
        let token = body["token"].as_str().unwrap();
        assert_eq!(claims(token)["iss"], expected.api_key);
        assert!(verify_signature(token, &expected.api_secret));
        served.push(expected.id.clone());
    }

    // Round robin, so six requests over three providers use all three. The old
    // room-name hash could send every room to the same one.
    served.sort();
    served.dedup();
    assert_eq!(served, vec!["eu", "primary", "us"]);

    server.shutdown().await;
    remove_database(db_path).await;
}

/// A fixed room name is handed to the agent verbatim, so the handler has to
/// read
/// the provider out of it rather than take the next one off the counter.
#[tokio::test]
async fn a_fixed_room_name_pins_the_provider_it_names() {
    let (mut config, cookie, db_path) = signed_in_web_config("provider-fixed-room");
    config.room_prefix = "interview".to_string();
    config.fixed_room_name = Some("interview-eu-fixed".to_string());
    config.production = false;
    config.pool = codetrial::config::ProviderPool {
        providers: vec![
            provider(codetrial::config::PRIMARY_PROVIDER_ID, "primary"),
            provider("eu", "eu"),
        ],
    };
    let (base, server) = spawn_web_server(config).await;
    let client = http_client();

    for _ in 0..3 {
        let body = client
            .post(format!("{base}/api/token"))
            .header("cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(body["roomName"], "interview-eu-fixed");
        assert_eq!(body["serverUrl"], "wss://eu.livekit.cloud");
    }

    server.shutdown().await;
    remove_database(db_path).await;
}

/// A provider failure arriving after a good completion leaves the file alone.
///
/// LiveKit retries webhooks, so its news can arrive twice and out of order. The
/// state table has to allow `transferring` to `failed`, because that is how the
/// delivery worker reports an upload it could not finish, and that same edge
/// let a stale `EGRESS_FAILED` fail a recording whose file was already queued
/// and whole. The candidate was then told to record again over a video that
/// existed.
#[tokio::test]
async fn a_late_provider_failure_does_not_fail_a_queued_recording() {
    let provider = std::sync::Arc::new(FakeRecordingProvider::default());
    let (base, server, path, client, cookie) =
        recorded_server_with_provider("late-failure", std::sync::Arc::clone(&provider)).await;
    let interview = start_interview(&client, &base, &cookie).await;
    assert_eq!(
        client
            .post(format!("{base}/api/token"))
            .header("cookie", cookie.clone())
            .json(&json!({ "interviewId": interview }))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{base}/api/interviews/{interview}/recording"))
            .header("cookie", cookie.clone())
            .send()
            .await
            .unwrap()
            .status(),
        202
    );

    let recording_id: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT id FROM recordings WHERE interview_id = ?1",
            [&interview],
            |row| row.get(0),
        )
        .unwrap();

    // The object this recording is going to transfer, named the way the webhook
    // predicate expects it, so this is a completion that really carries a file.
    let complete = json!({
        "event": "egress_ended",
        "id": "EV_complete",
        "createdAt": "1770000123",
        "egressInfo": {
            "egressId": "EG_web_fake",
            "status": "EGRESS_COMPLETE",
            "fileResults": [{
                "filename": format!("codetrial/{recording_id}.mp4"),
                "size": "1024"
            }]
        }
    })
    .to_string();
    assert_eq!(post_webhook(&client, &base, &complete).await.status(), 200);

    let state = |path: &std::path::Path| -> (String, Option<String>) {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT state, error FROM recordings WHERE id = ?1",
                [&recording_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(
        state(&path).0,
        "transferring",
        "a completion carrying the expected object queues the transfer"
    );

    // Queued, not merely moved. The row reaching `transferring` is what the
    // delivery worker looks for, and the queue entry is what tells it to look:
    // without it the recording waits for the sweeper to notice it was orphaned.
    let queued = |path: &std::path::Path| -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM delivery_queue WHERE recording_id = ?1",
                [&recording_id],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(queued(&path), 1, "the completion queued the delivery");

    // The same egress, reported failed afterwards. Nothing here is delivered,
    // so the row can only move if this webhook moves it.
    let failed = json!({
        "event": "egress_ended",
        "id": "EV_late_failure",
        "createdAt": "1770000456",
        "egressInfo": {
            "egressId": "EG_web_fake",
            "status": "EGRESS_FAILED",
            "error": "provider gave up"
        }
    })
    .to_string();
    assert_eq!(post_webhook(&client, &base, &failed).await.status(), 200);

    assert_eq!(
        state(&path),
        ("transferring".to_string(), None),
        "a stale provider failure must not take back a completed egress"
    );
    assert_eq!(
        queued(&path),
        1,
        "and it did not queue a second delivery either"
    );

    server.shutdown().await;
    remove_database(path).await;
}

/// The pool probes itself, rather than making the first candidate of every
/// minute pay for the answer.
///
/// Before this, the quota verdict was learned lazily on `/api/token` and cached
/// for a minute, so one request in each window waited on an HTTP round trip per
/// project it had to consider. The background refresher keeps the cache warm.
///
/// What this server-level test can see is the probe arriving with nobody
/// asking for a token. That the request path then reads the cache is proved in
/// `refresh_all_preserves_each_project_verdict`, where the refresh is awaited:
/// here the stub counts a probe before the refresher has written its verdict,
/// so a token request sent in between would probe on its own and fail a test
/// that asserted otherwise.
#[tokio::test]
async fn the_pool_probes_in_the_background_without_a_token_request() {
    let (provider, hits, stub) =
        spawn_counting_quota_stub(axum::http::StatusCode::OK, Duration::ZERO, "key", "secret")
            .await;

    let (mut config, _cookie, db_path) = signed_in_web_config("quota-background-probe");
    config.pool = primary_pool(&provider, "key", "secret");

    // The one test that wants the refresher, pointed at a local stub. Every
    // other server here leaves it off, so the suite makes no outbound request.
    config.probe_provider_quota = true;
    let (_base, server) = spawn_web_server(config).await;

    // The startup pass is the refresher's first action, not a step that blocks
    // the bind, so it is raced against here rather than assumed complete.
    let mut probed = 0;
    for _ in 0..500 {
        probed = hits.load(std::sync::atomic::Ordering::Relaxed);
        if probed > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        probed > 0,
        "the pool must probe its projects without being asked for a token"
    );

    server.shutdown().await;
    stub.shutdown().await;
    remove_database(db_path).await;
}

/// The quota stub is a tripwire, and this is what proves the wire is live.
///
/// Every other test here reads the stub's answer through the pool, and the pool
/// used to read anything that was not 429 as "still has minutes", so a stub
/// that had
/// quietly stopped checking would be invisible on each of them that answers
/// 200: the probe would arrive with no credential, be refused, and the refusal
/// would read as a healthy project. This asks the stub directly instead, which
/// is the only place the refusal is observable at all until somebody decides
/// what a 401 should mean for pooling.
#[tokio::test]
async fn the_quota_stub_refuses_a_probe_that_carries_the_wrong_credential() {
    /// The credential the probe mints, so the test asks with what production
    /// asks with rather than with a token shaped like it.
    fn probe_token(api_key: &str, api_secret: &str) -> String {
        codetrial::token::livekit_token(codetrial::token::LivekitTokenInput {
            api_key,
            api_secret,
            name: "quota-probe",
            identity: "quota-probe",
            room: "quota-probe",
            metadata: "",
            now_seconds: 1_700_000_000,
            agent: false,
        })
        .unwrap()
    }

    let (provider, stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "available-key",
        "available-secret",
    )
    .await;
    let client = http_client();

    // The pool entry is the `ws://` URL a LiveKit project would be configured
    // with, and the probe reaches the same server over `http://`;
    // `livekit_http_origin` is what production converts with and is
    // `pub(crate)`, so this does the one substitution that fixture's own shape
    // makes exact rather than widening a production surface for a test.
    let probe = format!("{}/rtc/validate", provider.replace("ws://", "http://"));

    for (why, bearer) in [
        ("no credential at all", None),
        (
            "a bearer that is not a token",
            Some("not-a-jwt".to_string()),
        ),
        (
            "a token signed with another project's secret",
            Some(probe_token("available-key", "someone-elses-secret")),
        ),
        (
            "a token minted for a different project",
            Some(probe_token("other-key", "available-secret")),
        ),
        // Four segments, and the fourth is a real signature over the first
        // three, so `rsplit_once` and `verify_hs256` between them would be
        // satisfied. What refuses it is `livekit_token_issuer`: it reads the
        // payload as everything between the first dot and the last, and the
        // glued segment puts a dot inside that, which base64url cannot decode.
        // Signed rather than glued on at random, because an unsigned tail is
        // refused by the signature check and would say nothing about the parser
        // that is really doing the work here.
        (
            "a fourth segment signed over the other three",
            Some({
                let inner = probe_token("available-key", "available-secret");
                let outer = codetrial::token::sign_hs256("available-secret", &inner);
                format!("{inner}.{outer}")
            }),
        ),
    ] {
        let request = client.get(&probe);
        let request = match bearer {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        assert_eq!(
            request.send().await.unwrap().status(),
            401,
            "the stub must refuse {why}, or it is not checking anything"
        );
    }

    // And the credential the pool actually mints is accepted, so the four
    // refusals above are a check rather than a stub that refuses everything.
    let accepted = client
        .get(&probe)
        .bearer_auth(probe_token("available-key", "available-secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200);

    stub.shutdown().await;
}

/// A quota check must never make starting an interview wait for the shared
/// client's 30-second backstop when a provider accepts connections but stalls.
#[tokio::test]
async fn a_stalled_quota_probe_does_not_delay_token_issuance() {
    let (provider, stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::from_secs(3),
        "available-key",
        "available-secret",
    )
    .await;
    let (mut config, cookie, db_path) = signed_in_web_config("quota-probe-timeout");
    config.probe_provider_quota = true;
    config.pool = primary_pool(&provider, "available-key", "available-secret");
    let (base, server) = spawn_web_server(config).await;

    let started = std::time::Instant::now();
    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a stalled quota probe must use its short timeout"
    );

    server.shutdown().await;
    stub.shutdown().await;
    remove_database(db_path).await;
}

/// A project out of connection minutes refuses the candidate's WebSocket, and a
/// browser cannot see the status on a refused upgrade: it surfaces as a bare
/// socket error, so the candidate is told only that the connection failed. The
/// pool exists precisely so one spent project is not the end of the interview,
/// so the server has to ask before it hands the token out.
#[tokio::test]
async fn a_provider_out_of_connection_minutes_is_passed_over() {
    let (exhausted, first) = spawn_livekit_quota_stub(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Duration::ZERO,
        "spent-key",
        "spent-secret",
    )
    .await;
    let (healthy, second) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "spare-key",
        "spare-secret",
    )
    .await;

    let (mut config, cookie, db_path) = signed_in_web_config("quota-failover");
    config.probe_provider_quota = true;
    config.pool = codetrial::config::ProviderPool {
        providers: vec![
            codetrial::config::Provider {
                id: codetrial::config::PRIMARY_PROVIDER_ID.to_string(),
                url: exhausted.clone(),
                api_key: "spent-key".to_string(),
                api_secret: "spent-secret".to_string(),
                google_api_keys: Vec::new(),
            },
            codetrial::config::Provider {
                id: "spare".to_string(),
                url: healthy.clone(),
                api_key: "spare-key".to_string(),
                api_secret: "spare-secret".to_string(),
                google_api_keys: Vec::new(),
            },
        ],
    };
    let (base, server) = spawn_web_server(config).await;

    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<Value>().await.unwrap();

    assert_eq!(
        body["serverUrl"], healthy,
        "the candidate must be sent to the project that still has minutes: {body}"
    );

    // The room name carries the provider id, which is how the agent resolves
    // the same project. Landing on the spare is not enough if the room still
    // says primary: the agent would join the project the candidate was moved
    // off.
    assert!(
        body["roomName"].as_str().unwrap().contains("-spare-"),
        "the room must name the project the candidate was moved to: {body}"
    );

    server.shutdown().await;
    first.shutdown().await;
    second.shutdown().await;
    remove_database(db_path).await;
}

/// A pinned `INTERVIEW_ROOM_NAME` is a convenience, not a promise the candidate
/// pays for. When the project it names is out of minutes the pin is dropped
/// rather than honoured: honouring it mints a token whose socket LiveKit
/// refuses with 429, which reaches the browser as "could not establish signal
/// connection" and sends the candidate looking at their own network.
///
/// The room name has to move with the project. It is the only thing a
/// separately launched `codetrial run-livekit ROOM` resolves the project from,
/// so a pinned name kept beside another project's credentials is how the
/// candidate and the agent end up in different ones.
#[tokio::test]
async fn a_pinned_room_on_an_exhausted_project_falls_back_to_the_pool() {
    let (exhausted, first) = spawn_livekit_quota_stub(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Duration::ZERO,
        "spent-key",
        "spent-secret",
    )
    .await;
    let (healthy, second) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "spare-key",
        "spare-secret",
    )
    .await;

    let (mut config, cookie, db_path) = signed_in_web_config("pinned-quota-failover");
    config.probe_provider_quota = true;
    config.fixed_room_name = Some("interview-local".to_string());
    config.pool = codetrial::config::ProviderPool {
        providers: vec![
            codetrial::config::Provider {
                id: codetrial::config::PRIMARY_PROVIDER_ID.to_string(),
                url: exhausted.clone(),
                api_key: "spent-key".to_string(),
                api_secret: "spent-secret".to_string(),
                google_api_keys: Vec::new(),
            },
            codetrial::config::Provider {
                id: "spare".to_string(),
                url: healthy.clone(),
                api_key: "spare-key".to_string(),
                api_secret: "spare-secret".to_string(),
                google_api_keys: Vec::new(),
            },
        ],
    };
    let (base, server) = spawn_web_server(config).await;

    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<Value>().await.unwrap();

    assert_eq!(
        body["serverUrl"], healthy,
        "an exhausted pin must not be handed to the candidate: {body}"
    );
    assert_ne!(
        body["roomName"], "interview-local",
        "the pinned name belongs to the exhausted project and must be dropped with it: {body}"
    );
    assert!(
        body["roomName"].as_str().unwrap().contains("-spare-"),
        "the room must name the project the candidate was moved to: {body}"
    );

    server.shutdown().await;
    first.shutdown().await;
    second.shutdown().await;
    remove_database(db_path).await;
}

/// A pinned room whose project still has minutes is left alone. The failover
/// above must not cost every local run its stable URL.
#[tokio::test]
async fn a_pinned_room_on_a_healthy_project_is_kept() {
    let (healthy, stub) =
        spawn_livekit_quota_stub(axum::http::StatusCode::OK, Duration::ZERO, "key", "secret").await;

    let (mut config, cookie, db_path) = signed_in_web_config("pinned-quota-healthy");
    config.probe_provider_quota = true;
    config.fixed_room_name = Some("interview-local".to_string());
    config.pool = primary_pool(&healthy, "key", "secret");
    let (base, server) = spawn_web_server(config).await;

    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<Value>().await.unwrap();

    assert_eq!(
        body["roomName"], "interview-local",
        "a healthy pin is the whole point of INTERVIEW_ROOM_NAME: {body}"
    );
    assert_eq!(body["serverUrl"], healthy);

    server.shutdown().await;
    stub.shutdown().await;
    remove_database(db_path).await;
}

/// With nothing left to fail over to, the honest answer names the cause. The
/// browser cannot: a refused upgrade reaches it as an unexplained socket error,
/// so a candidate would go looking at their own network for a quota this server
/// already knows is spent.
#[tokio::test]
async fn every_provider_out_of_minutes_is_refused_with_the_reason() {
    let (exhausted, stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Duration::ZERO,
        "spent-key",
        "spent-secret",
    )
    .await;

    let (mut config, cookie, db_path) = signed_in_web_config("quota-exhausted");
    config.probe_provider_quota = true;
    config.pool = primary_pool(&exhausted, "spent-key", "spent-secret");
    let (base, server) = spawn_web_server(config).await;

    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(body["code"], "livekit_quota_exhausted", "{body}");

    server.shutdown().await;
    stub.shutdown().await;
    remove_database(db_path).await;
}

/// A revoked credential looks different from spent minutes to the operator,
/// and it cannot receive a new room while the refusal remains fresh. The probe
/// stub accepts only its own credential, so configuring another secret creates
/// the same 401 the LiveKit validation endpoint returns after a rotation.
#[tokio::test]
async fn every_credential_refused_provider_is_refused_with_the_reason() {
    let (provider, stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "current-key",
        "current-secret",
    )
    .await;

    let (mut config, cookie, db_path) = signed_in_web_config("credential-refused");
    config.probe_provider_quota = true;
    config.pool = primary_pool(&provider, "current-key", "rotated-secret");
    let (base, server) = spawn_web_server(config).await;

    let response = http_client()
        .post(format!("{base}/api/token"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let body = response.json::<Value>().await.unwrap();
    assert_eq!(
        body["code"], "livekit_provider_credential_refused",
        "{body}"
    );
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("credential refused (401)")),
        "the error must name the refusal rather than spending: {body}"
    );

    server.shutdown().await;
    stub.shutdown().await;
    remove_database(db_path).await;
}

/// A dead project must not skew the pool. It still consumes its turn in the
/// rotation, so the project after it serves its own share and no more.
///
/// The failure this pins is not a refusal but a lopsided one: taking a single
/// rotation step per request and adding the retry offset locally leaves every
/// request starting at the dead project again, so its neighbour answers twice
/// as often as the rest. With three projects and one spent, six starts must
/// split three and three, not four and two.
#[tokio::test]
async fn a_dead_project_does_not_skew_the_rotation() {
    let (spent, spent_stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        Duration::ZERO,
        "spent-key",
        "spent-secret",
    )
    .await;
    let (first, first_stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "first-key",
        "first-secret",
    )
    .await;
    let (second, second_stub) = spawn_livekit_quota_stub(
        axum::http::StatusCode::OK,
        Duration::ZERO,
        "second-key",
        "second-secret",
    )
    .await;

    let (mut config, cookie, db_path) = signed_in_web_config("quota-rotation");
    config.probe_provider_quota = true;
    config.pool = codetrial::config::ProviderPool {
        providers: vec![
            codetrial::config::Provider {
                id: codetrial::config::PRIMARY_PROVIDER_ID.to_string(),
                url: spent.clone(),
                api_key: "spent-key".to_string(),
                api_secret: "spent-secret".to_string(),
                google_api_keys: Vec::new(),
            },
            codetrial::config::Provider {
                id: "first".to_string(),
                url: first.clone(),
                api_key: "first-key".to_string(),
                api_secret: "first-secret".to_string(),
                google_api_keys: Vec::new(),
            },
            codetrial::config::Provider {
                id: "second".to_string(),
                url: second.clone(),
                api_key: "second-key".to_string(),
                api_secret: "second-secret".to_string(),
                google_api_keys: Vec::new(),
            },
        ],
    };
    let (base, server) = spawn_web_server(config).await;

    let client = http_client();
    let mut served = std::collections::HashMap::new();
    for _ in 0..6 {
        let body = client
            .post(format!("{base}/api/token"))
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let url = body["serverUrl"].as_str().unwrap().to_string();
        *served.entry(url).or_insert(0) += 1;
    }

    assert_eq!(
        served.get(&spent),
        None,
        "a spent project must serve nobody"
    );
    assert_eq!(served.get(&first), Some(&3), "{served:?}");
    assert_eq!(served.get(&second), Some(&3), "{served:?}");

    server.shutdown().await;
    spent_stub.shutdown().await;
    first_stub.shutdown().await;
    second_stub.shutdown().await;
    remove_database(db_path).await;
}
