//! Rebase history dialog — lets the user rebase to any OS image from the
//! last 90 days via a calendar grid.
//!
//! Entry point: [`show_rebase_dialog`] — opens a modal `adw::Dialog`.
//!
//! ## Flow
//!
//! ```text
//! show_rebase_dialog()
//!   └── spawn background thread
//!         └── RegistryClient::detect() → fetch_versions(90)
//!               → result slot + timeout poll on UI thread
//!                    ├── Success → show calendar + details panel
//!                    └── Error   → show error page with retry
//! ```
//!
//! When the user picks a date and confirms, `bootc switch {full_ref}` is
//! run on the host via the same `flatpak-spawn --host pkexec` pattern as
//! the main update worker.

use adw::prelude::*;
use chrono::{Datelike, Local, NaiveDate};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crate::registry_client::ImageVersion;
use crate::service::{self, FamilyInfo};
use crate::update_worker::is_flatpak;

/// Open the rebase history dialog as a child of `parent`.
pub fn show_rebase_dialog(parent: &adw::ApplicationWindow, dev_mode: bool) {
    let dialog = adw::Dialog::builder()
        .title("Rebase to Previous Version")
        .content_width(520)
        .content_height(600)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);

    // ── Feature-switch variant selector ───────────────────────────────────
    //
    // Replaces the hardcoded Dakota/Dakota-Nvidia pair with one SwitchRow
    // per atomic feature available in the current Family — derived live from
    // the Family taxonomy (KNOWN_FAMILIES). The user picks Nvidia / DX / HWE
    // etc. by name rather than choosing a raw image; the resolved target
    // image is shown in a preview row at the bottom of the group.
    //
    // The legacy `variant_state: Rc<RefCell<String>>` interface is preserved
    // for the existing `start_version_fetch` API — we feed it an empty string
    // when the user hasn't picked features (no extra filter), so the loaded
    // page renders the full base-image history.
    let variant_state = Rc::new(RefCell::new(String::new()));
    let selected_features: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let selected_stream: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let current_family: Rc<RefCell<Option<FamilyInfo>>> = Rc::new(RefCell::new(None));

    let variant_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    variant_box.set_margin_start(16);
    variant_box.set_margin_end(16);
    variant_box.set_margin_top(12);
    variant_box.set_margin_bottom(12);

    let family_label = gtk::Label::new(Some("Loading family info…"));
    family_label.set_halign(gtk::Align::Start);
    family_label.add_css_class("heading");
    variant_box.append(&family_label);

    // Stream selector (populated once family is detected)
    let stream_row = adw::ComboBoxRow::builder().title("Stream").build();
    let stream_group = adw::PreferencesGroup::new();
    stream_group.add(&stream_row);
    variant_box.append(&stream_group);

    // PreferencesGroup hosts the dynamic SwitchRow list. Populated once the
    // initial fetch completes and we know which family we're on.
    let features_group = adw::PreferencesGroup::new();
    variant_box.append(&features_group);

    let target_image_row = adw::ActionRow::builder()
        .title("Target image")
        .subtitle("(select features above)")
        .build();
    let target_chip = gtk::Image::from_icon_name("emblem-default-symbolic");
    target_chip.add_css_class("dim-label");
    target_image_row.add_suffix(&target_chip);
    let target_image_group = adw::PreferencesGroup::new();
    target_image_group.add(&target_image_row);
    variant_box.append(&target_image_group);

    // ── Stack: loading / loaded / error ────────────────────────────────
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(200)
        .build();

    // Loading page
    let loading_page = {
        let status = adw::StatusPage::builder()
            .title("Loading Version History")
            .description("Fetching available image versions…")
            .build();
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(32, 32);
        status.set_child(Some(&spinner));
        status
    };
    stack.add_named(&loading_page, Some("loading"));

    // Error page
    let retry_button = gtk::Button::builder()
        .label("Retry")
        .halign(gtk::Align::Center)
        .build();
    let error_page = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("Couldn't Load Versions")
        .description("Check your internet connection and try again.")
        .build();
    error_page.set_child(Some(&retry_button));
    stack.add_named(&error_page, Some("error"));

    // Loaded page — built dynamically once data arrives
    let loaded_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let loaded_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    loaded_scroll.set_child(Some(&loaded_box));
    stack.add_named(&loaded_scroll, Some("loaded"));

    let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_box.append(&variant_box);
    let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    main_box.append(&separator);
    main_box.append(&stack);
    toolbar_view.set_content(Some(&main_box));
    dialog.set_child(Some(&toolbar_view));
    stack.set_visible_child_name("loading");

    let stack_for_retry = stack.clone();
    let loaded_box_for_retry = loaded_box.clone();
    let dialog_for_retry = dialog.clone();
    let parent_for_retry = parent.clone();
    let error_page_for_retry = error_page.clone();
    let variant_state_for_retry = variant_state.clone();
    let current_family_for_retry = current_family.clone();
    let selected_features_for_retry = selected_features.clone();
    retry_button.connect_clicked(move |_| {
        let variant = variant_state_for_retry.borrow().clone();
        start_version_fetch(
            stack_for_retry.clone(),
            loaded_box_for_retry.clone(),
            dialog_for_retry.clone(),
            parent_for_retry.clone(),
            error_page_for_retry.clone(),
            dev_mode,
            &variant,
            current_family_for_retry.clone(),
            selected_features_for_retry.clone(),
            INITIAL_FETCH_COUNT,
        );
    });

    // Family + feature switches are populated AFTER the initial fetch
    // completes (we need the detected RegistryClient to know which family
    // we're on). Switches are wired to recompute the target_image_row
    // suffix label as the user toggles them; restarting fetch on every
    // toggle would thrash the network — instead the user clicks Rebase to
    // commit a switch to a different image.
    populate_family_switches(
        &features_group,
        &family_label,
        &target_image_row,
        &stream_row,
        current_family.clone(),
        selected_features.clone(),
        selected_stream.clone(),
    );

    dialog.present(Some(parent));
    let initial_variant = variant_state.borrow().clone();
    start_version_fetch(
        stack.clone(),
        loaded_box.clone(),
        dialog.clone(),
        parent.clone(),
        error_page.clone(),
        dev_mode,
        &initial_variant,
        current_family.clone(),
        selected_features.clone(),
        INITIAL_FETCH_COUNT,
    );
}

/// Initial number of versions fetched when the rebase dialog opens.
/// Powers the dropdown of recent builds (the user only sees the top 4 in the
/// dropdown, but we pre-fetch a slightly larger window so the calendar has
/// content the instant "Show older builds" is clicked). The "Load older
/// builds" affordance inside the calendar re-fetches with EXPANDED_FETCH_COUNT.
const INITIAL_FETCH_COUNT: usize = 12;
const EXPANDED_FETCH_COUNT: usize = 120;
const RECENT_DROPDOWN_COUNT: usize = 4;

