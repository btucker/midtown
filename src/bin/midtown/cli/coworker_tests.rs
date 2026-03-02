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
