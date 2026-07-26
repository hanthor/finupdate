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

#[allow(dead_code)]
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
    #[allow(dead_code)]
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
    /// Dev-mode simulator scenario: "Success" | "AlreadyUpToDate" | "Failure".
    /// Only read on startup when `dev_mode` is true. Behave smoke tests poke
    /// this via `_write_settings(sim_scenario="...")` to drive the simulated
    /// update worker without UI interaction. `None` falls back to Success.
    #[serde(default)]
    pub sim_scenario: Option<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        // Development builds (meson -Dprofile=development, or a plain `cargo
        // build` where PROFILE is empty) must never fire a reboot or a
        // `bootc switch` at the host.
        //
        // This used to be spelled `dev_mode: is_dev_build`, which was too big a
        // hammer: dev_mode *simulates the whole update*, so a cargo-built
        // binary could never exercise the real orchestrator, registry, or
        // rebase code at all — the paths most in need of testing were the only
        // ones unreachable. It also silently contradicted the UI, which showed
        // "Developer Mode — updates are simulated" to anyone who just built
        // from source.
        //
        // `dry_run` is the right default instead: real code paths run, and the
        // privileged() chokepoint blocks the destructive commands at the point
        // of execution. Opt into simulation explicitly with --dev-mode.
        let is_dev_build = crate::config::PROFILE == "Devel" || crate::config::PROFILE.is_empty();

        Self {
            auto_updates: true,
            update_interval: UpdateInterval::Daily,
            pause_on_metered: true,
            custom_interval_hours: 6,
            dev_mode: false,
            mock_identity: None,
            dry_run: is_dev_build,
            include_app_updates: true,
            sim_scenario: None,
        }
    }
}

/// Per-run overrides that never touch `settings.json`.
///
/// # Why this exists
///
/// `--dev-mode` and `--sim=` used to be applied by mutating the on-disk
/// settings and calling `save()`. That made a test run *sticky*: after
/// `finupdate --dev-mode` exited, the user's real app stayed in developer mode
/// until someone noticed and turned it off. Worse for the GUI suite — every
/// scenario that set a simulator outcome permanently rewrote the developer's
/// config, so runs were order-dependent and not reproducible.
///
/// Overrides now live in process memory only. `Settings::load()` reads the
/// user's file exactly as written, then layers these on top, so the on-disk
/// config is never a side effect of how the binary was invoked.
///
/// `None` means "defer to the file"; `Some(v)` forces `v`.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOverrides {
    pub dev_mode: Option<bool>,
    pub dry_run: Option<bool>,
    pub sim_scenario: Option<String>,
    pub mock_identity: Option<MockBootcIdentity>,
}

static OVERRIDES: std::sync::OnceLock<RuntimeOverrides> = std::sync::OnceLock::new();

/// Install the process-wide overrides. Call once from `main()` before any
/// `Settings::load()`. Ignores a second call rather than panicking — a
/// double-init here is harmless and shouldn't take the app down.
pub fn set_runtime_overrides(o: RuntimeOverrides) {
    if OVERRIDES.set(o).is_err() {
        tracing::warn!("runtime overrides already set; ignoring second call");
    }
}

fn overrides() -> Option<&'static RuntimeOverrides> {
    OVERRIDES.get()
}

impl Settings {
    fn config_path() -> PathBuf {
        glib::user_config_dir()
            .join("finupdate")
            .join("settings.json")
    }

    /// Load settings from disk, falling back to defaults on any error, then
    /// apply any [`RuntimeOverrides`] installed by `main()`.
    ///
    /// In development builds, `dev_mode` defaults to true but can be toggled off.
    pub fn load() -> Self {
        let path = Self::config_path();
        let mut settings: Self = match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse settings (using defaults): {}", e);
                Self::default()
            }),
            Err(_) => Self::default(),
        };

        settings.apply_overrides(overrides());
        settings
    }

    /// Layer per-run overrides over a loaded value. Split out from `load()` so
    /// it can be tested without touching the real config path.
    fn apply_overrides(&mut self, o: Option<&RuntimeOverrides>) {
        let Some(o) = o else { return };
        if let Some(v) = o.dev_mode {
            self.dev_mode = v;
        }
        if let Some(v) = o.dry_run {
            self.dry_run = v;
        }
        if let Some(v) = &o.sim_scenario {
            self.sim_scenario = Some(v.clone());
        }
        if let Some(v) = &o.mock_identity {
            self.mock_identity = Some(v.clone());
        }
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

    // ── RuntimeOverrides ─────────────────────────────────────────────────
    //
    // apply_overrides() is tested directly rather than through load(), so
    // these don't depend on the process-wide OnceLock or the real config path.

    #[test]
    fn no_overrides_leaves_settings_untouched() {
        let mut s = Settings::default();
        s.dev_mode = false;
        s.dry_run = false;
        s.apply_overrides(None);
        assert!(!s.dev_mode);
        assert!(!s.dry_run);
    }

    #[test]
    fn none_fields_defer_to_the_loaded_file() {
        // An override struct that sets only dry_run must not clobber dev_mode.
        let mut s = Settings::default();
        s.dev_mode = true;
        s.dry_run = false;
        s.apply_overrides(Some(&RuntimeOverrides {
            dry_run: Some(true),
            ..Default::default()
        }));
        assert!(s.dev_mode, "dev_mode should be left as the file had it");
        assert!(s.dry_run, "dry_run should be forced on");
    }

    #[test]
    fn override_can_force_a_flag_off() {
        // Some(false) must win over a file that says true — otherwise you
        // could never use the CLI to escape a dev_mode=true config.
        let mut s = Settings::default();
        s.dev_mode = true;
        s.apply_overrides(Some(&RuntimeOverrides {
            dev_mode: Some(false),
            ..Default::default()
        }));
        assert!(!s.dev_mode);
    }

    #[test]
    fn sim_scenario_and_mock_identity_override() {
        let mut s = Settings::default();
        s.apply_overrides(Some(&RuntimeOverrides {
            sim_scenario: Some("Failure".into()),
            mock_identity: Some(MockBootcIdentity {
                registry: "ghcr.io".into(),
                org: "ublue-os".into(),
                image: "bluefin".into(),
                tag: "stable".into(),
                digest: None,
                booted_at: None,
            }),
            ..Default::default()
        }));
        assert_eq!(s.sim_scenario.as_deref(), Some("Failure"));
        assert_eq!(
            s.mock_identity.map(|m| m.full_ref()),
            Some("ghcr.io/ublue-os/bluefin:stable".to_string())
        );
    }

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
            sim_scenario: Some("Failure".to_string()),
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
        assert_eq!(back.sim_scenario, original.sim_scenario);
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