#[allow(clippy::too_many_arguments)]
fn start_version_fetch(
    stack: gtk::Stack,
    loaded_box: gtk::Box,
    dialog: adw::Dialog,
    parent: adw::ApplicationWindow,
    error_page: adw::StatusPage,
    dev_mode: bool,
    variant: &str,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    max_versions: usize,
) {
    stack.set_visible_child_name("loading");
    error_page.set_description(Some("Check your internet connection and try again."));

    let variant_str = variant.to_string();
    let result_slot: Arc<Mutex<Option<FetchResult>>> = Arc::new(Mutex::new(None));
    spawn_fetch_thread(result_slot.clone(), &variant_str, max_versions);

    // Build a "reload with EXPANDED_FETCH_COUNT" closure that the loaded
    // page passes to the "Load older builds" button inside the calendar.
    // Has to be a Rc<dyn Fn()> so the calendar widget can clone it into
    // its click handler — and so it captures all the args this function
    // needs to recursively call itself.
    let reload_fn: Rc<dyn Fn()> = {
        let stack = stack.clone();
        let loaded_box = loaded_box.clone();
        let dialog = dialog.clone();
        let parent = parent.clone();
        let error_page = error_page.clone();
        let variant_owned = variant_str.clone();
        let current_family = current_family.clone();
        let selected_features = selected_features.clone();
        Rc::new(move || {
            start_version_fetch(
                stack.clone(),
                loaded_box.clone(),
                dialog.clone(),
                parent.clone(),
                error_page.clone(),
                dev_mode,
                &variant_owned,
                current_family.clone(),
                selected_features.clone(),
                EXPANDED_FETCH_COUNT,
            );
        })
    };

    // "is_expanded" flag drives whether we surface the "Load older builds"
    // button — if this call IS the expanded fetch, the button doesn't
    // belong (we already have all 120).
    let is_expanded = max_versions >= EXPANDED_FETCH_COUNT;

    let start_time = std::time::Instant::now();
    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if let Some(result) = result_slot.lock().ok().and_then(|mut guard| guard.take()) {
            match result {
                FetchResult::Ok(versions) => {
                    build_loaded_page(
                        &loaded_box,
                        &stack,
                        &dialog,
                        &parent,
                        versions,
                        dev_mode,
                        current_family.clone(),
                        selected_features.clone(),
                        if is_expanded {
                            None
                        } else {
                            Some(reload_fn.clone())
                        },
                    );
                    stack.set_visible_child_name("loaded");
                }
                FetchResult::DetectFailed => {
                    error_page.set_description(Some(
                        "Could not detect the current image. Is bootc installed and managing this system?",
                    ));
                    stack.set_visible_child_name("error");
                }
                FetchResult::Err(_) => {
                    error_page
                        .set_description(Some("Check your internet connection and try again."));
                    stack.set_visible_child_name("error");
                }
            }
            return glib::ControlFlow::Break;
        }

        if start_time.elapsed() > std::time::Duration::from_secs(20) {
            error_page.set_description(Some("Check your internet connection and try again."));
            stack.set_visible_child_name("error");
            return glib::ControlFlow::Break;
        }

        glib::ControlFlow::Continue
    });
}

fn spawn_fetch_thread(
    result_slot: Arc<Mutex<Option<FetchResult>>>,
    variant: &str,
    max_versions: usize,
) {
    let variant_str = variant.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            // Migrated from RegistryClient::detect + fetch_versions(90) to
            // the service layer. current_image() honours mock_identity →
            // bootc status → os-release; list_versions delegates to
            // fetch_versions internally with the config-blob date harvest
            // included. `max_versions` lets the caller scale up the fetch
            // when the user clicks "Load older builds" in the calendar.
            let svc = service::global();
            let result = match svc.current_image().await {
                Err(_) => FetchResult::DetectFailed,
                Ok(image) => match svc.list_versions(&image, max_versions).await {
                    Ok(mut versions) => {
                        if !variant_str.is_empty() && variant_str != "default" {
                            versions.retain(|v| v.version.contains(&variant_str));
                        }
                        FetchResult::Ok(versions)
                    }
                    Err(e) => FetchResult::Err(e.to_string()),
                },
            };
            *result_slot.lock().unwrap() = Some(result);
        });
    });
}

