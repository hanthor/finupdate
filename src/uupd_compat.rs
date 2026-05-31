// Consumed by the GUI binary; finupdate-cli compiles it but doesn't use
// it, which trips dead-code warnings under that bin. Module-level allow.
#![allow(dead_code)]

//! Compatibility shim for the optional host `uupd` binary.
//!
//! `uupd` is not required — finupdate has its own orchestrator. But if it IS
//! installed, we surface a toggle and a configuration subpage in Preferences
//! to control `uupd.timer` and `/etc/uupd/config.json`.
//!
//! ## uupd config schema (mirrors `/etc/uupd/config.json`)
//!
//! ```json
//! {
//!   "checks": {
//!     "hardware": {
//!       "enable": true,
//!       "bat-min-percent": 20,
//!       "cpu-max-percent": 50,
//!       "mem-max-percent": 90,
//!       "net-max-bytes": 700000
//!     }
//!   },
//!   "modules": {
//!     "brew":      { "disable": false },
//!     "distrobox": { "disable": false },
//!     "flatpak":   { "disable": false },
//!     "system":    { "disable": false }
//!   }
//! }
//! ```

use crate::update_worker::is_flatpak;
use serde::{Deserialize, Serialize};

pub const CONFIG_PATH: &str = "/etc/uupd/config.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UupdConfig {
    pub checks: UupdChecks,
    pub modules: UupdModules,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UupdChecks {
    pub hardware: UupdHardwareChecks,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UupdHardwareChecks {
    pub enable: bool,
    #[serde(rename = "bat-min-percent")]
    pub bat_min_percent: u32,
    #[serde(rename = "cpu-max-percent")]
    pub cpu_max_percent: u32,
    #[serde(rename = "mem-max-percent")]
    pub mem_max_percent: u32,
    #[serde(rename = "net-max-bytes")]
    pub net_max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UupdModules {
    pub brew: UupdModuleToggle,
    pub distrobox: UupdModuleToggle,
    pub flatpak: UupdModuleToggle,
    pub system: UupdModuleToggle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct UupdModuleToggle {
    pub disable: bool,
}

impl Default for UupdConfig {
    fn default() -> Self {
        // Mirrors the upstream config.json shipped with uupd.
        Self {
            checks: UupdChecks::default(),
            modules: UupdModules::default(),
        }
    }
}

impl Default for UupdChecks {
    fn default() -> Self {
        Self {
            hardware: UupdHardwareChecks::default(),
        }
    }
}

impl Default for UupdHardwareChecks {
    fn default() -> Self {
        Self {
            enable: true,
            bat_min_percent: 20,
            cpu_max_percent: 50,
            mem_max_percent: 90,
            net_max_bytes: 700_000,
        }
    }
}

impl Default for UupdModules {
    fn default() -> Self {
        Self {
            brew: UupdModuleToggle::default(),
            distrobox: UupdModuleToggle::default(),
            flatpak: UupdModuleToggle::default(),
            system: UupdModuleToggle::default(),
        }
    }
}

impl Default for UupdModuleToggle {
    fn default() -> Self {
        Self { disable: false }
    }
}

/// Read `/etc/uupd/config.json` from the host filesystem.
///
/// Returns the parsed config, or `UupdConfig::default()` if the file is missing
/// or unreadable (uupd treats a missing config as "use defaults" too).
pub fn read_config() -> UupdConfig {
    let bytes = if is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", CONFIG_PATH])
            .output()
            .ok()
            .and_then(|o| o.status.success().then_some(o.stdout))
    } else {
        std::fs::read(CONFIG_PATH).ok()
    };

    match bytes {
        Some(b) if !b.is_empty() => match serde_json::from_slice::<UupdConfig>(&b) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to parse {}: {} — using defaults", CONFIG_PATH, e);
                UupdConfig::default()
            }
        },
        _ => UupdConfig::default(),
    }
}

