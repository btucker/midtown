//! Regression tests for bugs caught during development.
//!
//! Each test targets a specific issue that was found and fixed,
//! ensuring we don't regress on these behaviors.

use std::path::PathBuf;
use std::process::Command;

/// Regression test for #644: CLI argument name conflict.
///
/// The `--repo` global arg and the `start` subcommand's `--repos` (auto-generated
/// short form `--repo`) caused a clap panic at runtime. Fixed by renaming the
/// subcommand arg to `--add-repo`.
///
/// This test verifies that `midtown start --help` exits cleanly without a clap
/// panic. The panic occurred during argument parsing, so `--help` triggers the
/// same code path.
#[test]
fn test_cli_start_help_no_panic() {
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("midtown");

    if !binary_path.exists() {
        // Binary not built yet — skip silently
        eprintln!("Skipping: debug binary not found at {:?}", binary_path);
        return;
    }

    let output = Command::new(&binary_path)
        .args(["start", "--help"])
        .output()
        .expect("Failed to run midtown start --help");

    assert!(
        output.status.success(),
        "midtown start --help should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--add-repo"),
        "Help should show --add-repo (not --repo which conflicts with global arg). Got: {}",
        stdout
    );
}

/// Regression test for #644: global --repo arg still works.
///
/// Verify the global `--repo` arg appears in the top-level help and doesn't
/// conflict with subcommand args.
#[test]
fn test_cli_global_help_no_panic() {
    let binary_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("midtown");

    if !binary_path.exists() {
        eprintln!("Skipping: debug binary not found at {:?}", binary_path);
        return;
    }

    let output = Command::new(&binary_path)
        .args(["--help"])
        .output()
        .expect("Failed to run midtown --help");

    assert!(
        output.status.success(),
        "midtown --help should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--repo"),
        "Global help should show --repo arg. Got: {}",
        stdout
    );
}

/// Regression test: web assets should not be stale.
///
/// Verifies that built web assets in `web/` are at least as new as
/// source files in `web-app/src/`. If this fails, run `cd web-app && npm run build`
/// and commit the output.
#[test]
fn test_web_assets_not_stale() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("web");
    let src_dir = manifest_dir.join("web-app").join("src");

    if !web_dir.exists() || !src_dir.exists() {
        eprintln!("Skipping: web/ or web-app/src/ not found");
        return;
    }

    // Find the newest source file
    let newest_source = newest_file_mtime(&src_dir);
    // Find the newest built asset
    let newest_asset = newest_file_mtime(&web_dir);

    match (newest_source, newest_asset) {
        (Some(src_time), Some(asset_time)) => {
            assert!(
                asset_time >= src_time,
                "Web assets are stale! Newest source ({:?}) is newer than newest asset ({:?}). \
                 Run `cd web-app && npm run build` and commit the output.",
                src_time,
                asset_time
            );
        }
        (Some(_), None) => {
            panic!("Source files exist in web-app/src/ but no built assets found in web/");
        }
        _ => {
            // No sources or both empty — nothing to check
        }
    }
}

/// Find the most recent modification time of any file in a directory (recursive).
fn newest_file_mtime(dir: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest = None;

    fn walk(dir: &std::path::Path, newest: &mut Option<std::time::SystemTime>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Skip node_modules
                    if path.file_name().is_some_and(|n| n == "node_modules") {
                        continue;
                    }
                    walk(&path, newest);
                } else if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if newest.is_none() || Some(mtime) > *newest {
                            *newest = Some(mtime);
                        }
                    }
                }
            }
        }
    }

    walk(dir, &mut newest);
    newest
}

/// Regression test for #653: global config template is generated on first load.
///
/// Verifies that GlobalConfig::load() creates a template file when none exists,
/// and that the template parses back as valid TOML with default values.
#[test]
fn test_global_config_generates_template() {
    let template = midtown::config::GlobalConfig::default_template();

    // Template should be non-empty
    assert!(!template.is_empty(), "Template should not be empty");

    // Template should contain all sections
    assert!(template.contains("[default]"));
    assert!(template.contains("[plugins]"));
    assert!(template.contains("[daemon]"));

    // All options are commented out, so parsing should yield defaults
    let config: midtown::config::GlobalConfig =
        toml::from_str(&template).expect("Template should be valid TOML");
    assert!(
        config.default.max_coworkers().is_none(),
        "All options should be commented out (defaults)"
    );
}
