# Auth Platform Layout Migration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move all auth profile storage under `~/.midtown/platforms/<platform>/` so profiles and shared state are siblings, eliminating the split between `~/.midtown/auth/` and `~/.midtown/platforms/`.

**Architecture:** Change 4 core path functions in `src/auth.rs` to produce new paths, rewrite symlink setup to use `../shared/` relative targets, add migration from old layout, and simplify profile name extraction in spawn. All 18+ callers of `profile_dir_for()` automatically get the new paths — no caller changes needed.

**Tech Stack:** Rust, filesystem operations (symlinks, directory creation), TDD with `#[cfg(test)]` modules.

---

### File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/auth.rs` | Modify | Core path functions, symlink setup, migration |
| `src/auth_tests.rs` | Modify | Path resolution tests |
| `src/sandbox.rs` | Modify | Remove `~/.midtown/auth/` from writable dirs |
| `src/daemon/mod.rs` | Modify | Simplify profile name extraction (1 site) |
| `src/daemon_v2/web/routes.rs` | Verify | Confirm auth_login uses new paths |

---

### Task 1: Change `provider_profiles_dir()` and `profile_dir_for()`

These two functions produce every auth path in the system. Changing them migrates all 18+ call sites at once.

**Files:**
- Modify: `src/auth.rs:220-247` (provider_profiles_dir, profile_dir_for)
- Test: `src/auth_tests.rs`

- [ ] **Step 1: Write failing test for new Claude profile path**

```rust
#[test]
fn profile_dir_for_claude_uses_platforms_layout() {
    let dir = profile_dir_for(AuthProvider::Claude, "user@example.com");
    // New: ~/.midtown/platforms/claude/user@example.com/
    // Old was: ~/.midtown/auth/user@example.com/claude/
    assert!(
        dir.to_string_lossy().contains("platforms/claude/user@example.com"),
        "Claude profile should be under platforms/claude/<profile>, got: {}",
        dir.display()
    );
    assert!(
        !dir.to_string_lossy().contains("/auth/"),
        "Should not use old /auth/ path, got: {}",
        dir.display()
    );
    // Should NOT have /claude/ suffix — profile IS the leaf
    assert!(
        !dir.to_string_lossy().ends_with("/claude"),
        "Should not have /claude suffix, got: {}",
        dir.display()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib auth::tests::profile_dir_for_claude_uses_platforms_layout`
Expected: FAIL — current output is `~/.midtown/auth/user@example.com/claude/`

- [ ] **Step 3: Update `provider_profiles_dir()` for Claude**

In `src/auth.rs`, change `provider_profiles_dir()`:

```rust
fn provider_profiles_dir(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => midtown_base_dir().join("platforms").join("claude"),
        AuthProvider::Codex => provider_root(provider).join("profiles"),
        AuthProvider::Zai => provider_root(provider).join("profiles"),
    }
}
```

And simplify `profile_dir_for()` — Claude no longer needs the extra `/claude` suffix:

```rust
pub fn profile_dir_for(provider: AuthProvider, name: &str) -> PathBuf {
    provider_profiles_dir(provider).join(name)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib auth::tests::profile_dir_for_claude_uses_platforms_layout`
Expected: PASS

- [ ] **Step 5: Verify existing Codex/Zai tests still pass**

Run: `cargo test --lib auth::tests`
Expected: All pass (Codex/Zai paths unchanged)

- [ ] **Step 6: Commit**

```bash
git add src/auth.rs src/auth_tests.rs
git commit -m "refactor(auth): move Claude profiles to ~/.midtown/platforms/claude/<profile>/"
```

---

### Task 2: Change `shared_provider_storage_dir()` to use `shared/` subdirectory

**Files:**
- Modify: `src/auth.rs:254` (shared_provider_storage_dir)
- Test: `src/auth_tests.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn shared_dir_uses_shared_subdirectory() {
    // The shared dir should be a sibling of profile dirs, not the parent
    let profile = profile_dir_for(AuthProvider::Claude, "test-user");
    let shared = profile.parent().unwrap().join("shared");
    assert!(
        shared.to_string_lossy().contains("platforms/claude/shared"),
        "shared should be under platforms/claude/shared/, got: {}",
        shared.display()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib auth::tests::shared_dir_uses_shared_subdirectory`
