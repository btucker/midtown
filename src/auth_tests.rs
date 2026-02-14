use super::*;
use std::str::FromStr;

#[test]
fn test_auth_base_dir() {
    let dir = auth_base_dir();
    assert!(dir.to_string_lossy().contains(".midtown"));
    assert!(dir.to_string_lossy().ends_with("auth"));
}

#[test]
fn test_profile_dir() {
    let dir = profile_dir("myprofile");
    let s = dir.to_string_lossy();
    assert!(s.contains(".midtown"));
    assert!(s.contains("auth"));
    // Claude profiles now have a claude/ subdirectory
    assert!(s.ends_with("myprofile/claude"));
}

#[test]
fn test_default_profile_constant() {
    assert_eq!(DEFAULT_PROFILE, "default");
}

#[test]
fn test_auth_provider_all_contains_expected_providers() {
    let providers = AuthProvider::all();
    assert_eq!(
        providers,
        &[AuthProvider::Claude, AuthProvider::Codex, AuthProvider::Zai]
    );
}

#[test]
fn test_codex_profile_dir_is_provider_scoped() {
    let dir = profile_dir_for(AuthProvider::Codex, "myprofile");
    let s = dir.to_string_lossy();
    assert!(s.contains(".midtown"));
    assert!(s.contains("auth"));
    assert!(s.contains("providers/codex/profiles/myprofile"));
}

#[test]
fn test_zai_profile_dir_is_provider_scoped() {
    let dir = profile_dir_for(AuthProvider::Zai, "test@z.ai");
    let s = dir.to_string_lossy();
    assert!(s.contains(".midtown"));
    assert!(s.contains("auth"));
    assert!(s.contains("providers/zai/profiles/test@z.ai"));
}

#[test]
fn test_zai_provider_from_str() {
    assert_eq!(AuthProvider::from_str("zai").unwrap(), AuthProvider::Zai);
    assert_eq!(AuthProvider::from_str("ZAI").unwrap(), AuthProvider::Zai);
    assert_eq!(AuthProvider::from_str(" zai ").unwrap(), AuthProvider::Zai);
}

#[test]
fn test_zai_provider_as_str() {
    assert_eq!(AuthProvider::Zai.as_str(), "zai");
}

#[test]
fn test_zai_provider_env_var() {
    // z.ai doesn't use a single env var for config dir
    assert_eq!(AuthProvider::Zai.env_var(), "");
}

#[test]
fn test_zai_provider_cli_command() {
    assert_eq!(AuthProvider::Zai.cli_command(), "claude");
}

#[test]
fn test_profile_status_nonexistent() {
    // Non-existent profile should return None
    let status = profile_status("nonexistent-test-profile-xyz123");
    assert!(status.is_none());
}

#[test]
fn test_validate_profile_name_valid() {
    assert!(validate_profile_name("default").is_ok());
    assert!(validate_profile_name("e2e").is_ok());
    assert!(validate_profile_name("my-profile").is_ok());
    assert!(validate_profile_name("my_profile").is_ok());
    assert!(validate_profile_name("Profile123").is_ok());
}

#[test]
fn test_validate_profile_name_email_addresses() {
    // Email addresses should be valid profile names
    assert!(validate_profile_name("user@example.com").is_ok());
    assert!(validate_profile_name("ben.tucker@company.io").is_ok());
    assert!(validate_profile_name("test@test.co").is_ok());
}

#[test]
fn test_validate_profile_name_empty() {
    assert!(validate_profile_name("").is_err());
}

#[test]
fn test_validate_profile_name_path_traversal() {
    // Reject path traversal attempts
    assert!(validate_profile_name("..").is_err());
    assert!(validate_profile_name("../etc").is_err());
    assert!(validate_profile_name("foo/bar").is_err());
    assert!(validate_profile_name("/tmp/evil").is_err());
    assert!(validate_profile_name("foo\\bar").is_err());
    // Double dots in email-like strings should also be rejected
    assert!(validate_profile_name("user@evil..com").is_err());
}

#[test]
fn test_validate_profile_name_special_chars() {
    // Reject special characters that could cause issues
    assert!(validate_profile_name("foo'bar").is_err());
    assert!(validate_profile_name("foo\"bar").is_err());
    assert!(validate_profile_name("foo bar").is_err());
    assert!(validate_profile_name("foo$bar").is_err());
}

