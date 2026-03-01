//! E2E test for the `curl | sh` install process.
//!
//! Spins up a mock HTTP server that mimics GitHub's release redirect and
//! asset download endpoints, creates a test tarball matching the release
//! format, and verifies install.sh correctly installs the binary and web-app.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::thread;

/// The install script, baked in at compile time.
const INSTALL_SH: &str = include_str!("../install.sh");

/// Reads an HTTP request from a TCP stream (up to 8KB, which is plenty
/// for the simple GET/HEAD requests the install script sends).
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).to_string()
}

#[test]
fn test_install_script_curl_bash() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    // ── Build a fake binary ──────────────────────────────────────────
    // A shell script that responds to `--version` like the real binary.
    let staging = tmp.path().join("staging");
    fs::create_dir_all(staging.join("web-app")).unwrap();

    fs::write(
        staging.join("midtown"),
        "#!/bin/sh\necho \"midtown 0.0.1-test\"\n",
    )
    .unwrap();
    fs::set_permissions(staging.join("midtown"), fs::Permissions::from_mode(0o755)).unwrap();

    fs::write(staging.join("web-app").join("index.html"), "<h1>test</h1>").unwrap();

    // ── Create tarball matching the release asset format ─────────────
    let os_name = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch_name = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else {
        "arm64"
    };
    let version = "0.0.1";
    let asset_name = format!("midtown-{os_name}-{arch_name}-v{version}.tar.gz");
    let tarball_path = tmp.path().join(&asset_name);

    let status = Command::new("tar")
        .args(["czf", tarball_path.to_str().unwrap(), "midtown", "web-app"])
        .current_dir(&staging)
        .status()
        .expect("tar command failed");
    assert!(status.success(), "Failed to create tarball");

    let tarball_bytes = fs::read(&tarball_path).unwrap();

    // ── Start mock HTTP server ───────────────────────────────────────
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    // Patch install.sh to talk to our mock server instead of GitHub.
    let modified_sh = INSTALL_SH.replace("https://github.com", &format!("http://127.0.0.1:{port}"));

    let serve_sh = modified_sh.clone();
    let serve_tarball = tarball_bytes;

    thread::spawn(move || {
        // The install flow makes exactly 3 requests:
        //   1. GET  /install.sh           (outer curl piped to sh)
        //   2. HEAD /.../releases/latest   (version detection inside the script)
        //   3. GET  /.../releases/download (tarball download)
        // Handle a generous number of connections to be safe.
        for stream in listener.incoming().take(10) {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);

            let response: Vec<u8> = if request.contains("/releases/latest") {
                // 302 redirect — the script parses the Location header
                // to extract the version tag (everything after /tag/).
                format!(
                    "HTTP/1.1 302 Found\r\n\
                     Location: /btucker/midtown/releases/tag/v{version}\r\n\
                     Connection: close\r\n\
                     Content-Length: 0\r\n\r\n"
                )
                .into_bytes()
            } else if request.contains(".tar.gz") {
                // Serve the tarball.
                let mut resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/gzip\r\n\
                     Connection: close\r\n\
                     Content-Length: {}\r\n\r\n",
                    serve_tarball.len()
                )
                .into_bytes();
                // Only include body for GET (not HEAD).
                if !request.starts_with("HEAD ") {
                    resp.extend_from_slice(&serve_tarball);
                }
                resp
            } else {
                // Default: serve the modified install script.
                let body = serve_sh.as_bytes();
                format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/plain\r\n\
                     Connection: close\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                )
                .into_bytes()
                .into_iter()
                .chain(body.iter().copied())
                .collect()
            };

            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });

    // ── Run `curl | sh` ─────────────────────────────────────────────
    let install_dir = tmp.path().join("install_target");
    fs::create_dir_all(&install_dir).unwrap();

    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "curl -fsSL http://127.0.0.1:{port}/install.sh | MIDTOWN_INSTALL_DIR='{}' sh",
            install_dir.display()
        ))
        .output()
        .expect("failed to run curl | sh");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "Install script failed.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // ── Assert: binary installed, executable, runs ───────────────────
    let binary = install_dir.join("midtown");
    assert!(binary.exists(), "Binary not found at {}", binary.display());

    let perms = fs::metadata(&binary).unwrap().permissions();
    assert!(
        perms.mode() & 0o111 != 0,
        "Binary is not executable (mode: {:o})",
        perms.mode()
    );

    let version_output = Command::new(&binary)
        .output()
        .expect("binary failed to run");
    assert!(version_output.status.success(), "Binary exited with error");
    let version_str = String::from_utf8_lossy(&version_output.stdout);
    assert!(
        version_str.contains("0.0.1-test"),
        "Unexpected version output: {version_str}"
    );

    // ── Assert: web-app/ installed ───────────────────────────────────
    let web_app = install_dir.join("web-app");
    assert!(web_app.exists(), "web-app/ directory not installed");
    assert!(
        web_app.join("index.html").exists(),
        "web-app/index.html not found"
    );

    // ── Assert: PATH warning shown ───────────────────────────────────
    // The install dir is a temp path that won't be in $PATH.
    assert!(
        stdout.contains("not in your PATH"),
        "PATH warning not shown. Full stdout:\n{stdout}"
    );
}

