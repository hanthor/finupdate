// Used by the GUI binary only — finupdate-cli compiles the module but
// doesn't call into it. Module-level allow to keep the multi-bin
// warnings list manageable.
#![allow(dead_code)]

//! SBOM diff — pure-Rust OCI referrer discovery and SPDX parsing.
//!
//! Replaces the previous `oras` subprocess approach with `oci-client`.
//!
//! ## Flow
//!
//! 1. For each image ref (booted + target), call the OCI Distribution v1.1
//!    referrers API to find a manifest with `artifactType: application/vnd.spdx+json`.
//! 2. Pull the SBOM blob from that referrer manifest.
//! 3. Parse the SPDX 2.3 JSON into a `name → version` map.
//! 4. Cache the map by referrer digest under `$XDG_CACHE_HOME/finupdate/sbom-cache/`.
//! 5. Diff the two maps and return `SbomDiffResult`.
//!
//! GHCR allows anonymous pulls for public images; `oci-client` handles the
//! `WWW-Authenticate: Bearer` token flow automatically.

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

use oci_client::manifest::OciManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use serde::Deserialize;

const SPDX_ARTIFACT_TYPE: &str = "application/vnd.spdx+json";

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PackageDiff {
    pub name: String,
    pub old_version: String,
    pub new_version: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SbomDiffResult {
    pub upgraded: Vec<PackageDiff>,
    pub removed: Vec<String>,
    pub added: Vec<PackageDiff>,
}

// ── Internal SPDX types ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SpdxDocument {
    packages: Option<Vec<SpdxPackage>>,
}

#[derive(Deserialize)]
struct SpdxPackage {
    name: String,
    #[serde(rename = "versionInfo")]
    version_info: Option<String>,
}

fn parse_spdx(bytes: &[u8]) -> Option<HashMap<String, String>> {
    let doc: SpdxDocument = serde_json::from_slice(bytes).ok()?;
    let mut map = HashMap::new();
    for pkg in doc.packages.unwrap_or_default() {
        let ver = pkg.version_info.unwrap_or_else(|| "unknown".to_string());
        map.insert(pkg.name, ver);
    }
    Some(map)
}

// ── Cache helpers ─────────────────────────────────────────────────────────────

fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".cache")
        });
    base.join("finupdate").join("sbom-cache")
}

fn cache_path(digest: &str) -> PathBuf {
    cache_dir().join(digest.replace(':', "_"))
}

fn load_cache(digest: &str) -> Option<HashMap<String, String>> {
    let data = std::fs::read(cache_path(digest)).ok()?;
    serde_json::from_slice(&data).ok()
}

fn save_cache(digest: &str, map: &HashMap<String, String>) {
    let dir = cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(data) = serde_json::to_vec(map) {
        let _ = std::fs::write(cache_path(digest), data);
    }
}

// ── OCI helpers ───────────────────────────────────────────────────────────────

fn make_client() -> Client {
    Client::default()
}

/// Raw OCI image index entry — `oci_client::ImageIndexEntry` doesn't expose
/// `artifactType`, which is what we need to filter referrers by media type.
/// Parsing the raw JSON ourselves is one extra struct vs. round-tripping
/// every entry to check its manifest type.
#[derive(Deserialize)]
struct ReferrersIndex {
    manifests: Vec<ReferrerEntry>,
}
#[derive(Deserialize)]
struct ReferrerEntry {
    digest: String,
    #[serde(rename = "artifactType")]
    artifact_type: Option<String>,
}

