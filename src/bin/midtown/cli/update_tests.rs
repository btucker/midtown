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
