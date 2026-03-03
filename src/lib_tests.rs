use crate::paths;

#[test]
fn resolve_web_dir_prefers_midtown_base_dir_when_exe_relative_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = paths::set_test_midtown_base_dir(tmp.path().to_path_buf());

    // Create web-app/dist under the fake midtown base dir
    let web_dist = tmp.path().join("web-app").join("dist");
    std::fs::create_dir_all(&web_dist).unwrap();

    let resolved = crate::resolve_web_dir();

    // Should resolve to the midtown base dir candidate (candidate 2),
    // since the exe-relative candidate (candidate 1) doesn't exist in test env
    assert_eq!(resolved, web_dist);
}

#[test]
fn resolve_web_dir_falls_back_to_cargo_manifest_dir_when_midtown_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = paths::set_test_midtown_base_dir(tmp.path().to_path_buf());

    // Don't create web-app/dist — both candidates 1 and 2 are absent
    let resolved = crate::resolve_web_dir();

    // Should fall back to the CARGO_MANIFEST_DIR-based path (candidate 3)
    let expected = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web-app")
        .join("dist");
    assert_eq!(resolved, expected);
}