/// Verify that the install script handles an already-existing web-app/
/// directory (atomic swap: rename old → .old, move new, delete .old).
#[test]
fn test_install_script_replaces_existing_web_app() {
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    // Build fake binary + web-app tarball (same setup as above).
    let staging = tmp.path().join("staging");
    fs::create_dir_all(staging.join("web-app")).unwrap();
    fs::write(
        staging.join("midtown"),
        "#!/bin/sh\necho \"midtown 0.0.2-test\"\n",
    )
    .unwrap();
    fs::set_permissions(staging.join("midtown"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(staging.join("web-app").join("index.html"), "<h1>v2</h1>").unwrap();

    let os_name = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let arch_name = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else {
        "arm64"
    };
    let version = "0.0.2";
    let asset_name = format!("midtown-{os_name}-{arch_name}-v{version}.tar.gz");
    let tarball_path = tmp.path().join(&asset_name);

    let status = Command::new("tar")
        .args(["czf", tarball_path.to_str().unwrap(), "midtown", "web-app"])
        .current_dir(&staging)
        .status()
        .unwrap();
    assert!(status.success());

    let tarball_bytes = fs::read(&tarball_path).unwrap();

    // Pre-populate install dir with an existing web-app/.
    let install_dir = tmp.path().join("install_target");
    fs::create_dir_all(install_dir.join("web-app")).unwrap();
    fs::write(install_dir.join("web-app").join("old.html"), "<h1>old</h1>").unwrap();

    // Start mock server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let modified_sh = INSTALL_SH.replace("https://github.com", &format!("http://127.0.0.1:{port}"));
    let serve_sh = modified_sh.clone();
    let serve_tarball = tarball_bytes;

    thread::spawn(move || {
        for stream in listener.incoming().take(10) {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);

            let response: Vec<u8> = if request.contains("/releases/latest") {
                format!(
                    "HTTP/1.1 302 Found\r\n\
                     Location: /btucker/midtown/releases/tag/v{version}\r\n\
                     Connection: close\r\n\
                     Content-Length: 0\r\n\r\n"
                )
                .into_bytes()
            } else if request.contains(".tar.gz") {
                let mut resp = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: application/gzip\r\n\
                     Connection: close\r\n\
                     Content-Length: {}\r\n\r\n",
                    serve_tarball.len()
                )
                .into_bytes();
                if !request.starts_with("HEAD ") {
                    resp.extend_from_slice(&serve_tarball);
                }
                resp
            } else {
                let body = serve_sh.as_bytes();
                format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/plain\r\n\
                     Connection: close\r\n\
                     Content-Length: {}\r\n\r\n",
                    body.len()
                )
                .into_bytes()
                .into_iter()
                .chain(body.iter().copied())
                .collect()
            };

            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });

    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "curl -fsSL http://127.0.0.1:{port}/install.sh | MIDTOWN_INSTALL_DIR='{}' sh",
            install_dir.display()
        ))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Install script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Old file should be gone (replaced by new web-app/).
    assert!(
        !install_dir.join("web-app").join("old.html").exists(),
        "Old web-app files should have been replaced"
    );
    // New file should be present.
    assert!(
        install_dir.join("web-app").join("index.html").exists(),
        "New web-app/index.html should be installed"
    );
    // .old directory should be cleaned up.
    assert!(
        !install_dir.join("web-app.old").exists(),
        "web-app.old should have been cleaned up"
    );
}
