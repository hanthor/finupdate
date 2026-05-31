//! Persistent application settings.
//!
//! Stored as JSON at `$XDG_CONFIG_HOME/finupdate/settings.json`.
//! Uses `gtk::glib::user_config_dir()` for correct XDG path resolution.

use gtk::glib;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How often the automatic update timer should fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateInterval {
    Hourly,
    Daily,
    Weekly,
    Custom,
}

impl UpdateInterval {
    /// Index into the UI combo-row model (Hourly=0, Daily=1, Weekly=2, Custom=3).
    pub fn to_index(&self) -> u32 {
        match self {
            Self::Hourly => 0,
            Self::Daily => 1,
            Self::Weekly => 2,
            Self::Custom => 3,
        }
    }

    pub fn from_index(i: u32) -> Self {
        match i {
            0 => Self::Hourly,
            1 => Self::Daily,
            2 => Self::Weekly,
            _ => Self::Custom,
        }
    }
}

/// Override the detected currently-booted image identity.
///
/// When set, the app pretends the system is booted on this image — every
/// downstream rendering path (image source row, history, changelog, variant
/// list, rebase dialog) sees this identity instead of the real `bootc status`
/// answer. Real network calls (GHCR tags, GitHub commits, SBOM diff) still
/// hit live endpoints. Pair with `dry_run = true` to block destructive
/// subprocess calls.
///
/// Used by GUI tests to exercise the app against many bootc image families
/// without actually booting them. None in production.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MockBootcIdentity {
    pub registry: String,
    pub org: String,
    pub image: String,
    pub tag: String,
    #[serde(default)]
    pub digest: Option<String>,
    /// RFC3339 timestamp. Feeds "running since" / "booted N days ago" labels.
    #[serde(default)]
    pub booted_at: Option<String>,
}

impl MockBootcIdentity {
    /// Render as the canonical full image reference: `registry/org/image:tag`.
    pub fn full_ref(&self) -> String {
        format!("{}/{}/{}:{}", self.registry, self.org, self.image, self.tag)
    }
}

/// All persistent user preferences for finupdate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whether the uupd systemd timer is allowed to run automatically.
    pub auto_updates: bool,
    /// How often the timer fires (informational — actual timer is managed by systemd).
    pub update_interval: UpdateInterval,
    /// Suppress automatic updates when on a metered/limited connection.
    pub pause_on_metered: bool,
    /// Custom interval in hours (only used when `update_interval == Custom`).
    pub custom_interval_hours: u32,
    /// Run a simulated update instead of the real uupd process.
    /// Useful for UI development and demos without root or a live system.
    pub dev_mode: bool,
    /// Override the detected currently-booted image. See [`MockBootcIdentity`].
    #[serde(default)]
    pub mock_identity: Option<MockBootcIdentity>,
    /// When true, every destructive subprocess (reboot, `bootc switch`,
    /// uupd timer toggle, uupd config write) is logged and short-circuited
    /// to synthetic success instead of executing. Independent of `dev_mode`
    /// so tests can drive the real-update worker path while still blocking
    /// reboot/rebase.
    #[serde(default)]
    pub dry_run: bool,
    /// When true (default), the updater also refreshes Flatpaks, Homebrew,
    /// and Distrobox containers along with the bootc system image. When
    /// false, only the system image is updated — useful for users who manage
    /// app updates separately (Software, brew upgrade, etc.). The toggle is
    /// honoured by the finupdate-runner shell script via the
    /// `FINUPDATE_SYSTEM_ONLY` env var.
    #[serde(default = "default_true")]
    pub include_app_updates: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        // Development builds (meson -Dprofile=development or cargo build without meson)
        // default to dev_mode=true so destructive actions like reboot are never triggered
        // during testing.
        let is_dev_build = crate::config::PROFILE == "Devel" || crate::config::PROFILE.is_empty();

        Self {
            auto_updates: true,
            update_interval: UpdateInterval::Daily,
            pause_on_metered: true,
            custom_interval_hours: 6,
            dev_mode: is_dev_build,
            mock_identity: None,
            dry_run: false,
            include_app_updates: true,
        }
    }
}

impl Settings {
    fn config_path() -> PathBuf {
        glib::user_config_dir()
            .join("finupdate")
            .join("settings.json")
    }