// ── Loaded page builder ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_loaded_page(
    container: &gtk::Box,
    stack: &gtk::Stack,
    dialog: &adw::Dialog,
    parent: &adw::ApplicationWindow,
    versions: Vec<ImageVersion>,
    dev_mode: bool,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    reload_fn: Option<Rc<dyn Fn()>>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    // Version lookup map
    let version_map: HashMap<NaiveDate, ImageVersion> =
        versions.iter().map(|v| (v.date, v.clone())).collect();
    let version_map = Rc::new(version_map);

    // Find "current" date from the version whose full_ref matches bootc status
    // (best-effort: use the most recent version as current for now).
    let current_date: Option<NaiveDate> = versions.last().map(|v| v.date);

    // ── Selected version state ──────────────────────────────────────────
    let selected: Rc<RefCell<Option<NaiveDate>>> = Rc::new(RefCell::new(None));

    // ── Details panel (hidden until selection) ──────────────────────────
    let details_group = adw::PreferencesGroup::builder()
        .title("Selected Version")
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    details_group.set_visible(false);

    let version_row = adw::ActionRow::builder().title("Version").build();
    let kernel_row = adw::ActionRow::builder().title("Kernel").build();
    let built_row = adw::ActionRow::builder().title("Built").build();
    let commit_row = adw::ActionRow::builder().title("Commit").build();

    details_group.add(&version_row);
    details_group.add(&kernel_row);
    details_group.add(&built_row);
    details_group.add(&commit_row);

    // ── Rebase button (disabled until selection) ────────────────────────
    let rebase_btn = gtk::Button::builder()
        .label("Rebase…")
        .sensitive(false)
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(16)
        .build();
    // Inline action button (in-page, not a centered StatusPage CTA) — per
    // GNOME HIG / control-center About "Donate" pattern, use .suggested-action
    // alone, without .pill. .pill is reserved for standalone hero CTAs.
    rebase_btn.add_css_class("suggested-action");

    // ── Build calendar grid ─────────────────────────────────────────────
    let calendar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    calendar_box.set_margin_start(8);
    calendar_box.set_margin_end(8);
    calendar_box.set_margin_top(16);
    calendar_box.set_margin_bottom(8);

    // Current displayed month — starts at current month.
    let today = Local::now().date_naive();
    let initial_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    let displayed_month: Rc<RefCell<NaiveDate>> = Rc::new(RefCell::new(initial_month));

    // ── Month nav row ───────────────────────────────────────────────────
    let prev_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text("Previous month")
        .build();
    prev_btn.add_css_class("flat");
    prev_btn.add_css_class("circular");

    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text("Next month")
        .build();
    next_btn.add_css_class("flat");
    next_btn.add_css_class("circular");
    // Initially disabled (already on current month)
    next_btn.set_sensitive(false);

    let month_label = gtk::Label::builder()
        .hexpand(true)
        .halign(gtk::Align::Center)
        .build();
    month_label.add_css_class("title-4");

    let nav_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_bottom(12)
        .build();
    nav_row.append(&prev_btn);
    nav_row.append(&month_label);
    nav_row.append(&next_btn);
    calendar_box.append(&nav_row);

    // Weekday headers
    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .margin_bottom(4)
        .build();
    for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
        let lbl = gtk::Label::new(Some(day));
        lbl.add_css_class("caption");
        lbl.add_css_class("dim-label");
        lbl.set_hexpand(true);
        lbl.set_halign(gtk::Align::Center);
        header_row.append(&lbl);
    }
    calendar_box.append(&header_row);

    // Day grid — 7 columns × 6 rows, pre-populated
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(4)
        .row_homogeneous(true)
        .column_homogeneous(true)
        .build();
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = gtk::Button::new();
            btn.add_css_class("flat");
            btn.add_css_class("day-btn");
            grid.attach(&btn, col, row, 1, 1);
        }
    }
    calendar_box.append(&grid);

    // "Load older builds" button at the bottom of the calendar — only when
    // the initial fetch was the small INITIAL_FETCH_COUNT cut. Clicking
    // restarts start_version_fetch with EXPANDED_FETCH_COUNT=120 so the
    // user can roll back past the initial window. Hidden when the page is
    // already showing expanded results.
    if let Some(reload) = reload_fn.as_ref() {
        let load_older_btn = gtk::Button::builder()
            .label("Load older builds")
            .halign(gtk::Align::Center)
            .margin_top(8)
            .margin_bottom(4)
            .build();
        load_older_btn.add_css_class("flat");
        let reload = reload.clone();
        let btn = load_older_btn.clone();
        load_older_btn.connect_clicked(move |_| {
            // Spinner-as-label + insensitive so the user knows the fetch
            // is in flight; start_version_fetch swaps the whole loaded
            // page out so we don't have to reset this state ourselves.
            btn.set_label("Loading…");
            btn.set_sensitive(false);
            reload();
        });
        calendar_box.append(&load_older_btn);
    }

    inject_calendar_css();

    // ── Recent versions dropdown (top of the dialog) ──────────────────────
    // User direction: surface the most recent N builds prominently. The
    // calendar is overkill when the user is just rolling back to last week.
    // Top-RECENT_DROPDOWN_COUNT versions render as a PreferencesGroup of
    // activatable ActionRows; click activates the same selection state the
    // calendar grid uses, so the rebase button + details panel get the same
    // wiring.
    let recent_group = adw::PreferencesGroup::builder()
        .title("Recent builds")
        .description("Most recent images — click to select, then Rebase")
        .margin_start(16)
        .margin_end(16)
        .margin_top(8)
        .margin_bottom(0)
        .build();
    let recent_take = RECENT_DROPDOWN_COUNT.min(versions.len());
    for v in versions.iter().take(recent_take) {
        let row = adw::ActionRow::builder()
            .title(v.date.format("%B %-d, %Y").to_string())
            .subtitle(v.version.clone())
            .activatable(true)
            .build();
        row.set_accessible_role(gtk::AccessibleRole::Button);
        let chev = gtk::Image::from_icon_name("go-next-symbolic");
        chev.add_css_class("dim-label");
        row.add_suffix(&chev);

        let date = v.date;
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let details_group_rc = details_group.clone();
        let version_row_rc = version_row.clone();
        let kernel_row_rc = kernel_row.clone();
        let built_row_rc = built_row.clone();
        let commit_row_rc = commit_row.clone();
        let rebase_btn_rc = rebase_btn.clone();
        row.connect_activated(move |_| {
            *selected_rc.borrow_mut() = Some(date);
            if let Some(v) = version_map_rc.get(&date) {
                update_details(
                    &details_group_rc,
                    &version_row_rc,
                    &kernel_row_rc,
                    &built_row_rc,
                    &commit_row_rc,
                    &rebase_btn_rc,
                    v,
                    &date,
                    current_date,
                );
            }
        });
        recent_group.add(&row);
    }

    // ── "Show older builds" reveals the full calendar ─────────────────────
    let calendar_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .reveal_child(false)
        .build();
    calendar_revealer.set_child(Some(&calendar_box));

    let show_older_btn = gtk::Button::builder()
        .label("Show older builds…")
        .halign(gtk::Align::Center)
        .margin_top(8)
        .margin_bottom(0)
        .build();
    show_older_btn.add_css_class("flat");
    {
        let revealer = calendar_revealer.clone();
        let btn = show_older_btn.clone();
        show_older_btn.connect_clicked(move |_| {
            let new_state = !revealer.reveals_child();
            revealer.set_reveal_child(new_state);
            btn.set_label(if new_state {
                "Hide calendar"
            } else {
                "Show older builds…"
            });
        });
    }

    // ── Assemble container ──────────────────────────────────────────────
    container.append(&recent_group);
    container.append(&show_older_btn);
    container.append(&calendar_revealer);
    container.append(&details_group);
    container.append(&rebase_btn);

    // ── Helpers for re-drawing the grid ────────────────────────────────
    let version_map_rc = version_map.clone();
    let selected_rc = selected.clone();
    let details_group_rc = details_group.clone();
    let version_row_rc = version_row.clone();
    let kernel_row_rc = kernel_row.clone();
    let built_row_rc = built_row.clone();
    let commit_row_rc = commit_row.clone();
    let rebase_btn_rc = rebase_btn.clone();
    let month_label_rc = month_label.clone();
    let next_btn_rc = next_btn.clone();

    let redraw = Rc::new(move |grid: &gtk::Grid, displayed: NaiveDate| {
        redraw_grid(
            grid,
            displayed,
            &version_map_rc,
            current_date,
            &selected_rc,
            &details_group_rc,
            &version_row_rc,
            &kernel_row_rc,
            &built_row_rc,
            &commit_row_rc,
            &rebase_btn_rc,
            &month_label_rc,
            &next_btn_rc,
        );
    });

    // Initial draw
    redraw(&grid, *displayed_month.borrow());

    // ── Month navigation ────────────────────────────────────────────────
    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        prev_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let prev = if current.month() == 1 {
                NaiveDate::from_ymd_opt(current.year() - 1, 12, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() - 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = prev;
            redraw(&grid, prev);
        });
    }

    {
        let grid = grid.clone();
        let displayed_month = displayed_month.clone();
        let redraw = redraw.clone();
        next_btn.connect_clicked(move |_| {
            let current = *displayed_month.borrow();
            let next = if current.month() == 12 {
                NaiveDate::from_ymd_opt(current.year() + 1, 1, 1).unwrap_or(current)
            } else {
                NaiveDate::from_ymd_opt(current.year(), current.month() + 1, 1).unwrap_or(current)
            };
            *displayed_month.borrow_mut() = next;
            redraw(&grid, next);
        });
    }

    // ── Rebase button click → confirm → run bootc switch ───────────────
    {
        let selected_rc = selected.clone();
        let version_map_rc = version_map.clone();
        let dialog_rc = dialog.clone();
        let parent_rc = parent.clone();
        let stack_rc = stack.clone();
        let current_family_rc = current_family.clone();
        let selected_features_rc = selected_features.clone();

        rebase_btn.connect_clicked(move |_| {
            let Some(date) = *selected_rc.borrow() else {
                return;
            };
            let Some(version) = version_map_rc.get(&date).cloned() else {
                return;
            };

            // Resolve the target image from the feature switches. If a family
            // is detected, swap the image name in `version.full_ref` to the
            // one whose suffix matches the selected features (e.g. flipping
            // `nvidia` on bluefin → `bluefin-nvidia`). If the combination
            // isn't published, fall back to the booted image so the user
            // doesn't end up on a bogus ref.
            let family_ref = current_family_rc.borrow();
            let target_full_ref = resolve_target_ref(
                &version.full_ref,
                family_ref.as_ref(),
                &selected_features_rc.borrow(),
            );
            drop(family_ref);
            let switching_image = target_full_ref != version.full_ref;

            let body = if switching_image {
                format!(
                    "Your system will be rebased to:\n\n{}\n\nThis is a different image variant than what you're currently running. A restart is required and the full image will be re-downloaded.",
                    target_full_ref,
                )
            } else {
                format!(
                    "Your system will be rebased to the {} build (version {}).\n\nThis requires a restart to take effect and will re-download the full image.",
                    date.format("%B %-d, %Y"),
                    version.version,
                )
            };

            let confirm = adw::AlertDialog::builder()
                .heading("Rebase System?")
                .body(body)
                .build();

            confirm.add_response("cancel", "_Cancel");
            confirm.add_response("rebase", "_Rebase");
            confirm.set_response_appearance("rebase", adw::ResponseAppearance::Suggested);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");

            let full_ref = target_full_ref;
            let stack = stack_rc.clone();
            let dialog_close = dialog_rc.clone();

            confirm.connect_response(None, move |_, response| {
                if response == "rebase" {
                    if dev_mode {
                        run_rebase_simulated(full_ref.clone(), stack.clone(), dialog_close.clone());
                    } else {
                        run_rebase(full_ref.clone(), stack.clone(), dialog_close.clone());
                    }
                }
            });

            confirm.present(Some(&parent_rc));
        });
    }
}

/// Substitute the image name in `registry/org/image:tag` based on the
/// resolved family + feature selection. Returns the original ref unchanged
/// if no family was detected or the feature combination has no published
/// image — keeps us from constructing refs the registry doesn't serve.
///
/// Delegates the family → image resolution to the service layer
/// ([`UpdaterService::resolve_target`]) so a future alternative frontend can
/// share the same logic without re-implementing it.
fn resolve_target_ref(
    full_ref: &str,
    family: Option<&FamilyInfo>,
    selected_features: &[String],
) -> String {
    let Some(family) = family else {
        return full_ref.to_string();
    };
    let Some(target) = service::global().resolve_target(family, selected_features) else {
        return full_ref.to_string();
    };
    // full_ref = registry/org/image:tag — swap `image` only, preserving the
    // tag the user picked from the calendar.
    let Some((before_tag, tag)) = full_ref.rsplit_once(':') else {
        return full_ref.to_string();
    };
    let Some((reg_org, _old_image)) = before_tag.rsplit_once('/') else {
        return full_ref.to_string();
    };
    format!("{reg_org}/{}:{tag}", target.image)
}

