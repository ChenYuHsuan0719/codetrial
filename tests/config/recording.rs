use std::collections::BTreeMap;

use codetrial::config::{
    DEFAULT_RECORDING_BITRATE, DEFAULT_RECORDING_GCS_PREFIX, DEFAULT_RECORDING_MAX_MINUTES,
    PRIMARY_PROVIDER_ID, Provider, ProviderPool, load_recording,
};

const SERVICE_ACCOUNT: &str = r#"{"type":"service_account","client_email":"codetrial@example.iam.gserviceaccount.com","private_key":"KEYMATERIAL-4bd2"}"#;
const RECORDING_SECRET: &str = "recording-api-secret-9f3c";
const RECORDING_KEY: &str = "APIrecordingkey";

fn pool() -> ProviderPool {
    ProviderPool {
        providers: vec![Provider {
            id: PRIMARY_PROVIDER_ID.to_string(),
            url: "wss://project.livekit.cloud".to_string(),
            api_key: "devkey".to_string(),
            api_secret: "devsecret".to_string(),
            google_api_keys: vec!["google".to_string()],
        }],
    }
}

fn values(
    pairs: impl IntoIterator<Item = (&'static str, &'static str)>,
) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, String> = [
        ("CODETRIAL_RECORDING_ENABLED", "true"),
        ("CODETRIAL_RECORDING_GCS_BUCKET", "codetrial-staging"),
        ("CODETRIAL_RECORDING_DRIVE_ID", "0AKfixtureDriveId"),
        ("CODETRIAL_RECORDING_SERVICE_ACCOUNT_JSON", SERVICE_ACCOUNT),
        (
            "CODETRIAL_RECORDING_TEMPLATE_BASE_URL",
            "https://recording.codetrial.example",
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    for (key, value) in pairs {
        if value.is_empty() {
            values.remove(key);
        } else {
            values.insert(key.to_string(), value.to_string());
        }
    }
    values
}

#[test]
fn recording_is_off_unless_it_is_turned_on() {
    assert_eq!(
        load_recording(&BTreeMap::new(), &pool(), 45, false),
        Ok(None),
        "the default deployment records nothing and needs no credentials to say so"
    );
}

#[test]
fn recording_accepts_complete_configuration() {
    let config = load_recording(&values([]), &pool(), 45, false)
        .expect("a complete recording block should load")
        .expect("enabled recording should produce a config");

    assert_eq!(config.gcs_bucket, "codetrial-staging");
    assert_eq!(config.drive_id, "0AKfixtureDriveId");
    assert_eq!(config.gcs_prefix, DEFAULT_RECORDING_GCS_PREFIX);
    assert_eq!(config.max_minutes, DEFAULT_RECORDING_MAX_MINUTES);
    assert_eq!(config.bitrate, DEFAULT_RECORDING_BITRATE);
    assert!(!config.kill_switch);
    assert!(
        config.livekit.is_none(),
        "with no override, recording follows the project that owns the room"
    );

    // The trailing slash goes here rather than at every call site that appends
    // the template path.
    let trailing = load_recording(
        &values([(
            "CODETRIAL_RECORDING_TEMPLATE_BASE_URL",
            "https://recording.codetrial.example/",
        )]),
        &pool(),
        45,
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        trailing.template_base_url,
        "https://recording.codetrial.example"
    );
}

#[test]
fn recording_accepts_a_livekit_override_naming_a_pooled_project() {
    let config = load_recording(
        &values([
            (
                "CODETRIAL_RECORDING_LIVEKIT_URL",
                "wss://project.livekit.cloud",
            ),
            ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
            ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
        ]),
        &pool(),
        45,
        false,
    )
    .expect("an override naming a pooled project should load")
    .unwrap();

    let livekit = config.livekit.expect("the override should be recorded");
    assert_eq!(livekit.api_key, RECORDING_KEY);
    assert_eq!(livekit.api_secret, RECORDING_SECRET);
}

#[test]
fn recording_rejects_partial_credentials() {
    for missing in [
        "CODETRIAL_RECORDING_GCS_BUCKET",
        "CODETRIAL_RECORDING_DRIVE_ID",
        "CODETRIAL_RECORDING_SERVICE_ACCOUNT_JSON",
        "CODETRIAL_RECORDING_TEMPLATE_BASE_URL",
    ] {
        let error = load_recording(&values([(missing, "")]), &pool(), 45, false)
            .expect_err("a missing credential must fail closed");
        assert!(
            error.missing_keys.contains(&missing),
            "{missing} should be named as missing, got {:?}",
            error.missing_keys
        );
    }

    // The LiveKit override is all three or none. Two of three is a project
    // nothing can call, and defaulting the third would silently record
    // somewhere the operator did not name.
    let error = load_recording(
        &values([(
            "CODETRIAL_RECORDING_LIVEKIT_URL",
            "wss://project.livekit.cloud",
        )]),
        &pool(),
        45,
        false,
    )
    .expect_err("a half-configured override must fail closed");
    assert!(
        error
            .missing_keys
            .contains(&"CODETRIAL_RECORDING_LIVEKIT_API_KEY")
    );
    assert!(
        error
            .missing_keys
            .contains(&"CODETRIAL_RECORDING_LIVEKIT_API_SECRET")
    );
}

#[test]
fn recording_rejects_max_minutes_below_interview_duration() {
    let error = load_recording(
        &values([("CODETRIAL_RECORDING_MAX_MINUTES", "30")]),
        &pool(),
        45,
        false,
    )
    .expect_err("a recording that ends before the interview is worthless");
    assert!(
        error
            .invalid_entries
            .iter()
            .any(|entry| entry.contains("CODETRIAL_RECORDING_MAX_MINUTES")),
        "the reason should name the key, got {:?}",
        error.invalid_entries
    );

    // Equal is fine: the recording covers exactly the interview. Asserted as
    // `Some`, not as `is_ok`, because `Ok(None)` is also `is_ok` and means
    // recording never loaded at all.
    assert!(
        load_recording(
            &values([("CODETRIAL_RECORDING_MAX_MINUTES", "45")]),
            &pool(),
            45,
            false
        )
        .unwrap()
        .is_some()
    );
}

#[test]
fn recording_rejects_a_value_it_cannot_read() {
    // `optional_u32` defaults on a parse failure, which is right for a tuning
    // knob and wrong for a bill: a server that starts and records at whatever
    // the default happened to be is worse than one that refuses.
    for (key, value) in [
        ("CODETRIAL_RECORDING_BITRATE", "2 Mbps"),
        ("CODETRIAL_RECORDING_MAX_MINUTES", "forty-five"),
    ] {
        let error = load_recording(&values([(key, value)]), &pool(), 45, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains(key), "{key} should be named, got {error}");
    }

    // And a mistyped switch is a startup failure, not a silent opt-out. Failing
    // closed by accident still means no consent prompt, no artifact, and
    // nothing said.
    for key in [
        "CODETRIAL_RECORDING_ENABLED",
        "CODETRIAL_RECORDING_KILL_SWITCH",
    ] {
        let error = load_recording(&values([(key, "ture")]), &pool(), 45, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains(key), "{key} should be named, got {error}");
    }
}

#[test]
fn recording_stays_off_without_grading_settings_nothing_reads() {
    // A deployment that does not record must not fail to start over a typo in a
    // value that is ignored while the switch is off.
    let mut off: BTreeMap<String, String> = BTreeMap::new();
    off.insert(
        "CODETRIAL_RECORDING_ENABLED".to_string(),
        "false".to_string(),
    );
    off.insert(
        "CODETRIAL_RECORDING_KILL_SWITCH".to_string(),
        "ture".to_string(),
    );
    off.insert(
        "CODETRIAL_RECORDING_BITRATE".to_string(),
        "2 Mbps".to_string(),
    );
    assert_eq!(load_recording(&off, &pool(), 45, false), Ok(None));
}

#[test]
fn recording_tells_two_ports_on_one_host_apart() {
    // A bracketed IPv6 authority hides its port behind colons that belong to
    // the address. Reading the last colon as the separator refuses the port and
    // treats `[addr]:8443` as the default, which would accept an override for a
    // different endpoint.
    let ipv6_pool = ProviderPool {
        providers: vec![Provider {
            id: PRIMARY_PROVIDER_ID.to_string(),
            url: "wss://[2001:db8::1]".to_string(),
            api_key: "devkey".to_string(),
            api_secret: "devsecret".to_string(),
            google_api_keys: vec!["google".to_string()],
        }],
    };
    let override_at = |url: &'static str| {
        load_recording(
            &values([
                ("CODETRIAL_RECORDING_LIVEKIT_URL", url),
                ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
                ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
            ]),
            &ipv6_pool,
            45,
            false,
        )
    };
    assert!(
        override_at("wss://[2001:db8::1]").is_ok(),
        "the same endpoint must still match"
    );
    assert!(
        override_at("wss://[2001:db8::1]:8443").is_err(),
        "a different port is a different project"
    );
}

