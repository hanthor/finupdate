//! GHCR registry client for querying historical image versions.
//!
//! Fetches the list of dated image tags from `ghcr.io`, filters to the last
//! `days` days, then retrieves OCI manifest annotations for each tag in
//! parallel to collect version metadata (build time, kernel, git revision).
//!
//! All network I/O is async (tokio). Callers run this on a background thread.
//!
//! ## Tag format
//!
//! Universal Blue images use the pattern:
//! ```text
//! {stream}-{YYYYMMDD}    e.g.  stable-daily-43-20260222
//! {stream}.{YYYYMMDD}    e.g.  stable-daily-43.20260222   (dot variant)
//! ```
//! Both separators are supported; the dot form is preferred.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use std::collections::HashMap;

// ── Public data types ─────────────────────────────────────────────────────────

/// Metadata for a single dated image build available for rebasing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageVersion {
    /// Calendar date the image was built (UTC, YYYYMMDD from the tag).
    pub date: NaiveDate,
    /// Full OCI image reference — pass this to `bootc switch`.
    pub full_ref: String,
    /// Human-readable version string from `org.opencontainers.image.version`.
    pub version: String,
    /// Kernel version from `ostree.linux` annotation.
    pub kernel: String,
    /// Short git commit hash (first 8 chars of `org.opencontainers.image.revision`).
    pub revision: String,
    /// Build timestamp from `org.opencontainers.image.created`.
    pub created: DateTime<Utc>,
}

/// Error type for registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("No dated tags found for stream '{0}'")]
    NoTags(String),
    #[error("Unable to detect current image — is bootc installed?")]
    NoCurrentImage,
}

/// One entry in the available-tags list.
///
/// `raw` is the actual tag string the registry serves (what gets passed to
/// `bootc switch` / `image:tag` refs). `display` is what the tag dropdown
/// shows the user — typically equal to `raw`, but for sha-tagged manifests
/// (dakota-nvidia and other Project Bluefin images) we substitute a human-
/// friendly `"Build YYYY-MM-DD"` label so the dropdown isn't full of 40-char
/// hex hashes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AvailableTag {
    pub display: String,
    pub raw: String,
}

/// One coherent product family — a user-facing concept that groups a set of
/// sibling image *names* (the GPU/hardware variants like `-nvidia`, `-dx`,
/// `-deck`) and the tag streams (channels) under which they're published.
///
/// A given GHCR image can belong to multiple families — Bluefin Stable and
/// Bluefin LTS, for instance, share the `ublue-os/bluefin` image but use
/// disjoint stream sets (`stable*` vs `lts*`).
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    /// Display name for menus / dropdowns: "Bluefin Stable", "Bluefin LTS".
    pub name: &'static str,
    /// Registry org owning every image in this family.
    pub org: &'static str,
    /// Sibling image names — what the rebase dialog's variant chips render.
    /// First entry is the canonical/default for chip rendering. Each entry
    /// resolves to `ghcr.io/{org}/{name}:{stream}` at rebase time.
    pub images: &'static [&'static str],
    /// Tag streams this family publishes under. The rebase / changelog UI
    /// can offer a stream picker. First entry is the canonical default.
    pub streams: &'static [&'static str],
}

/// Catalogue of Universal Blue + Project Bluefin product families.
///
/// **Add new families / variants here as Universal Blue ships them.** Source
/// of truth for the user-visible "family" concept across the app. The rebase
/// dialog's two-toggle UI resolves user picks against this table via
/// [`Family::best_match`] + [`UpdaterService::resolve_target`], so adding a
/// variant here automatically makes it selectable.
pub const KNOWN_FAMILIES: &[Family] = &[
    Family {
        name: "Bluefin Stable",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-nvidia-open",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-dx-nvidia-open",
            "bluefin-asus",
            "bluefin-asus-nvidia",
            "bluefin-surface",
            "bluefin-framework",
        ],
        streams: &["latest", "stable", "stable-daily", "beta", "gts"],
    },
    Family {
        name: "Bluefin LTS",
        org: "ublue-os",
        images: &[
            "bluefin",
            "bluefin-nvidia",
            "bluefin-dx",
            "bluefin-dx-nvidia",
            "bluefin-gdx",
        ],
        streams: &["lts", "lts-hwe", "lts-amd64", "lts-arm64", "gdx"],
    },
    Family {
        name: "Aurora",
        org: "ublue-os",
        images: &[
            "aurora",
            "aurora-nvidia",
            "aurora-nvidia-open",
            "aurora-dx",
            "aurora-dx-nvidia",
            "aurora-dx-nvidia-open",
        ],
        streams: &["latest", "stable", "stable-daily", "beta"],
    },
    Family {
        name: "Bazzite KDE",
        org: "ublue-os",
        images: &[
            "bazzite",
            "bazzite-nvidia",
            "bazzite-nvidia-open",
            "bazzite-deck",
            "bazzite-deck-nvidia",
            "bazzite-asus",
            "bazzite-framework",
        ],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    Family {
        name: "Bazzite GNOME",
        org: "ublue-os",
        images: &["bazzite-gnome", "bazzite-gnome-nvidia"],
        streams: &["stable", "testing", "unstable", "latest"],
    },
    // ucore intentionally omitted — server image, out of scope for the
    // desktop bootc settings app. If you ever booted a finupdate user onto
    // ucore by accident, the "Family not recognized" fallback in the rebase
    // dialog catches it.
    Family {
        name: "Bluefin Dakota",
        org: "projectbluefin",
        // Only `dakota` and `dakota-nvidia` are published on GHCR (verified
        // 2026-05-30). The Bluefin/Aurora-style -dx and -nvidia-open variants
        // don't exist for Dakota — leaving them here would let the rebase
        // dialog show feature switches that resolve to bogus refs.
        images: &["dakota", "dakota-nvidia"],
        streams: &["latest", "testing"],
    },
];