// ── Grid redraw ──────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn redraw_grid(
    grid: &gtk::Grid,
    displayed: NaiveDate,
    versions: &HashMap<NaiveDate, ImageVersion>,
    current_date: Option<NaiveDate>,
    selected: &Rc<RefCell<Option<NaiveDate>>>,
    details_group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    month_label: &gtk::Label,
    next_btn: &gtk::Button,
) {
    let today = Local::now().date_naive();

    // Update label
    month_label.set_label(&displayed.format("%B %Y").to_string());

    // Disable next if we're on current month
    let current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    next_btn.set_sensitive(displayed < current_month);

    let days_in_month = days_in_month(displayed);
    // ISO weekday Mon=0, Sun=6
    let first_weekday = displayed.weekday().num_days_from_monday() as i32;
    let selected_date = *selected.borrow();

    let mut slot = 0i32;
    for row in 0..6i32 {
        for col in 0..7i32 {
            let btn = grid
                .child_at(col, row)
                .and_then(|w| w.downcast::<gtk::Button>().ok());
            let Some(btn) = btn else {
                slot += 1;
                continue;
            };

            let day_num = slot - first_weekday + 1;

            if day_num < 1 || day_num > days_in_month as i32 {
                btn.set_label("");
                btn.set_visible(false);
                btn.set_sensitive(false);
            } else {
                btn.set_visible(true);
                btn.set_label(&day_num.to_string());

                let date =
                    NaiveDate::from_ymd_opt(displayed.year(), displayed.month(), day_num as u32);

                // Clear state classes
                for cls in ["day-available", "day-current", "day-selected", "day-today"] {
                    btn.remove_css_class(cls);
                }

                if let Some(d) = date {
                    let is_available = versions.contains_key(&d);
                    let is_current = current_date == Some(d);
                    let is_selected = selected_date == Some(d);
                    let is_today = d == today;
                    let is_future = d > today;

                    btn.set_sensitive(is_available && !is_future);

                    if is_today {
                        btn.add_css_class("day-today");
                    }
                    if is_available {
                        btn.add_css_class("day-available");
                    }
                    if is_current {
                        btn.add_css_class("day-current");
                    }
                    if is_selected {
                        btn.add_css_class("day-selected");
                    }

                    if is_available && !is_future {
                        // Wire click — disconnect any existing handler first
                        if let Some(hid) =
                            unsafe { btn.steal_data::<glib::SignalHandlerId>("day-handler") }
                        {
                            btn.disconnect(hid);
                        }

                        let selected_inner = selected.clone();
                        let grid_inner = grid.clone();
                        let displayed_inner = displayed;
                        let versions_inner = versions.clone();
                        let current_date_inner = current_date;
                        let details_group_inner = details_group.clone();
                        let version_row_inner = version_row.clone();
                        let kernel_row_inner = kernel_row.clone();
                        let built_row_inner = built_row.clone();
                        let commit_row_inner = commit_row.clone();
                        let rebase_btn_inner = rebase_btn.clone();
                        let month_label_inner = month_label.clone();
                        let next_btn_inner = next_btn.clone();

                        let hid = btn.connect_clicked(move |_| {
                            // Toggle or set selection
                            let prev = *selected_inner.borrow();
                            if prev == Some(d) {
                                *selected_inner.borrow_mut() = None;
                            } else {
                                *selected_inner.borrow_mut() = Some(d);
                            }

                            // Redraw to update selection highlight
                            redraw_grid(
                                &grid_inner,
                                displayed_inner,
                                &versions_inner,
                                current_date_inner,
                                &selected_inner,
                                &details_group_inner,
                                &version_row_inner,
                                &kernel_row_inner,
                                &built_row_inner,
                                &commit_row_inner,
                                &rebase_btn_inner,
                                &month_label_inner,
                                &next_btn_inner,
                            );

                            // Update details panel
                            if let Some(sel_date) = *selected_inner.borrow() {
                                if let Some(v) = versions_inner.get(&sel_date) {
                                    update_details(
                                        &details_group_inner,
                                        &version_row_inner,
                                        &kernel_row_inner,
                                        &built_row_inner,
                                        &commit_row_inner,
                                        &rebase_btn_inner,
                                        v,
                                        &sel_date,
                                        current_date_inner,
                                    );
                                }
                            } else {
                                details_group_inner.set_visible(false);
                                rebase_btn_inner.set_sensitive(false);
                            }
                        });

                        unsafe { btn.set_data("day-handler", hid) };
                    }
                } else {
                    btn.set_sensitive(false);
                }
            }
            slot += 1;
        }
    }
}

fn update_details(
    group: &adw::PreferencesGroup,
    version_row: &adw::ActionRow,
    kernel_row: &adw::ActionRow,
    built_row: &adw::ActionRow,
    commit_row: &adw::ActionRow,
    rebase_btn: &gtk::Button,
    v: &ImageVersion,
    date: &NaiveDate,
    current_date: Option<NaiveDate>,
) {
    version_row.set_subtitle(&v.version);
    kernel_row.set_subtitle(&v.kernel);
    built_row.set_subtitle(&v.created.format("%b %-d, %Y · %H:%M UTC").to_string());
    commit_row.set_subtitle(if v.revision.is_empty() {
        "—"
    } else {
        &v.revision
    });

    group.set_visible(true);

    let is_current = current_date == Some(*date);
    if is_current {
        rebase_btn.set_label("Currently Installed");
        rebase_btn.set_sensitive(false);
    } else {
        rebase_btn.set_label(&format!("Rebase to {}…", date.format("%b %-d")));
        rebase_btn.set_sensitive(true);
    }
}

// ── Rebase worker ────────────────────────────────────────────────────────────

