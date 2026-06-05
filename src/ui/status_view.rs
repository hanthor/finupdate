//! Status view component — the main content area of the app.
//!
//! Pattern: State-driven view switching
//! Uses a `gtk::Stack` to switch between different visual states:
//! - Idle: Card-based overview with hero, update banner, and settings actions
//! - Updating: Progress indicator + image badge + UpdateList + live log + timer + cancel
//! - Complete: Success status page with reboot option
//! - UpToDate: "You're already up to date" status page
//! - Error: Error status page with retry option

use adw::prelude::*;
use relm4::prelude::*;
use serde_json::Value;
use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::time::Instant;

use crate::app::{AppState, PreflightStatus};
use crate::registry_client::ImageVersion;
use crate::settings::Settings;
use crate::ui::log_view::{LogView, LogViewInput};
use crate::ui::segmented_progress::{SegmentedProgress, same_segment};
use crate::ui::update_list::{UpdateList, UpdateListInput};

/// Mock deployment representation for the collapsible version history list.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MockDeployment {
    pub id: String,
    pub state: String, // "current" | "staged" | "previous" | "archived"
    pub title: String,
    pub image: String,
    pub tag: String,
    pub digest: String,
    pub deployed: String,
    pub deployed_full: String,
    pub size: String,
    pub kernel: String,
    pub package_count: u32,
    pub signer: String,
    pub pinned: bool,
}

/// State of the SBOM diff fetch for the changelog page. Renders a different
/// section in `rebuild_changelog_page` for each value so the user sees
/// "comparing packages…" while we wait, instead of a silent gap that takes
/// 30+ seconds to fill in on a slow connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SbomStatus {
    /// Initial state — no changelog fetch has started yet.
    Pending,
    /// SBOM fetch is in flight (tokio task spawned).
    Loading,
    /// fetch_and_diff_sboms returned None — the registry didn't publish
    /// SPDX referrers for one of the images. Show a dim "not available"
    /// note instead of a spinner that never resolves.
    NotAvailable,
    /// Diff is loaded (stored in `sbom_diff`).
    Loaded,
}

/// Input messages for the StatusView component.
#[derive(Debug)]
pub enum StatusViewInput {
    /// Parent tells us the app state changed.
    StateChanged(AppState),
    /// Append a log line to the output view.
    AppendLog(String),
    /// Clear the log buffer.
    ClearLog,
    /// Timer tick — update elapsed time display.
    TimerTick,
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
    /// Dismiss the staged reboot banner.
    DismissBanner,
    /// Hero action button clicked — dispatch to StartUpdate or Reboot based
    /// on current state. The single inline button does double duty per the
    /// macOS Tahoe "Install" / "Restart" pattern on the Software Update card.
    HeroActionClicked,
    /// Parent App pushed updated Settings (Advanced dialog closed, CLI flag
    /// applied, etc.). StatusView refreshes any front-page widgets that
    /// mirror persistent settings — currently just the Auto Updates switch.
    SettingsChanged(Settings),
    /// "Restart Tonight" button clicked — schedules the host to reboot at
    /// 02:00 via `pkexec shutdown -r 02:00`. Only meaningful when
    /// reboot_pending is true (a deployment is staged); the button is hidden
    /// otherwise. Toast confirms; user can cancel manually with
    /// `sudo shutdown -c`.
    ScheduleRebootTonight,
    /// Copy log to clipboard.
    CopyLog,
    /// Navigate stack to a page name
    ShowPage(String),
    /// Save registry URI — fired by the EntryRow's `apply` signal (Enter
    /// or built-in ✓ button). Edit/Cancel variants were removed when the
    /// manual Change/Save/Cancel toggle was replaced with EntryRow.
    SaveRegistryUri(String),
    /// Select tag in Image Source
    SelectTag(String),
    /// Toggle pinned status of history deployment
    TogglePin(String),
    /// Roll back to a specific deployment
    RollbackTo(MockDeployment),
    /// Confirm rollback
    ConfirmRollback,
    /// Set a deployment as default boot
    SetDefaultBoot(MockDeployment),
    /// Select a version in Changelog
    SelectChangelogVersion(String),
    /// Registry versions loaded in background
    RegistryVersionsLoaded(Vec<crate::registry_client::ImageVersion>),
    /// Available tags loaded from registry for the tag selector
    AvailableTagsLoaded(Vec<crate::registry_client::AvailableTag>),
    /// Github commits loaded in background: (sha, message, author, date)
    GithubCommitsLoaded(Vec<(String, String, String, String)>),
    /// SBOM package diff loaded in background
    SbomDiffLoaded(crate::sbom_diff::SbomDiffResult),
    /// SBOM fetch finished but no diff was computable (e.g. the registry
    /// didn't publish SPDX referrers for one of the images). Used to swap
    /// the in-flight spinner placeholder out for a "not available" hint.
    SbomDiffUnavailable,
    /// SBOM fetch has been kicked off — switch the changelog Stack section
    /// to a "Comparing packages…" placeholder until SbomDiffLoaded or
    /// SbomDiffUnavailable fires.
    SbomDiffStarted,
    /// Unpin the booted system back to a floating stream tag. Opens a
    /// confirmation dialog; on confirm, runs `bootc switch <registry>:<stream>`
    /// and toasts the result.
    UnpinToStream(String),
    /// A module has started running (from orchestrator).
    ModuleStarted(crate::orchestrator::Module),
    /// A module has finished (from orchestrator).
    ModuleFinished(
        crate::orchestrator::Module,
        crate::orchestrator::ModuleStatus,
    ),
}

/// Output messages the StatusView sends to its parent.
#[derive(Debug)]
pub enum StatusViewOutput {
    /// User wants to trigger an update.
    StartUpdate,
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    Reboot,
    /// User wants to open the rollback/rebase dialog.
    ShowRebase,
    /// User wants to open the update check dialog.
    OpenCheckDialog,
    /// Notify parent when page changes
    PageChanged(String),
    /// User clicked the "Advanced…" row on the main page. Parent opens the
    /// Advanced PreferencesDialog which hosts Image Source / Image History /
    /// Rebase / Powerwash / Factory Reset / settings.
    OpenAdvanced,
}

/// The status view model.
pub struct StatusView {
    state: AppState,
    log_view: Controller<LogView>,
    update_list: Controller<UpdateList>,
    /// Direct reference to the root stack for page switching in update().
    stack: gtk::Stack,
    /// When the current update started (for elapsed timer).
    update_start: Option<Instant>,
    /// Label showing elapsed time during updates.
    elapsed_label: gtk::Label,
    /// Accumulated log text for clipboard copy.
    log_text: String,
    /// Toast overlay for copy confirmation.
    toast_overlay: adw::ToastOverlay,
    /// Root widget for the idle page.
    idle_page: adw::PreferencesPage,
    /// Hero row showing the current image summary.
    hero_row: adw::ActionRow,
    /// Status pill shown in the hero row suffix.
    status_pill: gtk::Label,
    /// Primary action button in the hero row — "Install" or "Restart"
    /// depending on state, hidden when neither applies. macOS Tahoe-inspired
    /// layout: put the CTA inline on the hero card.
    hero_action_btn: gtk::Button,
    /// "Restart Tonight" button on the hero row — only shown when
    /// reboot_pending. Schedules a 02:00 reboot via `pkexec shutdown -r`.
    hero_schedule_btn: gtk::Button,
    /// Banner group shown when action is needed.
    update_banner_group: adw::PreferencesGroup,
    /// Banner row with dynamic title/subtitle.
    banner_title_row: adw::ActionRow,
    /// Banner install button.
    banner_install_btn: gtk::Button,
    /// Banner whats new button.
    banner_whats_new_btn: gtk::Button,
    /// Banner restart button.
    banner_restart_btn: gtk::Button,
    /// Banner discard button.
    banner_discard_btn: gtk::Button,
    /// Automatic updates toggle in the settings card.
    auto_update_switch: adw::SwitchRow,
    /// Preflight check result.
    preflight_status: PreflightStatus,
    /// Cached last-update text.
    last_update_text: Option<String>,
    /// Cached image info text.
    image_info: Option<String>,
    /// Segmented progress bar shown while updating.
    seg_progress: SegmentedProgress,
    /// The module key that is currently active (drives segment coloring).
    active_module: Option<&'static str>,
    /// Whether an update has been staged and needs a reboot.
    reboot_pending: bool,

    // Redesigned settings & subpage state variables.
    // `registry_editing` and `reg_edit_btn` were removed when the manual
    // Change/Save/Cancel toggle was replaced by adw::EntryRow's built-in
    // apply-button affordance.
    registry_uri: String,
    selected_tag: String,
    deployments: Vec<MockDeployment>,
    expanded_deployment_id: Option<String>,
    changelog_version: String,
    registry_versions: Vec<crate::registry_client::ImageVersion>,
    github_commits: Vec<(String, String, String, String)>,
    sbom_diff: Option<crate::sbom_diff::SbomDiffResult>,
    sbom_status: SbomStatus,

    // Image Source subpage widget references for dynamic updates.
    // EntryRow keeps `text` always-editable (Apply on Enter / button click),
    // ComboRow holds the tag selection via a StringList model.
    registry_entry_row: adw::EntryRow,
    registry_row_sub: gtk::Label,
    tag_row: adw::ComboRow,
    tag_model: gtk::StringList,
    /// Parallel list of raw tag strings, indexed the same as `tag_model`'s
    /// display entries. `tag_model` shows pretty names ("Build 2026-05-15"
    /// for sha tags) while bootc switch needs the actual sha — we look it up
    /// here on selection.
    tag_raws: Rc<RefCell<Vec<String>>>,
    history_list_box: gtk::ListBox,
    images_count_label: gtk::Label,
    changelog_box: gtk::Box,
    changelog_install_bar: gtk::Box,
    /// "Pinned to {tag}" front-page group — visible only when the booted tag
    /// is a specific build (date or sha) rather than a floating stream.
    /// Surfaces a one-click Unpin button back to the family's stream tag.
    pin_group: adw::PreferencesGroup,
    pin_row: adw::ActionRow,

    // Dialog rollback state
    rollback_target: Option<MockDeployment>,
}

impl StatusView {
    fn hero_title(&self) -> String {
        self.image_info.clone().unwrap_or_else(|| {
            read_image_info()
                .or_else(|| detect_bootc_image_info().map(|(title, _, _)| title))
                .unwrap_or_else(|| "System Image".to_string())
        })
    }

    fn idle_subtitle(&self) -> String {
        // Per user direction: "Booted 3 days ago" wasn't insightful. Prefer
        // "VERSION · shaXXXXXXXX" from bootc-status (read_booted_image_summary)
        // so the user can see exactly which build is on disk. Falls back
        // through the cached last-update text, then a generic message.
        if self.reboot_pending {
            return "Reboot to update".to_string();
        }
        read_booted_image_summary()
            .or_else(|| self.last_update_text.clone())
            .unwrap_or_else(|| "Current image".to_string())
    }

    fn refresh_idle_description(&self) {
        // Pin-state UI mirrors the booted tag: shown when pinned, hidden
        // when the user is back on a floating stream.
        let pinned = is_pinned_tag(&self.selected_tag);
        self.pin_group.set_visible(pinned);
        if pinned {
            self.pin_row
                .set_title(&format!("Pinned to :{}", self.selected_tag));
        }

        self.hero_row.set_title(&self.hero_title());
        // Hero subtitle is the booted image summary (version · sha). The
        // previous code prefixed it with a tag-display ("latest · " or
        // "Version 43 · ") which was redundant with the version string in
        // the summary itself.
        self.hero_row.set_subtitle(&self.idle_subtitle());

        for class in ["accent", "success", "warning", "dim-label"] {
            self.status_pill.remove_css_class(class);
        }

        let (pill_text, pill_class) = if self.reboot_pending {
            ("Staged", "warning")
        } else {
            match self.preflight_status {
                PreflightStatus::UpdateAvailable => ("Update ready", "accent"),
                PreflightStatus::UpToDate => ("Up to date", "success"),
                PreflightStatus::Checking => ("Checking", "dim-label"),
                PreflightStatus::Unknown => ("Ready", "dim-label"),
            }
        };
        self.status_pill.set_label(pill_text);
        self.status_pill.add_css_class(pill_class);

        // ── Hero-row action button + info icon ────────────────────────────
        // Inline-CTA pattern from macOS Tahoe Software Update: action button
        // sits on the same row as the OS identity, label swaps by state.
        // - update available → "Install" (.suggested-action)
        // - reboot pending  → "Restart"  (.suggested-action)
        // - up-to-date / checking → hidden, status_pill takes the slot

        // Hero action buttons (Install / Restart / Restart Tonight) are now
        // RESERVED for the reboot_pending state per user direction. When an
        // update is merely available (not yet installed), the action lives
        // on the banner row below. Hero stays minimal: just identity + (i).
        if self.reboot_pending {
            self.hero_action_btn.set_label("Restart");
            self.hero_action_btn.set_visible(true);
            self.hero_schedule_btn.set_visible(true);
            self.status_pill.set_visible(false);
        } else {
            self.hero_action_btn.set_visible(false);
            self.hero_schedule_btn.set_visible(false);
            self.status_pill.set_visible(!matches!(
                self.preflight_status,
                PreflightStatus::UpdateAvailable
            ));
        }

        // ── Banner group ──────────────────────────────────────────────────
        // The banner row now carries the Install button + a circular (i)
        // info button for the changelog, per user direction "the install
        // button and the light bulb may be moved down to be in the row
        // with the update available setting". The hero row's (i) goes to
        // Image Source instead; the banner's (i) goes to the changelog.
        if self.reboot_pending {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Reboot to finish updating");
            self.banner_title_row
                .set_subtitle("A new image is staged and will be used on next boot.");
            self.banner_install_btn.set_visible(false);
            self.banner_whats_new_btn.set_visible(true);
            self.banner_restart_btn.set_visible(false);
            self.banner_discard_btn.set_visible(true);
        } else if matches!(self.preflight_status, PreflightStatus::UpdateAvailable) {
            self.update_banner_group.set_visible(true);
            self.banner_title_row.set_title("Update available");
            self.banner_title_row
                .set_subtitle("A new system image is ready to install.");
            self.banner_install_btn.set_visible(true);
            self.banner_whats_new_btn.set_visible(true);
            self.banner_restart_btn.set_visible(false);
            self.banner_discard_btn.set_visible(false);
        } else {
            self.update_banner_group.set_visible(false);
        }
    }

    fn rebuild_changelog_page(&self, sender: &ComponentSender<StatusView>) {
        while let Some(child) = self.changelog_box.first_child() {
            self.changelog_box.remove(&child);
        }

        let version = self.changelog_version.as_str();

        // Show a loading indicator until versions arrive
        if self.registry_versions.is_empty() {
            let spinner = gtk::Spinner::new();
            spinner.set_spinning(true);
            spinner.set_size_request(24, 24);
            let load_label = gtk::Label::new(Some("Loading versions…"));
            load_label.add_css_class("dim-label");
            load_label.add_css_class("caption");
            let load_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            load_box.set_halign(gtk::Align::Center);
            load_box.append(&spinner);
            load_box.append(&load_label);
            self.changelog_box.append(&load_box);
        }

        let mut real_version: Option<&ImageVersion> = None;
        if !self.registry_versions.is_empty() {
            real_version = self
                .registry_versions
                .iter()
                .find(|v| v.version == self.changelog_version);
        }

        // Booted version — pulled from bootc-status's booted image ref (with
        // os-release fallback for Dakota where bootc-status fails). Anchors
        // the "from" side of the Stack diff. None when the booted tag can't
        // be matched against the recently-fetched registry_versions window.
        let booted_version: Option<&ImageVersion> = read_booted_tag_suffix()
            .as_deref()
            .and_then(|t| find_booted_match(&self.registry_versions, t));

        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header_box.set_margin_top(12);

        let info_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        info_box.set_hexpand(true);

        let tag_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let tag_code = gtk::Label::builder()
            .label(&format!("{}:{}", self.registry_uri, version))
            .halign(gtk::Align::Start)
            .build();
        tag_code.add_css_class("monospace");
        tag_box.append(&tag_code);

        // Pills in header
        let is_update = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            v.version != booted_tag
                && !self.reboot_pending
                && matches!(self.preflight_status, PreflightStatus::UpdateAvailable)
        } else {
            false
        };

        if is_update {
            let update_pill = gtk::Label::new(Some("Update"));
            update_pill.add_css_class("accent");
            update_pill.add_css_class("caption");
            tag_box.append(&update_pill);
        } else {
            let is_booted = if let Some(v) = real_version {
                let booted_tag = read_selected_tag();
                v.version == booted_tag
            } else {
                false
            };
            if is_booted {
                let booted_pill = gtk::Label::new(Some("✓ Booted"));
                booted_pill.add_css_class("success");
                booted_pill.add_css_class("caption");
                tag_box.append(&booted_pill);
            }
        }
        info_box.append(&tag_box);

        let stable_str = if let Some(v) = real_version {
            v.version.clone()
        } else {
            version.to_string()
        };
        let date_str = if let Some(v) = real_version {
            v.created.format("%B %-d, %Y").to_string()
        } else {
            "".to_string()
        };
        let meta_label = gtk::Label::builder()
            .label(&format!("{}  ·  {}", stable_str, date_str))
            .halign(gtk::Align::Start)
            .build();
        meta_label.add_css_class("caption");
        meta_label.add_css_class("dim-label");
        info_box.append(&meta_label);

        let summary_str = if let Some(v) = real_version {
            let booted_tag = read_selected_tag();
            if v.version == booted_tag {
                format!(
                    "Currently booted. Kernel {} · stable point release.",
                    v.kernel
                )
            } else {
                format!(
                    "Image build. Kernel {} · git commit {}.",
                    v.kernel,
                    if v.revision.len() >= 7 {
                        &v.revision[0..7]
                    } else {
                        &v.revision
                    }
                )
            }
        } else {
            "".to_string()
        };
        let summary_label = gtk::Label::builder()
            .label(&summary_str)
            .halign(gtk::Align::Start)
            .wrap(true)
            .max_width_chars(60)
            .build();
        summary_label.add_css_class("body");
        info_box.append(&summary_label);

        header_box.append(&info_box);

        if is_update {
            let install_btn = gtk::Button::builder()
                .label("Install")
                .icon_name("object-select-symbolic")
                .build();
            install_btn.add_css_class("suggested-action");
            install_btn.set_valign(gtk::Align::Center);
            let out_sender = sender.output_sender().clone();
            install_btn.connect_clicked(move |_| {
                let _ = out_sender.send(StatusViewOutput::StartUpdate);
            });
            header_box.append(&install_btn);
        }

        self.changelog_box.append(&header_box);

        let stack_title = gtk::Label::builder()
            .label("Stack")
            .halign(gtk::Align::Start)
            .margin_top(12)
            .build();
        stack_title.add_css_class("caption");
        stack_title.add_css_class("dim-label");
        self.changelog_box.append(&stack_title);

        // Build "from → to" rows for the Stack. The user is comparing the
        // booted build (left) against the selected target (right), so each
        // component renders as `current → target` with the target highlighted
        // when it differs. Booted info is missing when bootc-status couldn't
        // be read or when the booted tag is outside the registry window — in
        // that case we degrade to "—" for the current side so the row still
        // makes sense. `host_kernel` (uname -r) backstops the Kernel row for
        // Dakota, whose registry-side kernel is empty.
        let host_kernel = get_host_kernel();
        let mut stack_items = build_stack_items(
            booted_version,
            real_version,
            if host_kernel == "—" {
                None
            } else {
                Some(host_kernel.as_str())
            },
        );

        if self.sbom_diff.is_some() {
            stack_items.retain(|item| item.label != "Kernel");
        }