impl Family {
    /// Find every family that contains `image` under `org`. An image can
    /// belong to more than one family (Bluefin's image is shared between
    /// Bluefin Stable and Bluefin LTS; the stream tells them apart).
    pub fn all_for_image(org: &str, image: &str) -> Vec<&'static Family> {
        KNOWN_FAMILIES
            .iter()
            .filter(|f| f.org == org && f.images.iter().any(|i| *i == image))
            .collect()
    }

    /// Pick the family that best matches an `(org, image, stream)` triple by
    /// preferring families whose streams contain `stream` exactly. Falls back
    /// to any family containing the image, then `None`.
    pub fn best_match(org: &str, image: &str, stream: &str) -> Option<&'static Family> {
        let candidates = Self::all_for_image(org, image);
        candidates
            .iter()
            .find(|f| f.streams.iter().any(|s| *s == stream))
            .copied()
            .or_else(|| candidates.first().copied())
    }

    /// The first image name is treated as the family's *base* — every other
    /// image in `images` is derived from it by adding feature suffixes.
    /// E.g. Bluefin Stable's base is "bluefin"; "bluefin-nvidia" is "bluefin"
    /// plus the {nvidia} feature; "bluefin-dx-nvidia" is base + {dx, nvidia}.
    pub fn base_image(&self) -> &'static str {
        self.images.first().copied().unwrap_or("")
    }

    /// Atomic feature suffixes available in this family — derived from the
    /// image names by splitting each non-base image's suffix on '-'. Powers
    /// the SwitchRow list in the rebase dialog: e.g. Bluefin Stable yields
    /// `["asus", "dx", "framework", "nvidia", "open", "surface"]`.
    ///
    /// The order is alphabetical for stable UI rendering. Not every
    /// combination is valid — call [`Family::select_image_for_features`] to
    /// resolve a switch state to a concrete image (returns `None` if no
    /// image in the family has that exact combination).
    pub fn available_features(&self) -> Vec<&'static str> {
        let base = self.base_image();
        let mut set: std::collections::BTreeSet<&'static str> = Default::default();
        for img in self.images {
            if *img == base {
                continue;
            }
            if let Some(suffix) = img.strip_prefix(&format!("{}-", base)) {
                for atom in suffix.split('-') {
                    set.insert(atom);
                }
            }
        }
        set.into_iter().collect()
    }

    /// Given a set of selected atomic features (`features`), find the image
    /// name in this family whose suffix is exactly that set.
    ///
    /// Returns `Some(image_name)` when the combination matches a published
    /// image (`"bluefin"` for `[]`, `"bluefin-nvidia"` for `["nvidia"]`,
    /// `"bluefin-dx-nvidia"` for `["dx", "nvidia"]`), or `None` if no image
    /// matches (e.g. `["open"]` alone — open driver requires nvidia).
    pub fn select_image_for_features(&self, features: &[&str]) -> Option<&'static str> {
        let base = self.base_image();
        if features.is_empty() {
            return self.images.iter().copied().find(|i| *i == base);
        }
        for img in self.images {
            if *img == base {
                continue;
            }
            let suffix = match img.strip_prefix(&format!("{}-", base)) {
                Some(s) => s,
                None => continue,
            };
            let mut have: Vec<&str> = suffix.split('-').collect();
            have.sort();
            let mut want: Vec<&str> = features.iter().copied().collect();
            want.sort();
            if have == want {
                return Some(img);
            }
        }
        None
    }
}

// ── Internal GHCR API types ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Deserialize)]
struct TagListResponse {
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestResponse {
    annotations: Option<HashMap<String, String>>,
}

// ── RegistryClient ────────────────────────────────────────────────────────────

/// Client for fetching dated image versions from GHCR.
pub struct RegistryClient {
    registry: String,
    org: String,
    image: String,
    /// Tag prefix for dated builds, e.g. `"stable-daily-43"`.
    stream: String,
    client: reqwest::Client,
}

impl RegistryClient {
    /// Create a client targeting the given image stream.
    ///
    /// `stream` is everything in the tag before the date, e.g. `"stable-daily-43"`.
    pub fn new(registry: &str, org: &str, image: &str, stream: &str) -> Self {
        Self {
            registry: registry.to_string(),
            org: org.to_string(),
            image: image.to_string(),
            stream: stream.to_string(),
            client: build_http_client(),
        }
    }

    pub fn registry(&self) -> &str {
        &self.registry
    }
    pub fn org(&self) -> &str {
        &self.org
    }
    pub fn image(&self) -> &str {
        &self.image
    }
    pub fn stream(&self) -> &str {
        &self.stream
    }

    /// Detect the current image stream from the running system.
    ///
    /// Precedence:
    /// 1. `Settings::mock_identity` (test override — no subprocess, no network).
    /// 2. `FINUPDATE_IMAGE` env var (demo/debug override from a terminal).
    /// 3. `bootc status --json` (most reliable on a real host).
    /// 4. `/etc/os-release` fallback (Flatpak-friendly via flatpak-spawn).
    pub async fn detect() -> Option<Self> {
        Self::detect_with_settings(&crate::settings::Settings::load()).await
    }

    /// Like [`Self::detect`], but reads the mock-identity override from the
    /// caller-supplied `Settings` instead of loading from disk. Lets tests
    /// (and any future preferences UI) drive detection without round-tripping
    /// through settings.json.
    pub async fn detect_with_settings(settings: &crate::settings::Settings) -> Option<Self> {
        println!("[debug] RegistryClient::detect_with_settings()");

        if let Some(mock) = settings.mock_identity.as_ref() {
            let stream = strip_date_suffix(&mock.tag).unwrap_or_else(|| mock.tag.clone());
            println!(
                "[debug] RegistryClient::detect_with_settings() mock_identity = {}/{}/{} stream={}",
                mock.registry, mock.org, mock.image, stream
            );
            return Some(Self::new(&mock.registry, &mock.org, &mock.image, &stream));
        }

        // FINUPDATE_IMAGE=registry/org/image:tag — quick-and-dirty override
        // when developing from a terminal. Same precedence as the legacy
        // status_view::detect_bootc_image_info path so the env var still works
        // after the UI migrates to the service.
        //
        // Parsing is lenient on the tag (uses it as-is if it isn't dated) so
        // `FINUPDATE_IMAGE=ghcr.io/ublue-os/bluefin:stable` works. parse_image_ref
        // is stricter — it requires a date suffix because it's used to interpret
        // bootc-status output where tags are always dated.
        if let Ok(override_ref) = std::env::var("FINUPDATE_IMAGE") {
            if !override_ref.is_empty() {
                if let Some((without_tag, tag)) = override_ref.rsplit_once(':') {
                    let parts: Vec<&str> = without_tag.splitn(3, '/').collect();
                    if parts.len() >= 3 {
                        let stream = strip_date_suffix(tag).unwrap_or_else(|| tag.to_string());
                        println!(
                            "[debug] RegistryClient::detect_with_settings() FINUPDATE_IMAGE = {}",
                            override_ref
                        );
                        return Some(Self::new(parts[0], parts[1], parts[2], &stream));
                    }
                }
            }
        }

        // Try bootc status --json for the most reliable answer.
        if let Some(client) = Self::detect_from_bootc().await {
            return Some(client);
        }
        // Fallback: parse os-release
        let fallback = Self::detect_from_os_release();
        println!(
            "[debug] RegistryClient::detect() fallback os-release = {:?}",
            fallback.as_ref().map(|c| c.stream.clone())
        );
        fallback
    }

