use super::*;

#[test]
fn test_is_newer_basic() {
    assert!(is_newer("0.7.0", "0.6.3"));
    assert!(is_newer("1.0.0", "0.6.3"));
    assert!(is_newer("0.6.4", "0.6.3"));
}

#[test]
fn test_is_newer_same() {
    assert!(!is_newer("0.6.3", "0.6.3"));
}

#[test]
fn test_is_newer_older() {
    assert!(!is_newer("0.6.2", "0.6.3"));
    assert!(!is_newer("0.5.0", "0.6.3"));
}

#[test]
fn test_is_newer_major_bump() {
    assert!(is_newer("2.0.0", "1.9.9"));
}

#[test]
fn test_is_newer_minor_bump() {
    assert!(is_newer("0.10.0", "0.9.99"));
}

#[test]
fn test_is_newer_prerelease_not_newer_than_stable() {
    // Pre-release "0.7.0-beta.1" should NOT be considered newer than stable "0.7.0"
    assert!(!is_newer("0.7.0-beta.1", "0.7.0"));
    assert!(!is_newer("0.7.0-rc.1", "0.7.0"));
    assert!(!is_newer("0.7.0-alpha", "0.7.0"));
}

#[test]
fn test_is_newer_prerelease_same_base_equal() {
    // Same base version: pre-release suffix is stripped, so both parse to (0,7,0)
    assert!(!is_newer("0.7.0-beta.1", "0.7.0-beta.2"));
    assert!(!is_newer("0.7.0", "0.7.0-beta.1"));
}

#[test]
fn test_is_newer_prerelease_higher_base() {
    // Higher base version should still be detected as newer
    assert!(is_newer("0.8.0-beta.1", "0.7.0"));
}

#[test]
fn test_detect_platform() {
    // Should succeed on any supported CI/dev machine
    let result = detect_platform();
    assert!(result.is_ok());
    let (os, arch) = result.unwrap();
    assert!(os == "darwin" || os == "linux");
    assert!(arch == "amd64" || arch == "arm64");
}

#[test]
fn test_last_check_file_path() {
    let path = last_check_file();
    assert!(path.to_string_lossy().contains("update-last-check"));
}

#[test]
fn test_replace_web_app_atomic_swap() {
    let tmp = tempfile::TempDir::new().unwrap();
    let install_dir = tmp.path();

    // Create existing web-app directory with content
    let target = install_dir.join("web-app");
    fs::create_dir_all(target.join("subdir")).unwrap();
    fs::write(target.join("index.html"), "old content").unwrap();
    fs::write(target.join("subdir/app.js"), "old js").unwrap();

    // Create new web-app source in a sibling temp dir (same filesystem for rename)
    let src_tmp = tempfile::TempDir::new_in(install_dir).unwrap();
    let new_web_app = src_tmp.path().join("web-app");
    fs::create_dir_all(new_web_app.join("subdir")).unwrap();
    fs::write(new_web_app.join("index.html"), "new content").unwrap();
    fs::write(new_web_app.join("subdir/app.js"), "new js").unwrap();

    replace_web_app(&new_web_app, install_dir).unwrap();

    // Verify new content is in place
    assert_eq!(
        fs::read_to_string(target.join("index.html")).unwrap(),
        "new content"
    );
    assert_eq!(
        fs::read_to_string(target.join("subdir/app.js")).unwrap(),
        "new js"
    );
    // Verify .old was cleaned up
    assert!(!install_dir.join("web-app.old").exists());
}

#[test]
fn test_replace_web_app_no_existing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let install_dir = tmp.path();

    // No existing web-app directory
    let src_tmp = tempfile::TempDir::new_in(install_dir).unwrap();
    let new_web_app = src_tmp.path().join("web-app");
    fs::create_dir_all(&new_web_app).unwrap();
    fs::write(new_web_app.join("index.html"), "fresh install").unwrap();

    replace_web_app(&new_web_app, install_dir).unwrap();

    let target = install_dir.join("web-app");
    assert_eq!(
        fs::read_to_string(target.join("index.html")).unwrap(),
        "fresh install"
    );
}

#[test]
fn test_replace_binary_sets_permissions() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Create a fake "current" binary
    let current_exe = tmp.path().join("midtown");
    fs::write(&current_exe, "old binary").unwrap();

    // Create a fake "new" binary
    let new_binary = tmp.path().join("midtown-new");
    fs::write(&new_binary, "new binary").unwrap();

    replace_binary(&new_binary, &current_exe).unwrap();

    // Verify new content
    assert_eq!(fs::read_to_string(&current_exe).unwrap(), "new binary");

    // Verify permissions on unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&current_exe).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o755);
    }

    // Verify backup was cleaned up
    assert!(!tmp.path().join("midtown.old").exists());
}

#[test]
fn test_replace_binary_restores_on_copy_failure() {
    let tmp = tempfile::TempDir::new().unwrap();

    // Create a fake "current" binary
    let current_exe = tmp.path().join("midtown");
    fs::write(&current_exe, "original binary").unwrap();

    // Point to a non-existent "new" binary
    let new_binary = tmp.path().join("does-not-exist");

    let result = replace_binary(&new_binary, &current_exe);
    assert!(result.is_err());

    // Verify the original was restored
    assert_eq!(fs::read_to_string(&current_exe).unwrap(), "original binary");
}
