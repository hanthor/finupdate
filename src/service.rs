//! Backend trait for finupdate operations — the seam between the GTK frontend
//! and the underlying bootc/registry/uupd machinery.
//!
//! Goal: give alternative frontends (CLI, TUI, web/D-Bus daemon, integration
//! tests) a single point of contact that has no GTK or relm4 dependencies. The
//! trait is intentionally narrow — it covers what the existing UI already does
//! today, no speculative additions. Things like "search for an arbitrary image"
//! belong in a follow-up once a real consumer needs them.
//!
//! ## Design choices
//!
//! - **Async fns under #[async_trait]** for the one-shot operations. Native
//!   async fn in trait would be nicer but `Box<dyn UpdaterService>` (which the
//!   frontends will want) requires `#[async_trait]` until Rust stabilises the
//!   async-fn-in-dyn-trait path. Cost: one boxed Future per call, acceptable
//!   for an interactive app.
//!
//! - **Streaming operations expose `tokio::sync::mpsc::Receiver`** rather than
//!   `impl Stream`. The orchestrator already uses mpsc, so a wrapper that goes
//!   `impl Stream` is just bookkeeping. The receiver pattern lets a GTK
//!   frontend poll via `glib::timeout_add_local` and a CLI frontend iterate
//!   with `while let Some(ev) = rx.recv().await`, both without pulling tokio
//!   runtime into the trait surface.
//!
//! - **Plain data types in/out**. `ImageRef`, `ImageVersion`, `FamilyInfo`,
//!   `Changelog`, `*Progress` are all owned, serde-friendly structs. No GTK
//!   widgets, no `Rc<RefCell<>>`, no GTK signals. A web frontend could
//!   JSON-serialise the entire trait surface.
//!
//! ## Migration plan
//!
//! Today this module ships the trait, types, and a `BootcUpdaterService` that
//! re-exports the existing free functions in `registry_client`, `orchestrator`,
//! `update_worker`, etc. The GTK UI keeps calling those free functions
//! directly for now. As each UI site gets refactored to take a
//! `&dyn UpdaterService`, the equivalent free function gets retired.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

// Re-export the existing concrete types so frontends only depend on this
// module. When/if registry_client gets split into a separate crate, these
// re-exports change to use the new path — single point of update.
pub use crate::registry_client::ImageVersion;

/// Registry I/O surface that `BootcUpdaterService` depends on.
///
/// Lifting this to a trait lets tests inject a fixture-backed implementation
/// (see `FixtureRegistry` in this module's tests) so we can verify
/// list_versions / current_image edge cases without hitting live ghcr.io.
/// The single production impl, `HttpRegistry`, wraps the existing
/// `registry_client` free functions and is a 1:1 behavioural replacement
/// for the direct calls that were inline in BootcUpdaterService.
#[async_trait::async_trait]
pub trait Registry: Send + Sync {
    /// Fetch up to `max` recent ImageVersions for an image, newest-first.
    /// Honours the config-blob date harvest fallback for sha-only-tagged
    /// images (dakota et al.); the harvest probe scales with `max` so a
    /// larger ask actually returns more (was hardcoded at 8 before).
    async fn fetch_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError>;

    /// Detect the currently-booted image, honouring mock_identity →
    /// bootc status → os-release in that order. Returns None when nothing
    /// in the chain produces a usable ref.
    async fn detect_booted_image(&self) -> Option<ImageRef>;

    /// Return the tags the registry publishes for this image's stream,
    /// ordered as the tag selector dropdown expects (stream tags first,
    /// then dated tags newest-first). Used by the home page's "Image
    /// source" subpage to populate the tag combo.
    async fn list_available_tags(
        &self,
        image: &ImageRef,
    ) -> Result<Vec<crate::registry_client::AvailableTag>, ServiceError>;
}

/// Production Registry implementation backed by the existing
/// `registry_client` module (live ghcr.io + bootc + os-release).
pub struct HttpRegistry;

#[async_trait::async_trait]
impl Registry for HttpRegistry {
    async fn fetch_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError> {
        let client = crate::registry_client::RegistryClient::new(
            &image.registry,
            &image.org,
            &image.image,
            &image.tag,
        );
        client
            .fetch_versions(max)
            .await
            .map_err(|e| ServiceError::Registry(e.to_string()))
    }