        if !stack_items.is_empty() {
            let stack_list = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            stack_list.add_css_class("card");

            for item in &stack_items {
                let row = adw::ActionRow::builder().title(item.label).build();

                let diff_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                diff_box.set_valign(gtk::Align::Center);

                let from_lbl = gtk::Label::new(Some(item.current.as_deref().unwrap_or("—")));
                from_lbl.add_css_class("monospace");
                from_lbl.add_css_class("caption");
                from_lbl.add_css_class("dim-label");
                diff_box.append(&from_lbl);

                let arrow_lbl = gtk::Label::new(Some("→"));
                arrow_lbl.add_css_class("dim-label");
                diff_box.append(&arrow_lbl);

                let to_lbl = gtk::Label::new(Some(&item.target));
                to_lbl.add_css_class("monospace");
                to_lbl.add_css_class("caption");
                if item.bumped {
                    to_lbl.add_css_class("success");
                } else {
                    to_lbl.add_css_class("dim-label");
                }
                diff_box.append(&to_lbl);

                row.add_suffix(&diff_box);
                stack_list.append(&row);
            }

            if let Some(ref diff) = self.sbom_diff {
                let targets = [
                    ("Kernel", vec!["Kernel"]),
                    ("Gnome", vec!["GNOME", "Gnome"]),
                    ("Mesa", vec!["Mesa"]),
                    ("Podman", vec!["Podman"]),
                    ("Nvidia", vec!["Nvidia", "NVIDIA"]),
                    ("bootc", vec!["bootc"]),
                    ("systemd", vec!["systemd"]),
                    ("pipewire", vec!["pipewire"]),
                    ("flatpak", vec!["Flatpak", "flatpak"]),
                ];
                for &(label, ref keys) in &targets {
                    let mut found = None;
                    for key in keys {
                        if let Some(versions) = diff.stack_info.get(*key) {
                            found = Some(versions);
                            break;
                        }
                    }
                    if let Some((booted_ver, target_ver)) = found {
                        let row = adw::ActionRow::builder().title(label).build();

                        let diff_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                        diff_box.set_valign(gtk::Align::Center);

                        let current = if booted_ver.is_empty() { "—" } else { booted_ver.as_str() };
                        let from_lbl = gtk::Label::new(Some(current));
                        from_lbl.add_css_class("monospace");
                        from_lbl.add_css_class("caption");
                        from_lbl.add_css_class("dim-label");
                        diff_box.append(&from_lbl);

                        let arrow_lbl = gtk::Label::new(Some("→"));
                        arrow_lbl.add_css_class("dim-label");
                        diff_box.append(&arrow_lbl);

                        let to_lbl = gtk::Label::new(Some(target_ver.as_str()));
                        to_lbl.add_css_class("monospace");
                        to_lbl.add_css_class("caption");
                        let bumped = booted_ver != target_ver;
                        if bumped {
                            to_lbl.add_css_class("success");
                        } else {
                            to_lbl.add_css_class("dim-label");
                        }
                        diff_box.append(&to_lbl);

                        row.add_suffix(&diff_box);
                        stack_list.append(&row);
                    }
                }
            }

            self.changelog_box.append(&stack_list);
        }

        let mut upgrades_list: Vec<(String, String, String)> = Vec::new();
        let mut added_list: Vec<(String, String)> = Vec::new();
        let mut removals_list: Vec<String> = Vec::new();

        if let Some(ref diff) = self.sbom_diff {
            for pkg in &diff.upgraded {
                upgrades_list.push((
                    pkg.name.clone(),
                    pkg.old_version.clone(),
                    pkg.new_version.clone(),
                ));
            }
            for pkg in &diff.added {
                added_list.push((pkg.name.clone(), pkg.new_version.clone()));
            }
            for pkg in &diff.removed {
                removals_list.push(pkg.clone());
            }
        }

        // SBOM placeholder: surface the in-flight state so the user knows the
        // package diff is loading. Without this the Stack section is silently
        // blank for 30+ seconds on a slow connection. Skipped once the diff
        // lands and we have real upgrades/removals to render below.
        match self.sbom_status {
            SbomStatus::Loading => {
                let title = gtk::Label::builder()
                    .label("Package changes")
                    .halign(gtk::Align::Start)
                    .margin_top(12)
                    .build();
                title.add_css_class("caption");
                title.add_css_class("dim-label");
                self.changelog_box.append(&title);

                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.set_margin_start(12);
                row.set_margin_end(12);
                row.set_margin_top(8);
                row.set_margin_bottom(8);
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(true);
                spinner.set_size_request(16, 16);
                let lbl = gtk::Label::new(Some("Comparing packages…"));
                lbl.add_css_class("dim-label");
                lbl.add_css_class("caption");
                row.append(&spinner);
                row.append(&lbl);

                let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
                placeholder.add_css_class("card");
                placeholder.append(&row);
                self.changelog_box.append(&placeholder);
            }
            SbomStatus::NotAvailable => {
                let title = gtk::Label::builder()
                    .label("Package changes")
                    .halign(gtk::Align::Start)
                    .margin_top(12)
                    .build();
                title.add_css_class("caption");
                title.add_css_class("dim-label");
                self.changelog_box.append(&title);

                let lbl = gtk::Label::builder()
                    .label("Package diff not available — registry didn't publish an SPDX SBOM for one of the images.")
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .max_width_chars(70)
                    .margin_start(12)
                    .margin_end(12)
                    .margin_top(8)
                    .margin_bottom(8)
                    .build();
                lbl.add_css_class("dim-label");
                lbl.add_css_class("caption");
                let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
                placeholder.add_css_class("card");
                placeholder.append(&lbl);
                self.changelog_box.append(&placeholder);
            }
            SbomStatus::Pending | SbomStatus::Loaded => {}
        }

        if !upgrades_list.is_empty() {
            let upgrades_title = gtk::Label::builder()
                .label(&format!("Updated  ·  {}", upgrades_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            upgrades_title.add_css_class("caption");
            upgrades_title.add_css_class("dim-label");
            self.changelog_box.append(&upgrades_title);

            let list_upgrades = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_upgrades.add_css_class("card");

            for (pkg, from, to) in upgrades_list {
                let row = adw::ActionRow::builder().title(&pkg).build();

                let val_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);

                let from_lbl = gtk::Label::new(Some(&from));
                from_lbl.add_css_class("dim-label");
                from_lbl.add_css_class("monospace");
                from_lbl.add_css_class("caption");

                let arr_lbl = gtk::Label::new(Some("→"));
                arr_lbl.add_css_class("dim-label");

                // Highlight the target (new) version in success-green so the
                // upgrade is visually obvious — mirrors the bluefin-changelog
                // TUI's `changed` styling. The `from` side stays dim.
                let to_lbl = gtk::Label::new(Some(&to));
                to_lbl.add_css_class("monospace");
                to_lbl.add_css_class("caption");
                to_lbl.add_css_class("success");

                val_box.append(&from_lbl);
                val_box.append(&arr_lbl);
                val_box.append(&to_lbl);

                row.add_suffix(&val_box);
                list_upgrades.append(&row);
            }
            self.changelog_box.append(&list_upgrades);
        }

        if !added_list.is_empty() {
            let added_title = gtk::Label::builder()
                .label(&format!("Added  ·  {}", added_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            added_title.add_css_class("caption");
            added_title.add_css_class("dim-label");
            self.changelog_box.append(&added_title);

            let list_added = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_added.add_css_class("card");

            for (pkg, ver) in added_list {
                let row = adw::ActionRow::builder().title(&pkg).build();

                // Prefix with a green `+` so additions stand out from
                // upgrades. ActionRow doesn't natively style the prefix
                // glyph — a Label with the `success` class does the job.
                let plus_lbl = gtk::Label::new(Some("+"));
                plus_lbl.add_css_class("success");
                plus_lbl.add_css_class("monospace");
                row.add_prefix(&plus_lbl);

                let ver_lbl = gtk::Label::new(Some(&ver));
                ver_lbl.add_css_class("monospace");
                ver_lbl.add_css_class("caption");
                ver_lbl.add_css_class("success");
                row.add_suffix(&ver_lbl);

                list_added.append(&row);
            }
            self.changelog_box.append(&list_added);
        }

        if !removals_list.is_empty() {
            let removals_title = gtk::Label::builder()
                .label(&format!("Removed  ·  {}", removals_list.len()))
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            removals_title.add_css_class("caption");
            removals_title.add_css_class("dim-label");
            self.changelog_box.append(&removals_title);

            let list_removals = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_removals.add_css_class("card");

            for pkg in removals_list {
                let row = adw::ActionRow::builder().title(&pkg).build();
                let dash_lbl = gtk::Label::new(Some("−"));
                dash_lbl.add_css_class("error");
                row.add_prefix(&dash_lbl);
                list_removals.append(&row);
            }
            self.changelog_box.append(&list_removals);
        }

        let commits_list: Vec<(String, String, String, String)> = self.github_commits.clone();

        if !commits_list.is_empty() {
            let commits_title = gtk::Label::builder()
                .label("Commits")
                .halign(gtk::Align::Start)
                .margin_top(12)
                .build();
            commits_title.add_css_class("caption");
            commits_title.add_css_class("dim-label");
            self.changelog_box.append(&commits_title);

            let list_commits = gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .build();
            list_commits.add_css_class("card");

            // Build GitHub URL from registry URI org/repo
            let github_url = parse_org_repo(&self.registry_uri)
                .map(|(org, repo)| format!("https://github.com/{}/{}", org, repo));

            for (sha, msg, author, date) in commits_list {
                let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row_box.set_margin_start(16);
                row_box.set_margin_end(16);
                row_box.set_margin_top(8);
                row_box.set_margin_bottom(8);

                let sha_short = if sha.len() >= 7 { &sha[0..7] } else { &sha };
                let sha_lbl = gtk::Label::new(Some(sha_short));
                sha_lbl.add_css_class("monospace");
                sha_lbl.add_css_class("caption");
                sha_lbl.add_css_class("dim-label");
                sha_lbl.set_valign(gtk::Align::Start);
                row_box.append(&sha_lbl);

                let msg_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
                msg_box.set_hexpand(true);

                let msg_lbl = gtk::Label::builder()
                    .label(&msg)
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .build();
                msg_lbl.add_css_class("body");
                msg_box.append(&msg_lbl);

                // Parse ISO8601 date and format as "MMM D, YYYY"
                let date_str = if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(&date) {
                    parsed.format("%b %d, %Y").to_string()
                } else {
                    date.clone()
                };

                let auth_lbl = gtk::Label::builder()
                    .label(&format!("{} · {}", author, date_str))
                    .halign(gtk::Align::Start)
                    .build();
                auth_lbl.add_css_class("caption");
                auth_lbl.add_css_class("dim-label");
                msg_box.append(&auth_lbl);

                row_box.append(&msg_box);

                // Whole-row click opens the GitHub commit page in the user's
                // default browser. Use gtk::UriLauncher (sandbox-aware xdg
                // portal call) rather than `xdg-open` directly — the bare
                // command silently no-ops inside a Flatpak because the
                // portal isn't on PATH from the sandbox.
                if let Some(ref base_url) = github_url {
                    let commit_url = format!("{}/commit/{}", base_url, sha);
                    let gesture = gtk::GestureClick::new();
                    let row_for_gesture = row_box.clone();
                    gesture.connect_pressed(move |_, _, _, _| {
                        let launcher = gtk::UriLauncher::new(&commit_url);
                        let parent = row_for_gesture
                            .root()
                            .and_then(|r| r.downcast::<gtk::Window>().ok());
                        launcher.launch(parent.as_ref(), gtk::gio::Cancellable::NONE, |result| {
                            if let Err(e) = result {
                                tracing::warn!("Couldn't open commit URL: {}", e);
                            }
                        });
                    });
                    row_box.add_controller(gesture);
                    row_box.set_cursor_from_name(Some("pointer"));
                }

                list_commits.append(&row_box);
            }
            self.changelog_box.append(&list_commits);
        }

        if is_update {
            self.changelog_install_bar.set_visible(true);
        } else {
            self.changelog_install_bar.set_visible(false);
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for StatusView {
    type Init = AppState;
    type Input = StatusViewInput;
    type Output = StatusViewOutput;

    view! {
        #[root]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,
            set_transition_duration: 200,

            // ─── Idle page ──────────────────────────────────────────────
            add_child = &model.idle_page.clone() -> adw::PreferencesPage {} -> {
                set_name: "idle",
            },

            // ─── Updating page ──────────────────────────────────────────
            add_child = &model.toast_overlay.clone() -> adw::ToastOverlay {} -> {
                set_name: "updating",
            },

            // ─── Complete page ──────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("object-select-symbolic"),
                set_title: "Update Complete",
                set_description: Some("Restart to apply changes."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 8,

                    gtk::Button {
                        set_label: "Restart…",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::Reboot).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Restart Later",
                        add_css_class: "flat",
                        connect_clicked[sender] => move |_| {
                            sender.input(StatusViewInput::StateChanged(AppState::Idle));
                        },
                    },
                },
            } -> {
                set_name: "complete",
            },

            // ─── Up to date page ────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("emblem-ok-symbolic"),
                set_title: "Up to Date",
                set_description: Some("No updates available."),

                #[wrap(Some)]
                set_child = &gtk::Button {
                    set_label: "Done",
                    add_css_class: "pill",
                    set_halign: gtk::Align::Center,
                    connect_clicked[sender] => move |_| {
                        sender.input(StatusViewInput::StateChanged(AppState::Idle));
                    },
                },
            } -> {
                set_name: "up_to_date",
            },

            // ─── Error page ─────────────────────────────────────────────
            add_child = &adw::StatusPage {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_title: "Update Failed",
                set_description: Some("Something went wrong."),

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_halign: gtk::Align::Center,
                    set_spacing: 8,

                    gtk::Button {
                        set_label: "Retry",
                        add_css_class: "suggested-action",
                        add_css_class: "pill",
                        connect_clicked[sender] => move |_| {
                            sender.output(StatusViewOutput::StartUpdate).unwrap();
                        },
                    },

                    gtk::Button {
                        set_label: "Dismiss",
                        add_css_class: "flat",
                        connect_clicked[sender] => move |_| {
                            sender.input(StatusViewInput::StateChanged(AppState::Idle));
                        },
                    },
                },
            } -> {
                set_name: "error",
            },
        }
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let log_view = LogView::builder().launch(()).detach();
        let update_list = UpdateList::builder().launch(()).detach();

        let elapsed_label = gtk::Label::new(Some("0:00"));
        elapsed_label.add_css_class("dim-label");
        elapsed_label.add_css_class("caption");
        elapsed_label.add_css_class("monospace");

        let toast_overlay = adw::ToastOverlay::new();

        // ── Idle page (built imperatively) ──────────────────────────────
        let initial_image_info = read_image_info();
        let initial_registry_uri = read_registry_uri().unwrap_or_else(|| String::new());
        let initial_selected_tag = read_selected_tag();
        let initial_last_update = get_last_update_time();
        let auto_updates_enabled = read_auto_updates_enabled();
        // Hero subtitle on first paint: build it from the booted image
        // summary if available (matches what idle_subtitle() returns once
        // we hit the first state-update). Same source — bootc-status JSON.
        let initial_subtitle = read_booted_image_summary()
            .or_else(|| initial_last_update.clone())
            .unwrap_or_else(|| "Current image".to_string());

        // adw::PreferencesPage gives us HIG-standard scrolling, clamp width,
        // and margins for free — same chrome gnome-control-center uses on its
        // settings panels. Groups are added via `.add(&group)` below.
        let idle_page = adw::PreferencesPage::new();

        let hero_group = adw::PreferencesGroup::new();
        let hero_row = adw::ActionRow::builder()
            .title(initial_image_info.as_deref().unwrap_or("System Image"))
            .subtitle(&initial_subtitle)
            // Two-line subtitle: image ref on line 1, digest + build date on
            // line 2. parse_booted_image_summary() splits with `\n`; set to 2
            // so ActionRow doesn't ellipsis-clip the second line.
            .subtitle_lines(2)
            // Per user direction: the SHA needs to be copyable so people
            // can paste it into diagnostics / bug reports. subtitle_selectable
            // is the gnome-control-center About-panel idiom (cc-about-page.blp
            // uses it on every property row); enables drag-to-select + the
            // context-menu Copy entry on the subtitle.
            .subtitle_selectable(true)
            .title_selectable(true)
            .build();
        hero_row.set_activatable(false);

        // Plain symbolic icon in the accent color — same pattern as
        // gnome-control-center's PreferencesRow prefixes. No gradient box.
        let logo_name = read_logo_icon_name();
        let hero_icon = gtk::Image::from_icon_name(&logo_name);
        hero_icon.set_pixel_size(32);
        hero_icon.add_css_class("accent");
        hero_row.add_prefix(&hero_icon);

        // "Change" button — opens the rebase dialog to switch to a different image.
        // Placed in the suffix area next to the other action buttons.
        let hero_change_btn = gtk::Button::with_label("Change");
        hero_change_btn.add_css_class("flat");
        hero_change_btn.set_tooltip_text(Some("Change image variant or stream"));
        hero_change_btn.set_valign(gtk::Align::Center);
        let change_sender = sender.output_sender().clone();
        hero_change_btn.connect_clicked(move |_| {
            change_sender.emit(StatusViewOutput::ShowRebase);
        });
        hero_row.add_suffix(&hero_change_btn);

        // macOS Tahoe-inspired layout: action buttons live inline on the hero
        // row, not in a separate banner. Status text + buttons share the
        // suffix area; update()'s state machine toggles which controls show.
        //
        // Status indicator — plain colored caption text. gnome-control-center
        // About uses the same idiom for state. Shown when idle / checking /
        // up-to-date; hidden when action buttons take its place.
        let status_pill = gtk::Label::new(Some("Checking"));
        status_pill.add_css_class("caption");
        status_pill.add_css_class("dim-label");
        status_pill.set_valign(gtk::Align::Center);
        hero_row.add_suffix(&status_pill);

        // "Restart Tonight" — scheduled-reboot button shown only when a
        // deployment is staged (reboot_pending). Schedules the host to reboot
        // at 02:00 (next occurrence) via `shutdown -r 02:00`, matching macOS
        // Software Update's "Update Tonight" affordance — but limited to the
        // reboot step only, no install scheduling (user direction).
        let hero_schedule_btn = gtk::Button::with_label("Restart Tonight");
        hero_schedule_btn.set_valign(gtk::Align::Center);
        hero_schedule_btn.set_visible(false);
        let schedule_sender = sender.input_sender().clone();
        hero_schedule_btn.connect_clicked(move |_| {
            schedule_sender.emit(StatusViewInput::ScheduleRebootTonight);
        });
        hero_row.add_suffix(&hero_schedule_btn);

        // Primary action button — Install when an update is available, Restart
        // when a deployment is staged for reboot. Same widget, label/handler
        // swap in update().
        let hero_action_btn = gtk::Button::with_label("Install");
        hero_action_btn.add_css_class("suggested-action");
        hero_action_btn.set_valign(gtk::Align::Center);
        hero_action_btn.set_visible(false);
        // Single click handler, state-aware dispatch in update(). Avoids the
        // bookkeeping of swapping closures when the label flips Install↔Restart.
        let hero_action_sender = sender.input_sender().clone();
        hero_action_btn.connect_clicked(move |_| {
            hero_action_sender.emit(StatusViewInput::HeroActionClicked);
        });
        hero_row.add_suffix(&hero_action_btn);

        hero_group.add(&hero_row);
        idle_page.add(&hero_group);

        // ── Pin group ─────────────────────────────────────────────────────
        // Surfaced when the user is booted on a specific build (date tag,
        // sha tag, etc.) rather than a floating stream. One-click "Unpin"
        // switches back to the family's default stream tag so auto-updates
        // resume. Hidden when the booted tag IS already a stream.
        let pin_group = adw::PreferencesGroup::new();
        let pin_row = adw::ActionRow::builder()
            .title("Pinned to a specific build")
            .subtitle("Automatic updates are paused. Unpin to resume.")
            .build();
        pin_row.set_activatable(false);
        let pin_icon = gtk::Image::from_icon_name("emblem-important-symbolic");
        pin_icon.set_pixel_size(20);
        pin_icon.add_css_class("warning");
        pin_row.add_prefix(&pin_icon);

        let unpin_btn = gtk::Button::with_label("Unpin");
        unpin_btn.add_css_class("suggested-action");
        unpin_btn.set_valign(gtk::Align::Center);
        let unpin_sender = sender.input_sender().clone();
        unpin_btn.connect_clicked(move |_| {
            // For now default to "latest" as the unpin target. Most families
            // use it; LTS users will need to pick a different stream via the
            // Change → rebase dialog (follow-up: query current_family() for
            // the canonical stream and switch on that).
            unpin_sender.emit(StatusViewInput::UnpinToStream("latest".to_string()));
        });
        pin_row.add_suffix(&unpin_btn);
        pin_group.add(&pin_row);
        pin_group.set_visible(is_pinned_tag(&initial_selected_tag));
        idle_page.add(&pin_group);

        // Banner group (visually distinct second card) is kept for the
        // descriptive paragraph + Discard action when a deployment is staged
        // — the things that don't fit in the compact hero suffix area.
        let update_banner_group = adw::PreferencesGroup::new();
        let banner_title_row = adw::ActionRow::builder()
            .title("Update available")
            .subtitle("A new system image is ready to install.")
            .build();
        banner_title_row.set_activatable(false);

        let banner_icon = gtk::Image::from_icon_name("software-update-available-symbolic");
        banner_icon.set_pixel_size(24);
        banner_icon.add_css_class("accent");
        banner_title_row.add_prefix(&banner_icon);

        // Keep restart + discard as banner-row suffixes so the staged-reboot
        // flow keeps its prominent buttons. Install moved to the hero row.
        // Circular list button shows the changelog of what's new in this update.
        // Uses view-list-symbolic to indicate it shows a detailed list of changes.
        let banner_whats_new_btn = gtk::Button::from_icon_name("view-list-symbolic");
        banner_whats_new_btn.add_css_class("flat");
        banner_whats_new_btn.add_css_class("circular");
        banner_whats_new_btn.set_tooltip_text(Some("What's new in this update"));
        banner_whats_new_btn.set_valign(gtk::Align::Center);
        let initial_selected_tag_3 = initial_selected_tag.clone();
        let whats_new_sender_2 = sender.input_sender().clone();
        banner_whats_new_btn.connect_clicked(move |_| {
            let ver = initial_selected_tag_3.clone();
            whats_new_sender_2.emit(StatusViewInput::SelectChangelogVersion(ver));
        });

        let banner_install_btn = gtk::Button::with_label("Install");
        banner_install_btn.add_css_class("accent");
        banner_install_btn.set_valign(gtk::Align::Center);
        let install_sender_2 = sender.output_sender().clone();
        banner_install_btn.connect_clicked(move |_| {
            let _ = install_sender_2.send(StatusViewOutput::StartUpdate);
        });

        let banner_restart_btn = gtk::Button::with_label("Restart");
        banner_restart_btn.add_css_class("accent");
        banner_restart_btn.set_valign(gtk::Align::Center);
        let restart_sender = sender.output_sender().clone();
        banner_restart_btn.connect_clicked(move |_| {
            let _ = restart_sender.send(StatusViewOutput::Reboot);
        });

        let banner_discard_btn = gtk::Button::with_label("Discard");
        banner_discard_btn.add_css_class("flat");
        let discard_sender = sender.input_sender().clone();
        banner_discard_btn.connect_clicked(move |_| {
            discard_sender.emit(StatusViewInput::DismissBanner);
        });

        banner_title_row.add_suffix(&banner_whats_new_btn);
        banner_title_row.add_suffix(&banner_install_btn);
        banner_title_row.add_suffix(&banner_restart_btn);
        banner_title_row.add_suffix(&banner_discard_btn);
        update_banner_group.add(&banner_title_row);
        update_banner_group.set_visible(false);
        idle_page.add(&update_banner_group);

        // Boxed List Settings Card (Left sidebar settings style)
        let check_row = adw::ActionRow::builder()
            .title("_Check for updates")
            .subtitle("System image, Flatpak, Homebrew, and Distrobox")
            .use_underline(true)
            .build();
        let check_btn = gtk::Button::with_label("Check");
        check_btn.set_valign(gtk::Align::Center);
        let check_sender = sender.output_sender().clone();
        check_btn.connect_clicked(move |_| {
            let _ = check_sender.send(StatusViewOutput::OpenCheckDialog);
        });
        check_row.add_suffix(&check_btn);

        // adw::SwitchRow (rather than ActionRow + Switch suffix) so the
        // entire row is the click target — matches gnome-control-center's
        // Privacy / Sharing toggles. Also gives us correct AT-SPI semantics
        // (it announces as a switch, not a generic list item).
        let auto_row = adw::SwitchRow::builder()
            .title("_Automatic updates")
            .subtitle("Refresh in the background on the systemd timer")
            .use_underline(true)
            .active(auto_updates_enabled)
            .build();
        let auto_update_switch = auto_row.clone();
        auto_row.connect_active_notify(move |row| {
            apply_auto_updates_setting(row.is_active());
        });

        // ── Main page is intentionally minimal ────────────────────────────
        // Per user direction (macOS Software Update model): only Check +
        // Automatic Updates on the main view. Image Source / Image History /
        // Powerwash / Factory Reset all move to the hamburger menu (still
        // one click away). Keeps the visual focus on "do I need to update?".
        //
        // The widgets below (source_row, history_row, powerwash_row,
        // factory_row, registry_row_sub, images_count_label) are still
        // constructed because the model fields reference them and update()
        // mutates their labels — but they're NOT added to idle_page, so they
        // never render on the main view. They live as orphaned widgets that
        // accept set_label calls; cheap and avoids a bigger refactor of the
        // update() method's text-mutation paths.
        let registry_row_sub = gtk::Label::new(Some(&format!(
            "{}:{}",
            initial_registry_uri, initial_selected_tag
        )));
        registry_row_sub.add_css_class("dim-label");
        let images_count_label = gtk::Label::new(Some("3 versions"));
        images_count_label.add_css_class("dim-label");

        let settings_card = adw::PreferencesGroup::new();
        settings_card.add(&check_row);
        settings_card.add(&auto_row);
        idle_page.add(&settings_card);

        // Single "Advanced…" row at the bottom — opens the Advanced dialog
        // (which hosts Image Source, Image History, Rebase, Powerwash,
        // Factory Reset, and the Updates / Network settings groups).
        // gnome-control-center doesn't bury panel-specific actions in the
        // hamburger menu; we follow the same convention.
        let advanced_row = adw::ActionRow::builder()
            .title("_Advanced")
            .subtitle("Image source, history, rollback, reset, and settings")
            .activatable(true)
            .use_underline(true)
            .build();
        advanced_row.set_accessible_role(gtk::AccessibleRole::Button);
        let adv_chev = gtk::Image::from_icon_name("go-next-symbolic");
        adv_chev.add_css_class("dim-label");
        advanced_row.add_suffix(&adv_chev);
        let advanced_sender = sender.output_sender().clone();
        advanced_row.connect_activated(move |_| {
            let _ = advanced_sender.send(StatusViewOutput::OpenAdvanced);
        });
        let advanced_group = adw::PreferencesGroup::new();
        advanced_group.add(&advanced_row);
        idle_page.add(&advanced_group);

        // ── Image Source Subpage (HIG-aligned) ────────────────────────────
        // adw::PreferencesPage + PreferencesGroup with canonical Adwaita
        // editing widgets: adw::EntryRow for the registry URL (always-
        // editable inline, Apply button on Enter), adw::ComboRow for the
        // tag picker (modern replacement for the deprecated ComboBoxText).
        // This is the same pattern gnome-control-center uses on its
        // Network → Wi-Fi properties and Online Accounts subpages.
        let source_page = adw::PreferencesPage::new();
        let source_group = adw::PreferencesGroup::builder()
            .description("Where this device pulls its OS image from. Changes apply on next update.")
            .build();

        // Registry URL — adw::EntryRow with show_apply_button=true gives us
        // a built-in ✓ apply button as suffix that fires the `apply` signal
        // on Enter or click. Drops the entire Edit/Save/Cancel toggle dance.
        let registry_entry_row = adw::EntryRow::builder()
            .title("Registry")
            .text(&initial_registry_uri)
            .show_apply_button(true)
            .build();
        let save_sender = sender.input_sender().clone();
        registry_entry_row.connect_apply(move |row| {
            save_sender.emit(StatusViewInput::SaveRegistryUri(row.text().to_string()));
        });
        source_group.add(&registry_entry_row);

        // Tag — adw::ComboRow with a StringList model. Selection notifies
        // via `selected-item` rather than the deprecated ComboBoxText's
        // `changed` signal. Reads slightly cleaner and matches the rest of
        // the app's Adwaita usage.
        let tag_row = adw::ComboRow::builder()
            .title("Tag")
            .subtitle("Always the newest stable release")
            .build();
        let tags = if let Some(config) = read_bootc_image_info_config() {
            config.tags
        } else {
            // Derive sensible defaults from the detected tag rather than showing
            // hardcoded version tags that don't apply to all OCI images.
            let cur = initial_selected_tag.clone();
            if !cur.is_empty() && cur != "latest" {
                vec!["latest".to_string(), cur]
            } else {
                vec!["latest".to_string()]
            }
        };
        let tag_model = gtk::StringList::new(&[]);
        // Display == raw for the bootstrap tags (`latest` / detected tag) —
        // no sha entries at construction time. The mapping evolves when
        // AvailableTagsLoaded fires with the real tag list from the registry.
        let tag_raws: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(tags.clone()));
        for t in &tags {
            tag_model.append(t);
        }
        tag_row.set_model(Some(&tag_model));
        let initial_idx = tags
            .iter()
            .position(|t| t == &initial_selected_tag)
            .unwrap_or(0) as u32;
        tag_row.set_selected(initial_idx);
        // Disable until the background fetch fills in real tags.
        tag_row.set_sensitive(tags.len() > 1);
        let select_sender = sender.input_sender().clone();
        let tag_raws_for_select = tag_raws.clone();
        tag_row.connect_selected_notify(move |row| {
            // Look up the raw tag by selected index — display strings may be
            // "Build YYYY-MM-DD" for sha-tagged manifests, but bootc switch
            // needs the actual sha-hex tag string.
            let idx = row.selected() as usize;
            if let Some(raw) = tag_raws_for_select.borrow().get(idx).cloned() {
                select_sender.emit(StatusViewInput::SelectTag(raw));
            }
        });
        source_group.add(&tag_row);

