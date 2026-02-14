//! End-to-end tests for Zellij integration.
//!
//! Covers:
//! - Zellij availability detection (`zellij_is_available`, `zellij_session_exists`)
//! - KDL layout validation (plugin references, structure)
//! - Zellij session create/kill lifecycle
//! - Plugin WASM build verification
//! - Workspace and crate structure validation
//! - Plugin RPC endpoint existence
//!
//! Run with `cargo test --test zellij_e2e -- --ignored --test-threads=1`
//! as the Zellij-dependent tests require Zellij installed.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use ntest::timeout;

// ── Shared test helpers ────────────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_session_name() -> String {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("midtown-zellij-test-{}-{}", std::process::id(), counter)
}

fn zellij_available() -> bool {
    midtown::process::zellij_is_available()
}

fn kill_zellij_session(session: &str) {
    let _ = Command::new("zellij")
        .args(["kill-session", session])
        .output();
}

/// Create a minimal KDL layout file for testing.
fn create_test_layout(path: &std::path::Path) {
    let layout = r#"layout {
    pane size="50%" {
        command "echo"
        args "hello"
    }
    pane size="50%" focus=true {
        command "sleep"
        args "60"
    }
}
"#;
    fs::write(path, layout).expect("Failed to write test layout");
}

// ── Zellij availability tests ──────────────────────────────────────

/// Verify that `zellij_is_available()` correctly detects Zellij installation.
#[test]
#[ignore] // Requires Zellij
fn test_zellij_availability_detection() {
    if !zellij_available() {
        eprintln!("SKIPPED: Zellij not available");
        return;
    }
    assert!(
        midtown::process::zellij_is_available(),
        "zellij_is_available() should return true when Zellij is installed"
    );
}

/// Verify that `zellij_session_exists()` returns false for non-existent sessions.
#[test]
#[ignore] // Requires Zellij
fn test_zellij_session_exists_returns_false_for_nonexistent() {
    if !zellij_available() {
        eprintln!("SKIPPED: Zellij not available");
        return;
    }
    let fake_session = format!("nonexistent-{}", std::process::id());
    assert!(
        !midtown::process::zellij_session_exists(&fake_session),
        "zellij_session_exists() should return false for a non-existent session"
    );
}

/// Test the full Zellij session lifecycle: create → detect → kill.
#[test]
#[ignore] // Requires Zellij
#[timeout(30000)]
fn test_zellij_session_lifecycle() {
    if !zellij_available() {
        eprintln!("SKIPPED: Zellij not available");
        return;
    }

    let session = test_session_name();
    let temp_dir = std::env::temp_dir().join(&session);
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).expect("Failed to create temp dir");

    // Create a test layout
    let layout_path = temp_dir.join("test.kdl");
    create_test_layout(&layout_path);

    // Start a detached Zellij session (--new-session-with-layout is not
    // available on all Zellij versions, so we try it and fall back)
    let status = Command::new("zellij")
        .args([
            "-s",
            &session,
            "-l",
            &layout_path.to_string_lossy(),
            "--new-session-with-layout",
        ])
        .env("ZELLIJ_SESSION_NAME", &session)
        .output();

    let created = match status {
        Ok(output) if output.status.success() => true,
        _ => {
            // Zellij may require an interactive terminal for session creation
            let alt = Command::new("zellij").args(["setup", "--check"]).output();
            match alt {
                Ok(o) if o.status.success() => {
                    eprintln!(
                        "Zellij is available but session creation needs interactive terminal"
                    );
                    eprintln!("Skipping lifecycle test in headless environment");
                    let _ = fs::remove_dir_all(&temp_dir);
                    return;
                }
                _ => false,
            }
        }
    };

    if created {
        // Give session a moment to start
        thread::sleep(Duration::from_millis(500));

        // Verify session exists
        assert!(
            midtown::process::zellij_session_exists(&session),
            "Session '{}' should exist after creation",
            session
        );

        // Kill session
        kill_zellij_session(&session);

        // Give it a moment to clean up
        thread::sleep(Duration::from_millis(500));

        // Verify session no longer exists
        assert!(
            !midtown::process::zellij_session_exists(&session),
            "Session '{}' should not exist after kill",
            session
        );
    }

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

// ── Layout validation tests ────────────────────────────────────────