#[test]
fn recording_matches_a_pooled_project_across_equivalent_urls() {
    // `wss://host` and `https://host:443` are one endpoint. Reporting them as
    // different projects would refuse a correct configuration.
    //
    // Asserted through `livekit`, not through `is_ok`: `Ok(None)` is also
    // `is_ok` and means recording never loaded, which is how this could have
    // gone green against a build that stopped reading the override.
    let matched = |pool: &ProviderPool, url: &'static str| {
        load_recording(
            &values([
                ("CODETRIAL_RECORDING_LIVEKIT_URL", url),
                ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
                ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
            ]),
            pool,
            45,
            false,
        )
        .map(|config| config.and_then(|config| config.livekit).is_some())
    };
    for equivalent in [
        "https://project.livekit.cloud",
        "wss://project.livekit.cloud:443",
        "wss://PROJECT.livekit.cloud",
    ] {
        assert_eq!(
            matched(&pool(), equivalent),
            Ok(true),
            "{equivalent} names the pooled project"
        );
    }

    // The plaintext pair is the same rule at the other default port, and it is
    // the pair a developer runs against: `ws://host` and `http://host:80` are
    // one endpoint, while `wss://host` is a second one on port 443. Nothing
    // reached the `ws`/`http` arm, so it could have defaulted to 443 with every
    // test still green.
    let local = ProviderPool {
        providers: vec![Provider {
            id: PRIMARY_PROVIDER_ID.to_string(),
            url: "ws://localhost".to_string(),
            api_key: "devkey".to_string(),
            api_secret: "devsecret".to_string(),
            google_api_keys: vec!["google".to_string()],
        }],
    };
    assert_eq!(matched(&local, "http://localhost:80"), Ok(true));
    assert_eq!(matched(&local, "ws://localhost:80"), Ok(true));
    assert!(
        matched(&local, "wss://localhost").is_err(),
        "443 is not 80, so this is a different endpoint"
    );
}