        // Signature row (read-only — sigstore policy is set at deployment
        // time, not via this UI). Plain ActionRow with a colored caption
        // suffix label, matching control-center About's "property" rows.
        let sig_row = adw::ActionRow::builder()
            .title("Require signed images")
            .subtitle("Only install images signed by the publisher.")
            .build();
        let sig_badge = gtk::Label::new(Some("✓ On"));
        sig_badge.add_css_class("success");
        sig_badge.add_css_class("caption");
        sig_badge.set_valign(gtk::Align::Center);
        sig_row.add_suffix(&sig_badge);
        source_group.add(&sig_row);

        source_page.add(&source_group);

        // ── Variants group: per-family feature toggles ────────────────────
        // User direction: NVIDIA / DX toggles should be visible on the
        // Image Source page so users can flip them without diving into
        // the rebase dialog. Same resolver the rebase dialog uses
        // (resolve_dx_nvidia) — toggling rewrites the registry entry to
        // the resolved image variant. The user then hits the entry's
        // Apply button (✓) to commit.
        let variants_group = adw::PreferencesGroup::builder()
            .title("Variants")
            .description(
                "Switch between feature variants of this image. \
                 Apply the registry change above to take effect on the next update.",
            )
            .build();
        let dx_switch = adw::SwitchRow::builder()
            .title("Developer Mode")
            .subtitle("Container tools, IDEs, and language SDKs")
            .build();
        let nvidia_switch = adw::SwitchRow::builder()
            .title("NVIDIA drivers")
            .subtitle("Required for NVIDIA GPUs")
            .build();
        variants_group.add(&dx_switch);
        variants_group.add(&nvidia_switch);
        source_page.add(&variants_group);