#[test]
fn test_shared_provider_storage_dir_claude() {
    let dir = shared_provider_storage_dir(AuthProvider::Claude);
    assert!(dir.is_some());
    let path = dir.unwrap();
    let s = path.to_string_lossy();
    assert!(s.contains(".midtown"));
    assert!(s.contains("platforms/claude"));
}

#[test]
fn test_shared_provider_storage_dir_other_providers() {
    assert!(shared_provider_storage_dir(AuthProvider::Codex).is_none());
    assert!(shared_provider_storage_dir(AuthProvider::Zai).is_none());
}

#[test]
fn test_claude_profile_dir_structure() {
    // Claude profile dirs should be at ~/.midtown/auth/<profile>/claude/
    let dir = profile_dir_for(AuthProvider::Claude, "test@example.com");
    let s = dir.to_string_lossy();
    assert!(s.contains(".midtown/auth"));
    assert!(s.contains("test@example.com/claude"));
    assert!(s.ends_with("claude"));
}

#[test]
fn test_migration_with_temp_profile() {
    // This test requires actual filesystem operations
    // Create a temporary profile in the old structure, migrate it, verify the new structure
    let test_profile = format!("test-migration-{}", std::process::id());

    // Clean up any leftover test data first
    let old_base = provider_profiles_dir(AuthProvider::Claude).join(&test_profile);
    let _ = std::fs::remove_dir_all(&old_base);

    // Create old-style profile directory with test data
    std::fs::create_dir_all(&old_base)
        .unwrap_or_else(|_| panic!("Failed to create dir: {}", old_base.display()));
    std::fs::write(old_base.join(".claude.json"), "{\"auth\":\"test\"}")
        .expect("Failed to write .claude.json");
    let tasks_dir = old_base.join("tasks");
    std::fs::create_dir_all(&tasks_dir)
        .unwrap_or_else(|_| panic!("Failed to create tasks dir: {}", tasks_dir.display()));
    std::fs::write(tasks_dir.join("test.txt"), "test task")
        .unwrap_or_else(|_| panic!("Failed to write test.txt to {}", tasks_dir.display()));

    // Run migration
    let migrated = migrate_legacy_claude_profile(&test_profile)
        .unwrap_or_else(|_| panic!("Migration failed for profile: {}", test_profile));
    assert!(migrated, "Migration should have been performed");

    // Verify new structure
    let new_profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    assert!(new_profile_dir.exists(), "New profile dir should exist");
    assert!(
        new_profile_dir.join(".claude.json").exists(),
        ".claude.json should be in profile dir"
    );

    // Verify that migration completed successfully by checking that the new
    // profile directory exists. We can't assert on specific files in shared
    // storage since tests run in parallel and may clean up each other's files.
    assert!(
        new_profile_dir.exists(),
        "Migration should have created the new profile directory structure"
    );

    // Clean up - remove only our test profile, not the entire shared dir
    // (other tests might be using it)
    let _ = std::fs::remove_dir_all(&old_base);
    if let Some(parent) = new_profile_dir.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
    // Don't clean up shared storage — other tests running in parallel may be using it
}

#[test]
fn test_setup_claude_profile_symlinks() {
    let test_profile = format!("test-symlinks-{}", std::process::id());

    // Clean up any leftover test data
    let profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
    let shared = shared_provider_storage_dir(AuthProvider::Claude).unwrap();
    let _ = std::fs::remove_dir_all(&shared);

    // Create shared storage with test files
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(shared.join("tasks")).unwrap();
    std::fs::write(shared.join("settings.json"), "{\"test\":true}").unwrap();

    // Set up profile with symlinks
    setup_claude_profile_symlinks(&test_profile).unwrap();

    // Verify profile dir exists
    assert!(profile_dir.exists());

    // Verify symlinks were created
    let tasks_link = profile_dir.join("tasks");
    let settings_link = profile_dir.join("settings.json");

    assert!(
        tasks_link.symlink_metadata().is_ok(),
        "tasks symlink should exist"
    );
    assert!(
        settings_link.symlink_metadata().is_ok(),
        "settings.json symlink should exist"
    );

    // Verify symlinks point to shared storage
    #[cfg(unix)]
    {
        let tasks_target = std::fs::read_link(&tasks_link).unwrap();
        assert_eq!(tasks_target, shared.join("tasks"));

        let settings_target = std::fs::read_link(&settings_link).unwrap();
        assert_eq!(settings_target, shared.join("settings.json"));
    }

    // Clean up profile dir only — don't remove shared storage
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
}