    async fn detect_from_bootc() -> Option<Self> {
        let cmd_name = if crate::update_worker::is_flatpak() {
            "flatpak-spawn --host bootc status --json"
        } else {
            "bootc status --json"
        };
        println!(
            "[debug] RegistryClient::detect_from_bootc() running {}",
            cmd_name
        );
        let output = if crate::update_worker::is_flatpak() {
            tokio::process::Command::new("flatpak-spawn")
                .args(["--host", "bootc", "status", "--json"])
                .output()
                .await
                .ok()?
        } else {
            tokio::process::Command::new("bootc")
                .args(["status", "--json"])
                .output()
                .await
                .ok()?
        };

        println!(
            "[debug] RegistryClient::detect_from_bootc() exit = {:?}",
            output.status
        );
        if !output.status.success() {
            return None;
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

        // Navigate: .status.booted.image.image.image  → full ref string
        let image_ref = json
            .pointer("/status/booted/image/image/image")
            .or_else(|| json.pointer("/status/booted/image/image"))
            .and_then(|v| v.as_str())?;

        // image_ref = "ghcr.io/ublue-os/bluefin:stable-daily-43.20260222"
        parse_image_ref(image_ref)
    }

    fn read_os_release_content() -> Option<String> {
        if crate::update_worker::is_flatpak() {
            let output = std::process::Command::new("flatpak-spawn")
                .args(["--host", "cat", "/etc/os-release"])
                .output()
                .ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        } else {
            std::fs::read_to_string("/etc/os-release").ok()
        }
    }

    pub fn detect_from_os_release() -> Option<Self> {
        if let Some(content) = Self::read_os_release_content() {
            let mut image_ref = None;
            let mut image_tag = None;
            let mut image_id = None;
            let mut version_id = None;
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("IMAGE_REF=") {
                    image_ref = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("IMAGE_TAG=") {
                    image_tag = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("IMAGE_ID=") {
                    image_id = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                    version_id = Some(v.trim_matches('"').to_string());
                }
            }

            if let Some(ref_str) = image_ref {
                let clean_ref = if let Some(pos) = ref_str.find("docker://") {
                    &ref_str[pos + 9..]
                } else {
                    &ref_str
                };
                let parts: Vec<&str> = clean_ref.split('/').collect();
                if parts.len() >= 3 {
                    let registry = parts[0];
                    let org = parts[1];
                    let image = parts[2..].join("/");
                    let tag = image_tag.unwrap_or_else(|| "latest".to_string());
                    let stream = strip_date_suffix(&tag).unwrap_or(tag);
                    return Some(Self::new(registry, org, &image, &stream));
                }
            }

            if let (Some(img), Some(ver)) = (image_id, version_id) {
                let org = if img.contains("dakota")
                    || img.contains("bluefin")
                    || img.contains("aurora")
                {
                    "projectbluefin"
                } else {
                    "ublue-os"
                };
                let stream = if ver == "latest" {
                    "latest".to_string()
                } else {
                    format!("stable-daily-{}", ver)
                };
                return Some(Self::new("ghcr.io", org, &img, &stream));
            }
        }
        None
    }

    /// Fetch the most recent `max` versions for this stream, newest-first.
    ///
    /// - Round trip 1: tag list
    /// - Round trip 2…N: manifest HEADs, up to 12 concurrent
    ///
    /// `max` is the CAP on the returned set — fewer entries are returned if
    /// the stream simply doesn't have that many builds. The internal SHA
    /// probe (for dakota-style sha-only-tagged images) is sized off the
    /// same value so larger `max` actually surfaces more results rather
    /// than capping at the old hardcoded CANDIDATE_CAP=8.
    pub async fn fetch_versions(&self, max: usize) -> Result<Vec<ImageVersion>, RegistryError> {
        let cache_record_key = cache_key(
            &self.registry,
            &self.org,
            &self.image,
            &self.stream,
            &format!("versions_{}", max),
        );
        if let Some(cached) = load_registry_cache::<Vec<ImageVersion>>(&cache_record_key) {
            tracing::info!(
                "Using cached versions for {}/{}/{}:{}",
                self.registry,
                self.org,
                self.image,
                self.stream
            );
            return Ok(cached);
        }

        let token = self.get_token().await?;
        let client = self.client.clone();

        // Fetch the full tag list.
        let tags_url = format!(
            "https://{}/v2/{}/{}/tags/list?n=1000",
            self.registry, self.org, self.image
        );
        let tag_resp: TagListResponse = client
            .get(&tags_url)
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;

        // Parse every dated tag for this stream. No date-window filter:
        // CANDIDATE_CAP below already bounds the work, and a window starves
        // stale variants of history (bluefin-nvidia stopped publishing
        // stable-daily in 2025-10 — their last 8 tags are still the rollback
        // targets users care about).
        let mut candidate_tags: Vec<(NaiveDate, String)> = tag_resp
            .tags
            .iter()
            .filter_map(|tag| parse_dated_tag(tag, &self.stream).map(|d| (d, tag.clone())))
            .collect();

        // Sort by date DESC, but DO NOT truncate yet — if we're short of
        // CANDIDATE_CAP we'll supplement via the sha-tag config-blob harvest
        // below, and a final sort+truncate happens after that.
        //
        // SHA_PROBE_CAP was bumped from 30 → 120 because dakota switched
        // fully to sha-tagged manifests around 2026-02; with the smaller
        // cap and unspecified GHCR tag-list ordering, the probe was
        // landing on a handful of old February tags and missing every
        // build since. 120 probes at 8-way concurrency runs in ~5-10s
        // against ghcr.io — acceptable for a one-shot changelog fetch.
        // CANDIDATE_CAP = caller-supplied `max`; SHA_PROBE_CAP scales with
        // it so a "load older builds" round trip actually surfaces more.
        let candidate_cap = max.max(1);
        let sha_probe_cap = candidate_cap.max(120);
        candidate_tags.sort_by(|a, b| b.0.cmp(&a.0));

        // Slow path: dakota-nvidia and similar images publish via sha-only
        // tags (40-hex commit shas) rather than dated names. parse_dated_tag
        // can't surface them, so we probe up to SHA_PROBE_CAP sha-tagged
        // manifests and ask their config blobs for `created` timestamps.
        // Two HTTP calls per probe (manifest + config blob), bounded by an
        // 8-way semaphore — measured at ~2-4s for 30 probes against
        // ghcr.io. Only runs when dated tags came up short.
        if candidate_tags.len() < candidate_cap {
            // Build full list of sha-only tags first, then take an evenly-spread
            // sample. IMPORTANT: GHCR returns tags alphabetically so sha hashes
            // starting with 0-3 come first. If we simply take sha_tags[..N] we
            // probe ~25% of the hash space and miss all recent builds whose hashes
            // start with 4-f. Dakota switched to pure sha-tagged manifests in
            // 2026-02 — taking only the first N tags was the root cause of the
            // "always shows February dates" bug. Stride-sampling ensures we probe
            // tags from every part of the hash alphabet regardless of registry ordering.
            let sha_tags: Vec<String> = tag_resp
                .tags
                .iter()
                .filter(|t| is_sha_only_tag(t))
                .cloned()
                .collect();

            let probe_list = if sha_tags.len() > sha_probe_cap {
                // Stride-sample: take every stride-th tag so we cover the full
                // alphabetic range of sha hashes rather than only tags starting
                // with 0x00-0x3f.
                let stride = sha_tags.len() / sha_probe_cap;
                sha_tags.iter().step_by(stride.max(1)).take(sha_probe_cap).cloned().collect()
            } else {
                sha_tags
            };

            if !probe_list.is_empty() {
                let probed = self.probe_sha_tag_dates(&probe_list, &token, &client).await;
                candidate_tags.extend(probed);
                candidate_tags.sort_by(|a, b| b.0.cmp(&a.0));
            }
        }

        candidate_tags.truncate(candidate_cap);

        if candidate_tags.is_empty() {
            // Fallback: nothing dated and nothing sha-probable — try the
            // `latest` tag with today's date as a synthetic placeholder.
            // Last resort for images that only ship :latest with no history.
            let latest_tag = "latest";
            if tag_resp.tags.contains(&latest_tag.to_string()) {
                let today = Utc::now().date_naive();
                let url = format!(
                    "https://{}/v2/{}/{}/manifests/{}",
                    self.registry, self.org, self.image, latest_tag
                );
                let full_ref = format!(
                    "{}/{}/{}:{}",
                    self.registry, self.org, self.image, latest_tag
                );
                if let Some(version) = fetch_version(&client, &url, &token, today, full_ref).await {
                    return Ok(vec![version]);
                }
            }
            return Err(RegistryError::NoTags(self.stream.clone()));
        }

        // Fetch manifests concurrently with a limit of 12 — significantly
        // faster than sequential chunking because slow manifests don't block
        // the entire batch.
        let registry = self.registry.clone();
        let org = self.org.clone();
        let image = self.image.clone();
        let concurrency = 12;
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));

        let futs: Vec<_> = candidate_tags
            .into_iter()
            .map(|(date, tag)| {
                let url = format!(
                    "https://{}/v2/{}/{}/manifests/{}",
                    registry, org, image, tag
                );
                let full_ref = format!("{}/{}/{}:{}", registry, org, image, tag);
                let client = client.clone();
                let token = token.clone();
                let permit = semaphore.clone();
                async move {
                    let _permit = permit.acquire().await.ok();
                    fetch_version(&client, &url, &token, date, full_ref).await
                }
            })
            .collect();

        let mut versions: Vec<ImageVersion> = futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect();

        versions.sort_by_key(|v| v.date);
        save_registry_cache(&cache_record_key, &versions);
        Ok(versions)
    }

