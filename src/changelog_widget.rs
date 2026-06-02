//! Self-contained "What's New" widget for embedding into other surfaces.
//!
//! This is the factoring step the integration plan called out: the
//! standalone app's `status_view.rs::rebuild_changelog_page` builds its
//! widget tree imperatively against an `adw::PreferencesPage` and assumes
//! it lives inside a Relm4 component. The gnome-control-center panel
//! needs to embed the same UI in a different container hierarchy, so we
//! extract the widget construction here and let it self-manage data
//! loading.
//!
//! This first revision intentionally renders a minimal "Stack + status"
//! placeholder so the FFI plumbing can be verified end-to-end. The full
//! rendering (commits list, SBOM diff, GitHub fetches) lands in a later
//! commit once the widget-transfer ownership semantics are nailed down
//! on the C side.

use adw::prelude::*;
use gtk::glib;
use std::sync::Arc;

use crate::service::{ImageRef, UpdaterService};

/// Build a self-contained changelog widget for the supplied image. The
/// returned widget is an `adw::PreferencesPage` that the caller can drop
/// into any container — a Cc panel subpage, a top-level dialog, the
/// standalone app's existing changelog page slot.
///
/// The widget kicks off its own background data fetches (registry
/// versions, GitHub commits, SBOM diff) and updates its rows in-place as
/// results arrive. Cancellation is best-effort: when the widget is
/// destroyed, in-flight tasks complete but their results are dropped
/// because their weak references no longer resolve.
pub fn build_changelog_widget(
    service: Arc<dyn UpdaterService>,
    runtime: &tokio::runtime::Runtime,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().build();

    // ── Hero group ──────────────────────────────────────────────────────
    // Image identity comes from the service synchronously — it's the
    // fastest signal we have and gives the user something to read while
    // the registry probe runs.
    let hero_group = adw::PreferencesGroup::new();
    let hero_row = adw::ActionRow::builder()
        .title("System Image")
        .subtitle("Detecting…")
        .subtitle_lines(2)
        .subtitle_selectable(true)
        .build();
    hero_row.set_activatable(false);

    if let Ok(image) = runtime.block_on(async { service.current_image().await }) {
        hero_row.set_title(&format!("{}/{}", image.org, image.image));
        hero_row.set_subtitle(&image.as_string());
    }
    hero_group.add(&hero_row);
    page.add(&hero_group);

    // ── Stack group ────────────────────────────────────────────────────
    // Placeholder until the version-history fetch lands a result. The
    // standalone app's `rebuild_changelog_page` would normally tear this
    // group down and rebuild it row-by-row from the registry response;
    // for this first revision we just show "Loading…" so the FFI
    // contract is observable.
    let stack_group = adw::PreferencesGroup::builder().title("Stack").build();
    let loading_row = adw::ActionRow::builder()
        .title("Loading registry…")
        .build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    loading_row.add_suffix(&spinner);
    stack_group.add(&loading_row);
    page.add(&stack_group);

    // Kick off the version probe asynchronously. The widget refs flow
    // into the task via downgrade to avoid holding the page alive longer
    // than its parent container wants — when the page is destroyed,
    // upgrade() returns None and the task's result is silently dropped.
    let booted = runtime
        .block_on(async { service.current_image().await })
        .ok();
    if let Some(image) = booted {
        spawn_version_fetch(
            runtime,
            service.clone(),
            image,
            stack_group.downgrade(),
            loading_row.downgrade(),
        );
    } else {
        loading_row.set_title("Couldn't detect the booted image");
        spinner.set_spinning(false);
    }

    page
}

/// Fetch the most recent N versions for `image` in a worker, then on the
/// main loop tear `loading_row` out of `stack_group` and replace it with
/// one row per version. Weak refs are upgraded on the GTK side so a
/// widget destruction between the fetch starting and finishing is a
/// no-op.
fn spawn_version_fetch(
    runtime: &tokio::runtime::Runtime,
    service: Arc<dyn UpdaterService>,
    image: ImageRef,
    stack_group: glib::WeakRef<adw::PreferencesGroup>,
    loading_row: glib::WeakRef<adw::ActionRow>,
) {
    // Channel from tokio worker → GLib main loop. tokio::sync wouldn't
    // play with `glib::timeout_add_local`; std::sync::mpsc is enough.
    let (tx, rx) = std::sync::mpsc::channel();

    runtime.spawn(async move {
        let result = service.list_versions(&image, 4).await;
        let _ = tx.send(result);
    });

    // Poll the channel from the GLib main loop. 80 ms is fast enough to
    // feel snappy and slow enough not to burn CPU.
    glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
        let Ok(result) = rx.try_recv() else {
            return glib::ControlFlow::Continue;
        };

        let Some(group) = stack_group.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(loading) = loading_row.upgrade() else {
            return glib::ControlFlow::Break;
        };

        // Drop the spinner row and append real entries (or an error
        // notice). PreferencesGroup doesn't expose a "remove" verb
        // directly — the row is a child of an internal listbox; calling
        // .set_visible(false) is the pragmatic fix until we factor a
        // proper rebuild_stack helper.
        loading.set_visible(false);

        match result {
            Ok(versions) if versions.is_empty() => {
                let row = adw::ActionRow::builder()
                    .title("No recent builds returned by the registry")
                    .build();
                group.add(&row);
            }
            Ok(versions) => {
                for v in versions.iter().take(4) {
                    let row = adw::ActionRow::builder()
                        .title(&v.version)
                        .subtitle(&v.created.format("%b %-d, %Y").to_string())
                        .build();
                    if !v.kernel.is_empty() {
                        let lbl = gtk::Label::new(Some(&v.kernel));
                        lbl.add_css_class("monospace");
                        lbl.add_css_class("caption");
                        lbl.add_css_class("dim-label");
                        row.add_suffix(&lbl);
                    }
                    group.add(&row);
                }
            }
            Err(e) => {
                let row = adw::ActionRow::builder()
                    .title("Couldn't reach the registry")
                    .subtitle(&e.to_string())
                    .build();
                group.add(&row);
            }
        }

        glib::ControlFlow::Break
    });
}
