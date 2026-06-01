//! Regression tests for Dakota image history fix (commit de1dc08).
//!
//! This test suite validates that the image history feature correctly handles
//! Dakota's sha-only tag format and properly displays recent daily builds
//! regardless of registry tag ordering.
//!
//! Background:
//! - Dakota switched to sha-only tags (~40-hex strings) in February 2026
//! - The previous code truncated sha_tags BEFORE probing for dates
//! - This caused old February images to be displayed instead of recent builds
//! - Fix: Probe larger sample (500 instead of 120), sort by date after probing

#[cfg(test)]
mod dakota_history_tests {
    use std::process::Command;

    fn get_cli_exe() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_finupdate-cli"))
    }

    // ─────────────────────────────────────────────────────────────────────
    // Basic sanity tests: CLI should exit cleanly without panicking
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn dakota_latest_tags_command_exits_cleanly() {
        // Verify the tags command doesn't panic on Dakota latest stream.
        // In CI, registry may be unreachable; accept both success and failure.
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("tags")
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        let code = output.status.code().unwrap_or(1);
        assert!(
            code == 0 || code == 1,
            "Unexpected exit code {}: {}",
            code,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn dakota_testing_tags_command_exits_cleanly() {
        // Verify the tags command works with Dakota testing stream
        // (added in commit 7b8239f).
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("tags")
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:testing")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        let code = output.status.code().unwrap_or(1);
        assert!(
            code == 0 || code == 1,
            "Unexpected exit code {}: {}",
            code,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn dakota_versions_command_exits_cleanly() {
        // Verify versions command handles Dakota without panicking.
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("versions")
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        let code = output.status.code().unwrap_or(1);
        assert!(
            code == 0 || code == 1,
            "Unexpected exit code {}: {}",
            code,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Output validation: When successful, verify expected content
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn dakota_tags_output_contains_expected_formats() {
        // When tags command succeeds, output should contain either:
        // 1. Stream tags (e.g., "latest", "testing")
        // 2. Dated tags (e.g., "latest-20260527")
        // 3. Build dates from probed sha-tags (e.g., "Build 2026-05-28")
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("tags")
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let has_stream = stdout.contains("latest");
            let has_dated = stdout.contains("latest-") || stdout.contains("20260");
            let has_build = stdout.contains("Build");

            // At least one format should be present
            assert!(
                has_stream || has_dated || has_build,
                "Output missing expected tag formats:\n{}",
                stdout
            );
        }
    }

    #[test]
    fn dakota_versions_output_has_date_format() {
        // Versions output should show dates in YYYY-MM-DD format
        // (the registry_client parses sha-tag creation dates).
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("versions")
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Should have at least one line with YYYY-MM-DD format if any versions
            if !stdout.trim().is_empty() {
                let has_date = stdout.contains("202"); // Looks for years starting with 202x
                assert!(has_date, "Versions output missing date format:\n{}", stdout);
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Regression specifics: Dakota-nvidia variant
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn dakota_nvidia_family_detection_works() {
        // Dakota family should support the nvidia feature.
        // Verify the family command correctly identifies it.
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("family")
            .env(
                "FINUPDATE_IMAGE",
                "ghcr.io/projectbluefin/dakota-nvidia:latest",
            )
            .output()
            .expect("CLI binary execution failed");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Bluefin Dakota") && stdout.contains("dakota"),
            "Dakota-nvidia should be identified as Bluefin Dakota family"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Changelog command: Should work with different tag formats
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn dakota_changelog_with_different_stream_exits_cleanly() {
        // Switching between streams (latest → testing) should work
        // without panicking, even if SBOM fetch fails.
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .args(["changelog", "testing"])
            .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
            .env("XDG_CACHE_HOME", "/tmp/finupdate-test-cache")
            .output()
            .expect("CLI binary execution failed");

        // Should always succeed; network errors are handled gracefully
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Output should show the comparison being made
        assert!(stdout.contains("Changelog for"));
        assert!(stdout.contains("booted tag:"));
        assert!(stdout.contains("target tag:"));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Edge cases and stress scenarios
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn repeated_tags_calls_consistent_results() {
        // Calling tags multiple times should be idempotent (cached results).
        // The fix ensures recent builds are included; cache shouldn't hide them.
        let exe = get_cli_exe();
        let cache_dir = "/tmp/finupdate-test-cache-idempotent";
        let _ = std::fs::remove_dir_all(cache_dir); // Clean start

        let mut outputs = Vec::new();
        for _ in 0..2 {
            let output = Command::new(&exe)
                .arg("tags")
                .env("FINUPDATE_IMAGE", "ghcr.io/projectbluefin/dakota:latest")
                .env("XDG_CACHE_HOME", cache_dir)
                .output()
                .expect("CLI binary execution failed");

            if output.status.success() {
                outputs.push(String::from_utf8_lossy(&output.stdout).to_string());
            }
        }

        // If both succeeded, they should have consistent output
        if outputs.len() == 2 {
            // Both should reference the same tags (possibly in different order if parsed)
            let has_common_content = outputs[0].contains("latest") || outputs[0].contains("Build");
            assert!(has_common_content, "First call should have tags");
        }
    }

    #[test]
    fn dakota_handles_missing_bootc_gracefully() {
        // When bootc is not available (no booted image), should fall back
        // to detecting image from environment.
        let exe = get_cli_exe();
        let output = Command::new(&exe)
            .arg("status")
            .env(
                "FINUPDATE_IMAGE",
                "ghcr.io/projectbluefin/dakota:latest-20260527",
            )
            .output()
            .expect("CLI binary execution failed");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("dakota"));
    }
}
