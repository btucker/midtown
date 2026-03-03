use crate::paths;

/// When the source tree has `web-app/dist`, it should be preferred over
/// the data dir — ensures `cargo run` serves locally built assets.
#[test]
fn resolve_web_dir_source_tree_preferred_over_data_dir() {
    let source_candidate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web-app")
        .join("dist");

    if !source_candidate.exists() {
        // Source tree web-app not built; can't verify ordering. Covered by
        // resolve_web_dir_falls_back_to_data_dir below.
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = paths::set_test_midtown_data_dir(tmp.path().to_path_buf());

    // Also create web-app/dist in the data dir
    let data_dist = tmp.path().join("web-app").join("dist");
    std::fs::create_dir_all(&data_dist).unwrap();

    let resolved = crate::resolve_web_dir();

    // Source tree (candidate 2) must win over data dir (candidate 3)
    assert_eq!(resolved, source_candidate);
}

/// When the source tree does NOT have `web-app/dist`, the XDG data dir
/// should be returned (binary install path).
#[test]
fn resolve_web_dir_falls_back_to_data_dir() {
    let source_candidate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web-app")
        .join("dist");

    if source_candidate.exists() {
        // Source tree has web-app/dist — data dir fallback won't trigger.
        // Covered by resolve_web_dir_source_tree_preferred_over_data_dir above.
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = paths::set_test_midtown_data_dir(tmp.path().to_path_buf());

    let data_dist = tmp.path().join("web-app").join("dist");
    std::fs::create_dir_all(&data_dist).unwrap();

    let resolved = crate::resolve_web_dir();
    assert_eq!(resolved, data_dist);
}

/// When no candidates exist on disk, the data dir path is returned
/// as a meaningful fallback for error messages.
#[test]
fn resolve_web_dir_data_dir_path_as_final_fallback() {
    let source_candidate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("web-app")
        .join("dist");

    if source_candidate.exists() {
        // Source tree has web-app/dist — the function returns it before
        // reaching the data dir fallback. Nothing to test.
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let _guard = paths::set_test_midtown_data_dir(tmp.path().to_path_buf());

    // Don't create web-app/dist anywhere
    let resolved = crate::resolve_web_dir();

    // Falls through to data dir path for error messages
    let expected = tmp.path().join("web-app").join("dist");
    assert_eq!(resolved, expected);
}
