//! Smoke test for the Accessibility permission FFI bindings.
//!
//! These tests verify that the macOS Accessibility symbols resolve at link
//! time and that the helper functions return without panicking. They do not
//! assert a specific trust state — that depends on the CI/dev environment.

#[cfg(target_os = "macos")]
#[test]
fn is_accessibility_trusted_links_and_runs() {
    // Just verify the FFI binding resolves and returns a bool.
    let _trusted: bool = rekody_hotkey::is_accessibility_trusted();
}

#[cfg(target_os = "macos")]
#[test]
fn request_accessibility_permission_does_not_panic() {
    // If already trusted, this returns true silently and does not prompt.
    // If not trusted, this would show a system dialog — but the CFDictionary
    // construction path must still not panic in either case.
    let already = rekody_hotkey::is_accessibility_trusted();
    if already {
        // Safe to call — no prompt since already granted.
        let result = rekody_hotkey::request_accessibility_permission();
        assert!(result, "already-trusted process should return true");
    }
    // If not already trusted, we skip calling request_accessibility_permission
    // from a test because it would pop a system dialog.
}