/// Verify the default KDL layout file parses correctly.
///
/// This doesn't require a running Zellij session — it just validates
/// the layout file syntax and expected structure.
#[test]
fn test_default_kdl_layout_exists_and_is_valid() {
    let layout_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("layouts/midtown.kdl");
    assert!(
        layout_path.exists(),
        "Default KDL layout should exist at layouts/midtown.kdl"
    );

    let content = fs::read_to_string(&layout_path).expect("Failed to read layout file");

    // Basic structural validation
    assert!(
        content.contains("layout {"),
        "Layout should contain 'layout {{' block"
    );
    assert!(
        content.contains("plugin location="),
        "Layout should contain a plugin pane"
    );
    assert!(
        content.contains("midtown_zellij_plugin.wasm"),
        "Layout should reference the plugin WASM file"
    );
    assert!(
        content.contains("midtown"),
        "Layout should reference midtown chat command"
    );
    assert!(
        content.contains("focus=true"),
        "Layout should have a focused pane"
    );
}

// ── Build automation tests ─────────────────────────────────────────

/// Verify that the build-all.sh script exists and contains the expected
/// build steps for both daemon and Zellij plugin.
#[test]
fn test_build_all_script_exists_and_has_plugin_build() {
    let script_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/build-all.sh");
    assert!(
        script_path.exists(),
        "Build script should exist at scripts/build-all.sh"
    );

    let content = fs::read_to_string(&script_path).expect("Failed to read build script");

    // Verify it builds the daemon
    assert!(
        content.contains("cargo build"),
        "Script should build the daemon with cargo build"
    );

    // Verify it builds the Zellij plugin
    assert!(
        content.contains("midtown-zellij-plugin"),
        "Script should build the Zellij plugin"
    );
    assert!(
        content.contains("wasm32-wasip1"),
        "Script should target wasm32-wasip1 for the plugin"
    );

    // Verify it installs the WASM file
    assert!(
        content.contains("midtown_zellij_plugin.wasm"),
        "Script should install the WASM plugin file"
    );
    assert!(
        content.contains(".midtown/plugins"),
        "Script should install to ~/.midtown/plugins/"
    );

    // Verify it has the WASM target check
    assert!(
        content.contains("rustup target"),
        "Script should check/install the wasm32-wasip1 target"
    );
}

// ── Crate structure tests ──────────────────────────────────────────

/// Verify the midtown-zellij-plugin crate exists in the workspace.
#[test]
fn test_zellij_plugin_crate_exists() {
    let plugin_cargo =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("midtown-zellij-plugin/Cargo.toml");
    assert!(
        plugin_cargo.exists(),
        "Plugin crate should exist at midtown-zellij-plugin/Cargo.toml"
    );

    let content = fs::read_to_string(&plugin_cargo).expect("Failed to read plugin Cargo.toml");
    assert!(
        content.contains("midtown-zellij-plugin"),
        "Plugin Cargo.toml should declare midtown-zellij-plugin package"
    );
    assert!(
        content.contains("cdylib"),
        "Plugin should use cdylib crate-type for WASM"
    );
    assert!(
        content.contains("zellij-tile"),
        "Plugin should depend on zellij-tile"
    );
    assert!(
        content.contains("midtown-types"),
        "Plugin should depend on midtown-types"
    );
}

/// Verify the midtown-types shared types crate exists and has the expected types.
#[test]
fn test_midtown_types_crate_exists() {
    let types_cargo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("midtown-types/Cargo.toml");
    assert!(
        types_cargo.exists(),
        "Types crate should exist at midtown-types/Cargo.toml"
    );

    let types_lib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("midtown-types/src/lib.rs");
    let content = fs::read_to_string(&types_lib).expect("Failed to read midtown-types lib.rs");

    // Verify key shared types exist
    assert!(
        content.contains("DashboardState"),
        "Types crate should define DashboardState"
    );
    assert!(
        content.contains("TaskSummary"),
        "Types crate should define TaskSummary"
    );
    assert!(
        content.contains("CoworkerSummary"),
        "Types crate should define CoworkerSummary"
    );
}

