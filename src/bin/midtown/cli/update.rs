use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::response::Response;

const GITHUB_REPO: &str = "btucker/midtown";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CHECK_INTERVAL: Duration = Duration::from_secs(3600); // 1 hour

/// Handle `midtown update` — download and install the latest release.
pub fn handle_update(check_only: bool) -> Result<Response, String> {
    let latest = fetch_latest_version()?;
    let latest_bare = latest.strip_prefix('v').unwrap_or(&latest);

    // Update the last-check timestamp regardless of outcome
    let _ = write_last_check_timestamp();

    if !is_newer(latest_bare, CURRENT_VERSION) {
        return Ok(Response::Message {
            message: format!("Already up to date (v{CURRENT_VERSION})"),
        });
    }

    if check_only {
        return Ok(Response::Message {
            message: format!(
                "midtown v{latest_bare} is available (current: v{CURRENT_VERSION}). Run `midtown update` to upgrade."
            ),
        });
    }

    eprintln!(
        "Updating midtown v{} → v{}...",
        CURRENT_VERSION, latest_bare
    );

    let (os, arch) = detect_platform()?;
    let asset_name = format!("midtown-{os}-{arch}-v{latest_bare}.tar.gz");
    let url = format!("https://github.com/{GITHUB_REPO}/releases/download/{latest}/{asset_name}");

    // Download to a temp directory (TempDir auto-cleans on drop, even on early ?-returns)
    let tmp_dir =
        tempfile::TempDir::new().map_err(|e| format!("Failed to create temp directory: {e}"))?;
    let tarball_path = tmp_dir.path().join(&asset_name);

    eprintln!("Downloading {asset_name}...");
    download_file(&url, &tarball_path)?;

    // Extract the tarball
    eprintln!("Extracting...");
    extract_tarball(&tarball_path, tmp_dir.path())?;

    // Determine install directory (same as the directory containing the current binary)
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {e}"))?;
    let install_dir = current_exe
        .parent()
        .ok_or("Cannot determine install directory")?;

    // Replace binary (atomic swap via rename)
    let new_binary = tmp_dir.path().join("midtown");
    if !new_binary.exists() {
        return Err("Downloaded archive does not contain 'midtown' binary".to_string());
    }
    replace_binary(&new_binary, &current_exe)?;

    // Replace web-app/ in XDG data dir if present in the tarball (matching install.sh)
    let new_web_app = tmp_dir.path().join("web-app");
    if new_web_app.is_dir() {
        let data_dir = midtown::paths::midtown_data_dir();
        replace_web_app(&new_web_app, &data_dir)?;
        // Clean up legacy web-app from exe dir if present
        let legacy_web_app = install_dir.join("web-app");
        if legacy_web_app.is_dir() {
            let _ = fs::remove_dir_all(&legacy_web_app);
        }
    }

    Ok(Response::Message {
        message: format!("Updated midtown v{CURRENT_VERSION} → v{latest_bare}"),
    })
}

/// Non-blocking version check for `midtown start`.
/// Returns a notice string if a newer version is available, None otherwise.
/// Respects the 1-hour cooldown between checks.
pub fn check_for_update_notice() -> Option<String> {
    if !should_check_version() {
        return None;
    }

    // Run the check in a background thread with a short timeout.
    // Use a channel with recv_timeout to enforce a 3-second wall-clock deadline,
    // rather than join() which would block for the full HTTP timeout (10s).
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_latest_version());
    });

    let result = match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(version)) => version,
        _ => {
            // Record timestamp even on failure/timeout so we don't retry every invocation
            let _ = write_last_check_timestamp();
            return None;
        }
    };

    let _ = write_last_check_timestamp();

    let latest_bare = result.strip_prefix('v').unwrap_or(&result);
    if is_newer(latest_bare, CURRENT_VERSION) {
        Some(format!(
            "midtown v{latest_bare} is available, run `midtown update` to upgrade"
        ))
    } else {
        None
    }
}

// ── Version checking ──────────────────────────────────────────────────────

/// Fetch the latest release version tag from GitHub using the redirect trick.
fn fetch_latest_version() -> Result<String, String> {
    let url = format!("https://github.com/{GITHUB_REPO}/releases/latest");

    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let resp = client
        .head(&url)
        .send()
        .map_err(|e| format!("Failed to check latest version: {e}"))?;

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or("GitHub did not return a redirect for latest release")?;

    // Location looks like: https://github.com/btucker/midtown/releases/tag/v0.7.0
    let version = location
        .rsplit("/tag/")
        .next()
        .ok_or("Could not parse version from redirect URL")?
        .trim()
        .to_string();

    if version.is_empty() {
        return Err("Empty version tag from GitHub".to_string());
    }

    Ok(version)
}