    async fn detect_booted_image(&self) -> Option<ImageRef> {
        let client = crate::registry_client::RegistryClient::detect().await?;
        Some(ImageRef {
            registry: "ghcr.io".to_string(),
            org: client.org().to_string(),
            image: client.image().to_string(),
            tag: client.stream().to_string(),
        })
    }

    async fn list_available_tags(
        &self,
        image: &ImageRef,
    ) -> Result<Vec<crate::registry_client::AvailableTag>, ServiceError> {
        let client = crate::registry_client::RegistryClient::new(
            &image.registry,
            &image.org,
            &image.image,
            &image.tag,
        );
        client
            .fetch_available_tags()
            .await
            .map_err(|e| ServiceError::Registry(e.to_string()))
    }
}

/// A reference to an OCI image as `registry/org/image:tag`.
///
/// Mirrors the canonical bootc spec format. Frontends construct one of these
/// from user input or from a previous `current_image()` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ImageRef {
    pub registry: String,
    pub org: String,
    pub image: String,
    pub tag: String,
}

impl ImageRef {
    /// Build from a `registry/org/image:tag` string. Returns None on malformed
    /// input. Symmetric with `to_string()`.
    pub fn parse(s: &str) -> Option<Self> {
        let (before_tag, tag) = s.rsplit_once(':')?;
        let mut parts = before_tag.splitn(3, '/');
        let registry = parts.next()?.to_string();
        let org = parts.next()?.to_string();
        let image = parts.next()?.to_string();
        Some(Self {
            registry,
            org,
            image,
            tag: tag.to_string(),
        })
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}/{}:{}", self.registry, self.org, self.image, self.tag)
    }
}

impl std::fmt::Display for ImageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_string())
    }
}

/// Information about a Family that's relevant to picking a target image.
///
/// Frontends use this to render the feature switches (one switch per entry in
/// `features`) and to display the family name. The base image and feature
/// list are enough for `resolve_target()` to compute the concrete target ref.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FamilyInfo {
    pub name: String,
    pub base_image: String,
    pub features: Vec<Feature>,
    /// Available streams for this family (e.g. "latest", "stable", "lts", "lts-hwe").
    /// Used by the rebase UI to offer stream selection. First entry is the default.
    pub streams: Vec<String>,
}

/// A switchable feature for the booted family — e.g. NVIDIA, DX, Steam Deck.
///
/// `id` is the atomic suffix used by `resolve_target` (e.g. `"nvidia"`).
/// `display_name` and `subtitle` are human-readable strings for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Feature {
    pub id: String,
    pub display_name: String,
    pub subtitle: String,
}

/// Errors a Service operation can return.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("registry: {0}")]
    Registry(String),
    #[error("io: {0}")]
    Io(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// Progress events streamed during a long-running `switch_image` operation.
///
/// The terminal events are `Done` and `Failed`. Receivers should treat the
/// channel as closed after either of those — `recv()` returning None after
/// `Done` is normal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwitchProgress {
    /// Initial state — the operation has been accepted and is pulling.
    Started { target: String },
    /// Optional per-layer progress (when we can parse it from bootc stdout).
    Layer {
        index: u32,
        total: u32,
        bytes_done: u64,
        bytes_total: u64,
    },
    /// A raw log line from the underlying tool, when no structured signal
    /// could be extracted. Frontends can show this in a "details" pane.
    Log { line: String },
    /// Operation finished successfully — the new image is staged for next boot.
    Done,
    /// Operation failed.
    Failed { message: String },
}

/// The minimal backend surface every finupdate frontend needs.
///
/// Implementations must be `Send + Sync` so they can be wrapped in `Arc<dyn>`
/// and shared between threads. The default `BootcUpdaterService` is a thin
/// stateless adapter over the existing registry/orchestrator helpers.
#[async_trait::async_trait]
pub trait UpdaterService: Send + Sync {
    /// Return the currently-booted image (respecting `mock_identity` for tests).
    async fn current_image(&self) -> Result<ImageRef, ServiceError>;

    /// Return the Family the current image belongs to, with its switchable
    /// features. None when the booted image isn't in the KNOWN_FAMILIES table.
    async fn current_family(&self) -> Result<Option<FamilyInfo>, ServiceError>;

