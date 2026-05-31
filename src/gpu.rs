//! Hardware-aware checks for the rebase dialog.
//!
//! Right now we just answer "does this host have an NVIDIA GPU?" — the
//! rebase dialog uses that to gate a warning when the user toggles NVIDIA
//! off (per user direction: warn before degrading to software rendering on
//! systems that need the proprietary / open kernel modules).
//!
//! Implementation reads `lspci -mm` (machine-readable format) and looks
//! for a VGA controller or 3D controller whose vendor field is "NVIDIA
//! Corporation". From inside the Flatpak sandbox the call routes through
//! `flatpak-spawn --host` since the host's PCI tree isn't visible to the
//! sandbox.

use std::process::Command;
use std::sync::OnceLock;

/// Cached result of the lspci probe — the GPU complement of a running system
/// can't change without a reboot, so once we've answered "is there an NVIDIA
/// card" the answer is stable for the life of the process.
///
/// The rebase dialog's NVIDIA toggle handler calls this on every flip; without
/// the cache each toggle would spawn an `lspci` (or `flatpak-spawn --host
/// lspci`) subprocess. With the cache the first toggle pays ~10ms, the rest
/// are a load from atomic memory.
static NVIDIA_GPU_CACHE: OnceLock<bool> = OnceLock::new();

/// Return true if `lspci -mm` reports an NVIDIA VGA / 3D controller.
///
/// Result is cached for the process lifetime — see [`NVIDIA_GPU_CACHE`].
///
/// Returns false on any kind of failure (lspci missing, exit non-zero,
/// unparseable output) — the conservative answer for a UI gate is "no
/// warning" rather than crashing the dialog.
pub fn has_nvidia_gpu() -> bool {
    *NVIDIA_GPU_CACHE.get_or_init(detect_nvidia_gpu_uncached)
}

fn detect_nvidia_gpu_uncached() -> bool {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "lspci", "-mm"])
            .output()
    } else {
        Command::new("lspci").arg("-mm").output()
    };
    match output {
        Ok(o) if o.status.success() => {
            parse_lspci_mm_for_nvidia(&String::from_utf8_lossy(&o.stdout))
        }
        _ => false,
    }
}

/// Scan the lines of `lspci -mm` output for an NVIDIA VGA / 3D controller.
/// Public so it's unit-testable without invoking lspci.
///
/// Sample matching line:
///   `01:00.0 "VGA compatible controller" "NVIDIA Corporation" "GA106 …`
fn parse_lspci_mm_for_nvidia(stdout: &str) -> bool {
    for line in stdout.lines() {
        let lower = line.to_lowercase();
        let is_display = lower.contains("vga compatible controller")
            || lower.contains("3d controller")
            || lower.contains("display controller");
        if !is_display {
            continue;
        }
        // Vendor field is the third quoted segment. lspci's "NVIDIA
        // Corporation" string is the canonical signal — case-folded to
        // handle the rare "Nvidia" variant that crops up.
        if lower.contains("\"nvidia corporation\"") || lower.contains(" nvidia ") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vga_controller_with_nvidia_vendor() {
        let stdout = "01:00.0 \"VGA compatible controller\" \"NVIDIA Corporation\" \"GA106 [GeForce RTX 3060]\" -ra1 \"ASUSTeK Computer Inc.\" \"Device 9d23\"\n";
        assert!(parse_lspci_mm_for_nvidia(stdout));
    }

    #[test]
    fn parses_3d_controller_for_laptop_nvidia() {
        // Discrete-only NVIDIA laptops show the dGPU as a 3D controller
        // (no VGA) since the iGPU drives the display.
        let stdout = "01:00.0 \"3D controller\" \"NVIDIA Corporation\" \"GA107M [GeForce RTX 3050 Mobile]\" -rb1 \"Dell\" \"Device 0a91\"\n";
        assert!(parse_lspci_mm_for_nvidia(stdout));
    }

    #[test]
    fn returns_false_for_intel_only_system() {
        let stdout = "00:02.0 \"VGA compatible controller\" \"Intel Corporation\" \"Alder Lake-S GT1 [UHD Graphics 730]\" -r0c \"Intel Corporation\" \"Device 7d50\"\n";
        assert!(!parse_lspci_mm_for_nvidia(stdout));
    }

    #[test]
    fn returns_false_for_amd_system() {
        let stdout = "03:00.0 \"VGA compatible controller\" \"Advanced Micro Devices, Inc. [AMD/ATI]\" \"Navi 33 [Radeon RX 7700S]\" -r0c \"Framework Computer\" \"Device 0030\"\n";
        assert!(!parse_lspci_mm_for_nvidia(stdout));
    }

    #[test]
    fn ignores_non_display_devices_named_nvidia() {
        // A weird edge case: an audio device on an NVIDIA card is itself
        // not the GPU. Class string disambiguates.
        let stdout = "01:00.1 \"Audio device\" \"NVIDIA Corporation\" \"GA106 High Definition Audio Controller\"\n";
        assert!(!parse_lspci_mm_for_nvidia(stdout));
    }

    #[test]
    fn handles_empty_output() {
        assert!(!parse_lspci_mm_for_nvidia(""));
    }

    #[test]
    fn finds_nvidia_among_multiple_gpus() {
        // Hybrid graphics laptop: Intel iGPU + NVIDIA dGPU. Should match.
        let stdout = "\
00:02.0 \"VGA compatible controller\" \"Intel Corporation\" \"Iris Xe Graphics\" -r0c\n\
01:00.0 \"3D controller\" \"NVIDIA Corporation\" \"GA107M [GeForce RTX 3050 Mobile]\" -rb1\n";
        assert!(parse_lspci_mm_for_nvidia(stdout));
    }
}