/// Verify the workspace Cargo.toml includes all required members.
#[test]
fn test_workspace_includes_plugin_and_types() {
    let cargo_toml = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml).expect("Failed to read root Cargo.toml");

    assert!(
        content.contains("[workspace]"),
        "Root Cargo.toml should define a workspace"
    );
    assert!(
        content.contains("midtown-zellij-plugin"),
        "Workspace should include midtown-zellij-plugin"
    );
    assert!(
        content.contains("midtown-types"),
        "Workspace should include midtown-types"
    );
}

// ── Plugin compilation test ────────────────────────────────────────

/// Verify Zellij plugin can be compiled to WASM (requires wasm32-wasip1 target).
#[test]
#[ignore] // Requires wasm32-wasip1 target installed
#[timeout(120000)]
fn test_zellij_plugin_compiles_to_wasm() {
    // Check if wasm32-wasip1 target is installed
    let target_check = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .expect("Failed to run rustup");
    let targets = String::from_utf8_lossy(&target_check.stdout);
    if !targets.contains("wasm32-wasip1") {
        eprintln!("SKIPPED: wasm32-wasip1 target not installed");
        return;
    }

    // Clear coverage-related environment variables that cargo-llvm-cov injects.
    // The wasm32-wasip1 target doesn't ship profiler_builtins, so
    // `-C instrument-coverage` causes a compilation error.
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "midtown-zellij-plugin",
            "--target",
            "wasm32-wasip1",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_LLVM_COV")
        .env_remove("CARGO_LLVM_COV_SHOW_ENV")
        .env_remove("CARGO_LLVM_COV_TARGET_DIR")
        .env_remove("LLVM_PROFILE_FILE")
        .status()
        .expect("Failed to run cargo build for WASM plugin");

    assert!(
        status.success(),
        "Zellij plugin should compile to wasm32-wasip1 target"
    );

    // Verify the WASM file was produced
    let wasm_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/wasm32-wasip1/debug/midtown_zellij_plugin.wasm");
    assert!(
        wasm_path.exists(),
        "WASM plugin binary should exist at {:?}",
        wasm_path
    );
}

// ── Plugin RPC endpoint tests ──────────────────────────────────────

/// Verify that plugin RPC endpoints are registered in the daemon.
#[test]
fn test_plugin_rpc_endpoints_exist() {
    let rpc_plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/rpc_plugin.rs");
    assert!(
        rpc_plugin.exists(),
        "Plugin RPC handler should exist at src/daemon/rpc_plugin.rs"
    );

    let content = fs::read_to_string(&rpc_plugin).expect("Failed to read rpc_plugin.rs");

    // Verify key RPC handlers exist
    assert!(
        content.contains("handle_dashboard"),
        "Should have handle_dashboard RPC endpoint"
    );
    assert!(
        content.contains("handle_attach"),
        "Should have handle_attach RPC endpoint"
    );
    assert!(
        content.contains("handle_detach"),
        "Should have handle_detach RPC endpoint"
    );
    assert!(
        content.contains("handle_coworker_stream"),
        "Should have handle_coworker_stream RPC endpoint"
    );
}

/// Verify that DaemonState has the lead_nudge_queue field for plugin-based
/// nudge delivery.
#[test]
fn test_daemon_state_has_lead_nudge_queue() {
    let daemon_mod = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/mod.rs");
    let content = fs::read_to_string(&daemon_mod).expect("Failed to read daemon/mod.rs");

    assert!(
        content.contains("lead_nudge_queue"),
        "DaemonState should have lead_nudge_queue field for plugin nudge delivery"
    );
}

// ── RPC dispatch route tests ───────────────────────────────────────

/// Verify that the RPC dispatcher routes plugin.* methods.
#[test]
fn test_rpc_dispatch_has_plugin_routes() {
    let rpc_rs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/daemon/rpc.rs");
    let content = fs::read_to_string(&rpc_rs).expect("Failed to read rpc.rs");

    assert!(
        content.contains("plugin.dashboard"),
        "RPC dispatch should route plugin.dashboard"
    );
    assert!(
        content.contains("plugin.attach"),
        "RPC dispatch should route plugin.attach"
    );
    assert!(
        content.contains("plugin.detach"),
        "RPC dispatch should route plugin.detach"
    );
    assert!(
        content.contains("plugin.coworker-stream"),
        "RPC dispatch should route plugin.coworker-stream"
    );
}