    /// Return the most recent N versions of the given image, newest-first.
    /// Surfaces config-blob `created` timestamps for sha-only-tagged images.
    async fn list_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError>;

    /// Return the tags published for this image's stream, ready to populate
    /// the "Image source" tag dropdown. Stream tags come first (e.g.
    /// `latest`, `stable`), then dated tags newest-first.
    async fn list_available_tags(
        &self,
        image: &ImageRef,
    ) -> Result<Vec<crate::registry_client::AvailableTag>, ServiceError>;
 
    /// Compute the target image ref for a family + selected features + stream.
    /// Returns None if the combination doesn't match a published image.
    ///
    /// # Arguments
    /// * `family` — the current family (from current_family())
    /// * `features` — selected feature IDs (e.g. ["nvidia"], ["dx", "nvidia"])
    /// * `stream` — the tag stream to target (e.g. "latest", "stable", "lts")
    fn resolve_target_with_stream(
        &self,
        family: &FamilyInfo,
        features: &[String],
        stream: &str,
    ) -> Option<ImageRef>;

    /// Compute the target image ref for a family + selected features (legacy).
    /// Defaults to the family's first (canonical) stream.
    /// Use resolve_target_with_stream() to specify a different stream.
    fn resolve_target(&self, family: &FamilyInfo, features: &[String]) -> Option<ImageRef> {
        let default_stream = family.streams.first().map(|s| s.as_str()).unwrap_or("latest");
        self.resolve_target_with_stream(family, features, default_stream)
    }

    /// Switch the booted system to a new image. Progress events stream on the
    /// returned receiver; the operation is complete when `Done` or `Failed`
    /// arrives and the channel closes.
    async fn switch_image(
        &self,
        target: &ImageRef,
    ) -> Result<mpsc::Receiver<SwitchProgress>, ServiceError>;
}

/// Process-wide service singleton.
///
/// The GTK frontend grabs this from anywhere via `service::global()` rather
/// than threading an `Arc<dyn UpdaterService>` through every closure. A CLI
/// frontend doing the same thing would init() its own singleton; integration
/// tests can swap a mock in by calling `init()` before any UI is constructed.
///
/// `OnceLock` lets us avoid the "did we initialise it?" check on every call
/// site — the only path that needs `init()` is `main()`. Calling `init()`
/// twice panics, since reinit would race with whatever's already running.
static SERVICE: OnceLock<Arc<dyn UpdaterService>> = OnceLock::new();

/// Install the process-wide UpdaterService. Call from `main()` exactly once
/// before any UI components are built.
pub fn init(svc: Arc<dyn UpdaterService>) {
    if SERVICE.set(svc).is_err() {
        panic!("service::init called twice");
    }
}

/// Return the process-wide UpdaterService. Panics if `init()` hasn't been
/// called yet — UI code that calls this should be reached only after `main()`.
pub fn global() -> Arc<dyn UpdaterService> {
    SERVICE
        .get()
        .expect("service::init() must be called before service::global()")
        .clone()
}

/// Default in-process implementation backed by the existing registry_client
/// and orchestrator modules. Constructed once at app startup and passed to
/// UI components as `Arc<dyn UpdaterService>`. The Registry dependency is
/// injected so tests can swap a fixture-backed impl in.
pub struct BootcUpdaterService {
    registry: Arc<dyn Registry>,
}

impl BootcUpdaterService {
    pub fn new() -> Arc<dyn UpdaterService> {
        Self::with_registry(Arc::new(HttpRegistry))
    }

    /// Construct a service with a caller-supplied Registry. Used by tests
    /// to inject a fixture, and by future frontends that might want to
    /// front a non-OCI source (a local cache, a different registry, etc.).
    pub fn with_registry(registry: Arc<dyn Registry>) -> Arc<dyn UpdaterService> {
        Arc::new(Self { registry })
    }
}

impl Default for BootcUpdaterService {
    fn default() -> Self {
        Self {
            registry: Arc::new(HttpRegistry),
        }
    }
}