        // Wire the toggles to recompute the target image and write it back
        // into the registry EntryRow. Same two-stage pattern as the rebase
        // dialog's populate_family_switches: a non-GTK background thread
        // fetches (family, image) via the service, then a glib timeout
        // running on the GTK thread does all the widget mutations.
        // adw::* widgets are GObject (not Send), so they MUST be touched
        // from the GTK thread only.
        let slot: std::sync::Arc<
            std::sync::Mutex<
                Option<(
                    Option<crate::service::FamilyInfo>,
                    Option<crate::service::ImageRef>,
                )>,
            >,
        > = std::sync::Arc::new(std::sync::Mutex::new(None));
        {
            let slot = slot.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                let detected = rt.block_on(async {
                    let svc = crate::service::global();
                    let family = svc.current_family().await.ok().flatten();
                    let image = svc.current_image().await.ok();
                    (family, image)
                });
                *slot.lock().unwrap() = Some(detected);
            });
        }

        let dx_switch_for_timer = dx_switch.clone();
        let nvidia_switch_for_timer = nvidia_switch.clone();
        let variants_group_for_timer = variants_group.clone();
        let registry_entry_for_timer = registry_entry_row.clone();
        let registry_uri_initial = initial_registry_uri.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let Some((family_opt, image_opt)) = slot.lock().ok().and_then(|mut g| g.take()) else {
                return gtk::glib::ControlFlow::Continue;
            };

            let Some(fam) = family_opt else {
                // Unknown family — hide the toggles entirely.
                variants_group_for_timer.set_visible(false);
                return gtk::glib::ControlFlow::Break;
            };

            // Derive initial state from booted image's suffix.
            let suffix = image_opt
                .as_ref()
                .and_then(|i| i.image.strip_prefix(&format!("{}-", fam.base_image)))
                .map(|s| s.to_string())
                .unwrap_or_default();
            let initial_dx = suffix.split('-').any(|p| p == "dx");
            let initial_nvidia = suffix.split('-').any(|p| p == "nvidia" || p == "open");

            let supports_dx = fam.features.iter().any(|f| f.id == "dx");
            let supports_nvidia = fam
                .features
                .iter()
                .any(|f| f.id == "nvidia" || f.id == "open");

            dx_switch_for_timer.set_visible(supports_dx);
            nvidia_switch_for_timer.set_visible(supports_nvidia);
            dx_switch_for_timer.set_active(initial_dx);
            nvidia_switch_for_timer.set_active(initial_nvidia);
            variants_group_for_timer.set_visible(supports_dx || supports_nvidia);

            let recompute = {
                let dx_switch = dx_switch_for_timer.clone();
                let nvidia_switch = nvidia_switch_for_timer.clone();
                let entry = registry_entry_for_timer.clone();
                let registry_uri = registry_uri_initial.clone();
                let fam = fam.clone();
                move || {
                    let dx = dx_switch.is_active();
                    let nvidia = nvidia_switch.is_active();
                    let svc = crate::service::global();
                    let mut feats: Vec<String> = Vec::new();
                    if dx {
                        feats.push("dx".to_string());
                    }
                    if nvidia {
                        feats.push("nvidia".to_string());
                        feats.push("open".to_string());
                    }
                    let resolved = svc.resolve_target(&fam, &feats).or_else(|| {
                        if nvidia {
                            let mut plain = if dx { vec!["dx".to_string()] } else { vec![] };
                            plain.push("nvidia".to_string());
                            svc.resolve_target(&fam, &plain)
                        } else {
                            None
                        }
                    });
                    if let Some(target) = resolved {
                        let parts: Vec<&str> = registry_uri.split('/').collect();
                        if parts.len() >= 2 {
                            entry.set_text(&format!("{}/{}/{}", parts[0], parts[1], target.image));
                        }
                    }
                }
            };
            let rc = Rc::new(recompute);
            let rc2 = rc.clone();
            dx_switch_for_timer.connect_active_notify(move |_| rc2());
            let rc3 = rc.clone();
            nvidia_switch_for_timer.connect_active_notify(move |_| rc3());

            gtk::glib::ControlFlow::Break
        });

        root.add_named(&source_page, Some("source"));

        // ── Version History Subpage ──────────────────────────────────────
        // HIG-aligned: PreferencesPage + PreferencesGroup with the description
        // doubling as page-level explanation. Rows are appended dynamically as
        // bootc-status results come in (see rebuild_history_list).
        let history_page = adw::PreferencesPage::new();
        let history_group = adw::PreferencesGroup::builder()
            .description(
                "Past images stay on disk so you can roll back. Pin a version to keep it across upgrades.",
            )
            .build();
        // history_list_box is still a gtk::ListBox so the existing
        // rebuild_history_list code (which builds custom row widgets, not
        // ActionRows) keeps working unchanged. PreferencesGroup hosts it as a
        // single custom widget — same visual outcome, less plumbing.
        let history_list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        history_list_box.add_css_class("boxed-list");
        history_group.add(&history_list_box);
        history_page.add(&history_group);
        root.add_named(&history_page, Some("history"));

        // ── Changelogs Subpage ───────────────────────────────────────────
        let changelog_page = gtk::ScrolledWindow::new();
        changelog_page.set_hscrollbar_policy(gtk::PolicyType::Never);
        changelog_page.set_vexpand(true);
        let changelog_clamp = adw::Clamp::new();
        changelog_clamp.set_maximum_size(600);
        let changelog_content = gtk::Box::new(gtk::Orientation::Vertical, 16);
        changelog_content.set_margin_start(24);
        changelog_content.set_margin_end(24);
        changelog_content.set_margin_top(24);
        changelog_content.set_margin_bottom(24);
        changelog_clamp.set_child(Some(&changelog_content));
        changelog_page.set_child(Some(&changelog_clamp));

        // Pills version switcher (built dynamically in rebuild_changelog_page)
        let changelog_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        changelog_content.append(&changelog_box);

        // Dynamic Install Action bar on Changelog
        let changelog_install_bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        changelog_install_bar.set_margin_top(12);
        changelog_install_bar.set_margin_bottom(12);
        let ch_install_label = gtk::Label::new(Some("A newer version is available."));
        ch_install_label.add_css_class("caption");
        ch_install_label.add_css_class("dim-label");
        let ch_install_btn = gtk::Button::with_label("Install");
        ch_install_btn.add_css_class("suggested-action");
        let ch_inst_sender = sender.output_sender().clone();
        ch_install_btn.connect_clicked(move |_| {
            let _ = ch_inst_sender.send(StatusViewOutput::StartUpdate);
        });
        changelog_install_bar.append(&ch_install_label);
        changelog_install_bar.append(&ch_install_btn);
        changelog_install_bar.set_visible(false);
        changelog_content.append(&changelog_install_bar);

        root.add_named(&changelog_page, Some("changelog"));

        // Build the "updating" page content imperatively.
        let seg_progress = SegmentedProgress::new();

        // Image info label for the updating page header.
        let updating_image_label = gtk::Label::new(read_image_info().as_deref());
        updating_image_label.add_css_class("caption");
        updating_image_label.add_css_class("dim-label");
        updating_image_label.add_css_class("monospace");
        updating_image_label.set_margin_top(8);
        updating_image_label.set_margin_bottom(4);
        updating_image_label.set_visible(read_image_info().is_some());

        let log_clamp = adw::Clamp::new();
        log_clamp.set_maximum_size(800);
        log_clamp.set_vexpand(true);
        log_clamp.set_child(Some(log_view.widget()));

        // Icon-only Copy button (per user direction). gnome-control-center
        // uses the same `edit-copy-symbolic` glyph in `.flat .circular`
        // styling for inline copy affordances; the tooltip carries the
        // semantics.
        let copy_btn = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy_btn.set_tooltip_text(Some("Copy log output to clipboard"));
        copy_btn.add_css_class("flat");
        copy_btn.add_css_class("circular");
        let copy_sender = sender.input_sender().clone();
        copy_btn.connect_clicked(move |_| {
            copy_sender.emit(StatusViewInput::CopyLog);
        });

        let cancel_btn = gtk::Button::builder()
            .label("Cancel")
            .tooltip_text("Cancel the running update")
            .build();
        cancel_btn.add_css_class("destructive-action");
        let cancel_sender = sender.output_sender().clone();
        cancel_btn.connect_clicked(move |_| {
            let _ = cancel_sender.send(StatusViewOutput::CancelUpdate);
        });

        let bottom_bar = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        bottom_bar.set_halign(gtk::Align::Center);
        bottom_bar.set_margin_top(12);
        bottom_bar.set_margin_bottom(12);
        bottom_bar.append(&elapsed_label);
        bottom_bar.append(&copy_btn);
        bottom_bar.append(&cancel_btn);

        let updating_content = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // HIG: Clamp non-log content to consistent max-width (matches log_clamp).
        let header_clamp = adw::Clamp::new();
        header_clamp.set_maximum_size(800);
        let header_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        header_box.append(&seg_progress.widget());
        header_box.append(&updating_image_label);
        header_box.append(update_list.widget());
        header_clamp.set_child(Some(&header_box));

        updating_content.append(&header_clamp);
        updating_content.append(&log_clamp);
        updating_content.append(&bottom_bar);

        toast_overlay.set_child(Some(&updating_content));

        spawn_changelog_fetch(
            initial_registry_uri.clone(),
            initial_selected_tag.clone(),
            sender.clone(),
        );

        let initial_selected_tag_3 = initial_selected_tag.clone();
        let model = StatusView {
            state: init,
            log_view,
            update_list,
            stack: root.clone(),
            update_start: None,
            elapsed_label: elapsed_label.clone(),
            log_text: String::new(),
            toast_overlay,
            idle_page,
            hero_row,
            status_pill,
            hero_action_btn,
            hero_schedule_btn,
            update_banner_group,
            banner_title_row,
            banner_install_btn,
            banner_whats_new_btn,
            banner_restart_btn,
            banner_discard_btn,
            auto_update_switch,
            preflight_status: PreflightStatus::Checking,
            last_update_text: initial_last_update,
            image_info: initial_image_info,
            seg_progress,
            active_module: None,
            reboot_pending: false,

            registry_uri: initial_registry_uri.clone(),
            selected_tag: initial_selected_tag.clone(),
            deployments: get_sample_deployments(false),
            expanded_deployment_id: None,
            changelog_version: initial_selected_tag_3.clone(),
            registry_versions: Vec::new(),
            github_commits: Vec::new(),
            sbom_diff: None,
            sbom_status: SbomStatus::Pending,

            registry_entry_row: registry_entry_row.clone(),
            registry_row_sub: registry_row_sub.clone(),
            tag_row: tag_row.clone(),
            tag_model: tag_model.clone(),
            tag_raws: tag_raws.clone(),
            history_list_box: history_list_box.clone(),
            images_count_label,
            changelog_box: changelog_box.clone(),
            changelog_install_bar: changelog_install_bar.clone(),
            pin_group: pin_group.clone(),
            pin_row: pin_row.clone(),
            rollback_target: None,
        };

        let widgets = view_output!();

        // Set initial idle description and visible page.
        model.refresh_idle_description();
        root.set_visible_child_name("idle");

        rebuild_history_list(
            &model.history_list_box,
            &model.deployments,
            model.expanded_deployment_id.as_deref(),
            &sender,
        );
        model
            .images_count_label
            .set_label(&format!("{} images", model.deployments.len()));
        model.rebuild_changelog_page(&sender);

        // Update elapsed timer every 250ms while the "updating" page is visible.
        let stack_ref = root.clone();
        let timer_sender = sender.input_sender().clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            if stack_ref.visible_child_name().as_deref() == Some("updating") {
                timer_sender.emit(StatusViewInput::TimerTick);
            }
            gtk::glib::ControlFlow::Continue
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            StatusViewInput::StateChanged(new_state) => {
                let stack_name = match &new_state {
                    AppState::Idle => "idle",
                    AppState::Updating => "updating",
                    AppState::Complete => "complete",
                    AppState::UpToDate => "up_to_date",
                    AppState::Error(_) => "error",
                };
                self.stack.set_visible_child_name(stack_name);

                match &new_state {
                    AppState::Updating => {
                        self.update_start = Some(Instant::now());
                        self.elapsed_label.set_label("0:00");
                        self.update_list.emit(UpdateListInput::Reset);
                        self.seg_progress.reset();
                        self.active_module = None;
                        self.reboot_pending = false;
                    }
                    AppState::Complete => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkAllComplete);
                        self.seg_progress.mark_all_complete();
                        self.active_module = None;
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = true;
                        self.refresh_idle_description();
                        self.deployments = get_sample_deployments(true);
                        rebuild_history_list(
                            &self.history_list_box,
                            &self.deployments,
                            self.expanded_deployment_id.as_deref(),
                            &sender,
                        );
                        self.images_count_label
                            .set_label(&format!("{} images", self.deployments.len()));
                    }
                    AppState::Error(_) => {
                        self.update_start = None;
                        self.update_list.emit(UpdateListInput::MarkCurrentFailed);
                        if let Some(key) = self.active_module {
                            self.seg_progress.set_module_failed(key);
                        }
                        self.active_module = None;
                    }
                    AppState::UpToDate => {
                        self.update_start = None;
                        self.preflight_status = PreflightStatus::UpToDate;
                        self.reboot_pending = false;
                        self.refresh_idle_description();
                    }
                    AppState::Idle => {
                        self.update_start = None;
                        self.last_update_text = get_last_update_time();
                        self.image_info = read_image_info();
                        self.refresh_idle_description();
                    }
                }

                // Dynamically set error description from the error payload.
                if let AppState::Error(ref err) = new_state {
                    if let Some(page) = self.stack.child_by_name("error") {
                        if let Ok(status_page) = page.downcast::<adw::StatusPage>() {
                            status_page.set_description(Some(err.as_str()));
                        }
                    }
                }

                self.state = new_state;
            }

            StatusViewInput::AppendLog(line) => {
                if !self.log_text.is_empty() {
                    self.log_text.push('\n');
                }
                self.log_text.push_str(&line);
                self.update_list
                    .emit(UpdateListInput::ProcessLine(line.clone()));
                self.log_view.emit(LogViewInput::AppendLine(line.clone()));
            }

            StatusViewInput::ClearLog => {
                self.log_text.clear();
                self.log_view.emit(LogViewInput::Clear);
            }

            StatusViewInput::TimerTick => {
                if let Some(start) = self.update_start {
                    let elapsed = start.elapsed();
                    let secs = elapsed.as_secs();
                    let mins = secs / 60;
                    let remaining_secs = secs % 60;
                    self.elapsed_label
                        .set_label(&format!("{}:{:02}", mins, remaining_secs));
                }
            }

            StatusViewInput::PreflightResult(status) => {
                self.preflight_status = status;
                self.refresh_idle_description();
            }

            StatusViewInput::DismissBanner => {
                self.reboot_pending = false;
                self.preflight_status = PreflightStatus::UpToDate;
                self.refresh_idle_description();
            }

            StatusViewInput::SettingsChanged(new_settings) => {
                // Sync the front-page Auto Updates switch with the new
                // settings (e.g. user toggled it inside the Advanced dialog).
                // Block re-firing apply_auto_updates_setting via the active-
                // notify handler by using `block_signal`-style: just check
                // whether the desired state matches current first.
                if self.auto_update_switch.is_active() != new_settings.auto_updates {
                    self.auto_update_switch
                        .set_active(new_settings.auto_updates);
                }
            }

            StatusViewInput::HeroActionClicked => {
                // Single-button dispatch: Restart when a deployment is
                // staged for reboot, otherwise StartUpdate. update() also
                // hides the button when neither state holds, so we
                // shouldn't reach this branch in that case — but route
                // through StartUpdate as the safer fallback.
                if self.reboot_pending {
                    let _ = sender.output(StatusViewOutput::Reboot);
                } else {
                    let _ = sender.output(StatusViewOutput::StartUpdate);
                }
            }

            StatusViewInput::ScheduleRebootTonight => {
                // Honour the dry_run guard — never call shutdown(8) on a
                // test/dev host. Surface what we *would* have done via toast.
                let settings = Settings::load();
                if settings.dry_run || settings.dev_mode {
                    tracing::warn!(
                        "Reboot Tonight suppressed (dry_run={}, dev_mode={}). \
                         Would have called: pkexec shutdown -r 02:00",
                        settings.dry_run,
                        settings.dev_mode
                    );
                    let t = adw::Toast::new("Restart scheduled for 02:00 (dry-run)");
                    t.set_timeout(4);
                    self.toast_overlay.add_toast(t);
                } else {
                    schedule_reboot_tonight(&self.toast_overlay);
                }
            }

            StatusViewInput::CopyLog => {
                if let Some(display) = gtk::gdk::Display::default() {
                    let clipboard = display.clipboard();
                    clipboard.set_text(&self.log_text);
                    let toast = adw::Toast::new("Log copied to clipboard");
                    toast.set_timeout(3);
                    self.toast_overlay.add_toast(toast);
                }
            }

            StatusViewInput::ShowPage(page) => {
                let target = if page == "main" || page == "idle" {
                    "idle"
                } else {
                    &page
                };
                self.stack.set_visible_child_name(target);
                let _ = sender.output(StatusViewOutput::PageChanged(page));
            }

            StatusViewInput::SaveRegistryUri(uri) => {
                // Fired by adw::EntryRow's `apply` signal — on Enter or click
                // of the built-in ✓ button. No separate edit/cancel state to
                // manage; the row is always editable inline.
                if !uri.trim().is_empty() {
                    self.registry_uri = uri;
                    self.registry_entry_row.set_text(&self.registry_uri);

                    let name = self
                        .registry_uri
                        .split('/')
                        .next_back()
                        .unwrap_or(&self.registry_uri);
                    self.registry_row_sub
                        .set_label(&format!("{}:{}", name, self.selected_tag));

                    let toast = adw::Toast::new("Image source updated");
                    self.toast_overlay.add_toast(toast);

                    spawn_changelog_fetch(
                        self.registry_uri.clone(),
                        self.selected_tag.clone(),
                        sender.clone(),
                    );
                }
            }

            StatusViewInput::SelectTag(tag) => {
                // Idempotency guard: AvailableTagsLoaded calls
                // tag_combo.set_active_id() which fires the `changed` signal
                // → emits SelectTag → would re-spawn the changelog fetch →
                // populate AvailableTagsLoaded again. Without this early-return
                // the home page spins on changelog fetches forever and burns
                // GHCR + GitHub rate limit. Only re-fetch when the tag really
                // changed.
                if tag == self.selected_tag {
                    return;
                }
                self.selected_tag = tag.clone();
                let desc = match tag.as_str() {
                    "latest" => "Always the newest stable build",
                    _ if tag.chars().all(|c| c.is_ascii_digit()) => "Pinned to this version",
                    _ => "Custom tag",
                };
                self.tag_row.set_subtitle(desc);

                let name = self
                    .registry_uri
                    .split('/')
                    .last()
                    .unwrap_or(&self.registry_uri);
                self.registry_row_sub
                    .set_label(&format!("{}:{}", name, self.selected_tag));

                let toast = adw::Toast::new(&format!("Tag set to :{}", tag));
                self.toast_overlay.add_toast(toast);

                spawn_changelog_fetch(
                    self.registry_uri.clone(),
                    self.selected_tag.clone(),
                    sender.clone(),
                );
            }

            StatusViewInput::TogglePin(action) => {
                if let Some(id) = action.strip_prefix("expand:") {
                    if self.expanded_deployment_id.as_deref() == Some(id) {
                        self.expanded_deployment_id = None;
                    } else {
                        self.expanded_deployment_id = Some(id.to_string());
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label
                        .set_label(&format!("{} images", self.deployments.len()));
                } else if action == "powerwash" {
                    let window = self
                        .stack
                        .root()
                        .and_then(|r| r.downcast::<gtk::Window>().ok());
                    let dialog = adw::AlertDialog::builder()
                        .heading("Powerwash this device?")
                        .body("`/etc` will be reset to image defaults and all installed apps will be removed. Your home directory, files, and signed-in accounts are kept.")
                        .build();

                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("powerwash", "Powerwash");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.set_response_appearance("powerwash", adw::ResponseAppearance::Suggested);

                    let toast_overlay = self.toast_overlay.clone();
                    let settings_snapshot = Settings::load();
                    dialog.connect_response(None, move |dlg, response| {
                        if response == "powerwash" {
                            // Powerwash = uninstall every user Flatpak + remove
                            // every Distrobox container. Leaves /var/home, /etc,
                            // and the bootc image untouched, so the dialog copy
                            // ("home, files, accounts are kept") is honest.
                            // Factory Reset is the destructive bootc-install-
                            // reset path; the two are intentionally different.
                            if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                                tracing::warn!(
                                    "POWERWASH suppressed (dry_run={}, dev_mode={}). \
                                     Would have called:\n  \
                                     1. flatpak uninstall --user --all -y\n  \
                                     2. distrobox rm -f -a",
                                    settings_snapshot.dry_run,
                                    settings_snapshot.dev_mode
                                );
                                let toast =
                                    adw::Toast::new("Powerwash staged (dry-run, no commands run)");
                                toast_overlay.add_toast(toast);
                            } else {
                                run_powerwash(&toast_overlay);
                            }
                        }
                        dlg.close();
                    });
                    dialog.present(window.as_ref());
                } else if action == "factory" {
                    let window = self
                        .stack
                        .root()
                        .and_then(|r| r.downcast::<gtk::Window>().ok());

                    let entry = gtk::Entry::builder()
                        .placeholder_text("reset")
                        .margin_top(12)
                        .margin_bottom(12)
                        .build();
                    entry.add_css_class("entry");

                    let dialog = adw::AlertDialog::builder()
                        .heading("Factory reset?")
                        .body("Erases all user data, accounts, apps, rollback images, and settings, then redeploys the factory image. This cannot be undone.")
                        .extra_child(&entry)
                        .build();

                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("reset", "Factory Reset");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
                    dialog.set_response_enabled("reset", false);

                    let dlg_clone = dialog.clone();
                    entry.connect_changed(move |ent| {
                        let text = ent.text().to_string();
                        dlg_clone.set_response_enabled("reset", text == "reset");
                    });

                    let toast_overlay = self.toast_overlay.clone();
                    let settings_snapshot = Settings::load();
                    dialog.connect_response(None, move |dlg, response| {
                        if response == "reset" {
                            // Factory reset = bootc's canonical `install reset`,
                            // which creates a fresh stateroot with /etc from
                            // the image and an empty /var. Old deployment is
                            // preserved at /sysroot/ostree/deploy/<old-stateroot>
                            // for recovery.
                            // See: https://bootc.dev/bootc/experimental-install-reset.html
                            if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                                tracing::warn!(
                                    "FACTORY RESET suppressed (dry_run={}, dev_mode={}). \
                                     Would have called:\n  \
                                     pkexec bootc install reset --experimental --apply",
                                    settings_snapshot.dry_run,
                                    settings_snapshot.dev_mode
                                );
                                let toast = adw::Toast::new(
                                    "Factory reset queued (dry-run, no commands run)",
                                );
                                toast_overlay.add_toast(toast);
                            } else {
                                run_bootc_install_reset(&toast_overlay, "Factory reset");
                            }
                        }
                        dlg.close();
                    });
                    dialog.present(window.as_ref());
                } else {
                    for d in &mut self.deployments {
                        if d.id == action {
                            d.pinned = !d.pinned;
                            let toast_msg = if d.pinned {
                                format!("Pinned {} (preventing pruning)", d.tag)
                            } else {
                                format!("Unpinned {} (allowing pruning)", d.tag)
                            };
                            let toast = adw::Toast::new(&toast_msg);
                            self.toast_overlay.add_toast(toast);
                            break;
                        }
                    }
                    rebuild_history_list(
                        &self.history_list_box,
                        &self.deployments,
                        self.expanded_deployment_id.as_deref(),
                        &sender,
                    );
                    self.images_count_label
                        .set_label(&format!("{} images", self.deployments.len()));
                }
            }

            StatusViewInput::RollbackTo(d) => {
                let window = self
                    .stack
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let dialog = adw::AlertDialog::builder()
                    .heading(format!("Roll back to {}?", d.tag))
                    .body(format!(
                        "The next boot will use {}:{}.\nYour current image stays on disk and remains available to roll forward.",
                        d.image, d.tag
                    ))
                    .build();

                dialog.add_response("cancel", "Cancel");
                dialog.add_response("rollback", "Roll back");
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("rollback", adw::ResponseAppearance::Suggested);

                let dialog_sender = sender.input_sender().clone();
                dialog.connect_response(None, move |dlg, response| {
                    if response == "rollback" {
                        dialog_sender.emit(StatusViewInput::ConfirmRollback);
                    }
                    dlg.close();
                });
                self.rollback_target = Some(d);
                dialog.present(window.as_ref());
            }

            StatusViewInput::ConfirmRollback => {
                if let Some(target) = self.rollback_target.take() {
                    let toast = adw::Toast::new(&format!("Rolling back to {}…", target.tag));
                    self.toast_overlay.add_toast(toast);
                }
            }

            StatusViewInput::SetDefaultBoot(d) => {
                let toast = adw::Toast::new(&format!("Set {} as default boot", d.tag));
                self.toast_overlay.add_toast(toast);
            }

            StatusViewInput::SelectChangelogVersion(version) => {
                // Short-circuit: if the user clicks (i) repeatedly without
                // having changed the selected tag, the changelog page is
                // already built and the data behind it (commits, sbom diff,
                // registry versions) hasn't changed. Skip the expensive
                // rebuild_changelog_page widget tear-down + reconstruction
                // and just stack-switch. Cuts perceived latency from
                // hundreds of ms to instant.
                //
                // Pass the version as-is (empty string from the hero's (i)
                // button is treated as "keep current") and only rebuild
                // when the actual selection changed.
                let target = if version.is_empty() {
                    self.changelog_version.clone()
                } else {
                    version
                };
                if target != self.changelog_version {
                    let t0 = std::time::Instant::now();
                    self.changelog_version = target;
                    self.rebuild_changelog_page(&sender);
                    tracing::debug!("changelog: rebuild took {}ms", t0.elapsed().as_millis());
                }
                self.stack.set_visible_child_name("changelog");
                let _ = sender.output(StatusViewOutput::PageChanged("changelog".to_string()));
            }

            StatusViewInput::RegistryVersionsLoaded(versions) => {
                // Merge incoming versions, deduplicating by version string.
                // Collect owned keys first to avoid the simultaneous borrow.
                let existing: std::collections::HashSet<String> = self
                    .registry_versions
                    .iter()
                    .map(|v| v.version.clone())
                    .collect();
                for v in versions {
                    if !existing.contains(&v.version) {
                        self.registry_versions.push(v);
                    }
                }
                self.registry_versions.sort_by_key(|v| v.date);
                if let Some(latest) = self.registry_versions.last() {
                    self.changelog_version = latest.version.clone();
                }
                self.rebuild_changelog_page(&sender);

                // Merge remote registry versions into the history list.
                // Cap the visible history at HISTORY_MAX entries — the 8 most
                // recent builds — so the page doesn't grow unbounded as the
                // upstream registry accumulates daily tags.
                const HISTORY_MAX: usize = 8;

                let local_tags: std::collections::HashSet<&str> =
                    self.deployments.iter().map(|d| d.tag.as_str()).collect();
                let mut merged = self.deployments.clone();
                // Walk versions newest-first (they're sorted ascending by date)
                // so the cap drops oldest, not newest.
                for v in self.registry_versions.iter().rev() {
                    if merged.len() >= HISTORY_MAX {
                        break;
                    }
                    if !local_tags.contains(v.version.as_str()) {
                        let date_str = v.date.format("%b %-d, %Y").to_string();
                        merged.push(MockDeployment {
                            id: format!("remote-{}", v.version),
                            state: "remote".to_string(),
                            title: self
                                .image_info
                                .clone()
                                .unwrap_or_else(|| "System Image".to_string()),
                            image: self.registry_uri.clone(),
                            tag: v.version.clone(),
                            digest: v.revision.clone(),
                            deployed: format!("Available · {}", date_str),
                            deployed_full: format!(
                                "Built: {} · {}",
                                date_str,
                                v.created.format("%H:%M UTC")
                            ),
                            size: "—".to_string(),
                            kernel: v.kernel.clone(),
                            package_count: 0,
                            signer: "Remote registry".to_string(),
                            pinned: false,
                        });
                    }
                }
                self.deployments = merged;
                rebuild_history_list(
                    &self.history_list_box,
                    &self.deployments,
                    self.expanded_deployment_id.as_deref(),
                    &sender,
                );
                self.images_count_label
                    .set_label(&format!("{} images", self.deployments.len()));
            }

            StatusViewInput::AvailableTagsLoaded(tags) => {
                // Repopulate the StringList model in-place with display
                // strings; keep a parallel raw-tag list so the SelectTag
                // dispatcher can map index → real tag (sha hash for dakota,
                // verbatim for stream/dated tags).
                while self.tag_model.n_items() > 0 {
                    self.tag_model.remove(0);
                }
                let mut raws = Vec::with_capacity(tags.len());
                for t in &tags {
                    self.tag_model.append(&t.display);
                    raws.push(t.raw.clone());
                }
                let active_idx = raws
                    .iter()
                    .position(|raw| raw == &self.selected_tag)
                    .unwrap_or(0) as u32;
                *self.tag_raws.borrow_mut() = raws;
                self.tag_row.set_selected(active_idx);
                self.tag_row.set_sensitive(tags.len() > 1);
            }

            StatusViewInput::GithubCommitsLoaded(commits) => {
                self.github_commits = commits;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffLoaded(diff) => {
                self.sbom_diff = Some(diff);
                self.sbom_status = SbomStatus::Loaded;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffStarted => {
                self.sbom_status = SbomStatus::Loading;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::SbomDiffUnavailable => {
                self.sbom_status = SbomStatus::NotAvailable;
                self.rebuild_changelog_page(&sender);
            }

            StatusViewInput::UnpinToStream(stream_tag) => {
                let window = self
                    .stack
                    .root()
                    .and_then(|r| r.downcast::<gtk::Window>().ok());
                let dialog = adw::AlertDialog::builder()
                    .heading(format!("Unpin to :{}?", stream_tag))
                    .body(format!(
                        "Your system will switch back to the floating `{}` tag and resume receiving automatic updates. A restart is required after the switch.",
                        stream_tag
                    ))
                    .build();
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("unpin", "Unpin");
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                dialog.set_response_appearance("unpin", adw::ResponseAppearance::Suggested);

                let toast_overlay = self.toast_overlay.clone();
                let registry_uri = self.registry_uri.clone();
                let settings_snapshot = Settings::load();
                dialog.connect_response(None, move |dlg, response| {
                    if response == "unpin" {
                        if settings_snapshot.dry_run || settings_snapshot.dev_mode {
                            tracing::warn!(
                                "unpin suppressed (dry_run={}, dev_mode={})",
                                settings_snapshot.dry_run,
                                settings_snapshot.dev_mode
                            );
                            let t = adw::Toast::new("Unpin staged (dry-run, no commands run)");
                            toast_overlay.add_toast(t);
                        } else {
                            run_unpin_to_stream(
                                &toast_overlay,
                                registry_uri.clone(),
                                stream_tag.clone(),
                            );
                        }
                    }
                    dlg.close();
                });
                dialog.present(window.as_ref());
            }

            StatusViewInput::ModuleStarted(module) => {
                let key = module.key();
                let is_same_seg = self
                    .active_module
                    .map(|prev| same_segment(prev, key))
                    .unwrap_or(false);
                if !is_same_seg {
                    if let Some(prev) = self.active_module {
                        self.seg_progress.set_module_complete(prev);
                    }
                    self.seg_progress.set_module_active(key);
                }
                self.active_module = Some(key);
                self.update_list.emit(UpdateListInput::ProcessLine(format!(
                    "Starting module: {}",
                    key
                )));
            }

            StatusViewInput::ModuleFinished(module, status) => {
                use crate::orchestrator::ModuleStatus;
                let key = module.key();
                match status {
                    ModuleStatus::Success | ModuleStatus::UpToDate | ModuleStatus::Skipped => {
                        self.seg_progress.set_module_complete(key);
                    }
                    ModuleStatus::Failed(_) => {
                        self.seg_progress.set_module_failed(key);
                    }
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_auto_updates_enabled() -> bool {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "systemctl", "is-enabled", "uupd.timer"])
            .output()
    } else {
        Command::new("systemctl")
            .args(["is-enabled", "uupd.timer"])
            .output()
    };

    match output {
        Ok(output) => match String::from_utf8_lossy(&output.stdout).trim() {
            "enabled" => true,
            "disabled" => false,
            _ => Settings::load().auto_updates,
        },
        Err(_) => Settings::load().auto_updates,
    }
}

fn apply_auto_updates_setting(active: bool) {
    let mut settings = Settings::load();
    settings.auto_updates = active;
    settings.save();

    // Dry-run / dev_mode: persist the preference but don't actually toggle the
    // host systemd timer. Logs the would-have-run command so testers can see
    // what real mode would do.
    if settings.dry_run || settings.dev_mode {
        let verb = if active { "enable" } else { "disable" };
        tracing::warn!(
            "uupd.timer toggle suppressed (dry_run={}, dev_mode={}). \
             Would have called `pkexec systemctl {} --now uupd.timer`.",
            settings.dry_run,
            settings.dev_mode,
            verb
        );
        return;
    }

    std::thread::spawn(move || {
        let args = if active {
            ["enable", "--now", "uupd.timer"]
        } else {
            ["disable", "--now", "uupd.timer"]
        };

        let status = if crate::update_worker::is_flatpak() {
            Command::new("flatpak-spawn")
                .args(["--host", "pkexec", "systemctl"])
                .args(args)
                .status()
        } else {
            Command::new("pkexec").arg("systemctl").args(args).status()
        };

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!("Failed to toggle uupd.timer: {}", status),
            Err(err) => tracing::warn!("Failed to toggle uupd.timer: {}", err),
        }
    });
}

/// Read the current OS image name and variant from `/etc/os-release`.
/// Tries `/run/host/etc/os-release` first for Flatpak compatibility.
fn read_image_info() -> Option<String> {
    // Compose "{NAME} {IMAGE_NAME-capitalised}" when both are present —
    // e.g. NAME="Bluefin", IMAGE_NAME="dakota" → "Bluefin Dakota". Gives
    // the hero row a richer identity than PRETTY_NAME alone (which on
    // Bluefin is just "Bluefin"). Falls back to PRETTY_NAME, then
    // detect_bootc_image_info's "org/image" form, then IMAGE_ID +
    // VARIANT_ID — same chain as before, just with the composition step
    // bolted on top.
    let name = read_os_release_field("NAME");
    let image = read_os_release_field("IMAGE_NAME");
    if let (Some(n), Some(i)) = (name.as_deref(), image.as_deref()) {
        let capped = capitalise_first(i);
        if !n.eq_ignore_ascii_case(&capped) && !n.contains(capped.as_str()) {
            return Some(format!("{} {}", n, capped));
        }
    }

    if let Some(pretty) = read_os_release_field("PRETTY_NAME") {
        return Some(pretty);
    }

    if let Some((title, _, _)) = detect_bootc_image_info() {
        return Some(title);
    }

    if let Some(id) = read_os_release_field("IMAGE_ID") {
        if let Some(var) = read_os_release_field("VARIANT_ID") {
            return Some(format!("{}  ·  {}", id, var));
        }
        return Some(id);
    }
    None
}

/// Title-case the first character of `s`, leave the rest unchanged.
/// Used to turn `dakota` → `Dakota` for the hero-row display.
fn capitalise_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

fn read_os_release_field(key: &str) -> Option<String> {
    // When in a Flatpak, use flatpak-spawn to read the host's os-release
    if crate::update_worker::is_flatpak() {
        if let Ok(output) = std::process::Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/os-release"])
            .output()
        {
            if output.status.success() {
                if let Ok(content) = String::from_utf8(output.stdout) {
                    if let Some(v) = parse_os_release_field(&content, key) {
                        return Some(v);
                    }
                }
            }
        }
    }

    // Fall back to direct file reading
    for path in &["/run/host/etc/os-release", "/etc/os-release", "/usr/lib/os-release", "/usr/lib/os-release"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(v) = parse_os_release_field(&content, key) {
                return Some(v);
            }
        }
    }
    None
}