/// A LiveKit override the pool cannot match because it is not a URL.
///
/// `recording_livekit` reports an unusable URL and a URL naming an unpooled
/// project through two different branches with two different messages, and
/// only the second had a test: a build that stopped validating the override
/// would have fallen into the pool check and refused for the wrong reason,
/// or accepted `wss://` with no host as a project it could dial.
#[test]
fn recording_rejects_an_override_url_it_cannot_read() {
    for (url, expected) in [
        ("ftp://project.livekit.cloud", "must start with wss://"),
        ("wss://", "scheme but no host"),
        ("project.livekit.cloud", "must start with wss://"),
    ] {
        let error = load_recording(
            &values([
                ("CODETRIAL_RECORDING_LIVEKIT_URL", url),
                ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
                ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
            ]),
            &pool(),
            45,
            false,
        )
        .expect_err("an override nothing can dial must fail closed");
        assert!(
            error.invalid_entries.iter().any(|entry| entry
                .starts_with("CODETRIAL_RECORDING_LIVEKIT_URL:")
                && entry.contains(expected)),
            "{url} should be refused as unreadable, got {:?}",
            error.invalid_entries
        );
    }
}

/// The bitrate ceiling and floor, which nothing asserted.
///
/// `recording_number` already refuses a value it cannot parse; this is the
/// separate check that a number it can parse is one Egress will encode at.
/// A deployment that recorded at 40 kbps would produce a file, bill for it,
/// and hand a candidate something unwatchable.
#[test]
fn recording_rejects_a_bitrate_outside_the_range_egress_can_encode() {
    use codetrial::config::{MAX_RECORDING_BITRATE, MIN_RECORDING_BITRATE};

    for refused in ["0", "100", "9000"] {
        let error = load_recording(
            &values([("CODETRIAL_RECORDING_BITRATE", refused)]),
            &pool(),
            45,
            false,
        )
        .expect_err("a bitrate outside the range must fail closed");
        assert!(
            error
                .invalid_entries
                .iter()
                .any(|entry| entry.contains("CODETRIAL_RECORDING_BITRATE")),
            "{refused} kbps should be named, got {:?}",
            error.invalid_entries
        );
    }

    // Both ends are inclusive, so the bounds themselves load.
    for accepted in [MIN_RECORDING_BITRATE, MAX_RECORDING_BITRATE] {
        let mut pairs = values([]);
        pairs.insert(
            "CODETRIAL_RECORDING_BITRATE".to_string(),
            accepted.to_string(),
        );
        assert_eq!(
            load_recording(&pairs, &pool(), 45, false)
                .unwrap()
                .map(|config| config.bitrate),
            Some(accepted),
            "{accepted} is inside the range"
        );
    }
}