    /// For each sha-tagged manifest in `tags`, read the config blob's
    /// `created` timestamp and pair it with the tag. Used as a fallback when
    /// tag names don't carry a date — dakota-nvidia publishes via 40-hex
    /// commit shas, so the config blob is the only reliable date source.
    ///
    /// Two HTTP calls per tag (manifest GET + config blob GET), bounded by
    /// an 8-way semaphore so we don't fan out 60 calls against ghcr.io at
    /// once. Probes that fail (network, missing config blob, unparseable
    /// `created`) are silently dropped — the caller treats the result set
    /// as best-effort.
    async fn probe_sha_tag_dates(
        &self,
        tags: &[String],
        token: &str,
        client: &reqwest::Client,
    ) -> Vec<(NaiveDate, String)> {
        let sema = std::sync::Arc::new(tokio::sync::Semaphore::new(8));
        let futs: Vec<_> = tags
            .iter()
            .map(|tag| {
                let client = client.clone();
                let registry = self.registry.clone();
                let org = self.org.clone();
                let image = self.image.clone();
                let token = token.to_string();
                let tag = tag.clone();
                let sema = sema.clone();
                async move {
                    let _permit = sema.acquire().await.ok();
                    probe_config_created(&client, &registry, &org, &image, &tag, &token)
                        .await
                        .map(|date| (date, tag))
                }
            })
            .collect();
        futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect()
    }

    /// Return the tags available for this image, organised for the tag selector:
    /// - non-dated "stream/channel" tags first (e.g. `latest`, `gts`) — display == raw
    /// - dated tags for this stream, newest-first (e.g. `latest-20260527`) — display == raw
    /// - sha-tagged manifests (dakota-nvidia and similar) — display becomes
    ///   `"Build YYYY-MM-DD"` derived from the config-blob `created` field;
    ///   raw stays as the sha-hex string so `bootc switch` can use it.
    ///
    /// The sha branch reuses [`Self::probe_sha_tag_dates`], which is what
    /// [`Self::fetch_versions`] also calls — same date source, same 8-way
    /// concurrency cap. Bounded by `tag_cap` total entries (default 30) so
    /// the dropdown stays manageable on long-lived registries.
    pub async fn fetch_available_tags(&self) -> Result<Vec<AvailableTag>, RegistryError> {
        let cache_record_key = cache_key(
            &self.registry,
            &self.org,
            &self.image,
            &self.stream,
            "available_tags",
        );
        if let Some(cached) = load_registry_cache::<Vec<AvailableTag>>(&cache_record_key) {
            tracing::info!(
                "Using cached tags for {}/{}/{}",
                self.registry,
                self.org,
                self.image
            );
            return Ok(cached);
        }

        let token = self.get_token().await?;
        let tags_url = format!(
            "https://{}/v2/{}/{}/tags/list?n=1000",
            self.registry, self.org, self.image
        );
        let tag_resp: TagListResponse = self
            .client
            .get(&tags_url)
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;

        let mut stream_tags: Vec<String> = Vec::new();
        let mut dated: Vec<(NaiveDate, String)> = Vec::new();
        let mut sha_tags: Vec<String> = Vec::new();

        for tag in &tag_resp.tags {
            if tag.starts_with("sha256:") {
                // OCI digest reference (registry-internal) — skip; the sha-
                // hex commit tags below are the actual identifiers we want.
                continue;
            }
            if is_sha_only_tag(tag) {
                sha_tags.push(tag.clone());
                continue;
            }
            if let Some(date) = parse_dated_tag(tag, &self.stream) {
                dated.push((date, tag.clone()));
            } else if strip_date_suffix(tag).is_none() {
                stream_tags.push(tag.clone());
            }
        }

        // Probe sha tags for build dates so the dropdown shows readable
        // labels instead of 40-char hashes. Probe a much larger sample
        // (all available) so we can sort by date and select the newest ones.
        // Older truncation at probe time would miss recent daily builds when
        // the registry returns tags in unspecified order (e.g., Dakota's
        // February tags could occupy the first N positions).
        const SHA_PROBE_CAP: usize = 500;
        let probe_list = if sha_tags.len() > SHA_PROBE_CAP {
            sha_tags[..SHA_PROBE_CAP].to_vec()
        } else {
            sha_tags.clone()
        };
        let mut dated_sha: Vec<(NaiveDate, String)> = if probe_list.is_empty() {
            Vec::new()
        } else {
            let client = self.client.clone();
            self.probe_sha_tag_dates(&probe_list, &token, &client).await
        };

        // Sort by date to identify the newest builds, regardless of what
        // order the registry returned them in.
        dated_sha.sort_by(|a, b| b.0.cmp(&a.0));

        stream_tags.sort();
        dated.sort_by(|a, b| b.0.cmp(&a.0));

        let mut result: Vec<AvailableTag> = Vec::new();
        for t in stream_tags {
            result.push(AvailableTag {
                display: t.clone(),
                raw: t,
            });
        }
        for (_date, t) in dated.into_iter().take(30) {
            result.push(AvailableTag {
                display: t.clone(),
                raw: t,
            });
        }
        for (date, sha) in dated_sha {
            result.push(AvailableTag {
                display: format!("Build {}", date.format("%Y-%m-%d")),
                raw: sha,
            });
        }
        save_registry_cache(&cache_record_key, &result);
        Ok(result)
    }

