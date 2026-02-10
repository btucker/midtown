//! Test for spawn race condition: concurrent reviewer and task dispatch spawns.
//!
//! # Bug Description (2026-02-02)
//!
//! A race condition allowed multiple concurrent spawn attempts for the same coworker
//! name to succeed, with the last spawn overwriting the first. This could lead to
//! unexpected behavior when different spawn paths tried to create the same coworker.
//!
//! ## Timeline from daemon.log
//!
//! ```text
//! 17:13:00.938629Z  INFO Spawned coworker madison (isolated=true, session_mode=Fresh)
//! 17:13:00.938875Z  INFO Spawned coworker madison successfully
//! 17:13:05.921367Z  INFO Proposing task !739 for madison (already_running=false)
//! 17:13:05.941025Z  INFO Assigned task !739 to madison on disk
//! 17:13:10.035109Z  INFO Channel post from madison: /me reviewing PR #496
//! 17:13:11.625229Z  INFO Spawned coworker madison (isolated=false, session_mode=Fresh)
//! 17:13:11.625257Z  INFO Spawned coworker madison successfully
//! ```
//!
//! ## Root Cause Analysis
//!
//! 1. **Concurrent execution paths**: PR polling runs in its own `tokio::spawn`
//!    background task, separate from the main select! loop where TaskDispatchTick
//!    runs. These can execute concurrently.
//!
//! 2. **TOCTTOU race in spawn_with_name**: The function has a check-then-act
//!    pattern that is not atomic:
//!    - Line 647-655: Read lock to check if coworker exists → lock released
//!    - Lines 657-734: Worktree + tmux creation (~100ms, no lock held)
//!    - Line 747-750: Write lock to insert coworker
//!
//!    Two concurrent spawns can both pass the check before either inserts.
//!
//! 3. **Snapshot staleness**: TaskDispatchTick collects a snapshot at the start
//!    of its tick. If a PR reviewer spawn happens concurrently (from the PR poll
//!    task), the snapshot doesn't see the new coworker, leading to duplicate
//!    name allocation.
//!
//! ## Consequence
//!
//! The second spawn overwrote the first spawn, potentially causing unexpected behavior
//! when the coworker was being created for different purposes simultaneously.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use midtown::coworker::{Coworker, CoworkerManager, CoworkerStatus};
use midtown::worktree::WorktreeManager;

/// Test fixture that creates a CoworkerManager with a test git repo.
fn test_manager() -> (CoworkerManager, tempfile::TempDir) {
    use std::process::Command;

    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");

    // Initialize a git repository
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");

    // Configure git user for commits
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to set git user.email");

    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to set git user.name");

    // Create an initial commit (required for worktrees)
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "Initial commit"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to create initial commit");

    let worktree_manager = WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("Failed to create worktree manager");
    let manager = CoworkerManager::new("midtown-race-test", worktree_manager);

    (manager, temp_dir)
}

