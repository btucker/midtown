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

    let result =
        super::save_screenshot_locally(&tmp_path, "png", false, false, &screenshots_dir, None);

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

    let result =
        super::save_screenshot_locally(&tmp_path, "png", true, false, &screenshots_dir, None);
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
fn save_screenshot_locally_github_flag_produces_markdown_image() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-github-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        false,
        false,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "http",
            repo: "my-project",
            external_url: None,
        }),
    );

    assert!(result.is_ok(), "Expected success, got: {:?}", result);

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.starts_with("![screenshot](http://localhost:"),
            "GitHub output should be markdown image syntax, got: {}",
            message
        );
        assert!(
            message.contains("/api/projects/my-project/screenshots/"),
            "URL should contain project and screenshots path, got: {}",
            message
        );
        assert!(
            message.ends_with(".png)"),
            "URL should end with .png), got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_github_before_uses_before_alt_text() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-github-before-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        true,
        false,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "http",
            repo: "my-project",
            external_url: None,
        }),
    );

    assert!(result.is_ok());

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.starts_with("![before]("),
            "Before screenshot should use 'before' alt text, got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_github_url_encodes_repo_name() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-github-encode-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        false,
        false,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "http",
            repo: "my project#1",
            external_url: None,
        }),
    );

    assert!(result.is_ok(), "Expected success, got: {:?}", result);

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.contains("/api/projects/my%20project%231/screenshots/"),
            "Repo name with spaces/special chars should be URL-encoded, got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_github_after_uses_after_alt_text() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-github-after-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        false,
        true,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "http",
            repo: "my-project",
            external_url: None,
        }),
    );

    assert!(result.is_ok());

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.starts_with("![after]("),
            "After screenshot should use 'after' alt text, got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_external_url_overrides_localhost() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-external-url-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        false,
        false,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "https",
            repo: "my-project",
            external_url: Some("https://macbook-pro.taile2dd2b.ts.net:47022"),
        }),
    );

    assert!(result.is_ok(), "Expected success, got: {:?}", result);

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.starts_with("![screenshot](https://macbook-pro.taile2dd2b.ts.net:47022/api/projects/my-project/screenshots/"),
            "External URL should override localhost, got: {}",
            message
        );
        assert!(
            !message.contains("localhost"),
            "Should not contain localhost when external_url is set, got: {}",
            message
        );
        assert!(
            message.ends_with(".png)"),
            "URL should end with .png), got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}

#[test]
fn save_screenshot_locally_external_url_trailing_slash_stripped() {
    let screenshots_tmp = tempfile::tempdir().unwrap();
    let screenshots_dir = screenshots_tmp.path().join("screenshots");

    let dir = std::env::temp_dir();
    let tmp_path = dir.join(format!(
        "midtown-test-screenshot-external-url-slash-{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, b"fake png data").unwrap();

    let result = super::save_screenshot_locally(
        &tmp_path,
        "png",
        false,
        false,
        &screenshots_dir,
        Some(super::ScreenshotUrlConfig {
            scheme: "https",
            repo: "my-project",
            external_url: Some("https://example.com:47022/"),
        }),
    );

    assert!(result.is_ok(), "Expected success, got: {:?}", result);

    if let super::super::Response::Message { message } = result.unwrap() {
        assert!(
            message.contains("https://example.com:47022/api/projects/"),
            "Trailing slash should be stripped to avoid double slash, got: {}",
            message
        );
        assert!(
            !message.contains("//api"),
            "Should not have double slash before api, got: {}",
            message
        );
    } else {
        panic!("Expected Message response");
    }
}
