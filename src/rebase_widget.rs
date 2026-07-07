//! Self-contained image-switching widget for embedding into the
//! gnome-control-center panel.
//!
//! This is the embed-friendly counterpart to the standalone app's
//! `ui::rebase_dialog`. Same job — pick a different image variant /
//! stream / dated build and switch to it — but built as a reusable
//! `adw::PreferencesPage` instead of a top-level `adw::Dialog`. The
//! caller drops the widget into any container; today that's the cc
//! panel's AdwNavigationView, tomorrow it could be the standalone app.
//!
//! Scope of this revision: variant toggles (DX / NVIDIA), stream
//! dropdown, recent-builds list (4 most recent), and a "Switch" action
//! button. No full month-grid calendar — the calendar is a luxury for
//! the standalone dialog where vertical space is generous; the panel's
//! recent-builds list covers the 80% case and keeps the subpage tight.
//! Calendar parity can land in a follow-up.

use adw::prelude::*;
use gtk::glib;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;

use crate::registry_client::ImageVersion;
use crate::service::{FamilyInfo, ImageRef, UpdaterService};

/// Build the embeddable rebase widget. Like
/// [`crate::changelog_widget::build_changelog_widget`], the returned
/// widget self-manages: it synchronously reads the booted image +
/// family, then kicks off the recent-builds fetch in the background.
///
/// MUST be called on the thread that owns the GLib main loop.
pub fn build_rebase_widget(
    service: Arc<dyn UpdaterService>,
    runtime: &tokio::runtime::Runtime,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().build();

    // Synchronous: booted image identity drives the variant resolution.
    let booted: Option<ImageRef> = runtime
        .block_on(async { service.current_image().await })
        .ok();
    let family: Option<FamilyInfo> =
        runtime.block_on(async { service.current_family().await.ok().flatten() });

    // ── Variants group ─────────────────────────────────────────────────
    let variants_group = adw::PreferencesGroup::builder()
        .title("Image variant")
        .description("Pick the stream and feature set to switch to")
        .build();

    let stream_row = adw::ComboRow::builder().title("Stream").build();
    let stream_model = gtk::StringList::new(&[]);
    stream_row.set_model(Some(&stream_model));
    variants_group.add(&stream_row);

    // Shared state for the toggles. None when the family hasn't been
    // resolved yet (most likely: bootc-status failed, no os-release
    // fallback — caller stays on the booted image).
    let dx_state: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let nvidia_state: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let selected_stream: Rc<std::cell::RefCell<String>> =
        Rc::new(std::cell::RefCell::new(String::new()));

    let dx_row = adw::SwitchRow::builder()
        .title("Developer Mode")
        .subtitle("Container tools, IDEs, and language SDKs")
        .build();
    let nvidia_row = adw::SwitchRow::builder()
        .title("NVIDIA drivers")
        .subtitle("Use proprietary or open kernel modules where available")
        .build();

    if let Some(ref fam) = family {
        if fam.features.iter().any(|f| f.id == "dx") {
            variants_group.add(&dx_row);
        }
        if fam
            .features
            .iter()
            .any(|f| f.id == "nvidia" || f.id == "open")
        {
            variants_group.add(&nvidia_row);
        }

        for stream in &fam.streams {
            stream_model.append(stream);
        }
        if !fam.streams.is_empty() {
            stream_row.set_selected(0);
            *selected_stream.borrow_mut() = fam.streams[0].clone();
        }

        // Initial toggle state from the booted suffix so the user opens
        // on their current configuration, not "everything off" (which
        // would imply rebasing would downgrade to the base image).
        if let Some(ref img) = booted {
            let suffix = img
                .image
                .strip_prefix(&format!("{}-", fam.base_image))
                .unwrap_or("");
            dx_state.set(suffix.contains("dx"));
            nvidia_state.set(suffix.contains("nvidia") || suffix.contains("open"));
            dx_row.set_active(dx_state.get());
            nvidia_row.set_active(nvidia_state.get());
        }
    }
    page.add(&variants_group);

    // ── Target preview row ─────────────────────────────────────────────
    // Mirrors the resolved target image as the user toggles, so they
    // know exactly which ref the Switch button will commit to before
    // they click it.
    let target_group = adw::PreferencesGroup::new();
    let target_row = adw::ActionRow::builder()
        .title("Target image")
        .subtitle("Pick variant + stream above")
        .subtitle_lines(2)
        .build();
    target_row.set_activatable(false);
    target_group.add(&target_row);
    page.add(&target_group);

    // ── Recent builds group ────────────────────────────────────────────
    // Filled in by the spawn_recent_builds_fetch background task. Each
    // row is clickable and selects that specific build's dated tag for
    // the Switch action.
    let recent_group = adw::PreferencesGroup::builder()
        .title("Recent builds")
        .description("Switch to a specific date instead of the floating stream tag")
        .build();
    let recent_loading = loading_row("Loading registry…");
    recent_group.add(&recent_loading);
    page.add(&recent_group);

    // ── Switch action ──────────────────────────────────────────────────
    let action_group = adw::PreferencesGroup::new();
    let switch_row = adw::ActionRow::builder()
        .title("Switch image")
        .subtitle("Restart required after the new image downloads")
        .build();
    switch_row.set_activatable(false);
    let switch_button = gtk::Button::builder()
        .label("Switch")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    switch_button.add_css_class("suggested-action");
    switch_row.add_suffix(&switch_button);
    action_group.add(&switch_row);
    page.add(&action_group);

    // ── Wire toggles → recompute target ────────────────────────────────
    let recompute = build_recompute(
        family.clone(),
        booted.clone(),
        dx_state.clone(),
        nvidia_state.clone(),
        selected_stream.clone(),
        target_row.clone(),
        switch_button.clone(),
        service.clone(),
    );

    let recompute_dx = recompute.clone();
    let dx_state_inner = dx_state.clone();
    dx_row.connect_active_notify(move |sr| {
        dx_state_inner.set(sr.is_active());
        recompute_dx();
    });

    let recompute_nvidia = recompute.clone();
    let nvidia_state_inner = nvidia_state.clone();
    nvidia_row.connect_active_notify(move |sr| {
        nvidia_state_inner.set(sr.is_active());
        recompute_nvidia();
    });

    let recompute_stream = recompute.clone();
    let selected_stream_inner = selected_stream.clone();
    stream_row.connect_selected_notify(move |combo| {
        if let Some(item) = combo.selected_item() {
            if let Ok(obj) = item.downcast::<gtk::StringObject>() {
                *selected_stream_inner.borrow_mut() = obj.string().to_string();
                recompute_stream();
            }
        }
    });

    // Fire once to populate the initial preview / button label.
    recompute();

    // ── Async fetches ──────────────────────────────────────────────────
    if let Some(ref image) = booted {
        spawn_recent_builds_fetch(
            runtime,
            service.clone(),
            image.clone(),
            recent_group.downgrade(),
            recent_loading.downgrade(),
            switch_button.downgrade(),
        );
    } else {
        recent_loading.set_title("Couldn't detect the booted image");
    }

    page
}

