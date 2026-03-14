#[test]
fn upload_image_fails_for_missing_file() {
    let result =
        super::handle_upload_image("/tmp/nonexistent-midtown-test-image.png", "screenshot");
    assert!(result.is_err(), "Should fail for missing file");
    let err = result.unwrap_err();
    assert!(
        err.contains("File not found"),
        "Error should mention file not found, got: {}",
        err
    );
}

#[test]
fn upload_to_github_fails_gracefully_in_test_env() {
    // In test/CI environments, upload_to_github will fail due to missing token
    // or missing GitHub repo context. We verify it returns Err (not a panic)
    // and produces a meaningful error message.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"fake image data").unwrap();

    let result = super::upload_to_github(tmp.path(), "png");
    assert!(
        result.is_err(),
        "upload_to_github should fail gracefully in test environment"
    );

    let err = result.unwrap_err();
    assert!(!err.is_empty(), "Error message should be non-empty");
}
