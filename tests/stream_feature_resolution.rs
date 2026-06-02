//! Tests for stream + feature resolution logic in the service layer.
//!
//! Validates that:
//! 1. FamilyInfo includes available streams from KNOWN_FAMILIES
//! 2. resolve_target_with_stream correctly handles streams
//! 3. Feature + stream combinations resolve to proper image refs

#[cfg(test)]
mod stream_feature_tests {
    use finupdate::service::{ImageRef, UpdaterService, BootcUpdaterService, FamilyInfo, Feature};
    use std::sync::Arc;

    fn create_test_service() -> Arc<dyn UpdaterService> {
        BootcUpdaterService::new()
    }

    fn create_bluefin_stable() -> FamilyInfo {
        FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![
                Feature {
                    id: "nvidia".to_string(),
                    display_name: "NVIDIA GPU".to_string(),
                    subtitle: "For NVIDIA graphics cards".to_string(),
                },
                Feature {
                    id: "dx".to_string(),
                    display_name: "DX Variant".to_string(),
                    subtitle: "Developer experience variant".to_string(),
                },
            ],
            streams: vec![
                "latest".to_string(),
                "stable".to_string(),
                "stable-daily".to_string(),
                "beta".to_string(),
            ],
        }
    }

    fn create_bluefin_lts() -> FamilyInfo {
        FamilyInfo {
            name: "Bluefin LTS".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![
                Feature {
                    id: "nvidia".to_string(),
                    display_name: "NVIDIA GPU".to_string(),
                    subtitle: "For NVIDIA graphics cards".to_string(),
                },
                Feature {
                    id: "dx".to_string(),
                    display_name: "DX Variant".to_string(),
                    subtitle: "Developer experience variant".to_string(),
                },
            ],
            streams: vec!["lts".to_string(), "lts-hwe".to_string()],
        }
    }

    fn create_dakota() -> FamilyInfo {
        FamilyInfo {
            name: "Bluefin Dakota".to_string(),
            base_image: "dakota".to_string(),
            features: vec![Feature {
                id: "nvidia".to_string(),
                display_name: "NVIDIA GPU".to_string(),
                subtitle: "For NVIDIA graphics cards".to_string(),
            }],
            streams: vec!["latest".to_string(), "testing".to_string()],
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Basic stream resolution
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_base_image_with_default_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();

        let target = svc.resolve_target(&family, &[]);
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin");
        assert_eq!(target.tag, "latest"); // default is first stream
    }

    #[test]
    fn resolve_base_image_with_explicit_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();

        let target = svc.resolve_target_with_stream(&family, &[], "stable");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin");
        assert_eq!(target.tag, "stable");
    }

    #[test]
    fn resolve_nvidia_with_latest_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();
        let features = vec!["nvidia".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "latest");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin-nvidia");
        assert_eq!(target.tag, "latest");
    }

    #[test]
    fn resolve_nvidia_with_stable_daily_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();
        let features = vec!["nvidia".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "stable-daily");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin-nvidia");
        assert_eq!(target.tag, "stable-daily");
    }

    #[test]
    fn resolve_dx_with_beta_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();
        let features = vec!["dx".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "beta");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin-dx");
        assert_eq!(target.tag, "beta");
    }

    #[test]
    fn resolve_dx_nvidia_combo_with_latest_stream() {
        let svc = create_test_service();
        let family = create_bluefin_stable();
        let features = vec!["dx".to_string(), "nvidia".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "latest");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin-dx-nvidia");
        assert_eq!(target.tag, "latest");
    }

    // ─────────────────────────────────────────────────────────────────────
    // LTS-specific streams (lts, lts-hwe)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_lts_base_with_lts_stream() {
        let svc = create_test_service();
        let family = create_bluefin_lts();

        let target = svc.resolve_target_with_stream(&family, &[], "lts");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin");
        assert_eq!(target.tag, "lts");
    }

    #[test]
    fn resolve_lts_nvidia_with_hwe_stream() {
        let svc = create_test_service();
        let family = create_bluefin_lts();
        let features = vec!["nvidia".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "lts-hwe");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "bluefin-nvidia");
        assert_eq!(target.tag, "lts-hwe");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Dakota: Limited variants
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_dakota_base_with_latest_stream() {
        let svc = create_test_service();
        let family = create_dakota();

        let target = svc.resolve_target_with_stream(&family, &[], "latest");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "dakota");
        assert_eq!(target.tag, "latest");
    }

    #[test]
    fn resolve_dakota_nvidia_with_testing_stream() {
        let svc = create_test_service();
        let family = create_dakota();
        let features = vec!["nvidia".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "testing");
        assert!(target.is_some());

        let target = target.unwrap();
        assert_eq!(target.image, "dakota-nvidia");
        assert_eq!(target.tag, "testing");
    }

    #[test]
    fn dakota_rejects_dx_variant() {
        let svc = create_test_service();
        let family = create_dakota();
        let features = vec!["dx".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "latest");
        assert!(target.is_none(), "Dakota should not have dx variant");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Invalid feature combinations
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn reject_invalid_feature_combination() {
        let svc = create_test_service();
        let family = create_bluefin_stable();
        let features = vec!["nonexistent".to_string()];

        let target = svc.resolve_target_with_stream(&family, &features, "latest");
        assert!(target.is_none(), "Should reject nonexistent feature");
    }

    #[test]
    fn reject_unknown_family() {
        let svc = create_test_service();
        let mut family = create_bluefin_stable();
        family.name = "Unknown Family".to_string(); // This family doesn't exist

        let target = svc.resolve_target(&family, &[]);
        assert!(target.is_none(), "Should reject unknown family");
    }

    // ─────────────────────────────────────────────────────────────────────
    // Stream information in FamilyInfo
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bluefin_stable_has_multiple_streams() {
        let family = create_bluefin_stable();
        assert!(!family.streams.is_empty());
        assert!(family.streams.contains(&"latest".to_string()));
        assert!(family.streams.contains(&"stable".to_string()));
        assert!(family.streams.contains(&"stable-daily".to_string()));
    }

    #[test]
    fn bluefin_lts_has_hwe_stream() {
        let family = create_bluefin_lts();
        assert!(family.streams.contains(&"lts".to_string()));
        assert!(family.streams.contains(&"lts-hwe".to_string()));
    }

    #[test]
    fn dakota_has_testing_stream() {
        let family = create_dakota();
        assert!(family.streams.contains(&"latest".to_string()));
        assert!(family.streams.contains(&"testing".to_string()));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Default stream behavior
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn default_stream_is_first_in_list() {
        let svc = create_test_service();
        let family = create_bluefin_stable();

        // resolve_target() without explicit stream should use family's first stream
        let default_target = svc.resolve_target(&family, &[]);
        let explicit_target = svc.resolve_target_with_stream(&family, &[], &family.streams[0]);

        assert_eq!(default_target.unwrap().tag, explicit_target.unwrap().tag);
    }

    #[test]
    fn lts_default_stream_is_lts_not_latest() {
        let svc = create_test_service();
        let family = create_bluefin_lts();

        let target = svc.resolve_target(&family, &[]);
        assert!(target.is_some());
        assert_eq!(target.unwrap().tag, "lts");
    }
}
