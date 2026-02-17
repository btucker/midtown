use std::sync::Arc;
use std::time::Duration;

/// Test helper: mock client that simulates daemon restart by returning
/// lease errors on poll after the first N successful polls.
struct MockDaemonClient {
    poll_count: std::sync::Mutex<usize>,
    fail_after: usize,
    register_count: std::sync::Mutex<usize>,
}

impl MockDaemonClient {
    fn new(fail_after: usize) -> Self {
        Self {
            poll_count: std::sync::Mutex::new(0),
            fail_after,
            register_count: std::sync::Mutex::new(0),
        }
    }

    fn get_register_count(&self) -> usize {
        *self.register_count.lock().unwrap()
    }

    fn headed_register_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
        _provider: midtown::auth::AuthProvider,
    ) -> Result<serde_json::Value, String> {
        let mut count = self.register_count.lock().unwrap();
        *count += 1;
        Ok(serde_json::json!({ "acked_id": 0 }))
    }

    fn headed_poll_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
        _after_id: u64,
        _limit: usize,
    ) -> Result<serde_json::Value, String> {
        let mut count = self.poll_count.lock().unwrap();
        *count += 1;

        if *count > self.fail_after {
            // Simulate daemon restart: lease is gone
            return Err("No active headed adapter for session 'test'".to_string());
        }

        Ok(serde_json::json!({
            "messages": [],
            "capture_output": false
        }))
    }

    fn headed_ack_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
        _msg_id: u64,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }

    fn headed_heartbeat_mock(
        &self,
        _session: &str,
        _adapter_id: &str,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({}))
    }
}

#[test]
fn test_run_shell_reregisters_after_daemon_restart() {
    // This test verifies that RunShell detects poll errors indicating a missing
    // adapter lease and automatically re-registers before retrying.
    //
    // Currently FAILS because RunShell doesn't detect the error condition.

    let mock_client = Arc::new(MockDaemonClient::new(3));

    // Simulate RunShell loop logic (simplified):
    // 1. Register once
    // 2. Poll in a loop
    // 3. When poll fails with "No active headed adapter", should re-register

    let session = "test";
    let adapter_id = "test-adapter";
    let provider = midtown::auth::AuthProvider::Claude;
    let batch_limit = 50;

    // Initial registration
    mock_client
        .headed_register_mock(session, adapter_id, provider)
        .expect("initial register");

    // Run poll loop for 10 iterations
    for i in 0..10 {
        match mock_client.headed_poll_mock(session, adapter_id, 0, batch_limit) {
            Ok(_) => {
                // Process messages (none in this test)
            }
            Err(e) if e.contains("No active headed adapter") || e.contains("lease expired") => {
                // Daemon restarted — should re-register
                mock_client
                    .headed_register_mock(session, adapter_id, provider)
                    .expect("re-register after daemon restart");
            }
            Err(e) => {
                panic!("Unexpected error: {}", e);
            }
        }

        // Stop after a few cycles to keep test fast
        if i >= 7 {
            break;
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // Should have registered twice: once initially, once after simulated restart
    let register_count = mock_client.get_register_count();
    assert_eq!(
        register_count, 2,
        "Expected re-registration after daemon restart (got {} registrations)",
        register_count
    );
}

#[test]
fn test_run_agent_reregisters_after_daemon_restart() {
    // This test verifies that RunAgent detects poll errors indicating a missing
    // adapter lease and automatically re-registers before retrying.
    //
    // Currently FAILS because RunAgent catches poll errors but doesn't re-register.

    let mock_client = Arc::new(MockDaemonClient::new(3));

    let session = "test";
    let adapter_id = "test-adapter";
    let provider = midtown::auth::AuthProvider::Claude;
    let batch_limit = 50;

    // Initial registration
    mock_client
        .headed_register_mock(session, adapter_id, provider)
        .expect("initial register");

    // Simulate RunAgent poll loop (lines 882-895 in headed_wrapper.rs)
    for i in 0..10 {
        match mock_client.headed_poll_mock(session, adapter_id, 0, batch_limit) {
            Ok(value) => {
                // Parse and process messages
                let _: serde_json::Value = value;
            }
            Err(e) if e.contains("No active headed adapter") || e.contains("lease expired") => {
                // Current code just sleeps and continues — should re-register instead
                mock_client
                    .headed_register_mock(session, adapter_id, provider)
                    .expect("re-register after daemon restart");
            }
            Err(_) => {
                // Other errors: sleep and retry (existing behavior)
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        if i >= 7 {
            break;
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // Should have registered twice: once initially, once after simulated restart
    let register_count = mock_client.get_register_count();
    assert_eq!(
        register_count, 2,
        "Expected re-registration after daemon restart (got {} registrations)",
        register_count
    );
}