/// Write `/etc/uupd/config.json` via a single pkexec elevation.
///
/// Stages the new content into a user-owned temp file, then asks pkexec to
/// `install` it over `/etc/uupd/config.json` so the file ends up root-owned
/// with mode 0644.
pub async fn write_config(config: &UupdConfig) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(config)?;

    // Stage to a tempfile in the user's home (visible from the host via
    // /var/home/<user>/ even from inside the Flatpak sandbox).
    let staging = std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("finupdate-uupd-config-{}.json", std::process::id()));
    std::fs::write(&staging, &json)?;

    let staging_str = staging.to_string_lossy().to_string();

    let args = ["install", "-o", "root", "-g", "root", "-m", "0644", &staging_str, CONFIG_PATH];

    let status = if is_flatpak() {
        tokio::process::Command::new("flatpak-spawn")
            .args(["--host", "pkexec"])
            .args(args)
            .status()
            .await?
    } else {
        tokio::process::Command::new("pkexec")
            .args(args)
            .status()
            .await?
    };

    // Best-effort cleanup of the staging file.
    let _ = std::fs::remove_file(&staging);

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "pkexec install {} {} failed with code {:?}",
            staging_str,
            CONFIG_PATH,
            status.code()
        ))
    }
}

/// Returns `true` if `uupd` is present on the host.
pub fn is_uupd_installed() -> bool {
    let status = if is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "which", "uupd"])
            .output()
    } else {
        std::process::Command::new("which").arg("uupd").output()
    };
    status.map(|o| o.status.success()).unwrap_or(false)
}

/// Returns `Some(true)` if `uupd.timer` is enabled, `Some(false)` if disabled,
/// `None` if the state cannot be determined (uupd not installed, etc.).
pub fn is_uupd_timer_active() -> Option<bool> {
    let output = if is_flatpak() {
        std::process::Command::new("flatpak-spawn")
            .args(["--host", "systemctl", "is-enabled", "uupd.timer"])
            .output()
    } else {
        std::process::Command::new("systemctl")
            .args(["is-enabled", "uupd.timer"])
            .output()
    };

    match output {
        Ok(o) => parse_timer_state(&String::from_utf8_lossy(&o.stdout)),
        Err(e) => {
            tracing::warn!("Failed to read uupd.timer state: {e}");
            None
        }
    }
}

/// Parse the stdout of `systemctl is-enabled uupd.timer` into a tri-state.
///
/// `systemctl` returns one of `enabled`, `disabled`, `static`, `masked`,
/// `alias`, `indirect`, `linked`, etc. — we only treat enabled/disabled as
/// actionable; everything else maps to `None` so the UI hides the toggle.
fn parse_timer_state(stdout: &str) -> Option<bool> {
    match stdout.trim() {
        "enabled" | "enabled-runtime" => Some(true),
        "disabled" => Some(false),
        _ => None,
    }
}