fn run_rebase(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    // Build a progress page with a pulsing ProgressBar + elapsed-time label.
    // A live `bootc switch` measured against ghcr.io took 2m28s for a full
    // dakota-nvidia pull on a residential link — too long for a bare spinner.
    // Pulse mode (no fraction) is the honest representation until we parse
    // bootc's per-layer progress lines (task #24 phase 2).
    let progress_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_start(24)
        .margin_end(24)
        .margin_top(12)
        .margin_bottom(24)
        .build();

    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_pulse_step(0.08);
    progress_box.append(&progress_bar);

    let elapsed_label = gtk::Label::new(Some("Elapsed: 0:00"));
    elapsed_label.add_css_class("dim-label");
    elapsed_label.add_css_class("caption");
    progress_box.append(&elapsed_label);

    let progress_page = adw::StatusPage::builder()
        .title("Rebasing…")
        .description("Pulling the new image layers. This typically takes 2–5 minutes.")
        .build();
    progress_page.set_child(Some(&progress_box));
    stack.add_named(&progress_page, Some("rebasing"));
    stack.set_visible_child_name("rebasing");

    // The bar pulses until we see a parseable Fraction event from bootc; from
    // that point on we drive `set_fraction` directly and stop pulsing. A flag
    // shared with the pulse timer lets the progress consumer disable it.
    let start = std::time::Instant::now();
    let bar_clone = progress_bar.clone();
    let label_clone = elapsed_label.clone();
    let known_fraction: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let known_fraction_pulse = known_fraction.clone();
    let pulse_handle: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let pulse_handle_store = pulse_handle.clone();
    let id = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        if !known_fraction_pulse.get() {
            bar_clone.pulse();
        }
        let secs = start.elapsed().as_secs();
        label_clone.set_text(&format!("Elapsed: {}:{:02}", secs / 60, secs % 60));
        glib::ControlFlow::Continue
    });
    *pulse_handle_store.borrow_mut() = Some(id);

    // Cross-thread channel for streaming BootcProgress events from the
    // subprocess reader (background thread) to the GTK main loop where the
    // ProgressBar lives. std::sync::mpsc is enough here — we drain it from a
    // glib timeout below.
    let (prog_tx_std, prog_rx_std) = std::sync::mpsc::channel::<BootcProgress>();

    let result_slot: Arc<Mutex<Option<Result<(), String>>>> = Arc::new(Mutex::new(None));
    let result_bg = result_slot.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async move {
            // tokio mpsc bridges the async readers to a sync channel the GTK
            // thread can poll. Each parsed BootcProgress flows: stdout/stderr
            // reader → tokio channel → forwarder → std::sync::mpsc → GTK.
            let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<BootcProgress>();
            let prog_tx_std_inner = prog_tx_std.clone();
            let forward = tokio::spawn(async move {
                while let Some(p) = tokio_rx.recv().await {
                    let _ = prog_tx_std_inner.send(p);
                }
            });
            let result = run_bootc_switch(&full_ref, tokio_tx).await;
            let _ = forward.await;
            *result_bg.lock().unwrap() = Some(result);
        });
    });

    let progress_page_for_status = progress_page.clone();
    let bar_for_progress = progress_bar.clone();
    let known_fraction_for_progress = known_fraction.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
        // Drain everything currently in the queue so a burst of layer events
        // doesn't lag behind. We don't break on Empty — that just means no
        // new events yet, keep the timer going.
        loop {
            match prog_rx_std.try_recv() {
                Ok(BootcProgress::Fraction { current, total }) => {
                    known_fraction_for_progress.set(true);
                    let frac = (current as f64) / (total as f64);
                    bar_for_progress.set_fraction(frac.clamp(0.0, 1.0));
                }
                Ok(BootcProgress::Status(s)) => {
                    progress_page_for_status.set_description(Some(&s));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some(result) = result_slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };
        // Stop the pulse animation now that we have a final result.
        if let Some(id) = pulse_handle.borrow_mut().take() {
            id.remove();
        }
        match result {
            Ok(()) => {
                // Show success page — user needs to reboot.
                let done_page = adw::StatusPage::builder()
                    .title("Rebase Complete")
                    .description("Restart your system to boot into the selected version.")
                    .icon_name("object-select-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("suggested-action");
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                done_page.set_child(Some(&close_btn));
                stack.add_named(&done_page, Some("done"));
                stack.set_visible_child_name("done");
            }
            Err(msg) => {
                let fail_page = adw::StatusPage::builder()
                    .title("Rebase Failed")
                    .description(msg)
                    .icon_name("dialog-error-symbolic")
                    .build();
                let close_btn = gtk::Button::builder()
                    .label("Close")
                    .halign(gtk::Align::Center)
                    .build();
                close_btn.add_css_class("pill");
                let dialog_close = dialog.clone();
                close_btn.connect_clicked(move |_| {
                    dialog_close.close();
                });
                fail_page.set_child(Some(&close_btn));
                stack.add_named(&fail_page, Some("fail"));
                stack.set_visible_child_name("fail");
            }
        }
        glib::ControlFlow::Break
    });
}

/// One progress update parsed from a single `bootc switch` output line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BootcProgress {
    /// Layer-count fraction (e.g. "Layer 3/12" → 3/12 = 0.25).
    Fraction { current: u32, total: u32 },
    /// A human-readable status label to show under the progress bar
    /// (e.g. "Pulling…", "Importing…", "Staging deployment…").
    Status(String),
}

/// Parse a single line of `bootc switch` output for progress signal.
///
/// `bootc` doesn't have a stable machine-readable progress format, so this
/// is best-effort against the patterns we observe:
///   - `Layer N/M ...`            → Fraction
///   - `Pulled N/M layers`        → Fraction
///   - `N of M layers pulled`     → Fraction (alternative phrasing)
///   - lines starting with `Pulling` / `Fetching` / `Importing` /
///     `Staging` / `Writing`     → Status (so the page description tracks
///     the current high-level phase even when we can't extract a fraction)
///
/// Returns `None` for any line that doesn't match — the caller treats those
/// as "no signal, keep the bar pulsing" rather than letting them disrupt the
/// last-known progress.
fn parse_bootc_progress(line: &str) -> Option<BootcProgress> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Look for any "N/M" token where both sides are integers and N <= M.
    // bootc's output uses this for layer pulls. We don't require an explicit
    // "Layer" prefix because the wording has drifted across releases.
    for token in trimmed.split_ascii_whitespace() {
        if let Some((a, b)) = token.split_once('/') {
            if let (Ok(current), Ok(total)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if total > 0 && current <= total && total <= 1_000 {
                    return Some(BootcProgress::Fraction { current, total });
                }
            }
        }
    }

    // Fall back to phase-label detection. First word match is enough.
    let first_word = trimmed
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(|c: char| !c.is_alphabetic());
    let phase = match first_word.to_ascii_lowercase().as_str() {
        "pulling" => Some("Pulling new image layers…"),
        "fetching" => Some("Fetching image…"),
        "importing" => Some("Importing layers…"),
        "staging" => Some("Staging deployment…"),
        "writing" => Some("Writing deployment…"),
        "deploying" => Some("Deploying…"),
        _ => None,
    };
    phase.map(|s| BootcProgress::Status(s.to_string()))
}