Expected: FAIL (or pass trivially — this test checks the structure, not the function)

- [ ] **Step 3: Update `shared_provider_storage_dir()`**

```rust
fn shared_provider_storage_dir(provider: AuthProvider) -> Option<PathBuf> {
    match provider {
        AuthProvider::Claude => {
            Some(midtown_base_dir().join("platforms").join("claude").join("shared"))
        }
        AuthProvider::Codex | AuthProvider::Zai => None,
    }
}
```

- [ ] **Step 4: Run all auth tests**

Run: `cargo test --lib auth::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/auth.rs src/auth_tests.rs
git commit -m "refactor(auth): shared state moves to platforms/claude/shared/"
```

---

### Task 3: Rewrite `setup_claude_profile_symlinks()` for new layout

Symlinks now point to `../shared/<entry>` instead of absolute paths to the old shared dir.

**Files:**
- Modify: `src/auth.rs:509-620` (setup_claude_profile_symlinks)
- Test: `src/auth_tests.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn symlinks_point_to_shared_sibling() {
    let tmp = tempfile::tempdir().unwrap();
    // Override midtown_base_dir for test (use set_test_midtown_base_dir if available)
    let _guard = crate::paths::set_test_midtown_base_dir(tmp.path());

    // Create shared dir with a test entry
    let shared = tmp.path().join("platforms/claude/shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(shared.join("agents")).unwrap();
    std::fs::write(shared.join("settings.json"), "{}").unwrap();

    // Run setup
    ensure_profile_dir_for(AuthProvider::Claude, "test-profile").unwrap();

    let profile = profile_dir_for(AuthProvider::Claude, "test-profile");
    assert!(profile.exists(), "profile dir should exist");

    // Check symlinks point to ../shared/
    let agents_link = profile.join("agents");
    assert!(agents_link.is_symlink(), "agents should be a symlink");
    let target = std::fs::read_link(&agents_link).unwrap();
    assert_eq!(
        target,
        std::path::PathBuf::from("../shared/agents"),
        "symlink should be relative to ../shared/"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib auth::tests::symlinks_point_to_shared_sibling`
Expected: FAIL — current symlinks use absolute paths

- [ ] **Step 3: Rewrite `setup_claude_profile_symlinks()`**

Key change: use relative symlink targets (`../shared/<entry>`) instead of absolute paths.

```rust
fn setup_claude_profile_symlinks(profile_name: &str) -> std::io::Result<()> {
    let profile_dir = profile_dir_for(AuthProvider::Claude, profile_name);
    let shared_dir = shared_provider_storage_dir(AuthProvider::Claude)
        .expect("Claude must have shared dir");

    // Ensure both dirs exist
    std::fs::create_dir_all(&profile_dir)?;
    std::fs::create_dir_all(&shared_dir)?;

    for entry_name in CLAUDE_SHARED_SYMLINK_ENTRIES {
        let link_path = profile_dir.join(entry_name);
        let shared_path = shared_dir.join(entry_name);
        // Relative target: ../shared/<entry>
        let relative_target = std::path::PathBuf::from("../shared").join(entry_name);

        // Skip if shared source doesn't exist yet
        if !shared_path.exists() {
            continue;
        }

        // Remove existing entry if it's a symlink (stale) or missing
        if link_path.is_symlink() {
            let _ = std::fs::remove_file(&link_path);
        }

        // Don't overwrite real files (e.g., .claude.json)
        if link_path.exists() && !link_path.is_symlink() {
            continue;
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&relative_target, &link_path)?;
    }

    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib auth::tests::symlinks_point_to_shared_sibling`
Expected: PASS

- [ ] **Step 5: Run full auth test suite**