/// Find the digest of the SPDX referrer manifest for `image_ref`.
///
/// GHCR returns HTTP 303 (redirect to a non-OCI URL) for the OCI v1.1
/// referrers API endpoint, which `oci_client::pull_referrers` doesn't follow
/// — so that path silently returns no results. We use the spec-defined
/// fallback tag convention instead: `<image>:sha256-<hex-digest>` resolves
/// to an image index whose manifests are the referrers. Filter client-side
/// by `artifactType`. See OCI Distribution Spec §referrers (fallback).
async fn find_spdx_referrer(client: &Client, image_ref: &Reference) -> Option<String> {
    let (_, subject_digest) = client
        .pull_manifest(image_ref, &RegistryAuth::Anonymous)
        .await
        .ok()?;

    tracing::debug!("subject digest for {}: {}", image_ref, subject_digest);

    // Fetch the fallback referrers tag as raw JSON — oci_client's typed
    // ImageIndexEntry strips the artifactType field we need.
    let fallback_tag = subject_digest.replace(':', "-");
    let url = format!(
        "https://{}/v2/{}/manifests/{}",
        image_ref.registry(),
        image_ref.repository(),
        fallback_tag
    );

    let token = ghcr_anonymous_token(image_ref.repository()).await;
    tracing::debug!(
        "referrers fallback: url={} token_present={}",
        url,
        token.is_some()
    );
    let http = reqwest::Client::new();
    let mut req = http
        .get(&url)
        .header("Accept", "application/vnd.oci.image.index.v1+json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("referrers fallback fetch failed: {}", e);
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "referrers fallback tag {} returned HTTP {}",
            fallback_tag,
            resp.status()
        );
        return None;
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("referrers fallback body read failed: {}", e);
            return None;
        }
    };
    let index: ReferrersIndex = match serde_json::from_str(&body) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(
                "referrers fallback JSON parse failed: {} (body head: {})",
                e,
                &body.chars().take(200).collect::<String>()
            );
            return None;
        }
    };
    tracing::debug!(
        "referrers fallback: parsed {} entries",
        index.manifests.len()
    );
    for m in &index.manifests {
        tracing::trace!(
            "  entry: digest={} artifactType={:?}",
            m.digest,
            m.artifact_type
        );
    }

    let found = index
        .manifests
        .into_iter()
        .find(|m| m.artifact_type.as_deref() == Some(SPDX_ARTIFACT_TYPE))
        .map(|m| m.digest);
    if found.is_none() {
        tracing::warn!(
            "no SPDX referrer found in fallback index for {}",
            image_ref
        );
    }
    found
}

/// Mint a short-lived anonymous bearer token for pulling from a public
/// ghcr.io repository. GHCR requires a token even for anonymous reads.
/// Returns None on non-GHCR registries or token-endpoint failure (the
/// caller falls back to unauthenticated requests).
async fn ghcr_anonymous_token(repository: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct TokenResp {
        token: String,
    }
    let url = format!(
        "https://ghcr.io/token?service=ghcr.io&scope=repository:{}:pull",
        repository
    );
    reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .ok()?
        .json::<TokenResp>()
        .await
        .ok()
        .map(|t| t.token)
}