    async fn get_token(&self) -> Result<String, RegistryError> {
        let url = format!(
            "https://{}/token?scope=repository:{}/{}:pull&service={}",
            self.registry, self.org, self.image, self.registry
        );
        let resp: TokenResponse = self.client.get(&url).send().await?.json().await?;
        Ok(resp.token)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Fetch one manifest and extract `ImageVersion` from OCI annotations.
async fn fetch_version(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    date: NaiveDate,
    full_ref: String,
) -> Option<ImageVersion> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .ok()?;

    let manifest: ManifestResponse = resp.json().await.ok()?;
    // Older / docker-v2 manifests (e.g. ucore's stable-zfs-* tags) have no
    // OCI annotations. Treat that as "no metadata" rather than "skip this
    // version" — we still know the date and ref, which is enough for the
    // history list to render and for rollback targeting to work.
    let ann = manifest.annotations.unwrap_or_default();

    let version = ann
        .get("org.opencontainers.image.version")
        .cloned()
        .unwrap_or_else(|| date.format("%Y%m%d").to_string());

    let kernel = ann.get("ostree.linux").cloned().unwrap_or_default();

    let revision = ann
        .get("org.opencontainers.image.revision")
        .map(|r| r.chars().take(8).collect())
        .unwrap_or_default();

    let created = ann
        .get("org.opencontainers.image.created")
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| date.and_hms_opt(0, 0, 0).unwrap().and_utc());

    Some(ImageVersion {
        date,
        full_ref,
        version,
        kernel,
        revision,
        created,
    })
}

/// True for tags that look like a 40-char lowercase commit sha — the form
/// dakota-nvidia and many CI-driven images use exclusively.
fn is_sha_only_tag(tag: &str) -> bool {
    tag.len() == 40
        && tag
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Probe a single tag's config blob for its `created` timestamp.
///
/// Returns the date in UTC (truncated to a NaiveDate to match the rest of
/// the version-list flow) or None on any failure: network error, an OCI
/// index (multi-arch) where we can't single-out a platform manifest, a
/// missing config digest, missing `created` field, or an unparseable
/// RFC3339 timestamp. The caller treats None as "don't include this tag."
///
/// Used by `RegistryClient::probe_sha_tag_dates` to surface history for
/// images that publish via sha-tagged manifests without dated tag names.
async fn probe_config_created(
    client: &reqwest::Client,
    registry: &str,
    org: &str,
    image: &str,
    tag: &str,
    token: &str,
) -> Option<NaiveDate> {
    let manifest_url = format!("https://{registry}/v2/{org}/{image}/manifests/{tag}");
    let resp = client
        .get(&manifest_url)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let manifest: serde_json::Value = resp.json().await.ok()?;
    let config_digest = manifest
        .get("config")
        .and_then(|c| c.get("digest"))
        .and_then(|d| d.as_str())?;

    let blob_url = format!("https://{registry}/v2/{org}/{image}/blobs/{config_digest}");
    let config: serde_json::Value = client
        .get(&blob_url)
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let created_str = config.get("created").and_then(|v| v.as_str())?;
    let dt = DateTime::parse_from_rfc3339(created_str).ok()?;
    Some(dt.with_timezone(&Utc).date_naive())
}

/// Build a shared reqwest client with a reasonable timeout.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .unwrap_or_default()
}

/// Parse a full OCI ref like `ghcr.io/ublue-os/bluefin:stable-daily-43.20260222`
/// into a `RegistryClient` for that stream.
fn parse_image_ref(image_ref: &str) -> Option<RegistryClient> {
    // Format: registry/org/image:stream.date  OR  registry/org/image:stream-date
    let (without_tag, tag) = image_ref.rsplit_once(':')?;
    let parts: Vec<&str> = without_tag.splitn(3, '/').collect();
    if parts.len() < 3 {
        return None;
    }
    let (registry, org, image) = (parts[0], parts[1], parts[2]);

    // Strip the date suffix from the tag to get the stream prefix.
    let stream = strip_date_suffix(tag)?;

    Some(RegistryClient::new(registry, org, image, &stream))
}

/// Extract a `NaiveDate` from a dated image tag, accepting the four conventions
/// observed across the bootc image families we support:
///
/// 1. **Stream-suffixed** (Bluefin, Aurora):
///    `stable-daily-43.20260222`, `lts-hwe-20260224`, `latest.20260527`
///    → accepted for stream `"stable"`, `"lts"`, `"latest"` respectively (via
///      prefix match — see the prefix rule below).
///
/// 2. **Sub-revisioned** (Bazzite):
///    `testing-43.20260308.1`, `stable-43.20260301.2`
///    → trailing `.N` (1–4 digits) is treated as a build sub-revision and
///      stripped before the date extraction.
///
/// 3. **Stream-prefix match** (Bluefin, Aurora, Bazzite):
///    A tag like `stable-daily-43.20260527` is accepted when the caller asks
///    for stream `"stable"` — the prefix begins with `"stable-"`. This lets
///    callers ask for the broad channel ("stable") and get back any tagged
///    build in that family, regardless of the fully-qualified stream
///    (e.g. `stable-daily-43`, `stable-gts-42`).
///
/// 4. **Bare date** (Dakota):
///    `20260114` — 8 digits, no prefix. Accepted only when the caller asks
///    for stream `"latest"` or `""` (the implicit / pointer-tag streams).
///
/// Returns the parsed calendar date, or `None` if the tag doesn't match any
/// of these patterns or fails calendar validation (e.g. month 13).
fn parse_dated_tag(tag: &str, stream: &str) -> Option<NaiveDate> {
    // (4) Bare YYYYMMDD with no separator — accepted only for the implicit
    //     streams that don't qualify their dates.
    if (stream == "latest" || stream.is_empty())
        && tag.len() == 8
        && tag.chars().all(|c| c.is_ascii_digit())
    {
        return NaiveDate::parse_from_str(tag, "%Y%m%d").ok();
    }

    // (2) Strip an optional trailing build sub-revision `.N` (1-4 digits) so
    //     `testing-43.20260308.1` reduces to `testing-43.20260308`.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len()) && suffix.chars().all(|c| c.is_ascii_digit()) {
            // Only strip if doing so leaves a date-shaped tail. Otherwise
            // we'd corrupt something like `stable.20260527` (where the `.`
            // is the date separator, not a sub-revision).
            let candidate = &tag[..idx];
            if candidate.len() >= 8
                && candidate[candidate.len() - 8..]
                    .chars()
                    .all(|c| c.is_ascii_digit())
            {
                candidate
            } else {
                tag
            }
        } else {
            tag
        }
    } else {
        tag
    };

    // (1)/(3) Find a trailing `-YYYYMMDD` or `.YYYYMMDD` on `base`, then
    //         check the prefix matches the requested stream.
    for sep in ['.', '-'] {
        if let Some(idx) = base.rfind(sep) {
            let date_str = &base[idx + 1..];
            if date_str.len() != 8 || !date_str.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let prefix = &base[..idx];

            // Stream match rule: prefix is exactly `stream`, or begins with
            // `stream.` / `stream-` (qualified channel: stable-daily-43 etc.).
            let stream_matches = prefix == stream
                || prefix.starts_with(&format!("{}.", stream))
                || prefix.starts_with(&format!("{}-", stream));
            if !stream_matches {
                continue;
            }

            if let Some(date) = NaiveDate::parse_from_str(date_str, "%Y%m%d").ok() {
                return Some(date);
            }
        }
    }

    None
}

