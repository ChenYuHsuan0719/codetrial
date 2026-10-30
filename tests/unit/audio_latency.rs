use super::*;

#[test]
fn prompt_onsets_match_their_own_pulse() {
    let onsets = assign(&[300.0, 1300.0], &[340.0, 1350.0]);
    assert_eq!(onsets[0].delivery_ms, Some(40.0));
    assert_eq!(onsets[1].delivery_ms, Some(50.0));
}

/// A constant overrun lands each onset in the next pulse's window, where
/// it looks fast; the warmup is left without one, so the run cannot pass.
#[test]
fn a_delivery_slower_than_the_window_leaves_the_warmup_missing() {
    let onsets = assign(&[300.0, 1300.0, 2300.0], &[1400.0, 2400.0, 3400.0]);
    assert_eq!(onsets[0].delivery_ms, None);
    assert_eq!(onsets[0].onsets, 0);
    // The final pulse has no successor, so its overrun is reported late.
    assert_eq!(onsets[2].delivery_ms, None);
    assert_eq!(onsets[2].late_onsets, 1);
}

/// Backpressure can delay the next send past the window; an onset in that gap
/// is late for its own pulse rather than on time because no pulse followed yet.
#[test]
fn an_onset_after_the_window_but_before_the_next_send_is_late() {
    let onsets = assign(&[300.0, 1800.0], &[1500.0]);
    assert_eq!(onsets[0].delivery_ms, None);
    assert_eq!(onsets[0].onsets, 0);
    assert_eq!(onsets[0].late_onsets, 1);
    assert_eq!(onsets[1].onsets + onsets[1].late_onsets, 0);
}

#[test]
fn two_onsets_in_one_window_are_ambiguous() {
    let onsets = assign(&[300.0], &[320.0, 600.0]);
    assert_eq!(onsets[0].delivery_ms, None);
    assert_eq!(onsets[0].onsets, 2);
}

#[test]
fn noise_before_the_first_send_fails_the_warmup() {
    let onsets = assign(&[300.0], &[100.0, 320.0]);
    assert_eq!(onsets[0].delivery_ms, None);
    assert_eq!(onsets[0].late_onsets, 1);
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("audio-latency-test-{}-{id}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

struct TestRoom {
    saved: Option<std::path::PathBuf>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    id: usize,
    fail_unpublish: bool,
    fail_close: bool,
    stall: bool,
}

impl ProbeRoom for TestRoom {
    type Track = usize;

    fn tracks(&self) -> Vec<usize> {
        vec![0, 1]
    }

    async fn unpublish(&self, track: &usize) -> Result<()> {
        if let Some(path) = &self.saved {
            let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?).unwrap();
            assert_eq!(raw["teardown_complete"], false);
            assert_eq!(raw["samples"][0]["delivery_ms"], 12);
        }
        self.calls
            .lock()
            .unwrap()
            .push(format!("unpublish:{}:{track}", self.id));
        if self.stall {
            std::future::pending::<()>().await;
        }
        if self.fail_unpublish {
            return Err("unpublish refused".into());
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("close:{}", self.id));
        if self.fail_close {
            return Err("close refused".into());
        }
        Ok(())
    }
}

fn measurement() -> serde_json::Value {
    json!({"error": null, "teardown_complete": false, "teardown_errors": [], "samples": [{"delivery_ms": 12}]})
}

#[tokio::test(start_paused = true)]
async fn teardown_errors_preserve_samples_and_attempt_every_operation() {
    let scratch = Scratch::new();
    let path = scratch.0.join("run.json");
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let rooms: Vec<_> = (0..2)
        .map(|id| TestRoom {
            saved: Some(path.clone()),
            calls: std::sync::Arc::clone(&calls),
            id,
            fail_unpublish: id == 0,
            fail_close: true,
            stall: false,
        })
        .collect();
    let error = finish_probe(&path, Ok(measurement()), &rooms)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unpublish refused"));
    assert!(error.to_string().contains("close refused"));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "unpublish:0:0",
            "unpublish:0:1",
            "unpublish:1:0",
            "unpublish:1:1",
            "close:0",
            "close:1"
        ]
    );
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(raw["samples"], measurement()["samples"]);
    assert_eq!(raw["teardown_complete"], true);
    assert_eq!(raw["teardown_errors"].as_array().unwrap().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn teardown_success_completes_the_saved_record() {
    let scratch = Scratch::new();
    let path = scratch.0.join("run.json");
    let room = TestRoom {
        saved: Some(path.clone()),
        calls: Default::default(),
        id: 0,
        fail_unpublish: false,
        fail_close: false,
        stall: false,
    };
    finish_probe(&path, Ok(measurement()), &[room])
        .await
        .unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(raw["teardown_complete"], true);
    assert_eq!(raw["teardown_errors"], json!([]));
}

#[tokio::test(start_paused = true)]
async fn save_and_setup_failures_still_close_connected_rooms() {
    let scratch = Scratch::new();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let room = TestRoom {
        saved: None,
        calls: std::sync::Arc::clone(&calls),
        id: 0,
        fail_unpublish: false,
        fail_close: false,
        stall: false,
    };
    let path = scratch.0.join("missing/run.json");
    let error = finish_probe(&path, Ok(measurement()), std::slice::from_ref(&room))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("save measurements"));
    assert_eq!(calls.lock().unwrap().last().unwrap(), "close:0");
    calls.lock().unwrap().clear();
    let error = finish_probe(&path, Err("connection failed".into()), &[room])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("connection failed"));
    assert_eq!(calls.lock().unwrap().last().unwrap(), "close:0");
}

#[tokio::test(start_paused = true)]
async fn stalled_unpublishes_are_bounded_and_do_not_skip_close() {
    let scratch = Scratch::new();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let room = TestRoom {
        saved: None,
        calls: std::sync::Arc::clone(&calls),
        id: 0,
        fail_unpublish: false,
        fail_close: false,
        stall: true,
    };
    let error = finish_probe(&scratch.0.join("run.json"), Ok(measurement()), &[room])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert_eq!(calls.lock().unwrap().len(), 3);
    assert_eq!(calls.lock().unwrap().last().unwrap(), "close:0");
}

#[tokio::test(start_paused = true)]
async fn measurement_errors_and_missing_warmup_remain_failed_after_cleanup() {
    let scratch = Scratch::new();
    let path = scratch.0.join("run.json");
    let mut failed = measurement();
    failed["error"] = "capture timed out".into();
    let error = finish_probe::<TestRoom>(&path, Ok(failed), &[])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("capture timed out"));
    let mut missing = measurement();
    missing["samples"] = json!([{"delivery_ms": null}, {"delivery_ms": 12}]);
    let error = finish_probe::<TestRoom>(&path, Ok(missing), &[])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing"));
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(raw["teardown_complete"], true);
    assert_eq!(raw["samples"][1]["delivery_ms"], 12);
}