    /// Load settings from disk, falling back to defaults on any error.
    /// In development builds, `dev_mode` defaults to true but can be toggled off.
    pub fn load() -> Self {
        let path = Self::config_path();
        let settings = match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse settings (using defaults): {}", e);
                Self::default()
            }),
            Err(_) => Self::default(),
        };

        settings
    }

    /// Save settings to disk, logging errors but never panicking.
    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!("Failed to create config directory: {}", e);
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&path, data) {
                    tracing::error!("Failed to write settings: {}", e);
                }
            }
            Err(e) => tracing::error!("Failed to serialize settings: {}", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── UpdateInterval ───────────────────────────────────────────────────

    #[test]
    fn update_interval_index_round_trip() {
        for v in [
            UpdateInterval::Hourly,
            UpdateInterval::Daily,
            UpdateInterval::Weekly,
            UpdateInterval::Custom,
        ] {
            assert_eq!(UpdateInterval::from_index(v.to_index()), v);
        }
    }

    #[test]
    fn update_interval_unknown_index_falls_back_to_custom() {
        assert_eq!(UpdateInterval::from_index(99), UpdateInterval::Custom);
    }

    #[test]
    fn update_interval_serializes_lowercase() {
        let json = serde_json::to_string(&UpdateInterval::Daily).unwrap();
        assert_eq!(json, r#""daily""#);
    }

    #[test]
    fn update_interval_deserializes_lowercase() {
        let v: UpdateInterval = serde_json::from_str(r#""weekly""#).unwrap();
        assert_eq!(v, UpdateInterval::Weekly);
    }

    // ── Settings ─────────────────────────────────────────────────────────

    #[test]
    fn settings_round_trip_through_json() {
        let original = Settings {
            auto_updates: false,
            update_interval: UpdateInterval::Weekly,
            pause_on_metered: false,
            custom_interval_hours: 12,
            dev_mode: false,
            mock_identity: Some(MockBootcIdentity {
                registry: "ghcr.io".into(),
                org: "ublue-os".into(),
                image: "bluefin".into(),
                tag: "stable".into(),
                digest: None,
                booted_at: None,
            }),
            dry_run: true,
            include_app_updates: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.auto_updates, original.auto_updates);
        assert_eq!(back.update_interval, original.update_interval);
        assert_eq!(back.pause_on_metered, original.pause_on_metered);
        assert_eq!(back.custom_interval_hours, original.custom_interval_hours);
        assert_eq!(back.dev_mode, original.dev_mode);
        assert_eq!(back.mock_identity, original.mock_identity);
        assert_eq!(back.dry_run, original.dry_run);
        assert_eq!(back.include_app_updates, original.include_app_updates);
    }

    #[test]
    fn settings_include_app_updates_defaults_true_when_missing() {
        // Existing settings.json files (pre-include_app_updates) should load
        // unchanged with the new field defaulting to true — keeps current
        // behaviour for upgraders.
        let json = r#"{"auto_updates":true,"update_interval":"daily","pause_on_metered":true,"custom_interval_hours":6,"dev_mode":false}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.include_app_updates);
    }

    #[test]
    fn mock_identity_full_ref_formats_correctly() {
        let id = MockBootcIdentity {
            registry: "ghcr.io".into(),
            org: "ublue-os".into(),
            image: "bluefin-nvidia".into(),
            tag: "stable".into(),
            digest: None,
            booted_at: None,
        };
        assert_eq!(id.full_ref(), "ghcr.io/ublue-os/bluefin-nvidia:stable");
    }

    #[test]
    fn settings_missing_mock_identity_and_dry_run_default_to_none_false() {
        // Old settings.json files (pre-Iteration-A) must continue loading.
        let pre_iteration_a = r#"{"auto_updates": true, "dev_mode": false}"#;
        let s: Settings = serde_json::from_str(pre_iteration_a).unwrap();
        assert!(s.mock_identity.is_none());
        assert!(!s.dry_run);
    }

    #[test]
    fn settings_missing_fields_get_defaults() {
        // Partial JSON should fill in via serde(default).
        let partial = r#"{"auto_updates": false}"#;
        let s: Settings = serde_json::from_str(partial).unwrap();
        assert!(!s.auto_updates);
        // Other fields fall back to Default::default()'s values.
        assert_eq!(s.update_interval, UpdateInterval::Daily);
        assert_eq!(s.custom_interval_hours, 6);
        assert!(s.pause_on_metered);
    }

    #[test]
    fn settings_unknown_fields_are_ignored() {
        // Forward-compat: tomorrow's settings.json shouldn't break today's binary.
        let extended = r#"{"auto_updates": true, "future_field": 42}"#;
        let s: Settings = serde_json::from_str(extended).unwrap();
        assert!(s.auto_updates);
    }
}