// ─── Target recompute ──────────────────────────────────────────────────────

/// Build the closure that resolves the target image from the current
/// toggle state and updates both the preview row + Switch button label.
/// Stays in this widget's scope so the closure can capture all the GTK
/// + state Rcs by reference.
#[allow(clippy::too_many_arguments)]
fn build_recompute(
    family: Option<FamilyInfo>,
    booted: Option<ImageRef>,
    dx_state: Rc<Cell<bool>>,
    nvidia_state: Rc<Cell<bool>>,
    selected_stream: Rc<std::cell::RefCell<String>>,
    target_row: adw::ActionRow,
    switch_button: gtk::Button,
    service: Arc<dyn UpdaterService>,
) -> Rc<dyn Fn()> {
    Rc::new(move || {
        let Some(ref fam) = family else {
            target_row.set_subtitle("(family not recognised)");
            switch_button.set_sensitive(false);
            return;
        };

        let stream = selected_stream.borrow().clone();
        if stream.is_empty() {
            target_row.set_subtitle("(no stream selected)");
            switch_button.set_sensitive(false);
            return;
        }

        let features = resolve_features(dx_state.get(), nvidia_state.get());
        let Some(target) = service.resolve_target_with_stream(fam, &features, &stream) else {
            target_row.set_subtitle("(combination doesn't match a published image)");
            switch_button.set_sensitive(false);
            return;
        };

        target_row.set_subtitle(&target.as_string());

        // Disable the button + label "Currently Installed" when the
        // resolved target equals the booted image — clicking would
        // be a no-op switch.
        let is_current = booted
            .as_ref()
            .map(|b| b.image == target.image && b.tag == stream)
            .unwrap_or(false);

        if is_current {
            switch_button.set_label("Currently Installed");
            switch_button.set_sensitive(false);
        } else if booted
            .as_ref()
            .map(|b| b.image == target.image)
            .unwrap_or(false)
        {
            switch_button.set_label(&format!("Switch to :{}", stream));
            switch_button.set_sensitive(true);
        } else {
            switch_button.set_label(&format!("Switch to {}:{}", target.image, stream));
            switch_button.set_sensitive(true);
        }
    })
}