/// Remove the trailing `.YYYYMMDD[.N]` or `-YYYYMMDD[.N]` from a tag to get the
/// fully-qualified stream prefix.
///
/// Examples:
///   `stable-daily-43.20260527`     → `Some("stable-daily-43")`
///   `testing-43.20260308.1`        → `Some("testing-43")`   (sub-revision stripped)
///   `lts-hwe-20260224`             → `Some("lts-hwe")`
///   `latest`                       → `None`                  (no date)
///   `20260114`                     → `None`                  (no stream embedded)
pub fn strip_date_suffix(tag: &str) -> Option<String> {
    // Strip optional trailing sub-revision `.N` (1-4 digits) before looking
    // for the date — matches the Bazzite convention.
    let base = if let Some(idx) = tag.rfind('.') {
        let suffix = &tag[idx + 1..];
        if (1..=4).contains(&suffix.len())
            && suffix.chars().all(|c| c.is_ascii_digit())
            // Only strip when what's left ends in 8 digits — otherwise we'd
            // turn `stable.20260527` into `stable.20260527` again incorrectly.
            && idx >= 8
            && tag[..idx].as_bytes()[idx - 8..idx].iter().all(|b| b.is_ascii_digit())
        {
            &tag[..idx]
        } else {
            tag
        }
    } else {
        tag
    };

    for sep in ['.', '-'] {
        if let Some(pos) = base.rfind(sep) {
            let suffix = &base[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(base[..pos].to_string());
            }
        }
    }
    None
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RegistryCacheRecord<T> {
    timestamp: u64,
    data: T,
}

fn registry_cache_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join(".cache")
        });
    base.join("finupdate").join("registry-cache")
}

fn cache_key(registry: &str, org: &str, image: &str, stream: &str, suffix: &str) -> String {
    // Version 3: Fixed sha-tag sampling to use stride-spread instead of head-slice
    // (June 2026). GHCR returns tags alphabetically so head-slice only probed
    // sha hashes starting 0x00-0x3f and missed all recent builds. Bump
    // invalidates caches built with the old alphabetically-biased probe list.
    let version = "v3";
    format!(
        "{}_{}_{}_{}_{}_{}",
        version, registry, org, image, stream, suffix
    )
    .replace('/', "_")
    .replace(':', "_")
}

fn load_registry_cache<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
    let path = registry_cache_dir().join(key);
    let data = std::fs::read(path).ok()?;
    let record: RegistryCacheRecord<T> = serde_json::from_slice(&data).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now >= record.timestamp && now - record.timestamp < 3600 {
        Some(record.data)
    } else {
        None
    }
}

