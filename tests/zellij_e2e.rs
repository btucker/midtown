// Stub test file for CI compatibility.
// This test file was deleted in this PR, but the CI workflow on main still references it.
// GitHub Actions uses the workflow from the base branch for pull_request events,
// so we need this stub to prevent CI failures until the workflow is updated.
//
// This file will be removed once this PR merges and the workflow is updated.

#[test]
#[ignore]
fn test_stub_for_ci_compatibility() {
    // This test intentionally does nothing.
    // It exists only to prevent CI from failing when trying to run --test zellij_e2e
}