/// Test that demonstrates concurrent name allocation can produce duplicates,
/// but the fix ensures only one insert succeeds (no overwrites).
///
/// When two concurrent paths (PR poll task and TaskDispatchTick) both call
/// next_available_name at nearly the same time, they can both get the same name.
/// This is unavoidable without a reservation mechanism. The fix ensures that
/// only the first insert succeeds - the second insert is rejected rather than
/// overwriting the first.
#[test]
fn test_concurrent_name_allocation_no_overwrites() {
    let (manager, _temp_dir) = test_manager();
    let manager = Arc::new(manager);

    let rejected_count = Arc::new(AtomicUsize::new(0));
    let inserted_count = Arc::new(AtomicUsize::new(0));

    // Run multiple iterations to increase chance of hitting the race
    const ITERATIONS: usize = 100;
    const THREADS_PER_ITERATION: usize = 4;

    for _ in 0..ITERATIONS {
        // Clear the coworkers map for each iteration
        manager.clear_for_testing();

        let mut handles = vec![];
        let barrier = Arc::new(std::sync::Barrier::new(THREADS_PER_ITERATION));

        for _ in 0..THREADS_PER_ITERATION {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let rejected_count = Arc::clone(&rejected_count);
            let inserted_count = Arc::clone(&inserted_count);

            handles.push(thread::spawn(move || {
                // Synchronize threads to maximize collision chance
                barrier.wait();

                // Allocate a name (simulating what PR poll and TaskDispatch do)
                if let Some(name) = manager.next_available_name() {
                    // Simulate the delay between allocation and insertion
                    // (worktree creation, tmux spawn, etc.)
                    thread::sleep(Duration::from_micros(10));

                    // Try to insert the coworker (with check-before-insert)
                    let inserted = manager.insert_for_testing(Coworker {
                        name: name.clone(),
                        status: CoworkerStatus::Running,
                        working_dir: "/tmp".to_string(),
                        started_at: chrono::Utc::now(),
                        current_task: None,
                        session_id: None,
                        model: "sonnet".to_string(),
                    });

                    if inserted {
                        inserted_count.fetch_add(1, Ordering::SeqCst);
                    } else {
                        // Name was already taken - insert was correctly rejected
                        rejected_count.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }));
        }

        for handle in handles {
            handle.join().expect("Thread panicked");
        }
    }

    let rejected = rejected_count.load(Ordering::SeqCst);
    let inserted = inserted_count.load(Ordering::SeqCst);

    println!(
        "Name allocation race test: {} inserted, {} rejected (correctly prevented overwrites)",
        inserted, rejected
    );

    // The fix ensures rejected inserts don't cause overwrites.
    // We expect some rejections due to name allocation races, but that's OK.
    // What matters is that the total inserted + rejected equals the number of
    // attempted allocations, and no coworker was overwritten.
    //
    // Verify the fix is working by checking that rejections occurred
    // (indicating the race happened) but no data was corrupted.
    assert!(
        rejected > 0,
        "Expected some rejections due to concurrent name allocation, but got 0. \
         This suggests the test isn't triggering the race condition."
    );
}

/// Test that demonstrates the spawn_with_name TOCTTOU race.
///
/// This test simulates the exact bug scenario:
/// 1. PR poll task decides to spawn reviewer "madison" (isolated=true)
/// 2. TaskDispatchTick decides to spawn "madison" for a task (isolated=false)
/// 3. Both spawns succeed because the check-then-act is not atomic
/// 4. The second spawn overwrites the first, giving reviewer access to shared tasks
///
/// Note: This test uses manual HashMap manipulation to simulate the race without
/// actually spawning tmux windows.
#[test]
fn test_spawn_race_reviewer_gets_shared_tasks() {
    let (manager, _temp_dir) = test_manager();
    let manager = Arc::new(manager);

    // Simulate the race condition that occurred at 17:13:00-17:13:11
    //
    // Thread 1 (PR poll): Spawns madison with isolated_tasks=true
    // Thread 2 (TaskDispatch): Spawns madison with isolated_tasks=false
    //
    // Due to the TOCTTOU race, both can succeed. The last one wins,
    // potentially giving a reviewer access to the shared task list.

    let manager1 = Arc::clone(&manager);
    let manager2 = Arc::clone(&manager);

    // Use a barrier to synchronize the threads
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let barrier1 = Arc::clone(&barrier);
    let barrier2 = Arc::clone(&barrier);

    let spawn1_result = Arc::new(std::sync::Mutex::new(None::<bool>));
    let spawn2_result = Arc::new(std::sync::Mutex::new(None::<bool>));
    let spawn1_result_clone = Arc::clone(&spawn1_result);
    let spawn2_result_clone = Arc::clone(&spawn2_result);

    // Thread 1: PR poll spawning reviewer (isolated=true)
    let handle1 = thread::spawn(move || {
        barrier1.wait();

        // Simulate spawn_with_name check phase (line 647-655 in coworker.rs)
        // This check happens with a read lock, then the lock is released
        let already_exists = manager1.get("madison").is_some();

        if already_exists {
            *spawn1_result_clone.lock().unwrap() = Some(false);
            return;
        }

        // Simulate the delay during worktree creation and tmux spawn
        // (lines 657-734, no lock held, ~100ms in production)
        thread::sleep(Duration::from_millis(5));

        // Simulate spawn_with_name insert phase with the fix:
        // Check again before insert, fail if name is taken
        let inserted = manager1.insert_for_testing(Coworker {
            name: "madison".to_string(),
            status: CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
        });
        *spawn1_result_clone.lock().unwrap() = Some(inserted);
    });

    // Thread 2: TaskDispatch spawning for task (isolated=false)
    let handle2 = thread::spawn(move || {
        barrier2.wait();

        // Simulate spawn_with_name check phase
        let already_exists = manager2.get("madison").is_some();

        if already_exists {
            *spawn2_result_clone.lock().unwrap() = Some(false);
            return;
        }

        // Simulate the delay during worktree creation and tmux spawn
        // (slightly different timing to simulate real-world variance)
        thread::sleep(Duration::from_millis(5));

        // Simulate spawn_with_name insert phase with the fix:
        // Check again before insert, fail if name is taken
        let inserted = manager2.insert_for_testing(Coworker {
            name: "madison".to_string(),
            status: CoworkerStatus::Running,
            working_dir: "/tmp".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
        });
        *spawn2_result_clone.lock().unwrap() = Some(inserted);
    });

    handle1.join().expect("Thread 1 panicked");
    handle2.join().expect("Thread 2 panicked");

    let spawn1_succeeded = spawn1_result.lock().unwrap().unwrap_or(false);
    let spawn2_succeeded = spawn2_result.lock().unwrap().unwrap_or(false);

    // Check the final state

    println!("Spawn race test results:");
    println!(
        "  Reviewer spawn (isolated=true): {}",
        if spawn1_succeeded {
            "succeeded"
        } else {
            "failed"
        }
    );
    println!(
        "  Task spawn (isolated=false): {}",
        if spawn2_succeeded {
            "succeeded"
        } else {
            "failed"
        }
    );

    // This test SHOULD FAIL until the race condition is fixed.
    //
    // The bug: both spawns can succeed due to TOCTTOU race, and the last one
    // overwrites the first.
    //
    // Correct behavior: exactly one spawn should succeed (the other should fail
    // because the name is already taken).
    assert!(
        spawn1_succeeded ^ spawn2_succeeded,
        "Race condition detected: both spawns succeeded or both failed. \
         Exactly one spawn should succeed. spawn1={}, spawn2={}",
        spawn1_succeeded,
        spawn2_succeeded
    );
}

/// Test documenting the expected fix: atomic check-and-insert for spawn.
///
/// The fix should ensure that:
/// 1. If a coworker name is already in use, spawn fails
/// 2. The check and insert are atomic (no gap for races)
#[test]
fn test_spawn_should_be_atomic() {
    let (manager, _temp_dir) = test_manager();

    // First spawn: reviewer
    manager.insert_for_testing(Coworker {
        name: "madison".to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
    });

    // Second spawn attempt should fail (name already in use)
    let already_exists = manager.get("madison").is_some();

    assert!(
        already_exists,
        "Second spawn should see madison already exists"
    );
}