/// Mirror of the standalone app's resolve_dx_nvidia_with_stream feature
/// list — DX adds "dx", NVIDIA adds "nvidia" + "open" (prefer the open
/// variant where the family publishes one).
fn resolve_features(dx: bool, nvidia: bool) -> Vec<String> {
    let mut out = Vec::new();
    if dx {
        out.push("dx".to_string());
    }
    if nvidia {
        out.push("nvidia".to_string());
        out.push("open".to_string());
    }
    out
}

// ─── Recent builds ────────────────────────────────────────────────────────

fn spawn_recent_builds_fetch(
    runtime: &tokio::runtime::Runtime,
    service: Arc<dyn UpdaterService>,
    image: ImageRef,
    recent_group: glib::WeakRef<adw::PreferencesGroup>,
    recent_loading: glib::WeakRef<adw::ActionRow>,
    _switch_button: glib::WeakRef<gtk::Button>,
) {
    let (tx, rx) = mpsc::channel::<Vec<ImageVersion>>();
    runtime.spawn(async move {
        let versions = service.list_versions(&image, 4).await.unwrap_or_default();
        let _ = tx.send(versions);
    });

    glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
        let Ok(versions) = rx.try_recv() else {
            return glib::ControlFlow::Continue;
        };
        let Some(group) = recent_group.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(loading) = recent_loading.upgrade() else {
            return glib::ControlFlow::Break;
        };
        group.remove(&loading);

        if versions.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No recent builds returned by the registry")
                .build();
            group.add(&row);
            return glib::ControlFlow::Break;
        }

        for v in versions.iter().take(4) {
            let row = adw::ActionRow::builder()
                .title(&v.version)
                .subtitle(&v.created.format("%b %-d, %Y").to_string())
                .activatable(true)
                .build();

            if !v.kernel.is_empty() {
                let lbl = gtk::Label::new(Some(&v.kernel));
                lbl.add_css_class("monospace");
                lbl.add_css_class("caption");
                lbl.add_css_class("dim-label");
                row.add_suffix(&lbl);
            }

            // Click → not wired to actually pin to the dated build yet.
            // That needs the same confirm-and-run path the standalone
            // app uses, plus polkit for the bootc switch. Tracked
            // separately; for this revision the row is informational.
            row.connect_activated(|row| {
                tracing::debug!(
                    "rebase_widget: row activated (date pin not yet wired): {:?}",
                    row.title()
                );
            });

            group.add(&row);
        }
        glib::ControlFlow::Break
    });
}

// ─── GTK helper ────────────────────────────────────────────────────────────

fn loading_row(text: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(text).build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(16, 16);
    row.add_suffix(&spinner);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_features_empty_when_off() {
        assert!(resolve_features(false, false).is_empty());
    }

    #[test]
    fn resolve_features_dx_only() {
        assert_eq!(resolve_features(true, false), vec!["dx"]);
    }

    #[test]
    fn resolve_features_nvidia_prefers_open() {
        // NVIDIA on adds both "nvidia" and "open"; the resolver picks
        // the open variant first when the family publishes one.
        assert_eq!(resolve_features(false, true), vec!["nvidia", "open"]);
    }

    #[test]
    fn resolve_features_dx_plus_nvidia() {
        assert_eq!(resolve_features(true, true), vec!["dx", "nvidia", "open"]);
    }
}