/// Pure-function counterpart of [`read_os_release_field`] — extracted so
/// the key=value parsing is unit-testable without filesystem fixtures.
/// Strips surrounding double-quotes; rejects empty values; first match wins.
fn parse_os_release_field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    for line in content.lines() {
        if let Some(v) = line.strip_prefix(prefix.as_str()) {
            let val = v.trim_matches('"').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Build a short subtitle for the hero row from bootc-status JSON:
/// "VERSION · sha1234567" when both are available, just one when only one is.
/// Per user direction this is more informative than "Booted N days ago".
fn read_booted_image_summary() -> Option<String> {
    if let Some(mock) = Settings::load().mock_identity {
        let full_ref = mock.full_ref();
        let j = serde_json::json!({
            "status": {
                "booted": {
                    "image": {
                        "image": { "image": full_ref },
                        "imageDigest": mock.digest,
                        "timestamp": mock.booted_at
                    }
                }
            }
        });
        return parse_booted_image_summary(&j);
    }
    let json = get_cached_bootc_status()?;
    parse_booted_image_summary(&json)
}

/// Pure-function counterpart of [`read_booted_image_summary`] — extracted
/// for unit testing without spawning `bootc status --json`. Returns a
/// two-line subtitle:
///
/// ```text
/// ghcr.io/projectbluefin/dakota:latest
/// bc6d66c9 · 2026-05-30
/// ```
///
/// Image ref on line 1 (so the user can read the full identity);
/// short digest + build date on line 2 (small, dimmable in the row CSS).
/// The hero ActionRow needs `set_subtitle_lines(2)` to render both lines.
/// Missing pieces are skipped — the function never returns an empty
/// trailing " · " or a dangling newline.
fn parse_booted_image_summary(json: &Value) -> Option<String> {
    let booted = json.pointer("/status/booted")?;
    let image_ref = booted
        .pointer("/image/image/image")
        .or_else(|| booted.pointer("/image/image"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let digest = booted
        .pointer("/image/imageDigest")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix("sha256:").or(Some(s)))
        .map(|s| s.chars().take(8).collect::<String>());
    let date = booted
        .pointer("/image/timestamp")
        .and_then(|v| v.as_str())
        .filter(|s| s.len() >= 10)
        .map(|s| s[..10].to_string());

    // Compose line 2 ("digest · date") from whichever pieces we have.
    let mut line2_parts: Vec<String> = Vec::new();
    if let Some(d) = digest {
        line2_parts.push(d);
    }
    if let Some(t) = date {
        line2_parts.push(t);
    }
    let line2 = line2_parts.join(" · ");

    match (image_ref, line2.is_empty()) {
        (Some(r), true) => Some(r),
        (Some(r), false) => Some(format!("{}\n{}", r, line2)),
        (None, false) => Some(line2),
        (None, true) => None,
    }
}

/// Extract the booted image's tag suffix (everything after the final `:`) from
/// the cached bootc-status JSON — e.g. `stable-daily-43.20260530`. Used to
/// pair the booted build with its entry in `registry_versions` (whose
/// `version` field is the `org.opencontainers.image.version` annotation, in
/// practice equal to the dated tag for Universal Blue images).
///
/// Falls back to os-release fields when bootc-status fails. Dakota's
/// composefs deployment hits a "Multiple extra entries in /boot" error in
/// `bootc status` which makes the JSON path unavailable; os-release still
/// carries the booted build identity via `IMAGE_VERSION` / `VERSION_ID`.
fn read_booted_tag_suffix() -> Option<String> {
    if let Some(mock) = Settings::load().mock_identity {
        return Some(mock.tag);
    }
    if let Some(json) = get_cached_bootc_status() {
        if let Some(t) = parse_booted_tag_suffix(&json) {
            return Some(t);
        }
    }
    // os-release fallback. IMAGE_VERSION is more specific (Dakota writes
    // "20260530"), VERSION_ID is the broader handle (same value on Dakota,
    // version-id only on Bluefin). Either one anchors the booted entry in
    // registry_versions through find_booted_match's date/substring lookup.
    read_os_release_field("IMAGE_VERSION").or_else(|| read_os_release_field("VERSION_ID"))
}

/// Pure-function counterpart of [`read_booted_tag_suffix`] — extracted for
/// unit testing.
fn parse_booted_tag_suffix(json: &Value) -> Option<String> {
    let img = json
        .pointer("/status/booted/image/image/image")
        .or_else(|| json.pointer("/status/booted/image/image"))
        .and_then(|v| v.as_str())?;
    let (_, tag) = img.rsplit_once(':')?;
    if tag.is_empty() {
        return None;
    }
    Some(tag.to_string())
}

/// Match the booted anchor string against the registry version list with
/// progressive fallbacks. Anchors come from `read_booted_tag_suffix`, which
/// may yield:
/// - A fully-qualified Bluefin tag: `stable-daily-43.20260602`
/// - A bare date string from Dakota's os-release: `20260530`
/// - A floating stream tag: `latest`
///
/// Strategy:
/// 1. Exact `v.version == anchor` — direct hit for Bluefin.
/// 2. `v.version.contains(anchor)` — handles Dakota where the booted side
///    is `20260530` and the registry side is `latest.20260530`.
/// 3. `v.date == parsed_date(anchor)` — last resort when the version string
///    diverges entirely (Dakota's commit-sha tags annotate as `latest`).
fn find_booted_match<'a>(
    versions: &'a [ImageVersion],
    anchor: &str,
) -> Option<&'a ImageVersion> {
    if let Some(v) = versions.iter().find(|v| v.version == anchor) {
        return Some(v);
    }
    if let Some(v) = versions.iter().find(|v| v.version.contains(anchor)) {
        return Some(v);
    }
    if let Some(date) = extract_yyyymmdd_date(anchor) {
        return versions.iter().find(|v| v.date == date);
    }
    None
}

/// Find an embedded YYYYMMDD date anywhere in a tag/anchor string. Accepts
/// `20260530`, `latest.20260530`, `stable-daily-43.20260602`, etc. Returns
/// None when no 8-digit run parses as a valid Gregorian date.
fn extract_yyyymmdd_date(s: &str) -> Option<chrono::NaiveDate> {
    let bytes = s.as_bytes();
    if bytes.len() < 8 {
        return None;
    }
    for i in 0..=(bytes.len() - 8) {
        let slice = &bytes[i..i + 8];
        if slice.iter().all(|b| b.is_ascii_digit()) {
            // Reject if preceded/followed by another digit — that means the
            // run is longer than 8 and we'd be slicing through it.
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let next_ok = i + 8 == bytes.len() || !bytes[i + 8].is_ascii_digit();
            if prev_ok && next_ok {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(
                    std::str::from_utf8(slice).ok()?,
                    "%Y%m%d",
                ) {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// One row in the changelog "Stack" diff: a labelled component (Image,
/// Kernel, Revision, Build) plus its current and target value. `bumped` lets
/// the renderer flag rows whose value actually changes, so the user can
/// scan the section and see at a glance which components moved.
#[derive(Debug, Clone)]
struct StackItem {
    label: &'static str,
    current: Option<String>,
    target: String,
    bumped: bool,
}

/// Build the rows for the changelog Stack section. Compares the booted
/// `ImageVersion` against the selected target and emits a labelled
/// from→to row for each meaningful component (image tag, kernel, git
/// revision, build date). Returns an empty Vec when there's no target
/// to compare against — caller suppresses the whole section in that
/// case.
///
/// `host_kernel` is the live `uname -r` for the booted host; used to fill
/// in the booted side of the Kernel row when the registry-side annotation
/// is missing. Dakota doesn't publish `ostree.linux` anywhere accessible,
/// so without this the booted kernel would render as "—" even though we
/// know it from the running system.
fn build_stack_items(
    booted: Option<&ImageVersion>,
    target: Option<&ImageVersion>,
    host_kernel: Option<&str>,
) -> Vec<StackItem> {
    let Some(target) = target else {
        return Vec::new();
    };

    let mut out = Vec::new();

    // Image tag — what the user is on vs. what they're going to. Use the
    // version annotation (typically the dated tag) so the value stays
    // short enough to render in the row suffix. Full refs would overflow.
    let image_bumped = booted.map(|b| b.version != target.version).unwrap_or(true);
    out.push(StackItem {
        label: "Image",
        current: booted.map(|b| b.version.clone()),
        target: target.version.clone(),
        bumped: image_bumped,
    });

    // Kernel — the headline number for a bootc update. Only emit the row
    // when at least one side has a value; Dakota doesn't ship kernel data
    // for either side, and a "— → —" row is just noise.
    let target_kernel = if target.kernel.is_empty() {
        None
    } else {
        Some(target.kernel.clone())
    };
    let current_kernel = booted
        .map(|b| b.kernel.clone())
        .filter(|k| !k.is_empty())
        .or_else(|| host_kernel.map(|s| s.to_string()).filter(|s| !s.is_empty()));
    if target_kernel.is_some() || current_kernel.is_some() {
        let kernel_bumped = match (&current_kernel, &target_kernel) {
            (Some(c), Some(t)) => c != t,
            // We know the target but not the booted side → "new" value the
            // user would land on. Matches the Image/Revision/Built rows'
            // behaviour when booted info is missing.
            (None, Some(_)) => true,
            // Target side missing (Dakota: no kernel anywhere) — current is
            // just informational, not a change. Keep it dim.
            _ => false,
        };
        out.push(StackItem {
            label: "Kernel",
            current: current_kernel,
            target: target_kernel.unwrap_or_default(),
            bumped: kernel_bumped,
        });
    }

    // Short git commit — the actual code that built the image. Skipped on
    // images that don't publish `image.revision` (some Dakota builds).
    if !target.revision.is_empty() {
        let target_rev = short_sha(&target.revision);
        let current_rev = booted
            .map(|b| short_sha(&b.revision))
            .filter(|s| !s.is_empty());
        let rev_bumped = current_rev
            .as_deref()
            .map(|c| c != target_rev)
            .unwrap_or(true);
        out.push(StackItem {
            label: "Revision",
            current: current_rev,
            target: target_rev,
            bumped: rev_bumped,
        });
    }

    // Build date — surfaces "how recent" without making the user parse a
    // 40-char manifest digest.
    let target_built = target.created.format("%b %-d, %Y").to_string();
    let current_built = booted.map(|b| b.created.format("%b %-d, %Y").to_string());
    let built_bumped = current_built
        .as_deref()
        .map(|c| c != target_built)
        .unwrap_or(true);
    out.push(StackItem {
        label: "Built",
        current: current_built,
        target: target_built,
        bumped: built_bumped,
    });

    out
}

fn short_sha(s: &str) -> String {
    if s.len() >= 7 {
        s[..7].to_string()
    } else {
        s.to_string()
    }
}

fn read_logo_icon_name() -> String {
    // Read LOGO= from os-release first — gets us the distro's branded icon
    // (e.g. "bluefin", "dakota", "fedora-logo") when the icon theme actually
    // ships it. Fall through to a safe fallback chain if not.
    let mut candidates: Vec<String> = Vec::new();
    for path in &["/run/host/etc/os-release", "/etc/os-release"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                if let Some(v) = line.strip_prefix("LOGO=") {
                    let logo = v.trim_matches('"').to_string();
                    if !logo.is_empty() {
                        candidates.push(logo);
                    }
                }
            }
        }
    }
    // Always-available GNOME fallbacks. `distributor-logo-symbolic` is the
    // freedesktop spec name; `computer-symbolic` is guaranteed by Adwaita.
    candidates.push("distributor-logo-symbolic".to_string());
    candidates.push("computer-symbolic".to_string());

    // Pick the first candidate the icon theme actually has, so we never
    // render a blank prefix on the hero row. Falls back to the literal
    // "computer-symbolic" string if no display is available (shouldn't
    // happen at runtime — GTK requires a display — but keeps the function
    // total for tests).
    if let Some(display) = gtk::gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        for c in &candidates {
            if theme.has_icon(c) {
                return c.clone();
            }
        }
    }
    "computer-symbolic".to_string()
}

use std::sync::Mutex;
static BOOTC_STATUS_CACHE: Mutex<Option<Value>> = Mutex::new(None);

fn get_cached_bootc_status() -> Option<Value> {
    // Mock identity wins over real bootc status — tests don't want to spawn
    // a privileged subprocess (and the cached result would lie about which
    // image is "booted"). Cache stays empty when mocked.
    if Settings::load().mock_identity.is_some() {
        return None;
    }

    {
        let cache = BOOTC_STATUS_CACHE.lock().unwrap();
        if cache.is_some() {
            return cache.clone();
        }
    }

    use std::time::Duration;

    let run_cmd = |cmd_path: &str, args: &[&str]| -> Option<std::process::Output> {
        let mut cmd = Command::new(cmd_path);
        cmd.args(args);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return child.wait_with_output().ok();
                    } else {
                        return None;
                    }
                }
                Ok(None) if start.elapsed() > Duration::from_secs(3) => {
                    let _ = child.kill();
                    return None;
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    };

    let cmd_path = if crate::update_worker::is_flatpak() {
        "flatpak-spawn"
    } else {
        "bootc"
    };
    let args: &[&str] = if crate::update_worker::is_flatpak() {
        &["--host", "bootc", "status", "--json"]
    } else {
        &["status", "--json"]
    };

    let output = if let Some(out) = run_cmd(cmd_path, args) {
        out
    } else {
        let cmd_path_pk = if crate::update_worker::is_flatpak() {
            "flatpak-spawn"
        } else {
            "pkexec"
        };
        let args_pk: &[&str] = if crate::update_worker::is_flatpak() {
            &["--host", "pkexec", "bootc", "status", "--json"]
        } else {
            &["bootc", "status", "--json"]
        };
        run_cmd(cmd_path_pk, args_pk)?
    };

    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    let mut cache = BOOTC_STATUS_CACHE.lock().unwrap();
    *cache = Some(json.clone());
    Some(json)
}

fn detect_bootc_image_info() -> Option<(String, String, String)> {
    // Delegate to the UpdaterService. current_image() already encapsulates the
    // full precedence chain (mock_identity → FINUPDATE_IMAGE → bootc status →
    // os-release) inside RegistryClient::detect_with_settings, so this site
    // just transforms the resulting ImageRef into the (title, registry_uri,
    // selected_tag) triple the UI is shaped around.
    //
    // We block on the async call because every caller here runs on the GTK
    // thread, which is not a tokio runtime. The actual I/O it kicks off (a
    // bootc-status subprocess) is the same one the legacy implementation did
    // synchronously, so there's no change in user-perceived latency.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let image = rt.block_on(async { crate::service::global().current_image().await.ok() })?;

    let title = format!("{}/{}", image.org, image.image);
    let registry_uri = format!("{}/{}/{}", image.registry, image.org, image.image);
    let selected_tag = strip_date_suffix(&image.tag).unwrap_or(image.tag);
    println!(
        "[debug] service::current_image: title='{}' registry_uri='{}' tag='{}'",
        title, registry_uri, selected_tag
    );
    Some((title, registry_uri, selected_tag))
}

#[derive(serde::Deserialize, Debug, Clone)]
struct BootcImageInfoConfig {
    tags: Vec<String>,
}

fn read_bootc_image_info_config() -> Option<BootcImageInfoConfig> {
    let content = if crate::update_worker::is_flatpak() {
        let output = Command::new("flatpak-spawn")
            .args(["--host", "cat", "/etc/bootc-image-info.json"])
            .output()
            .ok()?;
        if output.status.success() {
            String::from_utf8(output.stdout).ok()
        } else {
            None
        }
    } else {
        std::fs::read_to_string("/etc/bootc-image-info.json").ok()
    }?;

    serde_json::from_str(&content).ok()
}

fn strip_date_suffix(tag: &str) -> Option<String> {
    for sep in ['.', '-'] {
        if let Some(pos) = tag.rfind(sep) {
            let suffix = &tag[pos + 1..];
            if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                return Some(tag[..pos].to_string());
            }
        }
    }
    None
}

fn read_registry_uri() -> Option<String> {
    detect_bootc_image_info().map(|(_, registry_uri, _)| registry_uri)
}

fn read_selected_tag() -> String {
    detect_bootc_image_info()
        .map(|(_, _, tag)| tag)
        .unwrap_or_else(|| "latest".to_string())
}

/// True when `tag` is a specific pinned build rather than a floating stream.
///
/// A "stream" tag is one of the canonical channel names a Family publishes
/// (`latest`, `testing`, `stable`, `gts`, `beta`, `lts`, `lts-hwe`, etc.).
/// Anything else is treated as pinned: 8-digit date strings (`20260530`),
/// dotted date suffixes (`latest.20260530`), and 40-char sha hex tags.
///
/// Used to decide whether to show the "Unpin to {stream}" affordance on the
/// front page — if the user is pinned, they're not getting auto-updates and
/// need a one-click escape hatch.
fn is_pinned_tag(tag: &str) -> bool {
    const STREAM_TAGS: &[&str] = &[
        "latest",
        "testing",
        "stable",
        "stable-daily",
        "beta",
        "gts",
        "lts",
        "lts-hwe",
        "lts-amd64",
        "lts-arm64",
        "gdx",
        "unstable",
    ];
    if STREAM_TAGS.contains(&tag) {
        return false;
    }
    // Sha-only (40-char hex) — definitely pinned.
    if tag.len() == 40 && tag.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // 8-digit date string — pinned.
    if tag.len() == 8 && tag.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // Dotted/dashed date suffix on a stream tag — pinned.
    if strip_date_suffix(tag).is_some() {
        return true;
    }
    // Anything else (unrecognised tag) — treat as pinned for safety; user
    // can still click unpin to go back to the family's default stream.
    true
}

/// Try to determine when the last successful update ran.
fn get_last_update_time() -> Option<String> {
    let paths = ["/var/lib/uupd/last-run", "/var/lib/uupd/.last-run"];

    for path in &paths {
        if let Ok(metadata) = std::fs::metadata(path) {
            if let Ok(modified) = metadata.modified() {
                let elapsed = modified.elapsed().ok()?;
                let hours = elapsed.as_secs() / 3600;
                if hours < 1 {
                    return Some("Last update: less than an hour ago".to_string());
                } else if hours < 24 {
                    return Some(format!("Last update: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("Last update: {} days ago", days));
                }
            }
        }
    }

    if let Ok(metadata) = std::fs::metadata("/sysroot/ostree/deploy") {
        if let Ok(modified) = metadata.modified() {
            if let Ok(elapsed) = modified.elapsed() {
                let hours = elapsed.as_secs() / 3600;
                if hours < 24 {
                    return Some(format!("System deployed: {} hours ago", hours));
                } else {
                    let days = hours / 24;
                    return Some(format!("System deployed: {} days ago", days));
                }
            }
        }
    }

    None
}

fn parse_image_ref_fields(img_ref: &str) -> (String, String, String) {
    if img_ref.is_empty() {
        return (
            "Unknown".to_string(),
            "latest".to_string(),
            "unknown".to_string(),
        );
    }
    let (without_tag, tag) = img_ref.rsplit_once(':').unwrap_or((img_ref, "latest"));
    let parts: Vec<&str> = without_tag.split('/').collect();
    let name = parts
        .last()
        .map(|s| s.to_string())
        .unwrap_or_else(|| without_tag.to_string());
    let org = if parts.len() >= 2 {
        parts[parts.len() - 2].to_string()
    } else {
        "unknown".to_string()
    };
    (name, tag.to_string(), org)
}

fn get_real_deployments_from_json(json: &Value) -> Option<Vec<MockDeployment>> {
    let mut ds = Vec::new();
    let status = json.get("status")?;
    let booted_kernel = get_host_kernel();

    // 1. Staged deployment
    if let Some(staged) = status
        .get("staged")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = staged
            .pointer("/image/image/image")
            .or_else(|| staged.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = staged
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = staged
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-staged".to_string(),
            state: "staged".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Staged · pending reboot".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: "—".to_string(),
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned: false,
        });
    }

    // 2. Booted deployment
    if let Some(booted) = status
        .get("booted")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = booted
            .pointer("/image/image/image")
            .or_else(|| booted.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = booted
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = booted
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pinned = booted
            .get("pinned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-current".to_string(),
            state: "current".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Currently booted".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: booted_kernel,
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned,
        });
    }

    // 3. Rollback deployment
    if let Some(rollback) = status
        .get("rollback")
        .and_then(|v| if v.is_null() { None } else { Some(v) })
    {
        let img_ref = rollback
            .pointer("/image/image/image")
            .or_else(|| rollback.pointer("/image/image"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let digest = rollback
            .pointer("/image/imageDigest")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let timestamp = rollback
            .pointer("/image/timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pinned = rollback
            .get("pinned")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let (name, tag, org) = parse_image_ref_fields(img_ref);
        let date_str = if timestamp.len() >= 10 {
            &timestamp[0..10]
        } else {
            "recently"
        };

        ds.push(MockDeployment {
            id: "d-rollback".to_string(),
            state: "previous".to_string(),
            title: name,
            image: img_ref.to_string(),
            tag,
            digest: digest.to_string(),
            deployed: "Rollback target".to_string(),
            deployed_full: format!("Built: {}", date_str),
            size: "—".to_string(),
            kernel: "—".to_string(),
            package_count: 0,
            signer: format!("{} (sigstore)", org),
            pinned,
        });
    }

    if ds.is_empty() { None } else { Some(ds) }
}

fn get_real_deployments() -> Option<Vec<MockDeployment>> {
    get_cached_bootc_status().and_then(|json| get_real_deployments_from_json(&json))
}

/// Run `bootc install reset --experimental --apply` on the host (the canonical
/// factory-reset command, per https://bootc.dev/bootc/experimental-install-reset.html)
/// and surface success / failure back through the toast overlay.
///
/// `label` is used in toast / log messages so the caller (Powerwash vs.
/// Factory reset) can distinguish them.
///
/// This is destructive. It should only be reached after the caller has
/// confirmed `!settings.dry_run && !settings.dev_mode` AND user confirmation.
fn run_bootc_install_reset(toast_overlay: &adw::ToastOverlay, label: &'static str) {
    // Defensive re-check: if settings.json was edited (or another tab
    // re-saved with dry_run=true) between the dialog opening and the user
    // clicking confirm, abort. Caller's gate is the primary line of defence,
    // this is belt-and-suspenders against accidental destructive runs.
    let current = Settings::load();
    if current.dry_run || current.dev_mode {
        tracing::warn!(
            "{} aborted at the last moment — settings now show dry_run={} dev_mode={}",
            label,
            current.dry_run,
            current.dev_mode
        );
        let abort_toast = adw::Toast::new(&format!("{label} aborted (settings now in dry-run)"));
        abort_toast.set_timeout(4);
        toast_overlay.add_toast(abort_toast);
        return;
    }

    let toast = adw::Toast::new(&format!(
        "{label} starting… (running `bootc install reset`)"
    ));
    toast.set_timeout(4);
    toast_overlay.add_toast(toast);

    // adw::ToastOverlay is GObject-but-not-Send, so we run the subprocess on
    // a std::thread and pipe the result back via an mpsc channel that's
    // drained on the GLib main loop (where the overlay can be touched).
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let toast_overlay = toast_overlay.clone();

    std::thread::spawn(move || {
        let cmd_result = if crate::update_worker::is_flatpak() {
            Command::new("flatpak-spawn")
                .args([
                    "--host",
                    "pkexec",
                    "bootc",
                    "install",
                    "reset",
                    "--experimental",
                    "--apply",
                ])
                .output()
        } else {
            Command::new("pkexec")
                .args(["bootc", "install", "reset", "--experimental", "--apply"])
                .output()
        };

        let summary = match cmd_result {
            Ok(out) if out.status.success() => {
                tracing::info!("{} succeeded — `bootc install reset` returned 0", label);
                format!("{label} complete — reboot to finish")
            }
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                let stderr_tail = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .to_string();
                tracing::error!(
                    "{} failed: `bootc install reset` exited {}: {}",
                    label,
                    code,
                    stderr_tail
                );
                format!("{label} failed (exit {code}): {stderr_tail}")
            }
            Err(e) => {
                tracing::error!("{} could not run `bootc install reset`: {}", label, e);
                format!("{label} could not start: {e}")
            }
        };

        let _ = tx.send(summary);
    });

    gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        match rx.try_recv() {
            Ok(summary) => {
                let t = adw::Toast::new(&summary);
                t.set_timeout(6);
                toast_overlay.add_toast(t);
                gtk::glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
        }
    });
}

/// Switch the booted system back to a floating stream tag — the "Unpin"
/// action surfaced when [`is_pinned_tag`] returns true for the booted tag.
/// Runs `pkexec bootc switch <registry>/<image>:<stream>` in the background
/// and toasts the result.
///
/// Same `flatpak-spawn --host` / direct `pkexec` split as the other
/// destructive runners — the Flatpak sandbox has no host pkexec on PATH.
fn run_unpin_to_stream(
    toast_overlay: &adw::ToastOverlay,
    registry_uri: String,
    stream_tag: String,
) {
    let target_ref = format!("{}:{}", registry_uri, stream_tag);

    let toast = adw::Toast::new(&format!("Unpinning… (switching to :{})", stream_tag));
    toast.set_timeout(4);
    toast_overlay.add_toast(toast);

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let toast_overlay = toast_overlay.clone();
    let target_for_thread = target_ref.clone();

    std::thread::spawn(move || {
        let cmd_result = if crate::update_worker::is_flatpak() {
            Command::new("flatpak-spawn")
                .args(["--host", "pkexec", "bootc", "switch", &target_for_thread])
                .output()
        } else {
            Command::new("pkexec")
                .args(["bootc", "switch", &target_for_thread])
                .output()
        };

        let summary = match cmd_result {
            Ok(out) if out.status.success() => {
                tracing::info!("unpin: bootc switch {} succeeded", target_for_thread);
                format!("Unpinned to {} — reboot to apply", target_for_thread)
            }
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                let stderr_tail = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .to_string();
                tracing::error!(
                    "unpin: bootc switch {} failed exit {}: {}",
                    target_for_thread,
                    code,
                    stderr_tail
                );
                format!("Unpin failed (exit {code}): {stderr_tail}")
            }
            Err(e) => {
                tracing::error!("unpin: bootc switch could not start: {}", e);
                format!("Unpin could not start: {e}")
            }
        };

        let _ = tx.send(summary);
    });

    gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        match rx.try_recv() {
            Ok(summary) => {
                let t = adw::Toast::new(&summary);
                t.set_timeout(8);
                toast_overlay.add_toast(t);
                gtk::glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
        }
    });
}