/// The credential is read at startup, not at the first delivery.
///
/// `codetrial::delivery::readable_service_account` had no test anywhere:
/// replacing its body with `Ok(())` left the whole suite green, and the
/// deployment it describes records interviews it can never hand over and
/// finds out an hour into the first one.
#[test]
fn recording_refuses_a_service_account_it_cannot_read() {
    for (broken, expected) in [
        ("not json at all", "not JSON"),
        (r#"{"private_key":"KEYMATERIAL-4bd2"}"#, "no client_email"),
        (
            r#"{"client_email":"","private_key":"KEYMATERIAL-4bd2"}"#,
            "no client_email",
        ),
        (
            r#"{"client_email":"codetrial@example.iam.gserviceaccount.com"}"#,
            "no private_key",
        ),
        (
            r#"{"client_email":"codetrial@example.iam.gserviceaccount.com","private_key":""}"#,
            "no private_key",
        ),
    ] {
        let mut pairs = values([]);
        pairs.insert(
            "CODETRIAL_RECORDING_SERVICE_ACCOUNT_JSON".to_string(),
            broken.to_string(),
        );
        let error = load_recording(&pairs, &pool(), 45, false)
            .expect_err("a credential nobody can sign with must fail closed");
        assert!(
            error.invalid_entries.iter().any(|entry| entry
                .starts_with("CODETRIAL_RECORDING_SERVICE_ACCOUNT_JSON:")
                && entry.contains(expected)),
            "{expected} should be reported, got {:?}",
            error.invalid_entries
        );

        // The reason reaches stderr, so it must not carry the credential.
        for entry in &error.invalid_entries {
            assert!(
                !entry.contains("KEYMATERIAL-4bd2"),
                "the key material must not appear in {entry}"
            );
        }
    }

    assert_eq!(
        codetrial::delivery::readable_service_account(SERVICE_ACCOUNT),
        Ok(())
    );
}

/// The two settings that were only ever asserted at their defaults.
///
/// Nothing turned the kill switch on or moved the staging prefix, so a
/// parser that ignored either key and answered from the default was
/// indistinguishable from one that read it. The kill switch is the control
/// an operator reaches for when recording has to stop now.
#[test]
fn recording_reads_the_kill_switch_and_the_prefix_rather_than_defaulting_them() {
    let config = |key: &'static str, value: &'static str| {
        load_recording(&values([(key, value)]), &pool(), 45, false)
            .expect("the switch and the prefix should load")
            .expect("recording is enabled")
    };

    for on in ["true", "TRUE", "1", "yes", "on"] {
        assert!(
            config("CODETRIAL_RECORDING_KILL_SWITCH", on).kill_switch,
            "{on} should stop recording"
        );
    }
    for off in ["false", "0", "no", "off"] {
        assert!(
            !config("CODETRIAL_RECORDING_KILL_SWITCH", off).kill_switch,
            "{off} should leave recording running"
        );
    }

    assert_eq!(
        config("CODETRIAL_RECORDING_GCS_PREFIX", "staging/runs").gcs_prefix,
        "staging/runs"
    );
}

#[test]
fn recording_rejects_unknown_provider_url() {
    let error = load_recording(
        &values([
            (
                "CODETRIAL_RECORDING_LIVEKIT_URL",
                "wss://elsewhere.livekit.cloud",
            ),
            ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
            ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
        ]),
        &pool(),
        45,
        false,
    )
    .expect_err("recording a project that holds no rooms records nothing");
    assert!(
        error
            .invalid_entries
            .iter()
            .any(|entry| entry.contains("provider pool")),
        "got {:?}",
        error.invalid_entries
    );
}

#[test]
fn recording_accepts_a_public_host_that_looks_like_a_private_prefix() {
    // The prefix-matching version of this check refused every hostname
    // beginning `fc` or `fd`, which includes `facebook.com`, and every one
    // beginning `10.` or `127.`.
    for public in [
        "https://fc-cdn.example.com",
        "https://fd-recording.example",
        "https://10.example.com",
        "https://127.example.com",
        "https://recording.localhostess.example",
        "https://recording.codetrial.example:8443",
    ] {
        // Asserted through the value that comes back, not through `is_ok`:
        // `Ok(None)` is also `is_ok` and means recording never loaded, so a
        // break anywhere before the URL check would have passed for acceptance.
        assert_eq!(
            load_recording(
                &values([("CODETRIAL_RECORDING_TEMPLATE_BASE_URL", public)]),
                &pool(),
                45,
                false,
            )
            .map(|config| config.map(|config| config.template_base_url)),
            Ok(Some(public.to_string())),
            "{public} is a public name and must be accepted"
        );
    }
}

#[test]
fn recording_rejects_a_template_url_egress_cannot_fetch() {
    for unreachable in [
        "http://recording.codetrial.example",
        "https://127.0.0.1:8443",
        "https://127.1.2.3",
        "https://localhost",
        "https://localhost.",
        "https://api.localhost",
        "https://[::1]:8443",
        "https://[::ffff:127.0.0.1]",
        "https://[fd00::1]",
        "https://[FE80::1]",
        "https://10.1.2.3",
        "https://172.16.0.1",
        "https://192.168.1.1",
        "https://169.254.169.254",
        "https://0.0.0.0",
        // A credential hiding inside a URL survives every redaction that was
        // written to catch a credential in a field of its own.
        "https://user:hunter2@recording.codetrial.example",
        "https://recording.codetrial.example/?layout=grid",
        // A port Egress cannot connect to is as unusable as a host it cannot
        // reach.
        "https://recording.codetrial.example:0",
        "https://recording.codetrial.example:not-a-port",
        "https://recording.codetrial.example:99999",
        // A truncated bracketed authority is not a host.
        "https://[2001:db8::1",
        "https://[2001:db8::1]junk",
    ] {
        let error = load_recording(
            &values([("CODETRIAL_RECORDING_TEMPLATE_BASE_URL", unreachable)]),
            &pool(),
            45,
            false,
        )
        .unwrap_err();
        assert!(
            error
                .invalid_entries
                .iter()
                .any(|entry| entry.contains("CODETRIAL_RECORDING_TEMPLATE_BASE_URL")),
            "{unreachable} should be refused, got {error:?}"
        );
    }
}

#[test]
fn recording_redacts_secrets() {
    let config = load_recording(
        &values([
            (
                "CODETRIAL_RECORDING_LIVEKIT_URL",
                "wss://project.livekit.cloud",
            ),
            ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
            ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
        ]),
        &pool(),
        45,
        false,
    )
    .unwrap()
    .unwrap();

    // `WebServerConfig` derives `Debug`, so anything reachable from it can
    // reach a log line. The private key is the worst of these: it is key
    // material living in a `String`.
    let printed = format!("{config:?}");
    for secret in [
        SERVICE_ACCOUNT,
        RECORDING_SECRET,
        RECORDING_KEY,
        "KEYMATERIAL-4bd2",
    ] {
        assert!(
            !printed.contains(secret),
            "Debug output leaked {secret}: {printed}"
        );
    }
    assert!(
        printed.contains("<redacted>"),
        "the redaction should be visible rather than silent: {printed}"
    );

    // Validation messages reach stderr too, so every rejection has to be
    // reason-only. This one fails on three counts at once.
    let error = load_recording(
        &values([
            (
                "CODETRIAL_RECORDING_LIVEKIT_URL",
                "wss://elsewhere.livekit.cloud",
            ),
            ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
            ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
            ("CODETRIAL_RECORDING_MAX_MINUTES", "5"),
            ("CODETRIAL_RECORDING_BITRATE", "999999"),
        ]),
        &pool(),
        45,
        false,
    )
    .unwrap_err();
    let message = error.to_string();
    for secret in [
        SERVICE_ACCOUNT,
        RECORDING_SECRET,
        RECORDING_KEY,
        "KEYMATERIAL-4bd2",
    ] {
        assert!(
            !message.contains(secret),
            "a validation message leaked {secret}: {message}"
        );
    }
    assert!(
        !message.contains("elsewhere.livekit.cloud"),
        "a LiveKit URL can carry a query string, so it never appears in a message: {message}"
    );

    // The same argument applies to `Debug`, which is why the override prints
    // its host rather than its URL.
    let with_credentials = load_recording(
        &values([
            (
                "CODETRIAL_RECORDING_LIVEKIT_URL",
                "wss://apikey:hunter2@project.livekit.cloud/?token=leaked",
            ),
            ("CODETRIAL_RECORDING_LIVEKIT_API_KEY", RECORDING_KEY),
            ("CODETRIAL_RECORDING_LIVEKIT_API_SECRET", RECORDING_SECRET),
        ]),
        &pool(),
        45,
        false,
    )
    .unwrap()
    .unwrap();
    let printed = format!("{with_credentials:?}");
    for secret in ["hunter2", "leaked", "apikey"] {
        assert!(
            !printed.contains(secret),
            "Debug leaked {secret} out of a URL: {printed}"
        );
    }
}