/// Compare two semver-ish version strings. Returns true if `latest` > `current`.
/// Strips pre-release suffixes (e.g., "0.7.0-beta.1" → "0.7.0") so that
/// pre-release versions are never considered newer than their stable counterpart.
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> (u32, u32, u32) {
        // Strip pre-release suffix (everything after first '-') before parsing
        let stable = v.split('-').next().unwrap_or(v);
        let parts: Vec<u32> = stable.split('.').filter_map(|p| p.parse().ok()).collect();
        (
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    };
    parse(latest) > parse(current)
}

// ── Platform detection ─────────────────────────────────────────────────────

fn detect_platform() -> Result<(&'static str, &'static str), String> {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        return Err(format!("Unsupported OS: {}", std::env::consts::OS));
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        return Err(format!(
            "Unsupported architecture: {}",
            std::env::consts::ARCH
        ));
    };

    Ok((os, arch))
}

// ── Download & extract ────────────────────────────────────────────────────

fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to download {url}: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!(
            "Download failed with HTTP {}: {}",
            resp.status(),
            url
        ));
    }

    let mut file = fs::File::create(dest)
        .map_err(|e| format!("Failed to create file {}: {e}", dest.display()))?;

    resp.copy_to(&mut file)
        .map_err(|e| format!("Failed to write download: {e}"))?;

    Ok(())
}

fn extract_tarball(tarball: &Path, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            &tarball.to_string_lossy(),
            "-C",
            &dest.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("Failed to run tar: {e}"))?;

    if !status.success() {
        return Err("Failed to extract tarball".to_string());
    }
    Ok(())
}

// ── Binary & web-app replacement ──────────────────────────────────────────

fn replace_binary(new_binary: &Path, current_exe: &Path) -> Result<(), String> {
    // On macOS/Linux, we can't overwrite a running binary directly.
    // Instead: rename current → .old, copy new → current, delete .old
    let backup = current_exe.with_extension("old");

    // Remove any leftover backup from a previous update
    let _ = fs::remove_file(&backup);

    fs::rename(current_exe, &backup)
        .map_err(|e| format!("Failed to move current binary to backup: {e}"))?;

    // Copy new binary into place (copy, not rename, since it may be on a different filesystem)
    fs::copy(new_binary, current_exe).map_err(|e| {
        // Try to restore backup on failure
        let _ = fs::rename(&backup, current_exe);
        format!("Failed to install new binary: {e}")
    })?;

    // Ensure executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(current_exe, perms)
            .map_err(|e| format!("Failed to set executable permissions: {e}"))?;
    }

    // Clean up backup
    let _ = fs::remove_file(&backup);

    Ok(())
}

fn replace_web_app(new_web_app: &Path, data_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|e| {
        format!(
            "Failed to create data directory {}: {e}",
            data_dir.display()
        )
    })?;
    let target = data_dir.join("web-app");
    let old = data_dir.join("web-app.old");
    let staging = data_dir.join("web-app.new");

    // Stage the new web-app on the target filesystem so rename() never crosses
    // device boundaries (EXDEV). The staging copy doesn't need to be atomic —
    // if it fails, the existing web-app is untouched.
    let _ = fs::remove_dir_all(&staging); // clean up any leftover staging dir
    copy_dir_recursive(new_web_app, &staging).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        format!("Failed to stage new web-app: {e}")
    })?;

    // Atomic swap: mv current → .old, mv staged → current, rm .old
    // All renames are on the same filesystem, so this matches the install.sh pattern.
    if target.is_dir() {
        let _ = fs::remove_dir_all(&old); // clean up any leftover .old
        fs::rename(&target, &old).map_err(|e| {
            let _ = fs::remove_dir_all(&staging);
            format!("Failed to move old web-app: {e}")
        })?;
    }

    fs::rename(&staging, &target).map_err(|e| {
        // Restore old on failure
        if old.is_dir() {
            let _ = fs::rename(&old, &target);
        }
        let _ = fs::remove_dir_all(&staging);
        format!("Failed to install new web-app: {e}")
    })?;

    // Clean up old
    let _ = fs::remove_dir_all(&old);

    eprintln!("Updated web UI");
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create directory {}: {e}", dst.display()))?;

    for entry in fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {e}"))?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)
                .map_err(|e| format!("Failed to copy {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

// ── Last-check timestamp (rate limiting) ──────────────────────────────────

fn last_check_file() -> PathBuf {
    midtown::paths::midtown_base_dir().join("update-last-check")
}

fn should_check_version() -> bool {
    let path = last_check_file();
    match fs::metadata(&path) {
        Ok(meta) => {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::MAX)
                > CHECK_INTERVAL
        }
        Err(_) => true, // No file = never checked
    }
}

fn write_last_check_timestamp() -> Result<(), String> {
    let path = last_check_file();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut f =
        fs::File::create(&path).map_err(|e| format!("Failed to write last-check file: {e}"))?;
    // Write current timestamp as content (not strictly needed, file mtime is what matters)
    let _ = writeln!(f, "{}", chrono::Utc::now().to_rfc3339());
    Ok(())
}

#[path = "update_tests.rs"]
#[cfg(test)]
mod tests;