/// Schedule a host reboot at 02:00 (next occurrence) via `pkexec shutdown -r`.
/// User can cancel with `sudo shutdown -c` if they change their mind.
///
/// `shutdown -r 02:00` accepts an HH:MM time string and reboots at the next
/// time the clock crosses that. If it's currently before 02:00 the reboot is
/// today; if after, it's tomorrow morning — both readings of "tonight" are
/// reasonable. We toast either way so the user knows it landed.
fn schedule_reboot_tonight(toast_overlay: &adw::ToastOverlay) {
    // adw::ToastOverlay is GObject-but-not-Send, so we run shutdown(8) on a
    // std::thread and pipe the summary back via mpsc that's drained on the
    // GLib main loop (where the overlay is touchable). Same shape as
    // run_bootc_install_reset above.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let toast_overlay = toast_overlay.clone();

    std::thread::spawn(move || {
        let result = if crate::update_worker::is_flatpak() {
            Command::new("flatpak-spawn")
                .args(["--host", "pkexec", "shutdown", "-r", "02:00"])
                .output()
        } else {
            Command::new("pkexec")
                .args(["shutdown", "-r", "02:00"])
                .output()
        };

        let summary = match result {
            Ok(out) if out.status.success() => {
                tracing::info!("Restart scheduled for 02:00 via shutdown -r");
                "Restart scheduled for 02:00 — `sudo shutdown -c` to cancel".to_string()
            }
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                let stderr_tail = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .to_string();
                tracing::error!(
                    "Failed to schedule reboot: shutdown exited {}: {}",
                    code,
                    stderr_tail
                );
                format!("Couldn't schedule restart (exit {code}): {stderr_tail}")
            }
            Err(e) => {
                tracing::error!("Failed to invoke shutdown: {}", e);
                format!("Couldn't schedule restart: {e}")
            }
        };

        let _ = tx.send(summary);
    });

    gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        match rx.try_recv() {
            Ok(summary) => {
                let t = adw::Toast::new(&summary);
                t.set_timeout(6);
                toast_overlay.add_toast(t);
                gtk::glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
        }
    });
}

/// Run the "powerwash" command set on the host: uninstall every user-installed
/// Flatpak, then remove every Distrobox container. Does NOT touch `/var/home`,
/// `/etc`, or the bootc image — what you get back is a system that boots the
/// same OS but with all third-party apps gone and a clean container fleet.
///
/// We deliberately avoid `bootc install reset` here. That command also wipes
/// `/var/home`, which contradicts the dialog copy ("Your home directory, files,
/// and signed-in accounts are kept"). Factory reset uses `bootc install reset`;
/// powerwash uses this lighter command set.
///
/// All commands run via `flatpak-spawn --host` when inside the sandbox. None
/// of them need pkexec (user-level flatpak uninstall and per-user distrobox
/// operations don't require root), so we don't gate this on polkit.
///
/// This is destructive (apps and containers go away). It should only be reached
/// after the caller has confirmed `!settings.dry_run && !settings.dev_mode` AND
/// user confirmation.
fn run_powerwash(toast_overlay: &adw::ToastOverlay) {
    let current = Settings::load();
    if current.dry_run || current.dev_mode {
        tracing::warn!(
            "Powerwash aborted at the last moment — settings now show dry_run={} dev_mode={}",
            current.dry_run,
            current.dev_mode
        );
        let abort = adw::Toast::new("Powerwash aborted (settings now in dry-run)");
        abort.set_timeout(4);
        toast_overlay.add_toast(abort);
        return;
    }

    let start_toast = adw::Toast::new("Powerwash starting… (uninstalling apps and containers)");
    start_toast.set_timeout(4);
    toast_overlay.add_toast(start_toast);

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let toast_overlay = toast_overlay.clone();

    std::thread::spawn(move || {
        // Each step records (label, ok?, optional error tail). We don't bail
        // on the first failure: even if distrobox isn't installed (no
        // containers to remove), the Flatpak uninstall should still proceed.
        let mut steps: Vec<(&'static str, bool, String)> = Vec::new();

        steps.push(run_host_command(
            "flatpak uninstall (user)",
            &["flatpak", "uninstall", "--user", "--all", "-y"],
        ));
        steps.push(run_host_command(
            "distrobox rm -fa",
            &["distrobox", "rm", "-f", "-a"],
        ));

        let ok_count = steps.iter().filter(|(_, ok, _)| *ok).count();
        let summary = if ok_count == steps.len() {
            "Powerwash complete — apps and containers cleared".to_string()
        } else {
            let failed = steps
                .iter()
                .filter(|(_, ok, _)| !*ok)
                .map(|(label, _, err)| format!("{}: {}", label, err))
                .collect::<Vec<_>>()
                .join("; ");
            format!("Powerwash finished with errors — {failed}")
        };
        for (label, ok, err) in &steps {
            if *ok {
                tracing::info!("Powerwash step '{}' succeeded", label);
            } else {
                tracing::warn!("Powerwash step '{}' failed: {}", label, err);
            }
        }
        let _ = tx.send(summary);
    });

    gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        match rx.try_recv() {
            Ok(summary) => {
                let t = adw::Toast::new(&summary);
                t.set_timeout(6);
                toast_overlay.add_toast(t);
                gtk::glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
        }
    });
}