/// Spawn `bootc switch`, stream stdout+stderr line-by-line, parse progress,
/// and forward each parsed event to the caller via `progress_tx`. Returns the
/// final result.
///
/// Splitting capture+parse out of `run_rebase` keeps the UI plumbing (timeouts,
/// widget updates) free of subprocess details, and lets us unit-test
/// [`parse_bootc_progress`] separately.
async fn run_bootc_switch(
    full_ref: &str,
    progress_tx: tokio::sync::mpsc::UnboundedSender<BootcProgress>,
) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut child = if is_flatpak() {
        tokio::process::Command::new("flatpak-spawn")
            .args(["--host", "pkexec", "bootc", "switch", full_ref])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    } else {
        tokio::process::Command::new("pkexec")
            .args(["bootc", "switch", full_ref])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    }
    .map_err(|e| format!("Failed to spawn bootc switch: {}", e))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let tx_out = progress_tx.clone();
    let stdout_task = tokio::spawn(async move {
        if let Some(stdout) = stdout {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(prog) = parse_bootc_progress(&line) {
                    let _ = tx_out.send(prog);
                }
            }
        }
    });
    let tx_err = progress_tx;
    let stderr_task = tokio::spawn(async move {
        if let Some(stderr) = stderr {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(prog) = parse_bootc_progress(&line) {
                    let _ = tx_err.send(prog);
                }
            }
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("Failed to wait for bootc switch: {}", e))?;
    let _ = stdout_task.await;
    let _ = stderr_task.await;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "bootc switch exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

// ── Family + feature-switch UI ──────────────────────────────────────────────

/// Detect the booted (or mocked) image's Family and render one SwitchRow per
/// atomic feature. As switches toggle, recompute the target image and write
/// it into `target_row`'s subtitle. The dialog uses this to show the user
/// the *resolved* image they'd land on without exposing the raw image names.
///
/// Runs the detection on a background thread (the same pattern as
/// [`spawn_fetch_thread`]) so the dialog stays responsive while bootc/os-release
/// IO completes.
fn populate_family_switches(
    features_group: &adw::PreferencesGroup,
    family_label: &gtk::Label,
    target_row: &adw::ActionRow,
    stream_row: &adw::ComboBoxRow,
    current_family: Rc<RefCell<Option<FamilyInfo>>>,
    selected_features: Rc<RefCell<Vec<String>>>,
    selected_stream: Rc<RefCell<String>>,
) {
    // Two pieces of state get resolved in the background thread:
    //   - the family the booted image belongs to (drives WHICH toggles show)
    //   - the booted ImageRef itself (drives the toggles' INITIAL state, so
    //     a user already on bluefin-dx-nvidia-open sees both toggles ON
    //     when the dialog opens, not OFF as if rebasing would downgrade)
    let slot: Arc<Mutex<Option<(Option<FamilyInfo>, Option<service::ImageRef>)>>> =
        Arc::new(Mutex::new(None));

    {
        let slot = slot.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let detected = rt.block_on(async move {
                let svc = service::global();
                let family = svc.current_family().await.ok().flatten();
                let image = svc.current_image().await.ok();
                (family, image)
            });
            *slot.lock().unwrap() = Some(detected);
        });
    }

    let features_group = features_group.clone();
    let family_label = family_label.clone();
    let target_row = target_row.clone();

    glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
        let Some((family_opt, image_opt)) = slot.lock().ok().and_then(|mut g| g.take()) else {
            return glib::ControlFlow::Continue;
        };

        let Some(family) = family_opt else {
            family_label.set_label("Family not recognized");
            target_row.set_subtitle("(this image isn't in the KNOWN_FAMILIES catalogue)");
            return glib::ControlFlow::Break;
        };

        family_label.set_label(&format!("Family: {}", family.name));

        // Populate stream dropdown with available streams for this family
        let stream_model = gtk::StringList::new(&[]);
        for stream in &family.streams {
            stream_model.append(stream);
        }
        stream_row.set_model(Some(&stream_model));

        // Set default stream to the first one (canonical)
        if !family.streams.is_empty() {
            stream_row.set_selected(0);
            *selected_stream.borrow_mut() = family.streams[0].clone();
        }

        // Derive the initial toggle state from the booted image's suffix so
        // users already on -dx / -nvidia / -dx-nvidia-open see the dialog
        // open with their current configuration represented, not with
        // everything OFF. Without this the dialog implies that rebasing
        // would *downgrade* to the base image.
        let (initial_dx, initial_nvidia) = derive_initial_toggle_state(&family, image_opt.as_ref());
        target_row.set_subtitle(
            image_opt
                .as_ref()
                .map(|img| format!("{} (currently booted)", img.image))
                .unwrap_or_else(|| format!("{} (no extras)", family.base_image))
                .as_str(),
        );

        // Two opinionated toggles instead of one-per-atomic-feature. Per user
        // direction: "we should have toggle for Developer Mode and Nvidia".
        // Granular features (hwe, deck, asus, surface, framework) aren't
        // user-facing here — KNOWN_FAMILIES still lists them so the resolver
        // can land on a published image if the user is currently booted on
        // one, but the rebase dialog only exposes the two switches users
        // think about.
        let supports_dx = family.features.iter().any(|f| f.id == "dx");
        let supports_nvidia = family
            .features
            .iter()
            .any(|f| f.id == "nvidia" || f.id == "open");

        let dx_state = Rc::new(Cell::new(initial_dx));
        let nvidia_state = Rc::new(Cell::new(initial_nvidia));

        let recompute = {
            let family = family.clone();
            let selected_features = selected_features.clone();
            let selected_stream = selected_stream.clone();
            let target_row = target_row.clone();
            let dx_state = dx_state.clone();
            let nvidia_state = nvidia_state.clone();
            move || {
                let stream = selected_stream.borrow().clone();
                let (feats, target) = resolve_dx_nvidia_with_stream(
                    &family,
                    dx_state.get(),
                    nvidia_state.get(),
                    &stream,
                );
                *selected_features.borrow_mut() = feats;
                match target {
                    Some(t) => target_row.set_subtitle(&format!("{} (resolved)", t.image)),
                    None => {
                        target_row.set_subtitle("(combination doesn't match any published image)")
                    }
                }
            }
        };

        if supports_dx {
            let row = adw::SwitchRow::builder()
                .title("Developer Mode")
                .subtitle("Container tools, IDEs, and language SDKs")
                .active(initial_dx)
                .build();
            let recompute_ = recompute.clone();
            let dx_state_ = dx_state.clone();
            row.connect_active_notify(move |sr| {
                dx_state_.set(sr.is_active());
                recompute_();
            });
            features_group.add(&row);
        }

        if supports_nvidia {
            // Adaptive subtitle: if we detect NVIDIA hardware, call that out so
            // the user understands why this toggle matters for them
            // specifically. Otherwise show the generic description.
            let nvidia_subtitle = if crate::gpu::has_nvidia_gpu() {
                "NVIDIA GPU detected — keep on for hardware-accelerated graphics (open kernel modules preferred)"
            } else {
                "Picks the open kernel modules where available, falls back to the proprietary driver"
            };
            let row = adw::SwitchRow::builder()
                .title("NVIDIA drivers")
                .subtitle(nvidia_subtitle)
                .active(initial_nvidia)
                .build();
            // Guard prevents the warn-and-revert path from re-firing this
            // handler when we programmatically flip the switch back to
            // its previous state after a "Cancel" on the warning dialog.
            let nvidia_guard: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            let recompute_ = recompute.clone();
            let nvidia_state_ = nvidia_state.clone();
            let guard_ = nvidia_guard.clone();
            row.connect_active_notify(move |sr| {
                if guard_.get() {
                    return;
                }
                let new_value = sr.is_active();
                let prev_value = nvidia_state_.get();
                let turning_off = prev_value && !new_value;
                if turning_off && crate::gpu::has_nvidia_gpu() {
                    let confirm = adw::AlertDialog::builder()
                        .heading("NVIDIA hardware detected")
                        .body("Your system has an NVIDIA GPU. Switching to a non-NVIDIA image will fall back to software rendering or the open Mesa driver — graphics performance will degrade significantly until you switch back.\n\nContinue?")
                        .build();
                    confirm.add_response("cancel", "_Cancel");
                    confirm.add_response("disable", "_Disable anyway");
                    confirm.set_response_appearance(
                        "disable",
                        adw::ResponseAppearance::Destructive,
                    );
                    confirm.set_default_response(Some("cancel"));
                    confirm.set_close_response("cancel");

                    let sr_clone = sr.clone();
                    let nvidia_state_clone = nvidia_state_.clone();
                    let recompute_clone = recompute_.clone();
                    let guard_clone = guard_.clone();
                    confirm.connect_response(None, move |_, response| {
                        if response == "disable" {
                            nvidia_state_clone.set(false);
                            recompute_clone();
                        } else {
                            // Revert the switch back to on without re-firing
                            // the handler (would re-trigger this dialog).
                            guard_clone.set(true);
                            sr_clone.set_active(true);
                            guard_clone.set(false);
                        }
                    });
                    confirm.present(None::<&gtk::Widget>);
                    // Don't apply yet — wait for the response callback.
                    return;
                }
                nvidia_state_.set(new_value);
                recompute_();
            });
            features_group.add(&row);
        }

        // Wire up stream selection to recompute target image
        let selected_stream_clone = selected_stream.clone();
        let recompute_clone = recompute.clone();
        stream_row.connect_selected_notify(move |combo| {
            if let Some(item) = combo.selected_item() {
                if let Ok(obj) = item.downcast::<gtk::StringObject>() {
                    if let Some(stream_str) = obj.string() {
                        *selected_stream_clone.borrow_mut() = stream_str.to_string();
                        recompute_clone();
                    }
                }
            }
        });

        *current_family.borrow_mut() = Some(family);
        glib::ControlFlow::Break
    });
}

/// Compute the selected feature set + target image for the current toggle
/// state + selected stream. Uses the new resolve_target_with_stream API.
///
/// Similar fallback chain as resolve_dx_nvidia: prefers -nvidia-open,
/// falls back to -nvidia if needed.
///
/// Returns (selected_features, resolved_image). The stream is embedded in
/// the ImageRef's tag field.
fn resolve_dx_nvidia_with_stream(
    family: &FamilyInfo,
    dx_on: bool,
    nvidia_on: bool,
    stream: &str,
) -> (Vec<String>, Option<service::ImageRef>) {
    let svc = service::global();
    let base: Vec<String> = if dx_on {
        vec!["dx".to_string()]
    } else {
        vec![]
    };

    if nvidia_on {
        // Prefer the -open variant (current for Bluefin / Bluefin LTS).
        let mut with_open = base.clone();
        with_open.push("nvidia".to_string());
        with_open.push("open".to_string());
        if let Some(img) = svc.resolve_target_with_stream(family, &with_open, stream) {
            return (with_open, Some(img));
        }
        // Fall back to plain -nvidia (Bazzite / Dakota / Bluefin's
        // pre-migration variant the user might currently be booted on).
        let mut plain = base.clone();
        plain.push("nvidia".to_string());
        let img = svc.resolve_target_with_stream(family, &plain, stream);
        return (plain, img);
    }

    let img = svc.resolve_target_with_stream(family, &base, stream);
    (base, img)
}