#[async_trait::async_trait]
impl UpdaterService for BootcUpdaterService {
    async fn current_image(&self) -> Result<ImageRef, ServiceError> {
        self.registry
            .detect_booted_image()
            .await
            .ok_or_else(|| ServiceError::NotFound("no booted image detected".into()))
    }

    async fn current_family(&self) -> Result<Option<FamilyInfo>, ServiceError> {
        let Some(image) = self.registry.detect_booted_image().await else {
            return Ok(None);
        };
        let Some(fam) =
            crate::registry_client::Family::best_match(&image.org, &image.image, &image.tag)
        else {
            return Ok(None);
        };
        let features = fam
            .available_features()
            .into_iter()
            .map(|feat| Feature {
                id: feat.to_string(),
                display_name: feature_display_name(feat).to_string(),
                subtitle: feature_subtitle(feat).to_string(),
            })
            .collect();
        Ok(Some(FamilyInfo {
            name: fam.name.to_string(),
            base_image: fam.base_image().to_string(),
            features,
            streams: fam.streams.to_vec(),
        }))
    }

    async fn list_versions(
        &self,
        image: &ImageRef,
        max: usize,
    ) -> Result<Vec<ImageVersion>, ServiceError> {
        let mut versions = self.registry.fetch_versions(image, max).await?;
        versions.sort_by(|a, b| b.date.cmp(&a.date));
        versions.truncate(max);
        Ok(versions)
    }

    async fn list_available_tags(
        &self,
        image: &ImageRef,
    ) -> Result<Vec<crate::registry_client::AvailableTag>, ServiceError> {
        self.registry.list_available_tags(image).await
    }

    fn resolve_target(&self, family: &FamilyInfo, features: &[String]) -> Option<ImageRef> {
        // Find the matching static Family for the resolution helper. We do
        // not carry &'static Family across the trait boundary so callers (and
        // alternative frontends) only see plain data.
        let fam = crate::registry_client::KNOWN_FAMILIES
            .iter()
            .find(|f| f.name == family.name)?;
        let want: Vec<&str> = features.iter().map(|s| s.as_str()).collect();
        let target_image = fam.select_image_for_features(&want)?;
        let default_stream = fam.streams.first().map(|s| s.as_str()).unwrap_or("latest");
        Some(ImageRef {
            registry: "ghcr.io".to_string(),
            org: fam.org.to_string(),
            image: target_image.to_string(),
            tag: default_stream.to_string(),
        })
    }

    fn resolve_target_with_stream(
        &self,
        family: &FamilyInfo,
        features: &[String],
        stream: &str,
    ) -> Option<ImageRef> {
        // Find the matching static Family for the resolution helper.
        let fam = crate::registry_client::KNOWN_FAMILIES
            .iter()
            .find(|f| f.name == family.name)?;
        let want: Vec<&str> = features.iter().map(|s| s.as_str()).collect();
        let target_image = fam.select_image_for_features(&want)?;
        Some(ImageRef {
            registry: "ghcr.io".to_string(),
            org: fam.org.to_string(),
            image: target_image.to_string(),
            tag: stream.to_string(),
        })
    }

    async fn switch_image(
        &self,
        _target: &ImageRef,
    ) -> Result<mpsc::Receiver<SwitchProgress>, ServiceError> {
        // The orchestrator currently invokes pkexec bootc switch via
        // tokio::process and only surfaces an exit code. Wiring real
        // streaming progress here belongs in task #24 phase 2 (parse the
        // bootc layer-by-layer stdout). Until that lands we return
        // Unsupported so callers fall back to the existing direct path.
        Err(ServiceError::Unsupported(
            "switch_image streaming not yet implemented; use run_bootc_switch directly".into(),
        ))
    }
}

// Friendly-name maps duplicated from rebase_dialog.rs so the trait
// implementation doesn't depend on the UI module. When rebase_dialog
// migrates to consume FamilyInfo, the originals can be deleted.
fn feature_display_name(feat: &str) -> &'static str {
    match feat {
        "nvidia" => "NVIDIA drivers (proprietary)",
        "open" => "NVIDIA open kernel modules",
        "dx" => "Developer extras (DX)",
        "hwe" => "Hardware-enablement kernel (HWE)",
        "gdx" => "GNOME Developer extras (GDX)",
        "deck" => "Steam Deck profile",
        "asus" => "ASUS ROG tuning",
        "surface" => "Microsoft Surface kernel",
        "framework" => "Framework laptop tuning",
        _ => "Variant feature",
    }
}