/// Run a host command (via `flatpak-spawn --host` inside the sandbox, or
/// directly on the host otherwise). Returns (label, ok, error_tail) for the
/// caller to aggregate into a status message.
///
/// `args[0]` is the program name, `args[1..]` are arguments. Exit-code-zero is
/// success; anything else is failure with the last line of stderr as the tail.
fn run_host_command(label: &'static str, args: &[&str]) -> (&'static str, bool, String) {
    let output = if crate::update_worker::is_flatpak() {
        let mut full = vec!["--host"];
        full.extend_from_slice(args);
        Command::new("flatpak-spawn").args(&full).output()
    } else {
        Command::new(args[0]).args(&args[1..]).output()
    };
    match output {
        Ok(out) if out.status.success() => (label, true, String::new()),
        Ok(out) => {
            let tail = String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("(no stderr)")
                .to_string();
            (label, false, tail)
        }
        Err(e) => (label, false, e.to_string()),
    }
}

fn get_host_kernel() -> String {
    let output = if crate::update_worker::is_flatpak() {
        Command::new("flatpak-spawn")
            .args(["--host", "uname", "-r"])
            .output()
    } else {
        Command::new("uname").arg("-r").output()
    };
    output
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "—".to_string())
}

fn get_sample_deployments(_reboot_pending: bool) -> Vec<MockDeployment> {
    // Always try real data first; return empty if unavailable rather than
    // hardcoding Fedora-specific mock data that doesn't apply to other images.
    if let Some(ds) = get_real_deployments() {
        return ds;
    }
    Vec::new()
}

fn rebuild_history_list(
    list_box: &gtk::ListBox,
    deployments: &[MockDeployment],
    expanded_id: Option<&str>,
    sender: &ComponentSender<StatusView>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    for d in deployments {
        let row_container = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let row_header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row_header.set_margin_start(16);
        row_header.set_margin_end(16);
        row_header.set_margin_top(12);
        row_header.set_margin_bottom(12);

        let indicator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let indicator_class = match d.state.as_str() {
            "current" => "deploy-indicator-current",
            "staged" => "deploy-indicator-staged",
            "remote" => "deploy-indicator-staged", // available to pull
            _ => "deploy-indicator-archive",
        };
        indicator.add_css_class(indicator_class);
        row_header.append(&indicator);

        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);

        let title_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name_label = gtk::Label::builder()
            .label(&d.title)
            .halign(gtk::Align::Start)
            .build();
        name_label.add_css_class("heading");
        title_box.append(&name_label);

        if d.state == "current" {
            let pill = gtk::Label::new(Some("Booted"));
            pill.add_css_class("success");
            pill.add_css_class("caption");
            title_box.append(&pill);
        } else if d.state == "staged" {
            let pill = gtk::Label::new(Some("Staged"));
            pill.add_css_class("accent");
            pill.add_css_class("caption");
            title_box.append(&pill);
        } else if d.state == "remote" {
            let pill = gtk::Label::new(Some("Remote"));
            pill.add_css_class("accent");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        if d.pinned {
            let pill = gtk::Label::new(Some("Pinned"));
            pill.add_css_class("warning");
            pill.add_css_class("caption");
            title_box.append(&pill);
        }
        text_box.append(&title_box);

        let digest_short = if d.digest.len() >= 12 {
            &d.digest[0..12]
        } else {
            &d.digest
        };
        let submeta_label = gtk::Label::builder()
            .label(&format!(
                "{}:{}  ·  {}…  ·  {}",
                d.image, d.tag, digest_short, d.deployed
            ))
            .halign(gtk::Align::Start)
            .build();
        submeta_label.add_css_class("caption");
        submeta_label.add_css_class("dim-label");
        text_box.append(&submeta_label);
        row_header.append(&text_box);

        let actions_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);

        let pin_btn = gtk::Button::builder()
            .icon_name("pin-symbolic")
            .tooltip_text(if d.pinned { "Unpin" } else { "Pin" })
            .build();
        pin_btn.add_css_class("flat");
        if d.pinned {
            pin_btn.add_css_class("warning");
        }
        let pin_sender = sender.input_sender().clone();
        let pin_id = d.id.clone();
        pin_btn.connect_clicked(move |_| {
            pin_sender.emit(StatusViewInput::TogglePin(pin_id.clone()));
        });
        if d.state != "remote" {
            actions_box.append(&pin_btn);
        }

        if d.state == "remote" {
            let pull_btn = gtk::Button::builder()
                .icon_name("document-save-symbolic")
                .tooltip_text("Pull this image from registry")
                .build();
            pull_btn.add_css_class("flat");
            let pull_d = d.clone();
            pull_btn.connect_clicked(move |_| {
                println!("[debug] Pull requested for {}:{}", pull_d.image, pull_d.tag);
            });
            actions_box.append(&pull_btn);
        } else if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .icon_name("edit-undo-symbolic")
                .tooltip_text("Roll back to this image")
                .build();
            rb_btn.add_css_class("flat");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            actions_box.append(&rb_btn);
        }

        let is_expanded = expanded_id == Some(&d.id);
        let chevron_icon = if is_expanded {
            "go-up-symbolic"
        } else {
            "go-down-symbolic"
        };
        let chev_btn = gtk::Button::builder().icon_name(chevron_icon).build();
        chev_btn.add_css_class("flat");

        let toggle_sender = sender.input_sender().clone();
        let toggle_id = d.id.clone();
        let text_click_sender = sender.input_sender().clone();
        let text_click_id = d.id.clone();

        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| {
            text_click_sender.emit(StatusViewInput::TogglePin(format!(
                "expand:{}",
                text_click_id
            )));
        });
        text_box.add_controller(gesture);

        chev_btn.connect_clicked(move |_| {
            toggle_sender.emit(StatusViewInput::TogglePin(format!("expand:{}", toggle_id)));
        });
        actions_box.append(&chev_btn);

        row_header.append(&actions_box);
        row_container.append(&row_header);

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(200);
        revealer.set_reveal_child(is_expanded);

        let detail_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
        detail_box.set_margin_start(56);
        detail_box.set_margin_end(24);
        detail_box.set_margin_top(8);
        detail_box.set_margin_bottom(16);

        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(16)
            .build();

        let fields = [
            ("Image", d.image.as_str()),
            ("Tag", d.tag.as_str()),
            ("Digest", d.digest.as_str()),
            ("Deployed", d.deployed_full.as_str()),
            ("Kernel", d.kernel.as_str()),
        ];

        for (row_idx, &(label, val)) in fields.iter().enumerate() {
            let lbl = gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .build();
            lbl.add_css_class("caption");
            lbl.add_css_class("dim-label");

            let val_lbl = gtk::Label::builder()
                .label(val)
                .halign(gtk::Align::Start)
                .build();
            val_lbl.add_css_class("caption");
            val_lbl.add_css_class("monospace");

            grid.attach(&lbl, 0, row_idx as i32, 1, 1);
            grid.attach(&val_lbl, 1, row_idx as i32, 1, 1);
        }

        let pkg_lbl = gtk::Label::builder()
            .label("Packages")
            .halign(gtk::Align::Start)
            .build();
        pkg_lbl.add_css_class("caption");
        pkg_lbl.add_css_class("dim-label");

        let pkg_val = gtk::Label::builder()
            .label(format!("{} installed", d.package_count))
            .halign(gtk::Align::Start)
            .build();
        pkg_val.add_css_class("caption");
        pkg_val.add_css_class("monospace");
        grid.attach(&pkg_lbl, 0, fields.len() as i32, 1, 1);
        grid.attach(&pkg_val, 1, fields.len() as i32, 1, 1);

        let sig_lbl = gtk::Label::builder()
            .label("Signature")
            .halign(gtk::Align::Start)
            .build();
        sig_lbl.add_css_class("caption");
        sig_lbl.add_css_class("dim-label");

        let sig_val = gtk::Label::builder()
            .label(format!("✓ Verified  ·  {}", d.signer))
            .halign(gtk::Align::Start)
            .build();
        sig_val.add_css_class("caption");
        sig_val.add_css_class("success");
        grid.attach(&sig_lbl, 0, (fields.len() + 1) as i32, 1, 1);
        grid.attach(&sig_val, 1, (fields.len() + 1) as i32, 1, 1);

        detail_box.append(&grid);

        let bottom_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        if d.state == "remote" {
            let pull_btn = gtk::Button::builder()
                .label("Pull this image")
                .icon_name("document-save-symbolic")
                .build();
            pull_btn.add_css_class("suggested-action");
            let pull_d = d.clone();
            pull_btn.connect_clicked(move |_| {
                println!("[debug] Pull requested for {}:{}", pull_d.image, pull_d.tag);
            });
            bottom_actions.append(&pull_btn);
        } else if d.state != "current" && d.state != "staged" {
            let rb_btn = gtk::Button::builder()
                .label("Roll back to this")
                .icon_name("edit-undo-symbolic")
                .build();
            rb_btn.add_css_class("suggested-action");
            let rb_sender = sender.input_sender().clone();
            let rb_d = d.clone();
            rb_btn.connect_clicked(move |_| {
                rb_sender.emit(StatusViewInput::RollbackTo(rb_d.clone()));
            });
            bottom_actions.append(&rb_btn);
        }

        if d.state != "current" && d.state != "remote" {
            let def_btn = gtk::Button::builder().label("Set as default boot").build();
            let def_sender = sender.input_sender().clone();
            let def_d = d.clone();
            def_btn.connect_clicked(move |_| {
                def_sender.emit(StatusViewInput::SetDefaultBoot(def_d.clone()));
            });
            bottom_actions.append(&def_btn);
        }

        let ch_btn = gtk::Button::builder().label("View changelog").build();
        ch_btn.add_css_class("flat");
        let ch_sender = sender.input_sender().clone();
        let ch_tag = d.tag.clone();
        ch_btn.connect_clicked(move |_| {
            ch_sender.emit(StatusViewInput::SelectChangelogVersion(ch_tag.clone()));
        });
        bottom_actions.append(&ch_btn);

        detail_box.append(&bottom_actions);
        revealer.set_child(Some(&detail_box));
        row_container.append(&revealer);

        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        row_container.append(&sep);

        list_box.append(&row_container);
    }
}