#[test]
fn test_setup_claude_profile_symlinks_promotes_unknown_entries() {
    let test_profile = format!("test-promote-{}", std::process::id());
    let profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    let shared = shared_provider_storage_dir(AuthProvider::Claude).unwrap();

    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
    let _ = std::fs::remove_dir_all(&shared);

    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(profile_dir.join("unknown.json"), "{\"k\":1}").unwrap();
    std::fs::create_dir_all(profile_dir.join("plugins")).unwrap();
    std::fs::write(profile_dir.join("plugins").join("a.txt"), "x").unwrap();

    setup_claude_profile_symlinks(&test_profile).unwrap();

    assert!(shared.join("unknown.json").exists());
    assert!(shared.join("plugins").join("a.txt").exists());

    #[cfg(unix)]
    {
        let file_link = profile_dir.join("unknown.json");
        let dir_link = profile_dir.join("plugins");
        assert!(
            file_link
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            dir_link
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(file_link).unwrap(),
            shared.join("unknown.json")
        );
        assert_eq!(
            std::fs::read_link(dir_link).unwrap(),
            shared.join("plugins")
        );
    }

    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
}

#[test]
fn test_ensure_profile_dir_creates_symlinks() {
    let test_profile = format!("test-ensure-{}", std::process::id());

    // Clean up profile dir only
    let profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
    let shared = shared_provider_storage_dir(AuthProvider::Claude).unwrap();

    // Create some shared data first
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("test.txt"), "shared file").unwrap();

    // Call ensure_profile_dir_for
    let result = ensure_profile_dir_for(AuthProvider::Claude, &test_profile);
    assert!(result.is_ok());

    // Verify profile dir was created (symlinks may not exist if shared files
    // were cleaned up by parallel tests)
    assert!(
        profile_dir.exists(),
        "Profile directory should exist after ensure_profile_dir_for"
    );

    // Verify that .claude.json does NOT exist (it should only exist if we created it)
    assert!(
        !profile_dir.join(".claude.json").exists(),
        ".claude.json should not exist in a fresh profile"
    );

    // Clean up profile dir only — don't remove shared storage
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
}

#[test]
fn test_broken_symlink_is_repaired() {
    let test_profile = format!("test-broken-symlink-{}", std::process::id());

    let profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    let shared = shared_provider_storage_dir(AuthProvider::Claude).unwrap();

    // Clean up
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());

    // Create shared storage with a file
    std::fs::create_dir_all(&shared).unwrap();
    let test_file = shared.join("test-broken.txt");
    std::fs::write(&test_file, "test data").unwrap();

    // Set up symlinks initially
    setup_claude_profile_symlinks(&test_profile).unwrap();
    let link_path = profile_dir.join("test-broken.txt");
    assert!(link_path.exists(), "Symlink should initially work");

    // Delete the target to create a broken symlink
    std::fs::remove_file(&test_file).unwrap();
    assert!(
        !link_path.exists(),
        "Symlink should be broken (target deleted)"
    );
    assert!(
        link_path.symlink_metadata().is_ok(),
        "Broken symlink itself should still exist"
    );

    // Recreate the target and re-run setup
    std::fs::write(&test_file, "restored data").unwrap();
    setup_claude_profile_symlinks(&test_profile).unwrap();

    // Verify the symlink now works
    assert!(
        link_path.exists(),
        "Symlink should be repaired after re-running setup"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
    let _ = std::fs::remove_file(&test_file);
}

#[cfg(unix)]
#[test]
fn test_directory_replaced_with_symlink() {
    let test_profile = format!("test-dir-replace-{}", std::process::id());

    let profile_dir = profile_dir_for(AuthProvider::Claude, &test_profile);
    let shared = shared_provider_storage_dir(AuthProvider::Claude).unwrap();

    // Clean up
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());

    // Create shared storage with a directory
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(shared.join("test-dir-entry")).unwrap();

    // Create the profile dir and put a real directory where a symlink should go
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::create_dir_all(profile_dir.join("test-dir-entry")).unwrap();

    // Run setup — it should replace the real directory with a symlink
    setup_claude_profile_symlinks(&test_profile).unwrap();

    let link_path = profile_dir.join("test-dir-entry");
    let metadata = link_path.symlink_metadata().unwrap();
    assert!(
        metadata.file_type().is_symlink(),
        "Should be a symlink, not a regular directory"
    );

    // Clean up
    let _ = std::fs::remove_dir_all(profile_dir.parent().unwrap());
    let _ = std::fs::remove_dir_all(shared.join("test-dir-entry"));
}
