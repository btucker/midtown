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
fn save_screenshot_locally_saves_and_cleans_up_temp() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    // Create a temp file simulating Playwright output
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-save-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();
    assert!(tmp_path.exists());

    let result = super::save_screenshot_locally(&tmp_path, "png", false, false, &screenshots_dir);

    // Should succeed
    assert!(
        result.is_ok(),
        "Expected save to succeed, got: {:?}",
        result
    );

    // The temp file should have been cleaned up by TempFileGuard
    assert!(
        !tmp_path.exists(),
        "TempFileGuard should remove temp file after save"
    );

    // Verify a file was saved to the screenshots directory
    let entries: Vec<_> = std::fs::read_dir(&screenshots_dir)
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1, "Expected one screenshot file");
    let saved_name = entries[0].file_name().to_string_lossy().to_string();
    assert!(saved_name.ends_with(".png"), "Should have .png extension");

    // Verify the response contains [Attached: ...]
    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.contains("[Attached:"),
            "Response should contain [Attached:], got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_before_after_prefix() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    // Test "before" prefix
    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-before-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake data").unwrap();

    let result = super::save_screenshot_locally(&tmp_path, "png", true, false, &screenshots_dir);
    assert!(result.is_ok());

    let entries: Vec<_> = std::fs::read_dir(&screenshots_dir)
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    let saved_name = entries[0].file_name().to_string_lossy().to_string();
    assert!(
        saved_name.starts_with("before-"),
        "Before screenshot should have before- prefix, got: {}",
        saved_name
    );
}

#[test]
fn save_screenshot_locally_always_returns_attached_format() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-attached-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();

    let result = super::save_screenshot_locally(&tmp_path, "png", false, false, &screenshots_dir);

    assert!(result.is_ok(), "Expected success, got: {:?}", result);

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.starts_with("[Attached:"),
            "Should return [Attached: ...] format, got: {}",
            message
        );
        assert!(
            message.contains(".png"),
            "Should contain .png extension, got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_after_prefix() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-after-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake data").unwrap();

    let result = super::save_screenshot_locally(&tmp_path, "png", false, true, &screenshots_dir);
    assert!(result.is_ok());

    let entries: Vec<_> = std::fs::read_dir(&screenshots_dir)
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    let saved_name = entries[0].file_name().to_string_lossy().to_string();
    assert!(
        saved_name.starts_with("after-"),
        "After screenshot should have after- prefix, got: {}",
        saved_name
    );
}

#[test]
fn upload_to_github_fails_gracefully_in_test_env() {
    // In test/CI environments, upload_to_github will fail due to missing token
    // or missing GitHub repo context. We verify it returns Err (not a panic)
    // and produces a meaningful error message.
    //
    // Note: We cannot safely unset GH_TOKEN/GITHUB_TOKEN because env vars are
    // global state and concurrent test threads would race. The function exercises
    // whichever error path triggers first in the current environment.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"fake image data").unwrap();

    let result = super::upload_to_github(tmp.path(), "png");
    assert!(
        result.is_err(),
        "upload_to_github should fail gracefully in test environment"
    );

    let err = result.unwrap_err();
    // Should produce a human-readable error, not a raw panic or empty string
    assert!(!err.is_empty(), "Error message should be non-empty");
}