fn parse_org_repo(uri: &str) -> Option<(String, String)> {
    let clean_uri = if let Some(pos) = uri.find("docker://") {
        &uri[pos + 9..]
    } else {
        uri
    };
    let parts: Vec<&str> = clean_uri.split('/').collect();
    if parts.len() >= 3 {
        let org = parts[1].to_string();
        let repo = parts[2..].join("/");
        Some((org, repo))
    } else if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

fn spawn_changelog_fetch(
    registry_uri: String,
    selected_tag: String,
    sender: ComponentSender<StatusView>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            let total_start = std::time::Instant::now();
            println!(
                "[debug] changelog: starting fetch for registry_uri={}",
                registry_uri
            );

            // Build an ImageRef from registry_uri + selected_tag for the
            // service-layer calls. The stream-level tag (strip the date
            // suffix so e.g. "stable-daily-43.20260527" becomes
            // "stable-daily-43") drives both list_versions and
            // list_available_tags.
            let mut newest_full_ref: Option<String> = None;
            let parts: Vec<&str> = registry_uri.split('/').collect();
            if parts.len() >= 3 {
                let stream =
                    strip_date_suffix(&selected_tag).unwrap_or_else(|| selected_tag.clone());
                let image_ref = crate::service::ImageRef {
                    registry: parts[0].to_string(),
                    org: parts[1].to_string(),
                    image: parts[2..].join("/"),
                    tag: stream,
                    digest: String::new(),
                };
                let svc = crate::service::global();

                // Each network round-trip is timed independently so we can
                // tell which path is the bottleneck (per #48). Look for
                // [debug] changelog: phase= lines in stdout / RUST_LOG output.
                let t = std::time::Instant::now();
                match svc.list_available_tags(&image_ref).await {
                    Ok(available) if !available.is_empty() => {
                        println!(
                            "[debug] changelog: phase=list_available_tags ms={} count={}",
                            t.elapsed().as_millis(),
                            available.len()
                        );
                        let _ = sender.input(StatusViewInput::AvailableTagsLoaded(available));
                    }
                    Ok(_) => println!(
                        "[debug] changelog: phase=list_available_tags ms={} count=0",
                        t.elapsed().as_millis()
                    ),
                    Err(e) => println!(
                        "[debug] changelog: phase=list_available_tags ms={} err={}",
                        t.elapsed().as_millis(),
                        e
                    ),
                }

                let t = std::time::Instant::now();
                match svc.list_versions(&image_ref, 8).await {
                    Ok(versions) => {
                        println!(
                            "[debug] changelog: phase=list_versions ms={} count={}",
                            t.elapsed().as_millis(),
                            versions.len()
                        );
                        // versions are sorted ASCENDING by date — `.last()` is
                        // the newest. Using `.first()` here picked the oldest
                        // ref (e.g. Feb date-stamped tags that don't publish
                        // SPDX SBOMs), breaking the diff every time.
                        newest_full_ref = versions.last().map(|v| v.full_ref.clone());
                        sender.input(StatusViewInput::RegistryVersionsLoaded(versions));
                    }
                    Err(e) => println!(
                        "[debug] changelog: phase=list_versions ms={} err={}",
                        t.elapsed().as_millis(),
                        e
                    ),
                }
            }

            // 2. Fetch GitHub commits (with dates for fallback version building)
            let t_github = std::time::Instant::now();
            if let Some((org, repo)) = parse_org_repo(&registry_uri) {
                #[derive(serde::Deserialize)]
                struct GithubCommit {
                    sha: String,
                    commit: CommitDetails,
                }
                #[derive(serde::Deserialize)]
                struct CommitDetails {
                    message: String,
                    author: AuthorDetails,
                }
                #[derive(serde::Deserialize)]
                struct AuthorDetails {
                    name: String,
                    date: String,
                }

                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .user_agent("Finupdate/0.1.0")
                    .build()
                    .unwrap_or_default();

                let mut all_commits: Vec<(String, String, String, String)> = Vec::new();

                // Fetch commits from main repository
                let url = format!("https://api.github.com/repos/{}/{}/commits", org, repo);
                println!("[debug] changelog: phase=github_commits url={}", url);
                if let Ok(resp) = client.get(&url).send().await {
                    if let Ok(commits_json) = resp.json::<Vec<GithubCommit>>().await {
                        all_commits.extend(
                            commits_json
                                .into_iter()
                                .map(|c| (c.sha, c.commit.message, c.commit.author.name, c.commit.author.date)),
                        );
                    }
                }

                // Fetch commits from projectbluefin/common submodule (feature implementations)
                let common_url = "https://api.github.com/repos/projectbluefin/common/commits";
                println!(
                    "[debug] changelog: phase=github_commits_common url={}",
                    common_url
                );
                if let Ok(resp) = client.get(common_url).send().await {
                    if let Ok(commits_json) = resp.json::<Vec<GithubCommit>>().await {
                        // Only include feat: commits from common repo to avoid clutter
                        all_commits.extend(
                            commits_json
                                .into_iter()
                                .filter(|c| c.commit.message.starts_with("feat:"))
                                .map(|c| (c.sha, c.commit.message, c.commit.author.name, c.commit.author.date)),
                        );
                    }
                }

                // Sort to put feat: commits first, then others
                all_commits.sort_by(|a, b| {
                    let a_is_feat = a.1.starts_with("feat:");
                    let b_is_feat = b.1.starts_with("feat:");
                    match (a_is_feat, b_is_feat) {
                        (true, false) => std::cmp::Ordering::Less, // feat comes first
                        (false, true) => std::cmp::Ordering::Greater, // non-feat comes last
                        _ => std::cmp::Ordering::Equal,            // same priority, preserve order
                    }
                });

                println!(
                    "[debug] changelog: phase=github_commits ms={} count={}",
                    t_github.elapsed().as_millis(),
                    all_commits.len()
                );
                let _ = sender.input(StatusViewInput::GithubCommitsLoaded(all_commits));
            }
            println!(
                "[debug] changelog: phase=total ms={}",
                total_start.elapsed().as_millis()
            );

            // 3. Fetch and diff SBOMs — lazily, in a detached task. SPDX
            //    artifacts are MB-scale tarballs that parse to thousands of
            //    package entries; running this on the same critical-path
            //    thread as the home-page registry fetch was the freeze the
            //    user reported. With tokio::spawn the task survives this
            //    runtime's scope and only emits SbomDiffLoaded when the
            //    user has already seen commits + history rendered.
            //
            //    IMPORTANT: Do NOT use strip_date_suffix() refs here. If both
            //    booted and target resolve to the same floating tag (e.g. both
            //    "ghcr.io/projectbluefin/dakota:latest") the diff is trivially
            //    empty. Use the actual full_ref from the newest registry version,
            //    and the actual booted image's digest from bootc status.
            let settings = Settings::load();
            // Prefer the actual date-stamped full_ref of the newest build;
            // fall back to the stream tag only if we got no versions.
            let target_ref = newest_full_ref
                .unwrap_or_else(|| format!("{}:{}", registry_uri, selected_tag));

            // Get the booted image's actual digest-pinned ref from bootc
            // status or mock_identity so we compare two distinct manifests.
            let booted_ref = if let Some(ref mock) = settings.mock_identity {
                if let Some(ref digest) = mock.digest {
                    format!("{}/{}/{}@{}", mock.registry, mock.org, mock.image, digest)
                } else {
                    format!("{}/{}/{}:{}", mock.registry, mock.org, mock.image, mock.tag)
                }
            } else {
                get_cached_bootc_status()
                    .and_then(|json| {
                        let booted = json.pointer("/status/booted")?;
                        let img = booted
                            .pointer("/image/image/image")
                            .or_else(|| booted.pointer("/image/image"))
                            .and_then(|v| v.as_str())?;
                        let digest = booted
                            .pointer("/image/imageDigest")
                            .and_then(|v| v.as_str())?;
                        Some(format!("{}@{}", img, digest))
                    })
                    .unwrap_or_else(|| {
                        format!("{}:{}", registry_uri, read_selected_tag())
                    })
            };

            if booted_ref != target_ref {
                // Tell the UI we're starting so it can render the
                // "Comparing packages…" placeholder. Without this the
                // Stack section is silently blank for 30+ seconds on
                // slow connections.
                let _ = sender.input(StatusViewInput::SbomDiffStarted);
                let sbom_sender = sender.clone();
                tokio::spawn(async move {
                    tracing::debug!(
                        "sbom_diff: deferred fetch booted_ref={} target_ref={}",
                        booted_ref,
                        target_ref
                    );
                    match crate::sbom_diff::fetch_and_diff_sboms(booted_ref, target_ref).await
                    {
                        Some(diff) => {
                            sbom_sender.input(StatusViewInput::SbomDiffLoaded(diff));
                        }
                        None => {
                            tracing::info!(
                                "sbom_diff: no diff available (registry didn't return SPDX referrers)"
                            );
                            sbom_sender.input(StatusViewInput::SbomDiffUnavailable);
                        }
                    }
                });
            } else {
                tracing::debug!("sbom_diff: skipped (booted == target, same image)");
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── parse_booted_image_summary ───────────────────────────────────────
    // Pure JSON-shape tests for the hero-row subtitle helper. The bootc
    // status JSON shape is `{ "status": { "booted": { "image": { ... } } } }`.

    #[test]
    fn booted_summary_with_image_digest_and_date() {
        let j = json!({
            "status": {
                "booted": {
                    "image": {
                        "image": { "image": "ghcr.io/projectbluefin/dakota:latest" },
                        "imageDigest": "sha256:bc6d66c90d1e230b89f71a459fcd9f07fd72582b5a2a633f71885e7f6bf722ed",
                        "timestamp": "2026-05-30T02:20:28Z"
                    }
                }
            }
        });
        // Two-line subtitle: ref on line 1, "shaDIGEST · YYYY-MM-DD" on line 2.
        assert_eq!(
            parse_booted_image_summary(&j),
            Some("ghcr.io/projectbluefin/dakota:latest\nbc6d66c9 · 2026-05-30".to_string())
        );
    }

    #[test]
    fn booted_summary_with_image_and_digest_no_date() {
        let j = json!({
            "status": {
                "booted": {
                    "image": {
                        "image": { "image": "ghcr.io/projectbluefin/dakota:latest" },
                        "imageDigest": "sha256:abcdef1234567890"
                    }
                }
            }
        });
        assert_eq!(
            parse_booted_image_summary(&j),
            Some("ghcr.io/projectbluefin/dakota:latest\nabcdef12".to_string())
        );
    }

    #[test]
    fn booted_summary_with_image_only() {
        let j = json!({
            "status": { "booted": { "image": { "image": { "image": "ghcr.io/projectbluefin/dakota:latest" } } } }
        });
        assert_eq!(
            parse_booted_image_summary(&j),
            Some("ghcr.io/projectbluefin/dakota:latest".to_string())
        );
    }

    #[test]
    fn booted_summary_with_digest_only() {
        let j = json!({
            "status": {
                "booted": { "image": { "imageDigest": "sha256:cafe1234ffff5678" } }
            }
        });
        // Digest-only (no image ref): renders as just the second-line piece.
        assert_eq!(
            parse_booted_image_summary(&j),
            Some("cafe1234".to_string())
        );
    }

    #[test]
    fn booted_summary_handles_unprefixed_digest() {
        // Some bootc versions emit the digest without the `sha256:` prefix.
        let j = json!({
            "status": {
                "booted": { "image": { "imageDigest": "00ff11ee22dd33cc" } }
            }
        });
        assert_eq!(
            parse_booted_image_summary(&j),
            Some("00ff11ee".to_string())
        );
    }

    #[test]
    fn booted_summary_missing_booted_returns_none() {
        let j = json!({ "status": {} });
        assert_eq!(parse_booted_image_summary(&j), None);
    }

    #[test]
    fn booted_summary_empty_image_returns_none() {
        let j = json!({ "status": { "booted": { "image": {} } } });
        assert_eq!(parse_booted_image_summary(&j), None);
    }

    // ── parse_booted_tag_suffix ──────────────────────────────────────────
    // Pulls the tag suffix from the booted image ref so the changelog page
    // can pair the booted build with its registry_versions entry.

    #[test]
    fn booted_tag_suffix_extracts_tag() {
        let j = json!({
            "status": {
                "booted": {
                    "image": { "image": { "image": "ghcr.io/projectbluefin/dakota:stable-daily-43.20260530" } }
                }
            }
        });
        assert_eq!(
            parse_booted_tag_suffix(&j),
            Some("stable-daily-43.20260530".to_string())
        );
    }

    #[test]
    fn booted_tag_suffix_missing_image_returns_none() {
        let j = json!({ "status": { "booted": {} } });
        assert_eq!(parse_booted_tag_suffix(&j), None);
    }

    #[test]
    fn booted_tag_suffix_untagged_image_returns_none() {
        // No `:tag` separator → nothing to extract.
        let j = json!({
            "status": { "booted": { "image": { "image": { "image": "ghcr.io/projectbluefin/dakota" } } } }
        });
        assert_eq!(parse_booted_tag_suffix(&j), None);
    }

    // ── build_stack_items ────────────────────────────────────────────────
    // Constructs the from→to rows the changelog Stack renders. Marks
    // `bumped=true` only when the value actually moved so the renderer
    // can highlight just the components that changed.

    fn fake_image_version(
        version: &str,
        kernel: &str,
        revision: &str,
        created_iso: &str,
    ) -> ImageVersion {
        ImageVersion {
            date: chrono::NaiveDate::from_ymd_opt(2026, 5, 30).unwrap(),
            full_ref: format!("ghcr.io/example/image:{version}"),
            version: version.to_string(),
            kernel: kernel.to_string(),
            revision: revision.to_string(),
            created: chrono::DateTime::parse_from_rfc3339(created_iso)
                .unwrap()
                .with_timezone(&chrono::Utc),
        }
    }

    #[test]
    fn stack_items_empty_without_target() {
        assert!(build_stack_items(None, None, None).is_empty());
    }

    #[test]
    fn stack_items_marks_changed_components_as_bumped() {
        let booted = fake_image_version(
            "stable-daily-43.20260527",
            "6.13.4-200.fc41",
            "abc1234deadbeef",
            "2026-05-27T12:00:00Z",
        );
        let target = fake_image_version(
            "stable-daily-43.20260530",
            "6.13.5-201.fc41",
            "def5678feedface",
            "2026-05-30T12:00:00Z",
        );
        let items = build_stack_items(Some(&booted), Some(&target), None);
        let by_label: std::collections::HashMap<&str, &StackItem> =
            items.iter().map(|i| (i.label, i)).collect();
        assert!(by_label["Image"].bumped);
        assert!(by_label["Kernel"].bumped);
        assert!(by_label["Revision"].bumped);
        assert!(by_label["Built"].bumped);
        assert_eq!(
            by_label["Image"].current.as_deref(),
            Some("stable-daily-43.20260527")
        );
        assert_eq!(by_label["Image"].target, "stable-daily-43.20260530");
        assert_eq!(by_label["Revision"].target, "def5678");
    }

    #[test]
    fn stack_items_marks_unchanged_components_not_bumped() {
        // Same booted as target — every row is bumped=false. Used when the
        // user is browsing the changelog for the version they're already on.
        let v = fake_image_version(
            "stable-daily-43.20260530",
            "6.13.5-201.fc41",
            "def5678feedface",
            "2026-05-30T12:00:00Z",
        );
        let items = build_stack_items(Some(&v), Some(&v), None);
        for item in &items {
            assert!(!item.bumped, "{} should not be bumped", item.label);
        }
    }

    #[test]
    fn stack_items_without_booted_treat_target_as_bumped() {
        // bootc-status missing → every component is unknown on the "from"
        // side and should render as bumped so the user sees the values they
        // would land on.
        let target = fake_image_version(
            "stable-daily-43.20260530",
            "6.13.5-201.fc41",
            "def5678feedface",
            "2026-05-30T12:00:00Z",
        );
        let items = build_stack_items(None, Some(&target), None);
        for item in &items {
            assert!(item.bumped, "{} should be bumped", item.label);
            assert!(item.current.is_none());
        }
    }

    // ── Dakota scenarios ─────────────────────────────────────────────────
    // Dakota's registry data is bare: no kernel annotation anywhere, an
    // empty revision on some builds, and a `version` annotation that may
    // be just a date ("20260530") rather than the Bluefin-style dated
    // stream ("stable-daily-43.20260530"). These cases verify the Stack
    // section degrades sensibly.

    #[test]
    fn stack_items_dakota_no_kernel_either_side_hides_kernel_row() {
        // Both sides empty kernel → row should be omitted entirely so the
        // user doesn't see "— → —".
        let booted = fake_image_version("20260527", "", "abc1234", "2026-05-27T12:00:00Z");
        let target = fake_image_version("20260530", "", "def5678", "2026-05-30T12:00:00Z");
        let items = build_stack_items(Some(&booted), Some(&target), None);
        assert!(
            !items.iter().any(|i| i.label == "Kernel"),
            "Kernel row should be hidden when both sides are empty"
        );
        // Image / Revision / Built still appear.
        assert!(items.iter().any(|i| i.label == "Image"));
        assert!(items.iter().any(|i| i.label == "Revision"));
        assert!(items.iter().any(|i| i.label == "Built"));
    }

    #[test]
    fn stack_items_dakota_uses_host_kernel_as_fallback() {
        // Registry side has no kernel, but uname -r is known — show the
        // host kernel on the current side so the user can at least see what
        // they're actually running.
        let booted = fake_image_version("20260527", "", "abc1234", "2026-05-27T12:00:00Z");
        let target = fake_image_version("20260530", "", "def5678", "2026-05-30T12:00:00Z");
        let items = build_stack_items(Some(&booted), Some(&target), Some("7.0.7"));
        let kernel = items.iter().find(|i| i.label == "Kernel");
        assert!(kernel.is_some(), "Kernel row should be present when host_kernel is known");
        let k = kernel.unwrap();
        assert_eq!(k.current.as_deref(), Some("7.0.7"));
        assert_eq!(k.target, "");
        // One-sided data → not flagged as bumped (we can't know if it
        // actually changed).
        assert!(!k.bumped);
    }

    #[test]
    fn stack_items_dakota_omits_revision_when_target_missing() {
        let target = fake_image_version("20260530", "", "", "2026-05-30T12:00:00Z");
        let items = build_stack_items(None, Some(&target), None);
        assert!(
            !items.iter().any(|i| i.label == "Revision"),
            "Revision should be hidden when target revision is empty"
        );
    }

    // ── extract_yyyymmdd_date ────────────────────────────────────────────

    #[test]
    fn extract_date_from_bare_date() {
        assert_eq!(
            extract_yyyymmdd_date("20260530"),
            chrono::NaiveDate::from_ymd_opt(2026, 5, 30)
        );
    }

    #[test]
    fn extract_date_from_dotted_stream_tag() {
        assert_eq!(
            extract_yyyymmdd_date("latest.20260530"),
            chrono::NaiveDate::from_ymd_opt(2026, 5, 30)
        );
    }

    #[test]
    fn extract_date_from_dashed_bluefin_tag() {
        assert_eq!(
            extract_yyyymmdd_date("stable-daily-43.20260602"),
            chrono::NaiveDate::from_ymd_opt(2026, 6, 2)
        );
    }

    #[test]
    fn extract_date_rejects_non_date_runs() {
        // 8-digit hex sha is not a date.
        assert_eq!(extract_yyyymmdd_date("abc12345"), None);
        // 12-digit run shouldn't be sliced into a date.
        assert_eq!(extract_yyyymmdd_date("000020260530"), None);
        assert_eq!(extract_yyyymmdd_date("latest"), None);
    }

    // ── find_booted_match ────────────────────────────────────────────────

    #[test]
    fn find_booted_match_exact_version() {
        let v1 = fake_image_version(
            "stable-daily-43.20260527",
            "6.13.4",
            "abc1234",
            "2026-05-27T12:00:00Z",
        );
        let v2 = fake_image_version(
            "stable-daily-43.20260530",
            "6.13.5",
            "def5678",
            "2026-05-30T12:00:00Z",
        );
        let versions = vec![v1, v2];
        let hit = find_booted_match(&versions, "stable-daily-43.20260530");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().version, "stable-daily-43.20260530");
    }

    #[test]
    fn find_booted_match_substring_handles_dakota_anchor() {
        // Dakota's os-release IMAGE_VERSION="20260530", but the registry
        // entry's version annotation is "latest.20260530". Substring match
        // gets us there.
        let v = fake_image_version("latest.20260530", "", "abc1234", "2026-05-30T12:00:00Z");
        let versions = vec![v];
        let hit = find_booted_match(&versions, "20260530");
        assert!(hit.is_some());
    }

    #[test]
    fn find_booted_match_date_fallback() {
        // Booted anchor is "latest" (no date), but we know the host's
        // booted date — wait, anchor would carry the date in the os-release
        // form. This guards the parse path: anchor "20260530" matches v.date
        // even if the version string is something unrelated.
        let v = fake_image_version("local-build", "", "abc1234", "2026-05-30T12:00:00Z");
        let mut v_dated = v.clone();
        v_dated.date = chrono::NaiveDate::from_ymd_opt(2026, 5, 30).unwrap();
        let versions = vec![v_dated];
        let hit = find_booted_match(&versions, "20260530");
        assert!(hit.is_some());
    }

    #[test]
    fn find_booted_match_returns_none_for_unrelated_anchor() {
        let v = fake_image_version("latest.20260530", "", "abc1234", "2026-05-30T12:00:00Z");
        let versions = vec![v];
        assert!(find_booted_match(&versions, "foobar").is_none());
    }

    // ── parse_os_release_field ───────────────────────────────────────────

    const SAMPLE_OS_RELEASE: &str = r#"NAME="Bluefin Dakota"
PRETTY_NAME="Bluefin Dakota"
ID=dakota
VERSION_ID="43"
IMAGE_ID=dakota
VARIANT_ID=dakota
LOGO=bluefin
"#;

    #[test]
    fn os_release_pretty_name_unquoted() {
        assert_eq!(
            parse_os_release_field(SAMPLE_OS_RELEASE, "PRETTY_NAME"),
            Some("Bluefin Dakota".to_string())
        );
    }

    #[test]
    fn os_release_unquoted_value() {
        assert_eq!(
            parse_os_release_field(SAMPLE_OS_RELEASE, "ID"),
            Some("dakota".to_string())
        );
    }

    #[test]
    fn os_release_missing_key_returns_none() {
        assert_eq!(parse_os_release_field(SAMPLE_OS_RELEASE, "BUILD_ID"), None);
    }

    #[test]
    fn os_release_empty_value_skipped() {
        // VARIANT="" should NOT be returned — empty strings aren't useful.
        let content = "ID=fedora\nVARIANT=\"\"\nLOGO=fedora\n";
        assert_eq!(parse_os_release_field(content, "VARIANT"), None);
        // But ID still wins.
        assert_eq!(
            parse_os_release_field(content, "ID"),
            Some("fedora".to_string())
        );
    }

    #[test]
    fn os_release_first_match_wins() {
        // os-release CAN have duplicate keys in pathological cases — first
        // occurrence wins (matches the read order).
        let content = "ID=first\nID=second\n";
        assert_eq!(
            parse_os_release_field(content, "ID"),
            Some("first".to_string())
        );
    }

    // ── strip_date_suffix ────────────────────────────────────────────────
    // Mirror of the parser in registry_client::strip_date_suffix but a
    // separate implementation lives here for the home page's tag parsing.
    // Tests guard against the two diverging.

    #[test]
    fn strip_date_suffix_dot_form() {
        assert_eq!(
            strip_date_suffix("stable-daily-43.20260527"),
            Some("stable-daily-43".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_dash_form() {
        assert_eq!(
            strip_date_suffix("lts-hwe-20260224"),
            Some("lts-hwe".to_string())
        );
    }

    #[test]
    fn strip_date_suffix_rejects_too_short() {
        assert_eq!(strip_date_suffix("stable-2026"), None);
    }

    #[test]
    fn strip_date_suffix_rejects_non_digits() {
        assert_eq!(strip_date_suffix("stable-20260abc"), None);
    }

    #[test]
    fn strip_date_suffix_rejects_no_separator() {
        assert_eq!(strip_date_suffix("stable20260527"), None);
    }

    #[test]
    fn strip_date_suffix_bare_date_returns_none() {
        // 20260527 alone is 8 digits but has no separator — so strip can't
        // detect where to split. The bare-date case is owned by
        // parse_dated_tag with stream==""; strip_date_suffix only handles
        // prefixed forms.
        assert_eq!(strip_date_suffix("20260527"), None);
    }

    // ── parse_image_ref_fields ───────────────────────────────────────────

    #[test]
    fn parse_image_ref_fields_empty_returns_placeholders() {
        let (name, tag, org) = parse_image_ref_fields("");
        assert_eq!(name, "Unknown");
        assert_eq!(tag, "latest");
        assert_eq!(org, "unknown");
    }

    #[test]
    fn parse_image_ref_fields_full_ref() {
        let (name, tag, org) = parse_image_ref_fields("ghcr.io/ublue-os/bluefin:stable");
        assert_eq!(name, "bluefin");
        assert_eq!(tag, "stable");
        assert_eq!(org, "ublue-os");
    }

    #[test]
    fn parse_image_ref_fields_no_colon_defaults_to_latest() {
        let (name, tag, org) = parse_image_ref_fields("ghcr.io/projectbluefin/dakota");
        assert_eq!(name, "dakota");
        assert_eq!(tag, "latest");
        assert_eq!(org, "projectbluefin");
    }

    #[test]
    fn parse_image_ref_fields_single_segment() {
        let (name, tag, org) = parse_image_ref_fields("standalone");
        assert_eq!(name, "standalone");
        assert_eq!(tag, "latest");
        assert_eq!(org, "unknown");
    }

    // ── parse_org_repo ───────────────────────────────────────────────────

    #[test]
    fn parse_org_repo_ghcr_three_parts() {
        let r = parse_org_repo("ghcr.io/ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_two_parts() {
        // No registry prefix — treat as org/repo directly.
        let r = parse_org_repo("ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_strips_docker_prefix() {
        let r = parse_org_repo("docker://ghcr.io/ublue-os/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_handles_nested_path() {
        // GHCR allows nested paths like /org/sub/image. We keep everything
        // past the first split as the repo so downstream code can construct
        // a valid GitHub URL.
        let r = parse_org_repo("ghcr.io/ublue-os/sub/bluefin");
        assert_eq!(r, Some(("ublue-os".to_string(), "sub/bluefin".to_string())));
    }

    #[test]
    fn parse_org_repo_rejects_single_segment() {
        assert!(parse_org_repo("bluefin").is_none());
    }

    // ── get_real_deployments_from_json ───────────────────────────────────
    // Validates the parsing that turns a bootc-status JSON blob into a
    // list of MockDeployment rows for the history page.

    #[test]
    fn deployments_parses_booted_only() {
        // get_real_deployments_from_json uses the "current"/"previous"/
        // "staged" labels — matching the home-page UI's history row badges
        // — instead of the raw bootc terms. The mapping:
        //    status.booted   → state="current"  (the row badged "Active")
        //    status.rollback → state="previous"
        //    status.staged   → state="staged"
        let json: Value = serde_json::from_str(r#"{
            "status": {
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:baea47c64413bc61a6901e99ceb052bee843d05d406fe33513497863074d84ef"
                    }
                }
            }
        }"#).unwrap();
        let deps = get_real_deployments_from_json(&json).expect("parses");
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].state, "current");
        assert_eq!(deps[0].title, "dakota");
        assert_eq!(deps[0].tag, "latest");
    }

    #[test]
    fn deployments_parses_booted_and_rollback() {
        let json: Value = serde_json::from_str(
            r#"{
            "status": {
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:aaaa"
                    }
                },
                "rollback": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-27T14:21:59Z",
                        "imageDigest": "sha256:bbbb"
                    }
                }
            }
        }"#,
        )
        .unwrap();
        let deps = get_real_deployments_from_json(&json).expect("parses");
        let states: Vec<&str> = deps.iter().map(|d| d.state.as_str()).collect();
        assert!(states.contains(&"current"), "states: {states:?}");
        assert!(states.contains(&"previous"), "states: {states:?}");
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn deployments_parses_staged_first() {
        // The function emits in fixed order: staged, current, previous. So
        // even though staged represents "the next boot", it appears first
        // in the result vector. Verify that ordering.
        let json: Value = serde_json::from_str(
            r#"{
            "status": {
                "staged": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota-nvidia:latest"},
                        "timestamp": "2026-05-30T02:20:28Z",
                        "imageDigest": "sha256:cccc"
                    }
                },
                "booted": {
                    "image": {
                        "image": {"image": "ghcr.io/projectbluefin/dakota:latest"},
                        "timestamp": "2026-05-28T16:14:49Z",
                        "imageDigest": "sha256:aaaa"
                    }
                }
            }
        }"#,
        )
        .unwrap();
        let deps = get_real_deployments_from_json(&json).expect("parses");
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].state, "staged");
        assert_eq!(deps[0].title, "dakota-nvidia");
        assert_eq!(deps[1].state, "current");
    }

    #[test]
    fn deployments_returns_none_for_empty_status() {
        let json: Value = serde_json::from_str(r#"{"status": {}}"#).unwrap();
        // No booted entry → can't surface anything useful.
        assert!(get_real_deployments_from_json(&json).is_none());
    }

    #[test]
    fn deployments_returns_none_when_status_missing() {
        let json: Value = serde_json::from_str(r#"{"apiVersion": "v1"}"#).unwrap();
        assert!(get_real_deployments_from_json(&json).is_none());
    }
}
