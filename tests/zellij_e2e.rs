// Stub test file for CI compatibility.
// The Zellij plugin was removed in this branch (codex/zellij-layout-configurable),
// but a merge from main brought back the full test implementation.
// Since the plugin doesn't exist in this branch, we use a stub to prevent CI failures.
//
// This file will be removed when this branch merges and the workflow is updated,
// or when the Zellij plugin is re-added.

#[test]
#[ignore]
fn test_stub_for_ci_compatibility() {
    // This test intentionally does nothing.
    // It exists only to prevent CI from failing when trying to run --test zellij_e2e
}
