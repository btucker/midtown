use super::TempFileGuard;

#[test]
fn temp_file_guard_cleans_up_on_drop() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("midtown-test-guard-{}", std::process::id()));

    // Create the file
    std::fs::write(&path, b"test data").unwrap();
    assert!(path.exists());

    // Guard should remove it on drop
    {
        let _guard = TempFileGuard { path: path.clone() };
    }

    assert!(!path.exists(), "TempFileGuard should remove file on drop");
}

#[test]
fn temp_file_guard_cleans_up_when_error_causes_early_return() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("midtown-test-guard-err-{}", std::process::id()));

    // Create the file
    std::fs::write(&path, b"test data").unwrap();
    assert!(path.exists());

    // Simulate an error path: guard is created, then scope exits via Err
    let result: Result<(), String> = {
        let _guard = TempFileGuard { path: path.clone() };
        Err("simulated upload failure".to_string())
    };

    assert!(result.is_err());
    assert!(
        !path.exists(),
        "TempFileGuard should clean up even on error return paths"
    );
}

#[test]
fn temp_file_guard_handles_already_deleted_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("midtown-test-guard-nofile-{}", std::process::id()));

    // Don't create the file — guard should not panic on drop
    {
        let _guard = TempFileGuard { path: path.clone() };
    }
    // No panic = success
}

#[test]
fn upload_and_cleanup_removes_temp_file_on_server_error() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // Start a local TCP server that returns HTTP 500
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_thread = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Read the request (drain it so the client doesn't get a broken pipe)
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            // Respond with HTTP 500
            let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 21\r\n\r\nInternal Server Error";
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    // Create a temp file simulating Playwright output
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-upload-cleanup-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();
    assert!(tmp_path.exists());

    // Point upload_and_cleanup at our mock server.
    // SAFETY: This test runs single-threaded (no parallel test reads this env var).
    unsafe { std::env::set_var("MIDTOWN_WEBHOOK_PORT", port.to_string()) };

    let result = super::upload_and_cleanup(&tmp_path, "test.png");

    // Clean up env var
    // SAFETY: Same single-threaded test context.
    unsafe { std::env::remove_var("MIDTOWN_WEBHOOK_PORT") };

    // Upload should have failed with HTTP 500
    assert!(result.is_err(), "Expected upload to fail with HTTP 500");
    let err = result.unwrap_err();
    assert!(
        err.contains("500"),
        "Error should mention HTTP 500, got: {}",
        err
    );

    // The temp file should have been cleaned up by TempFileGuard
    assert!(
        !tmp_path.exists(),
        "TempFileGuard should remove temp file even when upload fails"
    );

    server_thread.join().unwrap();
}