fn feature_subtitle(feat: &str) -> &'static str {
    match feat {
        "nvidia" => "Use the closed-source NVIDIA driver",
        "open" => "Use Mesa's NVK / NVIDIA open-source kernel driver",
        "dx" => "Includes container tools, IDEs, and language SDKs",
        "hwe" => "Newer kernel + drivers backported for fresh hardware",
        "gdx" => "GNOME-focused developer toolchain",
        "deck" => "Tuned for Steam Deck hardware (gamescope, Steam shell)",
        "asus" => "Kernel patches for ASUS ROG Ally / Strix laptops",
        "surface" => "Linux-surface kernel + camera fix-ups",
        "framework" => "Power profiles + fingerprint reader support",
        _ => "Additional ublue extras",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use std::sync::Mutex;

    // ── feature_display_name / feature_subtitle ───────────────────────────

    #[test]
    fn feature_display_name_covers_all_known_keys() {
        let cases = [
            ("nvidia", "NVIDIA drivers (proprietary)"),
            ("open", "NVIDIA open kernel modules"),
            ("dx", "Developer extras (DX)"),
            ("hwe", "Hardware-enablement kernel (HWE)"),
            ("gdx", "GNOME Developer extras (GDX)"),
            ("deck", "Steam Deck profile"),
            ("asus", "ASUS ROG tuning"),
            ("surface", "Microsoft Surface kernel"),
            ("framework", "Framework laptop tuning"),
        ];
        for (key, expected) in cases {
            assert_eq!(feature_display_name(key), expected, "key={key}");
        }
    }

    #[test]
    fn feature_display_name_unknown_key_returns_generic() {
        assert_eq!(feature_display_name("unknown-feature"), "Variant feature");
    }

    #[test]
    fn feature_subtitle_covers_all_known_keys() {
        let cases = [
            ("nvidia", "Use the closed-source NVIDIA driver"),
            ("open", "Use Mesa's NVK / NVIDIA open-source kernel driver"),
            ("dx", "Includes container tools, IDEs, and language SDKs"),
            (
                "hwe",
                "Newer kernel + drivers backported for fresh hardware",
            ),
            ("gdx", "GNOME-focused developer toolchain"),
            (
                "deck",
                "Tuned for Steam Deck hardware (gamescope, Steam shell)",
            ),
            ("asus", "Kernel patches for ASUS ROG Ally / Strix laptops"),
            ("surface", "Linux-surface kernel + camera fix-ups"),
            ("framework", "Power profiles + fingerprint reader support"),
        ];
        for (key, expected) in cases {
            assert_eq!(feature_subtitle(key), expected, "key={key}");
        }
    }

    #[test]
    fn feature_subtitle_unknown_key_returns_generic() {
        assert_eq!(
            feature_subtitle("unknown-feature"),
            "Additional ublue extras"
        );
    }

    // ── ImageRef ─────────────────────────────────────────────────────────

    #[test]
    fn image_ref_parse_roundtrip() {
        let r = ImageRef::parse("ghcr.io/ublue-os/bluefin:stable-daily-43.20260527").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.org, "ublue-os");
        assert_eq!(r.image, "bluefin");
        assert_eq!(r.tag, "stable-daily-43.20260527");
        assert_eq!(
            r.as_string(),
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527"
        );
    }

    #[test]
    fn image_ref_parse_rejects_missing_tag() {
        assert!(ImageRef::parse("ghcr.io/ublue-os/bluefin").is_none());
    }

    #[test]
    fn image_ref_parse_rejects_missing_registry() {
        assert!(ImageRef::parse("ublue-os/bluefin:stable").is_none());
    }

    #[test]
    fn image_ref_handles_image_with_slashes() {
        // GHCR allows nested namespaces like ghcr.io/org/sub/image:tag.
        // ImageRef::parse should keep the whole "sub/image" segment as image.
        let r = ImageRef::parse("ghcr.io/projectbluefin/dakota:latest").unwrap();
        assert_eq!(r.image, "dakota");
    }

    // ── Fixture Registry for service-layer tests ─────────────────────────

    /// In-memory Registry impl that returns whatever the test put in. The
    /// records counter lets tests assert that a method was actually invoked
    /// — useful for confirming caching paths or default-image fallthrough.
    struct FixtureRegistry {
        booted: Option<ImageRef>,
        versions: std::collections::HashMap<String, Vec<ImageVersion>>,
        tags: std::collections::HashMap<String, Vec<crate::registry_client::AvailableTag>>,
        fetch_versions_calls: Mutex<u32>,
        detect_calls: Mutex<u32>,
        list_tags_calls: Mutex<u32>,
    }

    impl FixtureRegistry {
        fn new() -> Self {
            Self {
                booted: None,
                versions: std::collections::HashMap::new(),
                tags: std::collections::HashMap::new(),
                fetch_versions_calls: Mutex::new(0),
                detect_calls: Mutex::new(0),
                list_tags_calls: Mutex::new(0),
            }
        }

        fn with_booted(mut self, image: ImageRef) -> Self {
            self.booted = Some(image);
            self
        }

        fn with_versions(mut self, image: &ImageRef, versions: Vec<ImageVersion>) -> Self {
            self.versions.insert(image.as_string(), versions);
            self
        }

        fn with_tags(mut self, image: &ImageRef, tags: Vec<&str>) -> Self {
            // Display == raw for plain stream / dated tags; that matches the
            // production transform for non-sha tags. Tests that need to
            // exercise the sha→date display swap should construct
            // `AvailableTag` values directly.
            self.tags.insert(
                image.as_string(),
                tags.into_iter()
                    .map(|t| crate::registry_client::AvailableTag {
                        display: t.to_string(),
                        raw: t.to_string(),
                    })
                    .collect(),
            );
            self
        }
    }

    #[async_trait::async_trait]
    impl Registry for FixtureRegistry {
        async fn fetch_versions(
            &self,
            image: &ImageRef,
            _max: usize,
        ) -> Result<Vec<ImageVersion>, ServiceError> {
            *self.fetch_versions_calls.lock().unwrap() += 1;
            self.versions
                .get(&image.as_string())
                .cloned()
                .ok_or_else(|| ServiceError::NotFound(format!("no fixture for {image}")))
        }

        async fn detect_booted_image(&self) -> Option<ImageRef> {
            *self.detect_calls.lock().unwrap() += 1;
            self.booted.clone()
        }

        async fn list_available_tags(
            &self,
            image: &ImageRef,
        ) -> Result<Vec<crate::registry_client::AvailableTag>, ServiceError> {
            *self.list_tags_calls.lock().unwrap() += 1;
            self.tags
                .get(&image.as_string())
                .cloned()
                .ok_or_else(|| ServiceError::NotFound(format!("no tag fixture for {image}")))
        }
    }

    fn dummy_version(date: &str, version: &str, full_ref: &str) -> ImageVersion {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("valid date");
        ImageVersion {
            date: d,
            full_ref: full_ref.to_string(),
            version: version.to_string(),
            kernel: String::new(),
            revision: String::new(),
            created: chrono::Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap()),
        }
    }

    fn dakota_ref() -> ImageRef {
        ImageRef {
            registry: "ghcr.io".to_string(),
            org: "projectbluefin".to_string(),
            image: "dakota".to_string(),
            tag: "latest".to_string(),
        }
    }

    // ── current_image: routes through Registry::detect_booted_image ──────

    #[tokio::test]
    async fn current_image_returns_booted_from_registry() {
        let reg = Arc::new(FixtureRegistry::new().with_booted(dakota_ref()));
        let svc = BootcUpdaterService::with_registry(reg.clone());
        let got = svc.current_image().await.unwrap();
        assert_eq!(got, dakota_ref());
    }

    #[tokio::test]
    async fn current_image_not_found_when_registry_has_none() {
        let reg = Arc::new(FixtureRegistry::new());
        let svc = BootcUpdaterService::with_registry(reg);
        let err = svc.current_image().await.unwrap_err();
        assert!(
            matches!(err, ServiceError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    // ── current_family: maps detection → FamilyInfo via KNOWN_FAMILIES ───

    #[tokio::test]
    async fn current_family_matches_dakota() {
        let reg = Arc::new(FixtureRegistry::new().with_booted(dakota_ref()));
        let svc = BootcUpdaterService::with_registry(reg);
        let fam = svc
            .current_family()
            .await
            .unwrap()
            .expect("dakota in KNOWN");
        assert_eq!(fam.name, "Bluefin Dakota");
        assert_eq!(fam.base_image, "dakota");
        // Dakota has a single feature: nvidia (per the trimmed KNOWN_FAMILIES).
        let ids: Vec<&str> = fam.features.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["nvidia"]);
        assert_eq!(fam.features[0].display_name, "NVIDIA drivers (proprietary)");
    }

    #[tokio::test]
    async fn current_family_matches_bluefin_stable_with_full_feature_slate() {
        let reg = Arc::new(FixtureRegistry::new().with_booted(ImageRef {
            registry: "ghcr.io".to_string(),
            org: "ublue-os".to_string(),
            image: "bluefin".to_string(),
            tag: "stable".to_string(),
        }));
        let svc = BootcUpdaterService::with_registry(reg);
        let fam = svc
            .current_family()
            .await
            .unwrap()
            .expect("bluefin in KNOWN");
        assert_eq!(fam.name, "Bluefin Stable");
        // Should include at least nvidia, open, dx (alphabetical from
        // Family::available_features).
        let ids: Vec<&str> = fam.features.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"nvidia"), "features missing nvidia: {ids:?}");
        assert!(ids.contains(&"open"), "features missing open: {ids:?}");
        assert!(ids.contains(&"dx"), "features missing dx: {ids:?}");
    }

    #[tokio::test]
    async fn current_family_returns_none_for_unknown_image() {
        let reg = Arc::new(FixtureRegistry::new().with_booted(ImageRef {
            registry: "ghcr.io".to_string(),
            org: "some-other-org".to_string(),
            image: "random-image".to_string(),
            tag: "latest".to_string(),
        }));
        let svc = BootcUpdaterService::with_registry(reg);
        assert!(svc.current_family().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn current_family_returns_none_when_nothing_booted() {
        let reg = Arc::new(FixtureRegistry::new());
        let svc = BootcUpdaterService::with_registry(reg);
        assert!(svc.current_family().await.unwrap().is_none());
    }

    // ── list_versions: sort + truncate + Registry pass-through ───────────

    #[tokio::test]
    async fn list_versions_returns_sorted_newest_first() {
        let img = dakota_ref();
        let reg = Arc::new(FixtureRegistry::new().with_versions(
            &img,
            vec![
                dummy_version(
                    "2026-02-10",
                    "0.1",
                    "ghcr.io/projectbluefin/dakota:latest.20260210",
                ),
                dummy_version(
                    "2026-02-12",
                    "0.3",
                    "ghcr.io/projectbluefin/dakota:latest.20260212",
                ),
                dummy_version(
                    "2026-02-11",
                    "0.2",
                    "ghcr.io/projectbluefin/dakota:latest.20260211",
                ),
            ],
        ));
        let svc = BootcUpdaterService::with_registry(reg);
        let got = svc.list_versions(&img, 8).await.unwrap();
        let dates: Vec<&str> = got.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(dates, vec!["0.3", "0.2", "0.1"]);
    }

    #[tokio::test]
    async fn list_versions_truncates_to_max() {
        let img = dakota_ref();
        let mut versions = vec![];
        for day in 1..=15u32 {
            versions.push(dummy_version(
                &format!("2026-02-{:02}", day),
                &format!("0.{day}"),
                &format!("ghcr.io/projectbluefin/dakota:latest.202602{:02}", day),
            ));
        }
        let reg = Arc::new(FixtureRegistry::new().with_versions(&img, versions));
        let svc = BootcUpdaterService::with_registry(reg);
        let got = svc.list_versions(&img, 8).await.unwrap();
        assert_eq!(got.len(), 8);
        // Confirm we got the 8 *newest* (days 8..=15), not the first 8.
        assert_eq!(got[0].version, "0.15");
        assert_eq!(got[7].version, "0.8");
    }

    #[tokio::test]
    async fn list_versions_propagates_registry_error() {
        let img = dakota_ref();
        // FixtureRegistry returns NotFound for unset images.
        let reg = Arc::new(FixtureRegistry::new());
        let svc = BootcUpdaterService::with_registry(reg);
        let err = svc.list_versions(&img, 8).await.unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_versions_handles_empty_result() {
        let img = dakota_ref();
        let reg = Arc::new(FixtureRegistry::new().with_versions(&img, vec![]));
        let svc = BootcUpdaterService::with_registry(reg);
        let got = svc.list_versions(&img, 8).await.unwrap();
        assert_eq!(got.len(), 0);
    }

    // ── resolve_target ───────────────────────────────────────────────────

    #[tokio::test]
    async fn resolve_target_routes_to_known_family() {
        let svc = BootcUpdaterService::default();
        let family = FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![],
        };
        let r = svc
            .resolve_target(&family, &["nvidia".to_string()])
            .unwrap();
        assert_eq!(r.image, "bluefin-nvidia");
    }

    #[tokio::test]
    async fn resolve_target_returns_none_for_unpublished_combo() {
        let svc = BootcUpdaterService::default();
        let family = FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![],
        };
        // "open" alone (without nvidia) isn't a published Bluefin image.
        assert!(svc.resolve_target(&family, &["open".to_string()]).is_none());
    }

    #[tokio::test]
    async fn resolve_target_handles_dakota_nvidia_combo() {
        // Verify the trimmed Dakota family resolves correctly — only
        // dakota and dakota-nvidia exist on GHCR.
        let svc = BootcUpdaterService::default();
        let family = FamilyInfo {
            name: "Bluefin Dakota".to_string(),
            base_image: "dakota".to_string(),
            features: vec![],
        };
        let r = svc
            .resolve_target(&family, &["nvidia".to_string()])
            .unwrap();
        assert_eq!(r.image, "dakota-nvidia");
        // dakota-dx is a ghost variant, must not resolve.
        assert!(svc.resolve_target(&family, &["dx".to_string()]).is_none());
    }

    #[tokio::test]
    async fn resolve_target_returns_none_for_unknown_family_name() {
        let svc = BootcUpdaterService::default();
        let family = FamilyInfo {
            name: "Fictional Family".to_string(),
            base_image: "imaginary".to_string(),
            features: vec![],
        };
        assert!(svc.resolve_target(&family, &[]).is_none());
    }

    // ── list_available_tags: tag dropdown population ─────────────────────

    #[tokio::test]
    async fn list_available_tags_returns_fixture_data() {
        let img = dakota_ref();
        let reg = Arc::new(FixtureRegistry::new().with_tags(
            &img,
            vec!["latest", "stable", "latest.20260212", "latest.20260211"],
        ));
        let svc = BootcUpdaterService::with_registry(reg);
        let tags = svc.list_available_tags(&img).await.unwrap();
        assert_eq!(tags.len(), 4);
        assert_eq!(tags[0].display, "latest");
        assert_eq!(tags[0].raw, "latest");
    }

    #[tokio::test]
    async fn list_available_tags_propagates_not_found() {
        let img = dakota_ref();
        let reg = Arc::new(FixtureRegistry::new());
        let svc = BootcUpdaterService::with_registry(reg);
        let err = svc.list_available_tags(&img).await.unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }

    #[tokio::test]
    async fn list_available_tags_routes_through_registry_each_call() {
        // No caching layer between service and registry yet — pin that
        // assumption so a future cache addition is deliberate.
        let img = dakota_ref();
        let reg = Arc::new(FixtureRegistry::new().with_tags(&img, vec!["latest"]));
        let reg_clone = reg.clone();
        let svc = BootcUpdaterService::with_registry(reg);
        let _ = svc.list_available_tags(&img).await.unwrap();
        let _ = svc.list_available_tags(&img).await.unwrap();
        let _ = svc.list_available_tags(&img).await.unwrap();
        assert_eq!(*reg_clone.list_tags_calls.lock().unwrap(), 3);
    }

    // ── switch_image still unimplemented ─────────────────────────────────

    #[tokio::test]
    async fn switch_image_is_unsupported_until_phase2() {
        let svc = BootcUpdaterService::default();
        let err = svc.switch_image(&dakota_ref()).await.unwrap_err();
        assert!(matches!(err, ServiceError::Unsupported(_)));
    }
}
