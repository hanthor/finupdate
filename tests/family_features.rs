//! Unit tests for image family variant selection logic.
//!
//! This test suite verifies the Family::select_image_for_features() method
//! to ensure that:
//! 1. Each image family correctly resolves feature requests to available variants
//! 2. Valid feature combinations resolve to published images
//! 3. Invalid combinations are properly rejected
//! 4. Feature toggles (nvidia, dx, hwe, open, etc.) work as expected

#[cfg(test)]
mod family_feature_selection {
    use finupdate::registry_client::{Family, KNOWN_FAMILIES};

    // Helper to find a family by name
    fn find_family(name: &str) -> &'static Family {
        KNOWN_FAMILIES
            .iter()
            .find(|f| f.name == name)
            .expect(&format!("Family {} not found", name))
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bluefin Stable: All feature combinations
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bluefin_stable_base_image_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(family.select_image_for_features(&[]), Some("bluefin"));
    }

    #[test]
    fn bluefin_stable_nvidia_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("bluefin-nvidia")
        );
    }

    #[test]
    fn bluefin_stable_nvidia_open_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["nvidia", "open"]),
            Some("bluefin-nvidia-open")
        );
    }

    #[test]
    fn bluefin_stable_dx_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["dx"]),
            Some("bluefin-dx")
        );
    }

    #[test]
    fn bluefin_stable_dx_nvidia_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia"]),
            Some("bluefin-dx-nvidia")
        );
    }

    #[test]
    fn bluefin_stable_dx_nvidia_open_resolves() {
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia", "open"]),
            Some("bluefin-dx-nvidia-open")
        );
    }

    #[test]
    fn bluefin_stable_invalid_dx_open_rejected() {
        // dx-open combination doesn't exist
        let family = find_family("Bluefin Stable");
        assert_eq!(family.select_image_for_features(&["dx", "open"]), None);
    }

    #[test]
    fn bluefin_stable_invalid_open_alone_rejected() {
        // open only exists with nvidia
        let family = find_family("Bluefin Stable");
        assert_eq!(family.select_image_for_features(&["open"]), None);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bluefin LTS: Different feature set (hwe, amd64, arm64, gdx)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bluefin_lts_base_image_resolves() {
        let family = find_family("Bluefin LTS");
        assert_eq!(family.select_image_for_features(&[]), Some("bluefin"));
    }

    #[test]
    fn bluefin_lts_nvidia_resolves() {
        let family = find_family("Bluefin LTS");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("bluefin-nvidia")
        );
    }

    #[test]
    fn bluefin_lts_dx_resolves() {
        let family = find_family("Bluefin LTS");
        assert_eq!(
            family.select_image_for_features(&["dx"]),
            Some("bluefin-dx")
        );
    }

    #[test]
    fn bluefin_lts_dx_nvidia_resolves() {
        let family = find_family("Bluefin LTS");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia"]),
            Some("bluefin-dx-nvidia")
        );
    }

    #[test]
    fn bluefin_lts_gdx_resolves() {
        let family = find_family("Bluefin LTS");
        assert_eq!(
            family.select_image_for_features(&["gdx"]),
            Some("bluefin-gdx")
        );
    }

    #[test]
    fn bluefin_lts_nvidia_open_rejected() {
        // LTS doesn't have nvidia-open variant
        let family = find_family("Bluefin LTS");
        assert_eq!(family.select_image_for_features(&["nvidia", "open"]), None);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Aurora: Similar to Bluefin Stable
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn aurora_base_image_resolves() {
        let family = find_family("Aurora");
        assert_eq!(family.select_image_for_features(&[]), Some("aurora"));
    }

    #[test]
    fn aurora_nvidia_resolves() {
        let family = find_family("Aurora");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("aurora-nvidia")
        );
    }

    #[test]
    fn aurora_nvidia_open_resolves() {
        let family = find_family("Aurora");
        assert_eq!(
            family.select_image_for_features(&["nvidia", "open"]),
            Some("aurora-nvidia-open")
        );
    }

    #[test]
    fn aurora_dx_resolves() {
        let family = find_family("Aurora");
        assert_eq!(family.select_image_for_features(&["dx"]), Some("aurora-dx"));
    }

    #[test]
    fn aurora_dx_nvidia_resolves() {
        let family = find_family("Aurora");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia"]),
            Some("aurora-dx-nvidia")
        );
    }

    #[test]
    fn aurora_dx_nvidia_open_resolves() {
        let family = find_family("Aurora");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia", "open"]),
            Some("aurora-dx-nvidia-open")
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bazzite KDE: Gaming-focused with deck variant
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bazzite_kde_base_image_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(family.select_image_for_features(&[]), Some("bazzite"));
    }

    #[test]
    fn bazzite_kde_nvidia_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("bazzite-nvidia")
        );
    }

    #[test]
    fn bazzite_kde_nvidia_open_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["nvidia", "open"]),
            Some("bazzite-nvidia-open")
        );
    }

    #[test]
    fn bazzite_kde_deck_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["deck"]),
            Some("bazzite-deck")
        );
    }

    #[test]
    fn bazzite_kde_deck_nvidia_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["deck", "nvidia"]),
            Some("bazzite-deck-nvidia")
        );
    }

    #[test]
    fn bazzite_kde_asus_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["asus"]),
            Some("bazzite-asus")
        );
    }

    #[test]
    fn bazzite_kde_framework_resolves() {
        let family = find_family("Bazzite KDE");
        assert_eq!(
            family.select_image_for_features(&["framework"]),
            Some("bazzite-framework")
        );
    }

    #[test]
    fn bazzite_kde_invalid_dx_rejected() {
        // KDE doesn't have dx variant
        let family = find_family("Bazzite KDE");
        assert_eq!(family.select_image_for_features(&["dx"]), None);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Bazzite GNOME: Minimal feature set
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bazzite_gnome_base_image_resolves() {
        let family = find_family("Bazzite GNOME");
        assert_eq!(family.select_image_for_features(&[]), Some("bazzite-gnome"));
    }

    #[test]
    fn bazzite_gnome_nvidia_resolves() {
        let family = find_family("Bazzite GNOME");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("bazzite-gnome-nvidia")
        );
    }

    #[test]
    fn bazzite_gnome_no_other_variants() {
        // GNOME only has base and nvidia, no dx, deck, etc.
        let family = find_family("Bazzite GNOME");
        assert_eq!(family.select_image_for_features(&["dx"]), None);
        assert_eq!(family.select_image_for_features(&["deck"]), None);
        assert_eq!(family.select_image_for_features(&["framework"]), None);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Dakota: Experimental, very limited feature set
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn dakota_base_image_resolves() {
        let family = find_family("Bluefin Dakota");
        assert_eq!(family.select_image_for_features(&[]), Some("dakota"));
    }

    #[test]
    fn dakota_nvidia_resolves() {
        let family = find_family("Bluefin Dakota");
        assert_eq!(
            family.select_image_for_features(&["nvidia"]),
            Some("dakota-nvidia")
        );
    }

    #[test]
    fn dakota_no_dx_variant() {
        // Dakota doesn't have dx variant (per comment in code)
        let family = find_family("Bluefin Dakota");
        assert_eq!(family.select_image_for_features(&["dx"]), None);
    }

    #[test]
    fn dakota_no_dx_nvidia_combo() {
        // Dakota doesn't have dx-nvidia combo
        let family = find_family("Bluefin Dakota");
        assert_eq!(family.select_image_for_features(&["dx", "nvidia"]), None);
    }

    #[test]
    fn dakota_no_nvidia_open() {
        // Dakota doesn't have nvidia-open variant
        let family = find_family("Bluefin Dakota");
        assert_eq!(family.select_image_for_features(&["nvidia", "open"]), None);
    }

    // ─────────────────────────────────────────────────────────────────────
    // Cross-family validation: Ensure correct families exist
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn all_expected_families_exist() {
        let expected = vec![
            "Bluefin Stable",
            "Bluefin LTS",
            "Aurora",
            "Bazzite KDE",
            "Bazzite GNOME",
            "Bluefin Dakota",
        ];

        for name in expected {
            assert!(
                KNOWN_FAMILIES.iter().any(|f| f.name == name),
                "Family '{}' not found in KNOWN_FAMILIES",
                name
            );
        }
    }

    #[test]
    fn family_count_matches_expected() {
        // Should have exactly 6 families (no ucore per comment in code)
        assert_eq!(
            KNOWN_FAMILIES.len(),
            6,
            "Expected 6 families, found {}",
            KNOWN_FAMILIES.len()
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Feature availability audit: What features does each family support?
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bluefin_stable_supports_nvidia_feature() {
        let family = find_family("Bluefin Stable");
        assert!(family.select_image_for_features(&["nvidia"]).is_some());
    }

    #[test]
    fn bluefin_stable_supports_dx_feature() {
        let family = find_family("Bluefin Stable");
        assert!(family.select_image_for_features(&["dx"]).is_some());
    }

    #[test]
    fn bluefin_stable_supports_open_feature() {
        let family = find_family("Bluefin Stable");
        assert!(
            family
                .select_image_for_features(&["nvidia", "open"])
                .is_some(),
            "open can only be combined with nvidia"
        );
    }

    #[test]
    fn bluefin_lts_does_not_support_nvidia_open() {
        // Unlike Stable, LTS doesn't have nvidia-open variant
        let family = find_family("Bluefin LTS");
        assert_eq!(family.select_image_for_features(&["nvidia", "open"]), None);
    }

    #[test]
    fn bluefin_lts_supports_gdx_feature() {
        // LTS-specific feature
        let family = find_family("Bluefin LTS");
        assert_eq!(
            family.select_image_for_features(&["gdx"]),
            Some("bluefin-gdx")
        );
    }

    #[test]
    fn dakota_minimal_feature_set() {
        // Dakota only supports base and nvidia
        let family = find_family("Bluefin Dakota");
        let mut features_supported = 0;

        if family.select_image_for_features(&["nvidia"]).is_some() {
            features_supported += 1;
        }
        // Should NOT support other common features
        assert_eq!(family.select_image_for_features(&["dx"]), None);
        assert_eq!(family.select_image_for_features(&["open"]), None);
        assert_eq!(family.select_image_for_features(&["framework"]), None);

        assert!(
            features_supported > 0,
            "Dakota should support at least nvidia"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // Feature combinations: Valid vs invalid
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bluefin_stable_feature_order_independent() {
        // select_image_for_features should handle features in any order
        let family = find_family("Bluefin Stable");
        assert_eq!(
            family.select_image_for_features(&["dx", "nvidia"]),
            family.select_image_for_features(&["nvidia", "dx"])
        );
        assert_eq!(
            family.select_image_for_features(&["nvidia", "open"]),
            family.select_image_for_features(&["open", "nvidia"])
        );
    }

    #[test]
    fn feature_combination_completeness() {
        // Each image variant exists: no orphaned features
        let family = find_family("Bluefin Stable");
        for image in family.images {
            if image == &family.base_image() {
                assert_eq!(family.select_image_for_features(&[]), Some(*image));
            } else {
                // Every non-base image should be selectable by some feature combo
                let base = family.base_image();
                let suffix = image
                    .strip_prefix(&format!("{}-", base))
                    .expect(&format!("Image {} doesn't start with {}-", image, base));
                let features: Vec<&str> = suffix.split('-').collect();

                // This should resolve to the image
                assert_eq!(
                    family.select_image_for_features(&features),
                    Some(*image),
                    "Image {} should be selectable via features {:?}",
                    image,
                    features
                );
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // Audit trail: Print feature mapping for documentation
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn print_all_families_and_variants() {
        println!("\n=== Image Family Variant Audit ===\n");
        for family in KNOWN_FAMILIES {
            println!("Family: {}", family.name);
            println!("  Org: {}", family.org);
            println!("  Base image: {}", family.base_image());
            println!("  Available streams: {:?}", family.streams);
            println!("  Images/variants:");
            for image in family.images {
                println!("    - {}", image);
            }
            println!();
        }
    }
}