/// Compute the selected feature set + target image for the current toggle
/// state. The fallback chain is what makes the single "NVIDIA drivers" switch
/// usable across the families:
///
///   nvidia on, prefer -nvidia-open first (current Bluefin / Bluefin LTS
///   convention) → fall back to plain -nvidia (Bazzite / deprecated Bluefin
///   variant). The user just toggles "NVIDIA"; we resolve to whichever
///   variant their family actually publishes.
///
/// Returns (selected_features, resolved_image). The features list flows into
/// the Rebase button click handler so the bootc-switch ref matches what the
/// preview shows.
fn resolve_dx_nvidia(
    family: &FamilyInfo,
    dx_on: bool,
    nvidia_on: bool,
) -> (Vec<String>, Option<service::ImageRef>) {
    let svc = service::global();
    let base: Vec<String> = if dx_on {
        vec!["dx".to_string()]
    } else {
        vec![]
    };

    if nvidia_on {
        // Prefer the -open variant (current for Bluefin / Bluefin LTS).
        let mut with_open = base.clone();
        with_open.push("nvidia".to_string());
        with_open.push("open".to_string());
        if let Some(img) = svc.resolve_target(family, &with_open) {
            return (with_open, Some(img));
        }
        // Fall back to plain -nvidia (Bazzite / Dakota / Bluefin's
        // pre-migration variant the user might currently be booted on).
        let mut plain = base.clone();
        plain.push("nvidia".to_string());
        let img = svc.resolve_target(family, &plain);
        return (plain, img);
    }

    let img = svc.resolve_target(family, &base);
    (base, img)
}

/// Reverse-engineer the toggle state from the user's booted image suffix so the
/// rebase dialog opens with the toggles matching reality.
///
/// Example: booted on `bluefin-dx-nvidia-open` with family base `bluefin` →
/// suffix `dx-nvidia-open` → `(dx=true, nvidia=true)`.
///
/// Returns `(false, false)` when the booted image is unknown, is the bare base,
/// or doesn't match the expected `{base}-{suffix}` shape. Conservative default —
/// if we can't be sure, show everything off rather than mislead the user about
/// what they're running.
fn derive_initial_toggle_state(
    family: &FamilyInfo,
    image: Option<&service::ImageRef>,
) -> (bool, bool) {
    let Some(image) = image else {
        return (false, false);
    };
    let Some(suffix) = image.image.strip_prefix(&format!("{}-", family.base_image)) else {
        // Bare base image (no suffix) or completely unrelated image name.
        return (false, false);
    };
    let parts: Vec<&str> = suffix.split('-').collect();
    let dx = parts.contains(&"dx");
    // Either `-nvidia` or `-nvidia-open` (or future `-open`-only variants) all
    // count as "NVIDIA on" from the user's mental model. resolve_dx_nvidia()
    // picks the right specific variant when they save.
    let nvidia = parts.contains(&"nvidia") || parts.contains(&"open");
    (dx, nvidia)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Simulated rebase for dev mode — shows the progress UI then succeeds after a delay.
fn run_rebase_simulated(full_ref: String, stack: gtk::Stack, dialog: adw::Dialog) {
    tracing::warn!(
        "Rebase suppressed — developer mode is active. \
         Would have called `bootc switch {}`.",
        full_ref
    );

    let progress_page = adw::StatusPage::builder()
        .title("Rebasing… (simulated)")
        .description("Developer mode — no actual changes are being made.")
        .build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    progress_page.set_child(Some(&spinner));
    stack.add_named(&progress_page, Some("rebasing"));
    stack.set_visible_child_name("rebasing");

    // Simulate a short delay then show success.
    glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
        let done_page = adw::StatusPage::builder()
            .title("Rebase Complete (simulated)")
            .description(
                "Developer mode — no changes were made.\nIn production, a restart would be needed.",
            )
            .icon_name("object-select-symbolic")
            .build();
        let close_btn = gtk::Button::builder()
            .label("Close")
            .halign(gtk::Align::Center)
            .build();
        close_btn.add_css_class("suggested-action");
        close_btn.add_css_class("pill");
        let dialog_close = dialog.clone();
        close_btn.connect_clicked(move |_| {
            dialog_close.close();
        });
        done_page.set_child(Some(&close_btn));
        stack.add_named(&done_page, Some("done"));
        stack.set_visible_child_name("done");
    });
}

fn days_in_month(date: NaiveDate) -> u32 {
    let next = if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1)
    };
    next.unwrap_or(date)
        .signed_duration_since(
            NaiveDate::from_ymd_opt(date.year(), date.month(), 1).unwrap_or(date),
        )
        .num_days() as u32
}

fn inject_calendar_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .day-btn {
            min-width: 36px;
            min-height: 36px;
            padding: 0;
            border-radius: 18px;
            font-size: 0.85em;
        }
        .day-btn:not(:sensitive) { opacity: 0.3; }
        .day-available           { color: @accent_color; font-weight: bold; }
        .day-current             { background-color: @accent_bg_color; color: @accent_fg_color; }
        .day-selected:not(.day-current) {
            outline: 2px solid @accent_color;
            outline-offset: -2px;
        }
        .day-today label { text-decoration: underline; }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ── Message type for background fetch ────────────────────────────────────────

enum FetchResult {
    Ok(Vec<ImageVersion>),
    Err(String),
    DetectFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Tests need a process-wide UpdaterService since resolve_target_ref calls
    /// service::global(). Install the default BootcUpdaterService once;
    /// service::init() will panic on the second call so guard with Once.
    fn ensure_service() {
        INIT.call_once(|| {
            service::init(service::BootcUpdaterService::new());
        });
    }

    fn bluefin_stable_info() -> FamilyInfo {
        // The features list isn't consulted by resolve_target_ref — the
        // service routes through KNOWN_FAMILIES via family.name — so we leave
        // it empty here. Service-level tests in service::tests cover the
        // feature-resolution paths.
        FamilyInfo {
            name: "Bluefin Stable".to_string(),
            base_image: "bluefin".to_string(),
            features: vec![],
        }
    }

    #[test]
    fn resolve_passthrough_with_no_family() {
        ensure_service();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            None,
            &[],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
    }