fn save_registry_cache<T: serde::Serialize>(key: &str, data: &T) {
    let dir = registry_cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let record = RegistryCacheRecord {
        timestamp: now,
        data,
    };
    if let Ok(serialized) = serde_json::to_vec(&record) {
        let _ = std::fs::write(dir.join(key), serialized);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_sha_only_tag ──────────────────────────────────────────────────

    #[test]
    fn is_sha_only_tag_accepts_40_hex() {
        assert!(is_sha_only_tag("fc308c8515de8b2f134bc0cbe756cc738c4870e1"));
    }

    #[test]
    fn is_sha_only_tag_rejects_short_sha() {
        assert!(!is_sha_only_tag("fc308c8"));
    }

    #[test]
    fn is_sha_only_tag_rejects_long_sha() {
        assert!(!is_sha_only_tag(&"a".repeat(41)));
    }

    #[test]
    fn is_sha_only_tag_rejects_uppercase() {
        // GHCR commit shas are lowercase hex; reject uppercase to avoid
        // false matches on whatever else might sneak into a tag list.
        assert!(!is_sha_only_tag("FC308C8515DE8B2F134BC0CBE756CC738C4870E1"));
    }

    #[test]
    fn is_sha_only_tag_rejects_non_hex() {
        // 40 chars but with a 'g' — not hex.
        let mut t = "g".to_string();
        t.push_str(&"0".repeat(39));
        assert!(!is_sha_only_tag(&t));
    }

    #[test]
    fn is_sha_only_tag_rejects_dated_form() {
        assert!(!is_sha_only_tag("stable-daily-43.20260222"));
    }

    // ── strip_date_suffix ────────────────────────────────────────────────

    #[test]
    fn strip_date_suffix_dot_form() {
        assert_eq!(
            strip_date_suffix("stable-daily-43.20260222"),
            Some("stable-daily-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_dash_form() {
        assert_eq!(
            strip_date_suffix("stable-daily-43-20260222"),
            Some("stable-daily-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_rejects_non_date_suffix() {
        assert_eq!(strip_date_suffix("latest"), None);
        assert_eq!(strip_date_suffix("stable-daily"), None);
        assert_eq!(strip_date_suffix("stable.notadate"), None);
    }

    #[test]
    fn strip_date_suffix_rejects_wrong_length() {
        assert_eq!(strip_date_suffix("stream-1234567"), None); // 7 digits
        assert_eq!(strip_date_suffix("stream-123456789"), None); // 9 digits
    }

    #[test]
    fn strip_date_suffix_rejects_non_digit_chars() {
        assert_eq!(strip_date_suffix("stream-2026022x"), None);
    }

    // ── parse_dated_tag ──────────────────────────────────────────────────

    #[test]
    fn parse_dated_tag_dot_separator() {
        let d = parse_dated_tag("stable-daily-43.20260222", "stable-daily-43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    #[test]
    fn parse_dated_tag_dash_separator() {
        let d = parse_dated_tag("stable-daily-43-20260222", "stable-daily-43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    #[test]
    fn parse_dated_tag_rejects_unrelated_tag() {
        assert!(parse_dated_tag("latest", "stable-daily-43").is_none());
        assert!(parse_dated_tag("dev-daily-20260222", "stable-daily").is_none());
    }

    #[test]
    fn parse_dated_tag_rejects_invalid_calendar_date() {
        // 2026-02-30 isn't a real date.
        assert!(parse_dated_tag("stable.20260230", "stable").is_none());
    }

    // ── parse_dated_tag: real-world per-family tag formats ─────────────────
    // Samples below are real tags pulled from GHCR on 2026-05-29 — see the
    // queries in the bring-up plan. Update if the upstream conventions change.

    /// Bluefin: `stable-daily-43.20260222` for stream `"stable"` (prefix match).
    #[test]
    fn parse_dated_tag_bluefin_stable_daily_dot() {
        let d = parse_dated_tag("stable-daily-43.20260222", "stable").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    /// Bluefin: `43-43.20260222` for stream `"43"` (exact prefix match).
    #[test]
    fn parse_dated_tag_bluefin_version_qualified_dot() {
        let d = parse_dated_tag("43-43.20260222", "43").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 22).unwrap());
    }

    /// Bluefin LTS: `lts-hwe.20260224` for stream `"lts"` (prefix match).
    #[test]
    fn parse_dated_tag_bluefin_lts_hwe_dot() {
        let d = parse_dated_tag("lts-hwe.20260224", "lts").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 24).unwrap());
    }

    /// Bluefin LTS, dash variant: `lts-hwe-20260224` for stream `"lts"`.
    #[test]
    fn parse_dated_tag_bluefin_lts_hwe_dash() {
        let d = parse_dated_tag("lts-hwe-20260224", "lts").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 24).unwrap());
    }

    /// Bazzite: `testing-43.20260308.1` — sub-revision is stripped before
    /// extracting the date.
    #[test]
    fn parse_dated_tag_bazzite_sub_revision() {
        let d = parse_dated_tag("testing-43.20260308.1", "testing").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 3, 8).unwrap());
    }

    /// Bazzite: `testing-43.20260301` without sub-revision still works.
    #[test]
    fn parse_dated_tag_bazzite_no_sub_revision() {
        let d = parse_dated_tag("testing-43.20260301", "testing").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap());
    }

    /// Dakota: `latest.20260114` for stream `"latest"` (exact prefix).
    #[test]
    fn parse_dated_tag_dakota_latest_dot() {
        let d = parse_dated_tag("latest.20260114", "latest").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Dakota: bare `20260114` accepted when stream is "latest" (implicit).
    #[test]
    fn parse_dated_tag_dakota_bare_date() {
        let d = parse_dated_tag("20260114", "latest").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Bare date is also accepted when stream is empty (no qualifier).
    #[test]
    fn parse_dated_tag_bare_date_empty_stream() {
        let d = parse_dated_tag("20260114", "").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 1, 14).unwrap());
    }

    /// Bare date is REJECTED when stream is anything else: a tag like
    /// `20260114` doesn't belong in stream `"stable"` results.
    #[test]
    fn parse_dated_tag_bare_date_rejected_for_qualified_stream() {
        assert!(parse_dated_tag("20260114", "stable").is_none());
    }

    /// Cross-family contamination: a `gts-*` tag must not appear in `stable`
    /// results even if the date is valid.
    #[test]
    fn parse_dated_tag_rejects_other_family() {
        assert!(parse_dated_tag("gts-daily-42.20260527", "stable").is_none());
    }

    /// Sub-revision must be 1–4 digits; `testing-43.20260308.55555` would be
    /// a malformed tag.
    #[test]
    fn parse_dated_tag_rejects_long_sub_revision() {
        assert!(parse_dated_tag("testing-43.20260308.55555", "testing").is_none());
    }

    /// `stable.20260527` — the `.20260527` is the date separator, not a
    /// sub-revision. The sub-revision stripper must not over-fire here.
    #[test]
    fn parse_dated_tag_does_not_strip_date_as_sub_revision() {
        let d = parse_dated_tag("stable.20260527", "stable").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 5, 27).unwrap());
    }

    // ── strip_date_suffix: sub-revisions and bare dates ─────────────────

    #[test]
    fn strip_date_suffix_strips_sub_revision() {
        assert_eq!(
            strip_date_suffix("testing-43.20260308.1"),
            Some("testing-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_bare_date_returns_none() {
        // Bare date has no stream prefix to return.
        assert_eq!(strip_date_suffix("20260114"), None);
    }

    // ── Family taxonomy disambiguation ──────────────────────────────────

    #[test]
    fn family_best_match_disambiguates_bluefin_stable_vs_lts_by_stream() {
        // The image `ublue-os/bluefin` belongs to both Bluefin Stable and
        // Bluefin LTS. The stream picks which family the user is on.
        let stable = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        assert_eq!(stable.name, "Bluefin Stable");

        let lts = Family::best_match("ublue-os", "bluefin", "lts").unwrap();
        assert_eq!(lts.name, "Bluefin LTS");

        let lts_hwe = Family::best_match("ublue-os", "bluefin", "lts-hwe").unwrap();
        assert_eq!(lts_hwe.name, "Bluefin LTS");
    }

    #[test]
    fn family_best_match_falls_back_to_first_when_stream_unknown() {
        // Unknown stream → first family containing the image wins.
        let f = Family::best_match("ublue-os", "bluefin", "moonshot-fictional").unwrap();
        // Bluefin Stable is declared first in KNOWN_FAMILIES.
        assert_eq!(f.name, "Bluefin Stable");
    }

    #[test]
    fn family_best_match_finds_aurora_by_image_alone() {
        let f = Family::best_match("ublue-os", "aurora", "stable").unwrap();
        assert_eq!(f.name, "Aurora");
        assert!(f.images.contains(&"aurora-nvidia"));
    }

    #[test]
    fn family_best_match_finds_bazzite_gnome_separately_from_kde() {
        let kde = Family::best_match("ublue-os", "bazzite", "stable").unwrap();
        assert_eq!(kde.name, "Bazzite KDE");

        let gnome = Family::best_match("ublue-os", "bazzite-gnome", "stable").unwrap();
        assert_eq!(gnome.name, "Bazzite GNOME");
    }

    #[test]
    fn family_best_match_returns_none_for_unknown_image() {
        assert!(Family::best_match("ublue-os", "totally-fake-image", "stable").is_none());
    }

    // ── Family feature switches ─────────────────────────────────────────

    #[test]
    fn family_base_image_is_first_in_list() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        assert_eq!(bluefin.base_image(), "bluefin");
        let dakota = Family::best_match("projectbluefin", "dakota", "latest").unwrap();
        assert_eq!(dakota.base_image(), "dakota");
    }

    #[test]
    fn family_available_features_lists_atomic_suffixes() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        let feats = bluefin.available_features();
        // From images like bluefin-nvidia / bluefin-nvidia-open / bluefin-dx /
        // bluefin-dx-nvidia / bluefin-dx-nvidia-open / bluefin-asus / etc.
        assert!(feats.contains(&"nvidia"));
        assert!(feats.contains(&"open"));
        assert!(feats.contains(&"dx"));
        assert!(feats.contains(&"asus"));
        assert!(feats.contains(&"surface"));
        assert!(feats.contains(&"framework"));
        // Alphabetical for stable UI rendering.
        let mut sorted = feats.clone();
        sorted.sort();
        assert_eq!(feats, sorted);
    }

    #[test]
    fn family_select_image_for_features_resolves_combinations() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();

        // Empty features → base.
        assert_eq!(bluefin.select_image_for_features(&[]), Some("bluefin"));
        // Single feature.
        assert_eq!(
            bluefin.select_image_for_features(&["nvidia"]),
            Some("bluefin-nvidia")
        );
        assert_eq!(
            bluefin.select_image_for_features(&["dx"]),
            Some("bluefin-dx")
        );
        // Two features, order-independent.
        assert_eq!(
            bluefin.select_image_for_features(&["dx", "nvidia"]),
            Some("bluefin-dx-nvidia")
        );
        assert_eq!(
            bluefin.select_image_for_features(&["nvidia", "dx"]),
            Some("bluefin-dx-nvidia")
        );
        // Three features — Bluefin Stable ships bluefin-dx-nvidia-open.
        assert_eq!(
            bluefin.select_image_for_features(&["dx", "nvidia", "open"]),
            Some("bluefin-dx-nvidia-open")
        );
    }

    #[test]
    fn family_select_image_for_features_returns_none_for_invalid_combo() {
        let bluefin = Family::best_match("ublue-os", "bluefin", "stable").unwrap();
        // "open" alone (without nvidia) doesn't map to a published image.
        assert!(bluefin.select_image_for_features(&["open"]).is_none());
        // "dx" + "framework" isn't a real combination.
        assert!(
            bluefin
                .select_image_for_features(&["dx", "framework"])
                .is_none()
        );
    }

    #[test]
    fn family_select_image_for_dakota_features() {
        let dakota = Family::best_match("projectbluefin", "dakota", "latest").unwrap();
        assert_eq!(dakota.select_image_for_features(&[]), Some("dakota"));
        assert_eq!(
            dakota.select_image_for_features(&["nvidia"]),
            Some("dakota-nvidia")
        );

        let dakota_testing = Family::best_match("projectbluefin", "dakota", "testing").unwrap();
        assert_eq!(dakota_testing.name, "Bluefin Dakota");
    }

    #[test]
    fn family_all_for_image_returns_both_bluefin_families() {
        let families = Family::all_for_image("ublue-os", "bluefin");
        let names: Vec<&str> = families.iter().map(|f| f.name).collect();
        assert!(names.contains(&"Bluefin Stable"));
        assert!(names.contains(&"Bluefin LTS"));
    }

    #[test]
    fn strip_date_suffix_does_not_strip_non_date_as_sub_revision() {
        // `stable.20260527` is `stream.date`, not `stream.sub-revision`.
        assert_eq!(
            strip_date_suffix("stable.20260527"),
            Some("stable".to_string())
        );
    }

    // ── parse_image_ref ──────────────────────────────────────────────────

    #[test]
    fn parse_image_ref_full_ghcr_with_dot_date() {
        let c = parse_image_ref("ghcr.io/ublue-os/bluefin:stable-daily-43.20260222").unwrap();
        assert_eq!(c.registry(), "ghcr.io");
        assert_eq!(c.org(), "ublue-os");
        assert_eq!(c.image(), "bluefin");
        assert_eq!(c.stream, "stable-daily-43");
    }

    #[test]
    fn parse_image_ref_full_ghcr_with_dash_date() {
        let c = parse_image_ref("ghcr.io/projectbluefin/dakota:latest-20260527").unwrap();
        assert_eq!(c.stream, "latest");

        let c2 = parse_image_ref("ghcr.io/projectbluefin/dakota:testing-20260527").unwrap();
        assert_eq!(c2.stream, "testing");
    }

    #[test]
    fn parse_image_ref_rejects_missing_org_or_image() {
        assert!(parse_image_ref("ghcr.io:tag").is_none()); // no slashes
        assert!(parse_image_ref("ghcr.io/org:tag").is_none()); // only 2 parts
    }

    #[test]
    fn parse_image_ref_rejects_tag_without_date() {
        assert!(parse_image_ref("ghcr.io/ublue-os/bluefin:latest").is_none());
    }

    #[test]
    fn parse_image_ref_handles_nested_image_path() {
        // Some registries use multi-segment image paths.
        let c =
            parse_image_ref("ghcr.io/ublue-os/bluefin-dx/extras:stable-daily.20260222").unwrap();
        assert_eq!(c.image(), "bluefin-dx/extras");
    }

    #[test]
    fn test_registry_cache_roundtrip() {
        let key = "test_cache_key_xyz";
        let tags = vec![
            AvailableTag {
                display: "Latest Build".to_string(),
                raw: "latest".to_string(),
            },
            AvailableTag {
                display: "Stable".to_string(),
                raw: "stable".to_string(),
            },
        ];

        let path = registry_cache_dir().join(key);
        let _ = std::fs::remove_file(&path);

        assert!(load_registry_cache::<Vec<AvailableTag>>(key).is_none());

        save_registry_cache(key, &tags);

        let loaded = load_registry_cache::<Vec<AvailableTag>>(key).unwrap();
        assert_eq!(loaded, tags);

        let _ = std::fs::remove_file(&path);
    }
}
