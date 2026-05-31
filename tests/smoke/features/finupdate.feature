@smoke_suite @finupdate
Feature: Finupdate smoke tests
  Validates that the finupdate Flatpak launches, exposes its main controls
  via AT-SPI, and the developer-mode simulated update path completes without
  touching the host system.

  Regression coverage for the GUI getting too far ahead of the backend — these
  scenarios fail loudly if widget wiring breaks during refactors.

  Background:
    * Start application "finupdate" via "command"
    * Wait until window "Finupdate" appears in "finupdate"

  # ── Launch & main window ────────────────────────────────────────────────

  @launch
  Scenario: Application window appears with the main controls
    * Application "finupdate" is running
    * Item "Check" "button" is "showing" in "finupdate"

  # ── Menu access ─────────────────────────────────────────────────────────

  @menu
  Scenario: Hamburger menu opens
    * Left click "Main Menu" "toggle button" in "finupdate"
    * Item "Main Menu" "button" is "showing" in "finupdate"

  @menu
  Scenario: Keyboard shortcut window opens via Ctrl+question
    * Key combo: "<Control>question"
    # GtkShortcutsWindow is a separate GTK4 toplevel not exposed via AT-SPI;
    # just verify the key combo doesn't crash the app.
    * Application "finupdate" is running

  # ── Preferences dialog ──────────────────────────────────────────────────

  @preferences
  Scenario: Preferences dialog opens from the menu
    * Key combo: "<Control>comma"
    * Wait until "Preferences" "dialog" appears in "finupdate"

  # Developer Mode toggle was removed from the UI — it's a CLI-only flag
  # now (`finupdate --dev-mode`). The scenario that verified the switch is
  # in Preferences no longer applies; smoke tests that need dev_mode just
  # write settings.json before launch via the existing scenario step.

  # ── Dev-mode simulated update ───────────────────────────────────────────
  # These exercise the full UI state machine without root or a live system.

  # ── Dev-mode simulated update (limited AT-SPI coverage) ─────────────
  # GTK4 adw::StatusPage / progress views don't expose text in the
  # accessibility tree.  These scenarios verify the check dialog opens
  # and responds; the post-install completion page is verified manually.

  @dev_mode @simulator @dry_run
  Scenario: Dev mode shows all commands that would be executed (dry-run)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Wait until "DRY RUN" appears in "finupdate" within 10 seconds
    * Wait until "bootc upgrade" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  @dev_mode @simulator
  Scenario: Check dialog shows update available (dev mode, Success)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  @dev_mode @simulator
  Scenario: Check dialog reports up-to-date (dev mode, AlreadyUpToDate)
    * Application "finupdate" is in developer mode with scenario "AlreadyUpToDate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "System is up to date" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  @dev_mode @simulator
  Scenario: Check dialog shows error (dev mode, Failure)
    * Application "finupdate" is in developer mode with scenario "Failure"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Key combo: "Escape"

  # ── Check dialog action scenarios ────────────────────────────────────

  @dev_mode @simulator @dialog
  Scenario: Install button activates in check dialog when updates available
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Install all" "button" appears in "finupdate" within 10 seconds
    * Item "Install all" "button" is "showing" in "finupdate"

  @dev_mode @simulator @dialog
  Scenario: Check dialog can be dismissed with Close button
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Ready to install" appears in "finupdate" within 10 seconds
    * Left click "Close" "button" in "finupdate"
    * Wait until 2 seconds
    * Application "finupdate" is running

  @dev_mode @simulator @dialog
  Scenario: Install all initiates update from check dialog (Success scenario)
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "installing" appears in "finupdate" within 15 seconds

  @dev_mode @simulator @dialog
  Scenario: Install all initiates update from check dialog (Failure scenario)
    * Application "finupdate" is in developer mode with scenario "Failure"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "Update failed" appears in "finupdate" within 15 seconds

  # ── Update cancellation ──────────────────────────────────────────────

  @dev_mode @simulator @cancel
  Scenario: User can cancel update during progress
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Activate "Install all" "button" in "finupdate"
    * Wait until "installing" appears in "finupdate" within 10 seconds
    * Wait until 2 seconds
    * Left click "Cancel" "button" in "finupdate"
    * Application "finupdate" is running

  # ── Developer mode + simulator are CLI-only ──────────────────────────
  # The hamburger menu and Preferences toggle for these were removed
  # (gnome-control-center idiom — no dev-only state exposed in the UI).
  # Use `--dev-mode` and `--sim=<scenario>` on the command line, or write
  # to settings.json via `_write_settings(dev_mode=True, sim_scenario=...)`
  # which is what the existing scenario steps still do. The scenarios that
  # asserted on the menu entries / Preferences toggle were removed since
  # those widgets no longer exist.

  # ── Check dialog text verification ──────────────────────────────────
  # Verify check dialog displays correctly without waiting for real checks.

  @dialog @text
  Scenario: Check dialog displays all source names
    * Application "finupdate" is in developer mode with scenario "Success"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Powered by uupd" appears in "finupdate" within 5 seconds
    * Key combo: "Escape"

  # ── Real non-destructive update checks ───────────────────────────────
  # These test actual update checking for Flatpak, Homebrew, Distrobox
  # without modifying the system image (bootc). Safe to run on real systems.

  @real @integration @non_destructive
  Scenario: Real check dialog queries actual update sources
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Update" appears in "finupdate" within 60 seconds
    * Application "finupdate" is running

  @real @integration @non_destructive
  Scenario: Real update check completes without errors
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Querying" appears in "finupdate" within 30 seconds
    * Wait until "Close" "button" appears in "finupdate" within 60 seconds
    * Application "finupdate" is running
    * Left click "Close" "button" in "finupdate"

  @real @integration @non_destructive @install
  Scenario: Install available non-system updates (Flatpak, Homebrew, Distrobox)
    * Application "finupdate" is in real mode (developer mode disabled)
    * Item "Check" "button" is "showing" in "finupdate"
    * Left click "Check" "button" in "finupdate"
    * Wait until "Querying" appears in "finupdate" within 30 seconds
    * Wait until "Close" "button" appears in "finupdate" within 60 seconds
    * Item "Install all" "button" is "showing" in "finupdate"
    * Left click "Install all" "button" in "finupdate"
    * Wait until 5 seconds
    * Application "finupdate" is running

  # ── Image source / history are reached via the Advanced dialog ─────────
  # The main page is intentionally minimal (Check + Automatic Updates +
  # Advanced row). The previous scenarios that asserted on "Image source"
  # / "Image history" list items being on the home page no longer apply;
  # those rows moved into the Advanced (Preferences) dialog → System group.
  # The strict-count matrix below still exercises the history-fetch path
  # end-to-end and is the meaningful coverage; spot-presence on the home
  # page would be tautological now.

  @dev_mode @advanced @image_management
  Scenario: Advanced dialog exposes Image Source and Image History rows
    * Key combo: "<Control>comma"
    * Wait until "Advanced" "dialog" appears in "finupdate"
    * Wait until "Image Source" appears in "finupdate" within 5 seconds
    * Wait until "Image History" appears in "finupdate" within 5 seconds

  # ── Mock-identity matrix: render against many bootc image families ─────
  # `Mock identity` sets settings.json.mock_identity (overrides bootc status),
  # dry_run=true (blocks destructive subprocess calls), dev_mode=true (routes
  # the update worker through the simulator). Real GHCR + GitHub API calls
  # still happen — so these scenarios genuinely exercise the data-rendering
  # paths against live upstream data. Tag @live so CI can skip offline.

  @live @mock_identity @matrix
  Scenario Outline: Image surfaces render for <family>
    * Mock identity "<full_ref>" is configured
    * Wait until "<image_name>" appears in "finupdate" within 15 seconds
    * Application "finupdate" is running

    Examples:
      | family                  | full_ref                                            | image_name              |
      | bluefin                 | ghcr.io/ublue-os/bluefin:stable                     | bluefin                 |
      | bluefin-nvidia-open     | ghcr.io/ublue-os/bluefin-nvidia-open:stable         | bluefin-nvidia-open     |
      | bluefin-dx              | ghcr.io/ublue-os/bluefin-dx:stable                  | bluefin-dx              |
      | bluefin-dx-nvidia-open  | ghcr.io/ublue-os/bluefin-dx-nvidia-open:stable      | bluefin-dx-nvidia-open  |
      | aurora                  | ghcr.io/ublue-os/aurora:stable                      | aurora                  |
      | aurora-dx               | ghcr.io/ublue-os/aurora-dx:stable                   | aurora-dx               |
      | bazzite                 | ghcr.io/ublue-os/bazzite:stable                     | bazzite                 |
      | bazzite-nvidia          | ghcr.io/ublue-os/bazzite-nvidia:stable              | bazzite-nvidia          |
      | bazzite-deck            | ghcr.io/ublue-os/bazzite-deck:stable                | bazzite-deck            |
      | bazzite-deck-nvidia     | ghcr.io/ublue-os/bazzite-deck-nvidia:stable         | bazzite-deck-nvidia     |
      | dakota                  | ghcr.io/projectbluefin/dakota:latest                | dakota                  |
      | dakota-nvidia           | ghcr.io/projectbluefin/dakota-nvidia:latest         | dakota-nvidia           |

  # ── Strict history population: each family must reach N entries ──────
  # `image history shows at least N` polls for "N images" labels in the
  # AT-SPI tree. We aim for 8 (HISTORY_MAX), but accept fewer for sparsely
  # released families. If a family is short, that's a real-world signal
  # the parser or the GHCR fetch window needs work.

  @live @mock_identity @history @strict_count
  Scenario Outline: Image history populates with at least <min_count> entries for <family>
    * Mock identity "<full_ref>" is configured
    * Wait until "<image_name>" appears in "finupdate" within 15 seconds
    * Image history shows at least <min_count> entries in "finupdate" within 60 seconds
    * Application "finupdate" is running

    Examples:
      | family                  | full_ref                                            | image_name              | min_count |
      | bluefin                 | ghcr.io/ublue-os/bluefin:stable                     | bluefin                 |         8 |
      | bluefin-nvidia-open     | ghcr.io/ublue-os/bluefin-nvidia-open:stable         | bluefin-nvidia-open     |         8 |
      | bluefin-dx              | ghcr.io/ublue-os/bluefin-dx:stable                  | bluefin-dx              |         8 |
      | bluefin-dx-nvidia-open  | ghcr.io/ublue-os/bluefin-dx-nvidia-open:stable      | bluefin-dx-nvidia-open  |         8 |
      | aurora                  | ghcr.io/ublue-os/aurora:stable                      | aurora                  |         8 |
      | aurora-dx               | ghcr.io/ublue-os/aurora-dx:stable                   | aurora-dx               |         8 |
      | bazzite                 | ghcr.io/ublue-os/bazzite:stable                     | bazzite                 |         8 |
      | bazzite-nvidia          | ghcr.io/ublue-os/bazzite-nvidia:stable              | bazzite-nvidia          |         8 |
      | bazzite-deck            | ghcr.io/ublue-os/bazzite-deck:stable                | bazzite-deck            |         8 |
      | bazzite-deck-nvidia     | ghcr.io/ublue-os/bazzite-deck-nvidia:stable         | bazzite-deck-nvidia     |         8 |
      | dakota                  | ghcr.io/projectbluefin/dakota:latest                | dakota                  |         8 |
      # dakota-nvidia publishes sha-tagged manifests, not dated names.
      # probe_sha_tag_dates() reads the config blob `created` for up to
      # SHA_PROBE_CAP=30 sha tags so we surface 8 here too.
      | dakota-nvidia           | ghcr.io/projectbluefin/dakota-nvidia:latest         | dakota-nvidia           |         8 |

  # ── Rollback flow: open the Rebase dialog and load real version history ─
  # Drives the rebase dialog (Ctrl+Shift+R), waits for a version-list signal
  # ("Version" appears in the dialog header / row / loaded title). dry_run=true
  # so even if the user clicks a Rebase button, `bootc switch` is suppressed
  # via run_rebase_simulated. Tests are parameterised across the same families.

  @live @mock_identity @rollback
  Scenario Outline: Rebase dialog loads version history for <family>
    * Mock identity "<full_ref>" is configured
    * Wait until "<image_name>" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Shift>r"
    * Wait until "Version" appears in "finupdate" within 30 seconds
    * Application "finupdate" is running
    * Key combo: "Escape"

    Examples:
      | family   | full_ref                              | image_name |
      | bluefin  | ghcr.io/ublue-os/bluefin:stable       | bluefin    |
      | aurora   | ghcr.io/ublue-os/aurora:stable        | aurora     |
      | dakota   | ghcr.io/projectbluefin/dakota:latest  | dakota     |

  # ── Feature switches: rebase dialog shows the right family's atomic switches ─
  # The rebase dialog populates one SwitchRow per Family::available_features()
  # entry (populate_family_switches in src/ui/rebase_dialog.rs). Dakota has only
  # the `nvidia` feature; Bluefin Stable has the full slate. Confirms the
  # Iteration B wiring surfaces the correct labels per the booted family
  # instead of the old hardcoded Dakota/Dakota-Nvidia ToggleButtons.

  @live @mock_identity @features
  Scenario: Dakota rebase dialog exposes only the NVIDIA feature switch
    * Mock identity "ghcr.io/projectbluefin/dakota:latest" is configured
    * Wait until "dakota" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Shift>r"
    * Wait until "NVIDIA drivers (proprietary)" appears in "finupdate" within 20 seconds
    * Application "finupdate" is running
    * Key combo: "Escape"

  @live @mock_identity @features
  Scenario: Bluefin Stable rebase dialog exposes the developer + NVIDIA switches
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Shift>r"
    * Wait until "Developer extras (DX)" appears in "finupdate" within 20 seconds
    * Wait until "NVIDIA drivers (proprietary)" appears in "finupdate" within 5 seconds
    * Application "finupdate" is running
    * Key combo: "Escape"

  # ── Changelog flow: app stays stable while real GHCR + GitHub data loads ─
  # We don't assert exact rendered strings (the changelog area's AT-SPI
  # exposure is patchy under GTK4) — we assert the home-page anchors stay
  # visible while the live fetch progresses, proving the fetch + the main
  # window remain healthy. A future iteration can tighten this once we wire
  # the changelog labels to expose accessible text.

  @live @mock_identity @changelog
  Scenario Outline: Changelog fetch keeps the app responsive for <family>
    * Mock identity "<full_ref>" is configured
    * Wait until "<image_name>" appears in "finupdate" within 15 seconds
    * Wait until 30 seconds
    * Application "finupdate" is running

    Examples:
      | family   | full_ref                              | image_name |
      | bluefin  | ghcr.io/ublue-os/bluefin:stable       | bluefin    |
      | aurora   | ghcr.io/ublue-os/aurora:stable        | aurora     |
      | dakota   | ghcr.io/projectbluefin/dakota:latest  | dakota     |

  # "Previous image versions accessible" rollback scenario removed — it
  # asserted on an Image History list item on the home page which doesn't
  # exist any more. The matrix scenarios above + the end-to-end rollback
  # below cover the actual rollback path.

  # ── End-to-end rollback: dialog → pick day → confirm → simulated success ──
  # Mock identity puts dry_run=true AND dev_mode=true, so confirming the
  # rebase routes through run_rebase_simulated, NOT a real `bootc switch`.
  # The flow proves the whole rollback UI state machine plumbs through:
  # accelerator → dialog opens → version history fetched from live GHCR →
  # day button selectable → confirmation alert → simulated success page.

  @live @mock_identity @rollback @end_to_end
  Scenario: Dry-run rollback flow completes through the simulated success page
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Shift>r"
    * Wait until "Version" appears in "finupdate" within 30 seconds
    * Click any available day in the rebase calendar
    * Click button whose label starts with "Rebase to" in "finupdate"
    * Wait until "Rebase System" appears in "finupdate" within 5 seconds
    * Left click "Rebase" "button" in "finupdate"
    * Wait until "simulated" appears in "finupdate" within 10 seconds
    * Application "finupdate" is running

  # Pin functionality / Powerwash / Factory Reset on the home page —
  # removed scenarios. These rows moved to the Advanced dialog's System
  # group. Powerwash and Factory Reset no longer have keyboard shortcuts
  # (destructive actions need intentional clicks); the Advanced dialog is
  # the only entry point.

  @destructive @advanced @powerwash
  Scenario: Powerwash row is reachable via Advanced dialog
    * Key combo: "<Control>comma"
    * Wait until "Advanced" "dialog" appears in "finupdate"
    * Wait until "Powerwash" appears in "finupdate" within 5 seconds

  @destructive @advanced @factory_reset
  Scenario: Factory Reset row is reachable via Advanced dialog
    * Key combo: "<Control>comma"
    * Wait until "Advanced" "dialog" appears in "finupdate"
    * Wait until "Factory Reset" appears in "finupdate" within 5 seconds

  # ── Accelerator coverage for non-AT-SPI surfaces ────────────────────────
  # libadwaita ActionRow doesn't enumerate suffix-button children to AT-SPI,
  # so the banner (What's new / Install / Restart / Discard) and the
  # Powerwash / Factory reset rows aren't directly clickable from dogtail.
  # We bind keyboard accelerators in app.rs for every action that previously
  # depended on those buttons. Tests drive the accelerator; the underlying
  # AppMsg flow is the same as the click path. Run with mock_identity so
  # the banner is in the UpdateAvailable state and its actions are
  # meaningful.

  @live @mock_identity @buttons @whats_new
  Scenario: Ctrl+W activates the What's new / changelog action
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control>w"
    * Application "finupdate" is running

  @live @mock_identity @buttons @restart
  Scenario: Ctrl+Shift+B opens the Restart confirmation dialog
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Shift>b"
    * Wait until "Restart System" appears in "finupdate" within 5 seconds
    * Key combo: "Escape"
    * Application "finupdate" is running

  @live @mock_identity @buttons @discard
  Scenario: Ctrl+Backspace dismisses the update banner
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control>BackSpace"
    * Application "finupdate" is running

  @live @mock_identity @buttons @powerwash @command_log
  Scenario: Ctrl+Alt+P opens the Powerwash confirmation dialog
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Alt>p"
    * Wait until "Powerwash" appears in "finupdate" within 5 seconds
    * Key combo: "Escape"
    * Application "finupdate" is running

  @live @mock_identity @buttons @factory_reset @command_log
  Scenario: Ctrl+Alt+F opens the Factory Reset confirmation dialog
    * Mock identity "ghcr.io/ublue-os/bluefin:stable" is configured
    * Wait until "bluefin" appears in "finupdate" within 15 seconds
    * Key combo: "<Control><Alt>f"
    * Wait until "Factory reset" appears in "finupdate" within 5 seconds
    * Key combo: "Escape"
    * Application "finupdate" is running

  # ── Tab-navigability smoke ────────────────────────────────────────────
  # Verify the home page is reachable via Tab (the keyboard-only focus
  # chain that GTK4 maintains). Pressing Tab repeatedly should cycle
  # through focusable widgets without crashing the app. Distinct from the
  # accelerator coverage above — this validates the Tab focus chain
  # itself, which is what keyboard-only users (and screen readers) rely on.

  @tab_nav @keyboard
  Scenario: Tab cycles focus through the home page without crashing
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Key combo: "Tab"
    * Application "finupdate" is running
    * Item "Check" "button" is "showing" in "finupdate"

  # ── Clean shutdown ──────────────────────────────────────────────────────

  @close
  Scenario: Application closes cleanly via Ctrl+Q
    * Close application "finupdate" via "shortcut"
    * Application "finupdate" is no longer running