    #[test]
    fn resolve_no_features_keeps_base_image() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            Some(&fam),
            &[],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527");
    }

    #[test]
    fn resolve_swaps_image_to_nvidia_variant() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable-daily-43.20260527",
            Some(&fam),
            &["nvidia".to_string()],
        );
        assert_eq!(
            r,
            "ghcr.io/ublue-os/bluefin-nvidia:stable-daily-43.20260527"
        );
    }

    #[test]
    fn resolve_combines_dx_and_nvidia() {
        ensure_service();
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin:stable",
            Some(&fam),
            &["dx".to_string(), "nvidia".to_string()],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin-dx-nvidia:stable");
    }

    #[test]
    fn resolve_unpublished_combination_falls_back() {
        ensure_service();
        // "open" alone (without nvidia) isn't a published image — keep the
        // original ref so we don't pkexec a bogus bootc switch.
        let fam = bluefin_stable_info();
        let original = "ghcr.io/ublue-os/bluefin:stable";
        let r = resolve_target_ref(original, Some(&fam), &["open".to_string()]);
        assert_eq!(r, original);
    }

    #[test]
    fn resolve_handles_missing_tag() {
        ensure_service();
        // Defensive: a malformed ref with no ':' should pass through.
        let fam = bluefin_stable_info();
        let r = resolve_target_ref(
            "ghcr.io/ublue-os/bluefin",
            Some(&fam),
            &["nvidia".to_string()],
        );
        assert_eq!(r, "ghcr.io/ublue-os/bluefin");
    }

    // ── resolve_dx_nvidia ────────────────────────────────────────────────
    // Pins the toggle-to-features fallback chain so the two-switch UI
    // (Developer Mode + NVIDIA) lands on the right image per family.

    fn dakota_info() -> FamilyInfo {
        FamilyInfo {
            name: "Bluefin Dakota".to_string(),
            base_image: "dakota".to_string(),
            features: vec![],
        }
    }

    fn bazzite_kde_info() -> FamilyInfo {
        FamilyInfo {
            name: "Bazzite KDE".to_string(),
            base_image: "bazzite".to_string(),
            features: vec![],
        }
    }

    #[test]
    fn dx_nvidia_both_off_returns_base() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, false);
        assert_eq!(feats, Vec::<String>::new());
        assert_eq!(img.unwrap().image, "bluefin");
    }

    #[test]
    fn dx_nvidia_dx_only_resolves_dx() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, false);
        assert_eq!(feats, vec!["dx".to_string()]);
        assert_eq!(img.unwrap().image, "bluefin-dx");
    }

    #[test]
    fn dx_nvidia_nvidia_only_on_bluefin_prefers_open() {
        ensure_service();
        // Bluefin's plain -nvidia is deprecated; the toggle should land on
        // -nvidia-open (the current variant).
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
        assert_eq!(img.unwrap().image, "bluefin-nvidia-open");
    }

    #[test]
    fn dx_nvidia_both_on_bluefin_yields_dx_nvidia_open() {
        ensure_service();
        let (feats, img) = resolve_dx_nvidia(&bluefin_stable_info(), true, true);
        assert_eq!(
            feats,
            vec!["dx".to_string(), "nvidia".to_string(), "open".to_string()]
        );
        assert_eq!(img.unwrap().image, "bluefin-dx-nvidia-open");
    }

    #[test]
    fn dx_nvidia_nvidia_on_dakota_falls_back_to_plain_nvidia() {
        ensure_service();
        // Dakota has no -nvidia-open variant published; the first probe
        // (`["nvidia", "open"]`) misses, the fallback (`["nvidia"]`)
        // lands on dakota-nvidia.
        let (feats, img) = resolve_dx_nvidia(&dakota_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string()]);
        assert_eq!(img.unwrap().image, "dakota-nvidia");
    }

    #[test]
    fn dx_nvidia_nvidia_on_bazzite_prefers_open() {
        ensure_service();
        // Bazzite KDE publishes both bazzite-nvidia AND bazzite-nvidia-open.
        // The resolver's -open-first preference picks the latter. Pin this
        // so a future KNOWN_FAMILIES trim (dropping plain -nvidia) doesn't
        // silently change which variant new users land on.
        let (feats, img) = resolve_dx_nvidia(&bazzite_kde_info(), false, true);
        assert_eq!(feats, vec!["nvidia".to_string(), "open".to_string()]);
        assert_eq!(img.unwrap().image, "bazzite-nvidia-open");
    }

    #[test]
    fn dx_nvidia_dx_on_dakota_has_no_published_image() {
        ensure_service();
        // Dakota has no -dx variant — the resolver returns None, the UI
        // shows the "doesn't match any published image" subtitle.
        let (feats, img) = resolve_dx_nvidia(&dakota_info(), true, false);
        assert_eq!(feats, vec!["dx".to_string()]);
        assert!(img.is_none());
    }

    // ── derive_initial_toggle_state ──────────────────────────────────────
    // The dialog must open with toggles reflecting the booted variant so a
    // user already on -dx-nvidia-open doesn't think rebasing would downgrade.

    fn image_ref(image: &str) -> service::ImageRef {
        service::ImageRef {
            registry: "ghcr.io".to_string(),
            org: "ublue-os".to_string(),
            image: image.to_string(),
            tag: "stable".to_string(),
        }
    }

    #[test]
    fn initial_toggles_no_image_returns_off() {
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), None);
        assert!(!dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_base_image_returns_off() {
        let img = image_ref("bluefin");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_dx_only() {
        let img = image_ref("bluefin-dx");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(dx);
        assert!(!nvidia);
    }

    #[test]
    fn initial_toggles_plain_nvidia() {
        let img = image_ref("bluefin-nvidia");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_nvidia_open() {
        let img = image_ref("bluefin-nvidia-open");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_dx_and_nvidia_open() {
        let img = image_ref("bluefin-dx-nvidia-open");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(dx);
        assert!(nvidia);
    }

    #[test]
    fn initial_toggles_unrelated_image_returns_off() {
        // Booted image's name doesn't share the family's base prefix — could
        // happen if KNOWN_FAMILIES drifts vs reality. Show off rather than
        // lie about state.
        let img = image_ref("aurora-dx");
        let (dx, nvidia) = derive_initial_toggle_state(&bluefin_stable_info(), Some(&img));
        assert!(!dx);
        assert!(!nvidia);
    }

    // ── parse_bootc_progress ─────────────────────────────────────────────
    // Pins the bootc-switch stdout/stderr parsing so the layer-by-layer
    // progress bar keeps working when bootc's output format drifts (or so we
    // notice when it does).

    #[test]
    fn progress_parses_layer_ratio() {
        let r = parse_bootc_progress("Layer 3/12 sha256:abc...");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 3,
                total: 12
            })
        );
    }

    #[test]
    fn progress_parses_pulled_ratio_phrasing() {
        let r = parse_bootc_progress("Pulled 8/8 layers");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 8,
                total: 8
            })
        );
    }

    #[test]
    fn progress_extracts_ratio_anywhere_in_line() {
        // Containers-image style: "Copying blob abc 5/12 (...) ETA 30s"
        let r = parse_bootc_progress("Copying blob abc 5/12 12.3 MiB / 30 MiB");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 5,
                total: 12
            })
        );
    }

    #[test]
    fn progress_rejects_zero_total() {
        // 0/0 isn't a valid fraction; ignore so we don't divide by zero later.
        assert_eq!(parse_bootc_progress("0/0 something"), None);
    }

    #[test]
    fn progress_rejects_inverted_ratio() {
        // current > total is nonsense — likely matched something unrelated
        // like a file size; bail rather than show "150% complete".
        assert_eq!(parse_bootc_progress("Copying 100/5 something"), None);
    }

    #[test]
    fn progress_rejects_huge_denominator() {
        // Byte counts ("12345/67890 bytes") would set the bar bouncing all
        // over. Cap total at 1000 — no bootc image has that many layers.
        assert_eq!(parse_bootc_progress("123/45678 bytes"), None);
    }

    #[test]
    fn progress_returns_status_for_pulling_lines() {
        let r = parse_bootc_progress("Pulling manifest from ghcr.io/...");
        assert_eq!(
            r,
            Some(BootcProgress::Status(
                "Pulling new image layers…".to_string()
            ))
        );
    }

    #[test]
    fn progress_returns_status_for_staging_lines() {
        let r = parse_bootc_progress("Staging deployment for switch");
        assert_eq!(
            r,
            Some(BootcProgress::Status("Staging deployment…".to_string()))
        );
    }

    #[test]
    fn progress_prefers_fraction_over_status_when_both_match() {
        // "Pulling 4/8 layers" — first word matches a status, but the ratio
        // is the more useful signal; ensure we don't lose it.
        let r = parse_bootc_progress("Pulling 4/8 layers");
        assert_eq!(
            r,
            Some(BootcProgress::Fraction {
                current: 4,
                total: 8
            })
        );
    }

    #[test]
    fn progress_ignores_unrelated_lines() {
        assert_eq!(parse_bootc_progress(""), None);
        assert_eq!(parse_bootc_progress("   "), None);
        assert_eq!(parse_bootc_progress("info: starting bootc switch"), None);
    }

    #[test]
    fn initial_toggles_dakota_plain_nvidia() {
        // Dakota's -nvidia variant — no -open suffix, but still "NVIDIA on".
        let img = service::ImageRef {
            registry: "ghcr.io".to_string(),
            org: "projectbluefin".to_string(),
            image: "dakota-nvidia".to_string(),
            tag: "latest".to_string(),
        };
        let (dx, nvidia) = derive_initial_toggle_state(&dakota_info(), Some(&img));
        assert!(!dx);
        assert!(nvidia);
    }
}
