// Fixture servers listen on loopback, so these clients ignore HTTP_PROXY: a
// proxy configured for the machine would otherwise receive, and usually fail,
// requests meant for 127.0.0.1. The live interview harness talks to real hosts
// and builds its own client so that it keeps proxy support.
use std::sync::OnceLock;
use std::time::Duration;

/// A backstop so a fixture that never answers fails its test instead of
/// hanging the run. Each test that exercises a timeout sets its own shorter
/// one.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    // The production trust store; see `bundled_roots_client_builder`.
    codetrial::bundled_roots_client_builder().no_proxy()
}

pub(crate) fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // Tests run on separate runtimes; a pooled connection would outlive
            // the runtime that opened it, so nothing is kept idle.
            client_builder()
                .pool_max_idle_per_host(0)
                .timeout(FIXTURE_TIMEOUT)
                .build()
                .expect("fixture HTTP client builds")
        })
        .clone()
}