/// Enable or disable `uupd.timer` via a single pkexec elevation.
pub async fn set_uupd_timer(enable: bool) -> anyhow::Result<()> {
    let action = if enable { "enable" } else { "disable" };
    let args = ["systemctl", action, "--now", "uupd.timer"];

    let status = if is_flatpak() {
        tokio::process::Command::new("flatpak-spawn")
            .args(["--host", "pkexec"])
            .args(args)
            .status()
            .await?
    } else {
        tokio::process::Command::new("pkexec")
            .args(args)
            .status()
            .await?
    };

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "pkexec systemctl {action} uupd.timer failed with code {:?}",
            status.code()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_enabled() {
        assert_eq!(parse_timer_state("enabled"), Some(true));
        assert_eq!(parse_timer_state("enabled\n"), Some(true));
        assert_eq!(parse_timer_state("  enabled  "), Some(true));
    }

    #[test]
    fn parses_enabled_runtime_as_enabled() {
        // systemctl reports "enabled-runtime" when enabled only for current boot.
        assert_eq!(parse_timer_state("enabled-runtime"), Some(true));
    }

    #[test]
    fn parses_disabled() {
        assert_eq!(parse_timer_state("disabled"), Some(false));
        assert_eq!(parse_timer_state("disabled\n"), Some(false));
    }

    #[test]
    fn other_states_yield_none() {
        // These are real systemctl states that aren't user-actionable.
        for state in ["static", "masked", "alias", "indirect", "linked", "generated", ""] {
            assert_eq!(parse_timer_state(state), None, "state={state}");
        }
    }

    #[test]
    fn unrelated_text_yields_none() {
        assert_eq!(parse_timer_state("Failed to get unit file state"), None);
        assert_eq!(parse_timer_state("ENABLED"), None); // case-sensitive
    }

    // ── UupdConfig schema ────────────────────────────────────────────────

    /// The exact JSON shipped by upstream as `/etc/uupd/config.json`,
    /// kept as a fixture in `docs/uupd-config.example.json` so it stays in
    /// lockstep with the upstream template.
    const UPSTREAM_TEMPLATE: &str = include_str!("../docs/uupd-config.example.json");

    #[test]
    fn parses_upstream_template() {
        let c: UupdConfig = serde_json::from_str(UPSTREAM_TEMPLATE).unwrap();
        assert!(c.checks.hardware.enable);
        assert_eq!(c.checks.hardware.bat_min_percent, 20);
        assert_eq!(c.checks.hardware.cpu_max_percent, 50);
        assert_eq!(c.checks.hardware.mem_max_percent, 90);
        assert_eq!(c.checks.hardware.net_max_bytes, 700_000);
        assert!(!c.modules.brew.disable);
        assert!(!c.modules.distrobox.disable);
        assert!(!c.modules.flatpak.disable);
        assert!(!c.modules.system.disable);
    }

    #[test]
    fn default_matches_upstream_template() {
        let parsed: UupdConfig = serde_json::from_str(UPSTREAM_TEMPLATE).unwrap();
        assert_eq!(parsed, UupdConfig::default());
    }

    #[test]
    fn config_round_trips_through_json() {
        let original = UupdConfig {
            checks: UupdChecks {
                hardware: UupdHardwareChecks {
                    enable: false,
                    bat_min_percent: 50,
                    cpu_max_percent: 80,
                    mem_max_percent: 95,
                    net_max_bytes: 1_500_000,
                },
            },
            modules: UupdModules {
                brew: UupdModuleToggle { disable: true },
                distrobox: UupdModuleToggle { disable: false },
                flatpak: UupdModuleToggle { disable: false },
                system: UupdModuleToggle { disable: true },
            },
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: UupdConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn config_preserves_kebab_case_keys() {
        let original = UupdConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        // These are the uupd-side keys; if we change them, uupd won't recognize them.
        assert!(json.contains("\"bat-min-percent\""));
        assert!(json.contains("\"cpu-max-percent\""));
        assert!(json.contains("\"mem-max-percent\""));
        assert!(json.contains("\"net-max-bytes\""));
    }

    #[test]
    fn missing_fields_fill_with_defaults() {
        // If uupd ships a new field, older finupdate should still load the file.
        let partial = r#"{"checks": {"hardware": {"bat-min-percent": 10}}}"#;
        let c: UupdConfig = serde_json::from_str(partial).unwrap();
        assert_eq!(c.checks.hardware.bat_min_percent, 10);
        // Other fields fall back to defaults.
        assert!(c.checks.hardware.enable);
        assert_eq!(c.checks.hardware.cpu_max_percent, 50);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // Forward-compat — newer uupd versions may add fields we don't know.
        let extended = r#"{
            "checks": {"hardware": {"enable": true}},
            "modules": {"system": {"disable": false}},
            "experimental_feature": "value"
        }"#;
        let c: UupdConfig = serde_json::from_str(extended).unwrap();
        assert!(c.checks.hardware.enable);
    }
}