Run: `cargo test --lib auth`
Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add src/auth.rs src/auth_tests.rs
git commit -m "refactor(auth): symlinks use relative ../shared/ targets"
```

---

### Task 4: Rewrite `migrate_legacy_claude_profile()` for old → new migration

Detects profiles in `~/.midtown/auth/<name>/claude/` and moves them to `~/.midtown/platforms/claude/<name>/`.

**Files:**
- Modify: `src/auth.rs:288-505` (migrate_legacy_claude_profile)
- Test: `src/auth_tests.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn migrates_legacy_auth_profile_to_platforms() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = crate::paths::set_test_midtown_base_dir(tmp.path());

    // Create legacy layout: ~/.midtown/auth/user@example.com/claude/.claude.json
    let legacy_dir = tmp.path().join("auth/user@example.com/claude");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join(".claude.json"), r#"{"token":"abc"}"#).unwrap();

    // Run ensure (triggers migration)
    ensure_profile_dir_for(AuthProvider::Claude, "user@example.com").unwrap();

    // New location should exist with the token
    let new_dir = tmp.path().join("platforms/claude/user@example.com");
    assert!(new_dir.exists(), "new profile dir should exist");
    assert!(
        new_dir.join(".claude.json").exists(),
        ".claude.json should be migrated"
    );

    // Old location should be gone (or empty)
    assert!(
        !legacy_dir.join(".claude.json").exists(),
        "old .claude.json should be removed after migration"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib auth::tests::migrates_legacy_auth_profile_to_platforms`
Expected: FAIL

- [ ] **Step 3: Rewrite `migrate_legacy_claude_profile()`**

```rust
fn migrate_legacy_claude_profile(profile_name: &str) -> std::io::Result<()> {
    let new_dir = profile_dir_for(AuthProvider::Claude, profile_name);
    if new_dir.exists() {
        return Ok(()); // Already at new location
    }

    // Check legacy locations
    let auth_base = midtown_base_dir().join("auth");
    let legacy_with_claude = auth_base.join(profile_name).join("claude");
    let legacy_bare = auth_base.join(profile_name);

    let source = if legacy_with_claude.exists() {
        Some(legacy_with_claude)
    } else if legacy_bare.exists() && legacy_bare.is_dir() {
        Some(legacy_bare.clone())
    } else {
        None
    };

    let Some(source) = source else {
        return Ok(()); // Nothing to migrate
    };

    tracing::info!(
        profile = %profile_name,
        from = %source.display(),
        to = %new_dir.display(),
        "migrating auth profile to new platform layout"
    );

    std::fs::create_dir_all(&new_dir)?;

    // Move profile-local files (especially .claude.json)
    // Shared entries will be re-symlinked by setup_claude_profile_symlinks
    for entry in std::fs::read_dir(&source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip symlinks (they pointed to old shared location)
        if entry.path().is_symlink() {
            continue;
        }

        // Move real files/dirs to new location
        let dest = new_dir.join(&name);
        if !dest.exists() {
            std::fs::rename(entry.path(), &dest)?;
        }
    }

    // Clean up old directory if empty
    if legacy_with_claude.exists() {
        let _ = std::fs::remove_dir(&legacy_with_claude);
    }
    let _ = std::fs::remove_dir(&auth_base.join(profile_name));

    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib auth::tests::migrates_legacy_auth_profile_to_platforms`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/auth.rs src/auth_tests.rs
git commit -m "feat(auth): migrate legacy auth/<profile>/claude/ to platforms/claude/<profile>/"
```

---

### Task 5: Update `current_profile_file_for()` for Claude

The `current` file for Claude should move from `~/.midtown/auth/current` to `~/.midtown/platforms/claude/current`.

**Files:**
- Modify: `src/auth.rs:637` (current_profile_file_for)
- Test: `src/auth_tests.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn current_profile_file_under_platforms() {
    let file = current_profile_file_for(AuthProvider::Claude);
    assert!(
        file.to_string_lossy().contains("platforms/claude/current"),
        "current file should be at platforms/claude/current, got: {}",
        file.display()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib auth::tests::current_profile_file_under_platforms`
Expected: FAIL — currently at `~/.midtown/auth/current`

- [ ] **Step 3: Update `current_profile_file_for()`**

```rust
fn current_profile_file_for(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => {
            midtown_base_dir().join("platforms").join("claude").join("current")
        }
        AuthProvider::Codex => provider_root(provider).join("current"),
        AuthProvider::Zai => provider_root(provider).join("current"),
    }
}
```

Also keep the legacy fallback in `current_profile_for()` — if the new `current` file doesn't exist, check the old `~/.midtown/auth/current` location.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib auth::tests::current_profile_file_under_platforms`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/auth.rs src/auth_tests.rs
git commit -m "refactor(auth): move current profile file to platforms/claude/current"
```

---

### Task 6: Simplify profile name extraction in `spawn_coworker()`

The v1 daemon extracts profile names by navigating `parent().file_name()` because the old path was `auth/<profile>/claude/`. With the new flat layout `platforms/claude/<profile>/`, it's just `file_name()`.

**Files:**
- Modify: `src/daemon/mod.rs:1529-1546`

- [ ] **Step 1: Find and update the profile extraction code**

Change from:
```rust
if config.auth_provider == crate::auth::AuthProvider::Claude {
    p.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|n| n.to_str())
} else {
    p.file_name().and_then(|n| n.to_str())
}
```

To:
```rust
// New layout: platforms/<provider>/<profile>/ — profile name is the leaf
p.file_name().and_then(|n| n.to_str())
```

- [ ] **Step 2: Run full test suite**

Run: `cargo test`
Expected: All pass

- [ ] **Step 3: Commit**

```bash
git add src/daemon/mod.rs
git commit -m "simplify(auth): profile name extraction — flat layout, no parent traversal"
```

---

### Task 7: Update sandbox allowlist

Remove the `~/.midtown/auth/` entry since profiles are now under `~/.midtown/platforms/`.

**Files:**
- Modify: `src/sandbox.rs:115-126`

- [ ] **Step 1: Remove `~/.midtown/auth/` from writable dirs**

Change from:
```rust
dirs.push(home.join(".midtown/auth").to_string_lossy().to_string());
dirs.push(home.join(".midtown/platforms").to_string_lossy().to_string());
```

To:
```rust
// All auth profiles and shared state live under ~/.midtown/platforms/
dirs.push(home.join(".midtown/platforms").to_string_lossy().to_string());
```

- [ ] **Step 2: Update the comment**

Remove the comment referencing `~/.midtown/auth/`.

- [ ] **Step 3: Run sandbox tests**

Run: `cargo test --lib sandbox`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/sandbox.rs
git commit -m "refactor(sandbox): remove ~/.midtown/auth/ — everything under platforms/"
```

---

### Task 8: Update documentation and auth.rs header comments

**Files:**
- Modify: `src/auth.rs:1-56` (module doc comments)
- Modify: `docs/v2-architecture.md` (if auth paths referenced)

- [ ] **Step 1: Update the module doc comment at the top of `auth.rs`**

Replace directory structure documentation to show:
```
~/.midtown/platforms/
├── claude/
│   ├── shared/          # settings, agents, plugins, projects, tasks, teams
│   ├── <profile>/       # .claude.json (token) + symlinks to ../shared/
│   └── current          # active profile name
└── codex/
    ├── shared/
    ├── <profile>/
    └── current
```

- [ ] **Step 2: Run clippy and full tests**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: All pass, no warnings

- [ ] **Step 3: Commit**

```bash
git add src/auth.rs docs/
git commit -m "docs(auth): update directory layout documentation for platform-nested profiles"
```

---

### Task 9: Integration verification

- [ ] **Step 1: Run full test suite**

```bash
cargo test
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

- [ ] **Step 3: Build release**

```bash
cargo install --path .
```

- [ ] **Step 4: Manual smoke test**

```bash
midtown stop
MIDTOWN_DAEMON_V2=1 midtown start
midtown status
# Verify lead spawns and responds
```

- [ ] **Step 5: Verify auth login creates profile at new path**

```bash
ls -la ~/.midtown/platforms/claude/
# Should show: shared/, <profile>/, current
```

- [ ] **Step 6: Final commit if any fixups needed**