/// Pull the SBOM blob from a referrer manifest digest and parse it into a
/// `name → version` map. Caches the result keyed by referrer digest.
async fn pull_sbom(
    client: &Client,
    image_ref: &Reference,
    referrer_digest: &str,
) -> Option<HashMap<String, String>> {
    if let Some(cached) = load_cache(referrer_digest) {
        tracing::debug!("SBOM cache hit for {}", referrer_digest);
        return Some(cached);
    }

    let referrer_ref = Reference::with_digest(
        image_ref.registry().to_string(),
        image_ref.repository().to_string(),
        referrer_digest.to_string(),
    );

    let (manifest, _) = client
        .pull_manifest(&referrer_ref, &RegistryAuth::Anonymous)
        .await
        .ok()?;

    // The SBOM is the first (and usually only) layer in the referrer manifest.
    let blob_digest = match manifest {
        OciManifest::Image(img) => img.layers.first()?.digest.clone(),
        OciManifest::ImageIndex(_) => return None,
    };

    let mut blob_bytes: Vec<u8> = Vec::new();
    client
        .pull_blob(&referrer_ref, blob_digest.as_str(), &mut blob_bytes)
        .await
        .ok()?;

    let map = parse_spdx(&blob_bytes)?;
    tracing::debug!(
        "parsed {} packages from SBOM {}",
        map.len(),
        referrer_digest
    );
    save_cache(referrer_digest, &map);
    Some(map)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Diff the SBOM packages between `booted_ref` and `target_ref`.
///
/// Both refs are full OCI image references, e.g.
/// `ghcr.io/projectbluefin/dakota:latest` or with a digest suffix.
///
/// Returns `None` if either SBOM cannot be fetched.
pub async fn fetch_and_diff_sboms(
    booted_ref: String,
    target_ref: String,
) -> Option<SbomDiffResult> {
    tracing::info!("SBOM diff: {} -> {}", booted_ref, target_ref);

    let client = make_client();
    let booted = Reference::from_str(&booted_ref).ok()?;
    let target = Reference::from_str(&target_ref).ok()?;

    let (booted_referrer, target_referrer) = tokio::join!(
        find_spdx_referrer(&client, &booted),
        find_spdx_referrer(&client, &target),
    );

    let booted_digest = booted_referrer?;
    let target_digest = target_referrer?;
    tracing::debug!("booted SPDX referrer: {}", booted_digest);
    tracing::debug!("target SPDX referrer: {}", target_digest);

    let (booted_map, target_map) = tokio::join!(
        pull_sbom(&client, &booted, &booted_digest),
        pull_sbom(&client, &target, &target_digest),
    );

    let booted_map = booted_map?;
    let target_map = target_map?;

    tracing::info!(
        "SBOM diff: {} booted packages, {} target packages",
        booted_map.len(),
        target_map.len()
    );

    Some(diff_packages(&booted_map, &target_map))
}

/// Compute the diff between two package maps.
pub fn diff_packages(
    booted_map: &HashMap<String, String>,
    target_map: &HashMap<String, String>,
) -> SbomDiffResult {
    let mut upgraded = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();

    for (name, booted_ver) in booted_map {
        match target_map.get(name) {
            Some(target_ver) if booted_ver != target_ver => {
                upgraded.push(PackageDiff {
                    name: name.clone(),
                    old_version: booted_ver.clone(),
                    new_version: target_ver.clone(),
                });
            }
            Some(_) => {}
            None => removed.push(name.clone()),
        }
    }

    for (name, target_ver) in target_map {
        if !booted_map.contains_key(name) {
            added.push(PackageDiff {
                name: name.clone(),
                old_version: String::new(),
                new_version: target_ver.clone(),
            });
        }
    }

    upgraded.sort_by(|a, b| a.name.cmp(&b.name));
    removed.sort();
    added.sort_by(|a, b| a.name.cmp(&b.name));

    SbomDiffResult {
        upgraded,
        removed,
        added,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn diff_identical_maps_yields_no_changes() {
        let m = map(&[("kernel", "7.0.7"), ("bash", "5.2.32")]);
        let result = diff_packages(&m, &m);
        assert!(result.upgraded.is_empty());
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
    }

    #[test]
    fn diff_detects_version_upgrade() {
        let booted = map(&[("kernel", "7.0.6")]);
        let target = map(&[("kernel", "7.0.7")]);
        let r = diff_packages(&booted, &target);
        assert_eq!(
            r.upgraded,
            vec![PackageDiff {
                name: "kernel".into(),
                old_version: "7.0.6".into(),
                new_version: "7.0.7".into(),
            }]
        );
        assert!(r.added.is_empty());
        assert!(r.removed.is_empty());
    }

    #[test]
    fn diff_detects_added_and_removed_packages() {
        let booted = map(&[("kernel", "7.0.6"), ("old-tool", "1.0")]);
        let target = map(&[("kernel", "7.0.6"), ("new-tool", "2.0")]);
        let r = diff_packages(&booted, &target);
        assert!(r.upgraded.is_empty());
        assert_eq!(r.removed, vec!["old-tool".to_string()]);
        assert_eq!(
            r.added,
            vec![PackageDiff {
                name: "new-tool".into(),
                old_version: String::new(),
                new_version: "2.0".into(),
            }]
        );
    }

    #[test]
    fn diff_outputs_are_sorted_alphabetically() {
        let booted = map(&[("zlib", "1.3"), ("apr", "1.7"), ("middle", "1.0")]);
        let target = map(&[("zlib", "1.4"), ("apr", "1.8"), ("middle", "1.0")]);
        let r = diff_packages(&booted, &target);
        let names: Vec<_> = r.upgraded.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["apr", "zlib"]);
    }

    #[test]
    fn diff_unchanged_package_excluded_from_upgraded() {
        let booted = map(&[("stable", "1.0"), ("changed", "1.0")]);
        let target = map(&[("stable", "1.0"), ("changed", "2.0")]);
        let r = diff_packages(&booted, &target);
        assert_eq!(r.upgraded.len(), 1);
        assert_eq!(r.upgraded[0].name, "changed");
    }

    #[test]
    fn parse_spdx_extracts_name_and_version() {
        let json = br#"{
            "packages": [
                {"name": "kernel", "versionInfo": "7.0.7"},
                {"name": "bash", "versionInfo": "5.2.32"}
            ]
        }"#;
        let m = parse_spdx(json).unwrap();
        assert_eq!(m.get("kernel"), Some(&"7.0.7".to_string()));
        assert_eq!(m.get("bash"), Some(&"5.2.32".to_string()));
    }

    #[test]
    fn parse_spdx_treats_missing_version_as_unknown() {
        let json = br#"{"packages": [{"name": "mystery"}]}"#;
        let m = parse_spdx(json).unwrap();
        assert_eq!(m.get("mystery"), Some(&"unknown".to_string()));
    }

    #[test]
    fn parse_spdx_handles_empty_or_missing_packages() {
        assert_eq!(parse_spdx(br#"{"packages": []}"#).unwrap().len(), 0);
        assert_eq!(parse_spdx(br#"{}"#).unwrap().len(), 0);
    }

    #[test]
    fn parse_spdx_rejects_malformed_json() {
        assert!(parse_spdx(b"not json").is_none());
    }

    #[test]
    fn cache_path_replaces_colons() {
        let p = cache_path("sha256:abc123");
        assert!(p.to_string_lossy().ends_with("sha256_abc123"));
    }
}
