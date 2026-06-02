//! Self-contained "What's New" widget for embedding into other surfaces.
//!
//! Mirrors the standalone app's [`status_view::rebuild_changelog_page`] but
//! with one critical difference: this widget owns its data lifecycle. The
//! caller hands in a service handle + tokio runtime, the widget kicks off
//! its own registry / SBOM / GitHub fetches, and rebuilds its rows as
//! results land on the GLib main loop.
//!
//! Used by the gnome-control-center panel
//! ([Path 1 of the integration plan](../docs/control-center-integration.md))
//! through `ffi::finupdate_changelog_widget_new`. The standalone app
//! continues to use its own `rebuild_changelog_page` for now — both
//! renderers share the data helpers below (`StackItem`,
//! `build_stack_items`, `short_sha`), and a follow-up commit will retire
//! the duplication when the standalone app migrates to this widget too.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;

use crate::registry_client::ImageVersion;
use crate::sbom_diff::SbomDiffResult;
use crate::service::{ImageRef, UpdaterService};

// ─── Public entry point ────────────────────────────────────────────────────

/// Build a self-contained changelog widget for the booted image. Returns
/// an `adw::PreferencesPage` the caller can embed in any container — a
/// Cc panel subpage, a dialog, or a top-level window.
///
/// The widget owns its own data loading: a synchronous bootc-status read
/// for the hero row, plus three background fetches (registry versions,
/// SBOM diff, GitHub commits) that update their respective groups as
/// results arrive. Widget destruction during in-flight fetches is safe —
/// the upgrade-from-weak-ref path silently drops late results.
pub fn build_changelog_widget(
    service: Arc<dyn UpdaterService>,
    runtime: &tokio::runtime::Runtime,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().build();

    // ── Hero group ──────────────────────────────────────────────────────
    let hero_group = adw::PreferencesGroup::new();
    let hero_row = adw::ActionRow::builder()
        .title("System Image")
        .subtitle("Detecting…")
        .subtitle_lines(2)
        .subtitle_selectable(true)
        .build();
    hero_row.set_activatable(false);

    let booted: Option<ImageRef> = runtime
        .block_on(async { service.current_image().await })
        .ok();
    if let Some(ref image) = booted {
        hero_row.set_title(&format!("{}/{}", image.org, image.image));
        hero_row.set_subtitle(&image.as_string());
    }
    hero_group.add(&hero_row);
    page.add(&hero_group);

    // ── Stack group ─────────────────────────────────────────────────────
    // Holds 1-4 rows showing the from→to diff for Image / Kernel /
    // Revision / Built. Populated by spawn_stack_fetch.
    let stack_group = adw::PreferencesGroup::builder().title("Stack").build();
    let stack_loading = loading_row("Loading registry…");
    stack_group.add(&stack_loading);
    page.add(&stack_group);

    // ── Package changes groups ──────────────────────────────────────────
    // Three separate groups so each can fill in independently and the
    // user sees progressive results instead of one big spinner. Each
    // group is removed entirely when it ends up empty (no upgrades, no
    // adds, no removes), so the page stays tight.
    let updated_group = adw::PreferencesGroup::builder().title("Updated").build();
    let updated_loading = loading_row("Comparing packages…");
    updated_group.add(&updated_loading);
    updated_group.set_visible(false);
    page.add(&updated_group);

    let added_group = adw::PreferencesGroup::builder().title("Added").build();
    added_group.set_visible(false);
    page.add(&added_group);

    let removed_group = adw::PreferencesGroup::builder().title("Removed").build();
    removed_group.set_visible(false);
    page.add(&removed_group);

    // ── Kick off the async fetches ──────────────────────────────────────
    if let Some(image) = booted {
        spawn_stack_fetch(
            runtime,
            service.clone(),
            image.clone(),
            stack_group.downgrade(),
            stack_loading.downgrade(),
            updated_group.downgrade(),
            updated_loading.downgrade(),
            added_group.downgrade(),
            removed_group.downgrade(),
        );
    } else {
        stack_loading.set_title("Couldn't detect the booted image");
        // Strip the spinner suffix in this terminal state.
        let empty = gtk::Image::from_icon_name("dialog-warning-symbolic");
        stack_loading.add_suffix(&empty);
    }

    page
}

// ─── Async orchestration ────────────────────────────────────────────────────

/// The chain that fills in the changelog. Runs as one async task on the
/// runtime so we make one fetch decision (list_versions → pick target →
/// SBOM diff) without bouncing back to the main loop in between.
///
/// Each stage sends its partial result through a dedicated channel; the
/// main-loop poll routes the result into the right group. Stages that
/// fail are reported in their own group only, not as a global error.
#[allow(clippy::too_many_arguments)]
fn spawn_stack_fetch(
    runtime: &tokio::runtime::Runtime,
    service: Arc<dyn UpdaterService>,
    booted: ImageRef,
    stack_group: glib::WeakRef<adw::PreferencesGroup>,
    stack_loading: glib::WeakRef<adw::ActionRow>,
    updated_group: glib::WeakRef<adw::PreferencesGroup>,
    updated_loading: glib::WeakRef<adw::ActionRow>,
    added_group: glib::WeakRef<adw::PreferencesGroup>,
    removed_group: glib::WeakRef<adw::PreferencesGroup>,
) {
    let (stack_tx, stack_rx) = mpsc::channel::<StackResult>();
    let (sbom_tx, sbom_rx) = mpsc::channel::<SbomResult>();

    let booted_for_async = booted.clone();
    let svc = service.clone();
    runtime.spawn(async move {
        // Stage 1: registry versions → identify the target.
        let result = match svc.list_versions(&booted_for_async, 8).await {
            Ok(versions) if !versions.is_empty() => {
                // Newest from list_versions is sorted descending, so first
                // entry is the target. Booted match is best-effort via
                // version-string substring.
                let target = versions[0].clone();
                let booted_match = versions
                    .iter()
                    .find(|v| v.version.contains(&booted_for_async.tag))
                    .cloned();
                StackResult::Ok {
                    booted: booted_match,
                    target,
                }
            }
            Ok(_) => StackResult::Empty,
            Err(e) => StackResult::Err(e.to_string()),
        };
        let target_ref = match &result {
            StackResult::Ok { target, .. } => Some(target.full_ref.clone()),
            _ => None,
        };
        let _ = stack_tx.send(result);

        // Stage 2: SBOM diff between booted and target. Skipped if we
        // couldn't find a target — there's nothing meaningful to diff.
        if let Some(target_ref) = target_ref {
            let sbom = sbom_diff_for_panel(&booted_for_async, &target_ref).await;
            let _ = sbom_tx.send(sbom);
        } else {
            let _ = sbom_tx.send(SbomResult::Skipped);
        }
    });

    // Poll the stack channel from the GLib main loop.
    let host_kernel = host_kernel_string();
    let booted_ref_for_stack = booted.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
        let Ok(result) = stack_rx.try_recv() else {
            return glib::ControlFlow::Continue;
        };
        let Some(group) = stack_group.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(loading) = stack_loading.upgrade() else {
            return glib::ControlFlow::Break;
        };
        group.remove(&loading);

        match result {
            StackResult::Ok { booted, target } => {
                render_stack_items(&group, booted.as_ref(), &target, host_kernel.as_deref());
            }
            StackResult::Empty => {
                let row = adw::ActionRow::builder()
                    .title("No recent builds returned by the registry")
                    .build();
                group.add(&row);
            }
            StackResult::Err(msg) => {
                let row = adw::ActionRow::builder()
                    .title("Couldn't reach the registry")
                    .subtitle(&msg)
                    .build();
                group.add(&row);
            }
        }
        let _ = booted_ref_for_stack; // suppress unused on no-target paths
        glib::ControlFlow::Break
    });

    // Poll the SBOM channel from the GLib main loop.
    glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
        let Ok(result) = sbom_rx.try_recv() else {
            return glib::ControlFlow::Continue;
        };

        let Some(updated) = updated_group.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Some(loading) = updated_loading.upgrade() else {
            return glib::ControlFlow::Break;
        };
        updated.remove(&loading);

        match result {
            SbomResult::Ok(diff) => {
                if !diff.upgraded.is_empty() {
                    updated.set_title(&format!("Updated  ·  {}", diff.upgraded.len()));
                    updated.set_visible(true);
                    for pkg in diff.upgraded {
                        let row = adw::ActionRow::builder().title(&pkg.name).build();
                        let from_to = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                        let from = caption_label(&pkg.old_version, &["dim-label", "monospace"]);
                        let arrow = caption_label("→", &["dim-label"]);
                        let to = caption_label(&pkg.new_version, &["monospace", "success"]);
                        from_to.append(&from);
                        from_to.append(&arrow);
                        from_to.append(&to);
                        row.add_suffix(&from_to);
                        updated.add(&row);
                    }
                }

                if let Some(added) = added_group.upgrade() {
                    if !diff.added.is_empty() {
                        added.set_title(&format!("Added  ·  {}", diff.added.len()));
                        added.set_visible(true);
                        for pkg in diff.added {
                            let row = adw::ActionRow::builder().title(&pkg.name).build();
                            let plus = caption_label("+", &["success", "monospace"]);
                            row.add_prefix(&plus);
                            let ver =
                                caption_label(&pkg.new_version, &["monospace", "success"]);
                            row.add_suffix(&ver);
                            added.add(&row);
                        }
                    }
                }

                if let Some(removed) = removed_group.upgrade() {
                    if !diff.removed.is_empty() {
                        removed.set_title(&format!("Removed  ·  {}", diff.removed.len()));
                        removed.set_visible(true);
                        for name in diff.removed {
                            let row = adw::ActionRow::builder().title(&name).build();
                            let dash = caption_label("−", &["error", "monospace"]);
                            row.add_prefix(&dash);
                            removed.add(&row);
                        }
                    }
                }
            }
            SbomResult::Unavailable => {
                updated.set_title("Package diff unavailable");
                updated.set_visible(true);
                let row = adw::ActionRow::builder()
                    .title("Registry didn't publish SPDX referrers for one of the images")
                    .build();
                row.add_css_class("dim-label");
                updated.add(&row);
            }
            SbomResult::Skipped => {
                // No target was found — nothing to diff. Stay hidden.
            }
        }
        glib::ControlFlow::Break
    });
}

/// Result of the registry-versions probe for the Stack section.
enum StackResult {
    Ok {
        booted: Option<ImageVersion>,
        target: ImageVersion,
    },
    Empty,
    Err(String),
}

/// Result of the SBOM-diff probe for the package-changes groups.
enum SbomResult {
    Ok(SbomDiffResult),
    Unavailable,
    Skipped,
}

async fn sbom_diff_for_panel(booted: &ImageRef, target_full_ref: &str) -> SbomResult {
    // Prefer the digest-pinned booted form so the diff compares two
    // distinct manifests on images whose floating tag updates between
    // fetches (Dakota's `:latest` is the canonical case). Falls back to
    // the tag form when the service couldn't get a digest from
    // bootc-status — diff is still meaningful on Bluefin where the
    // tag is dated.
    let booted_ref = booted.as_digest_pinned_string();
    if booted_ref == target_full_ref {
        return SbomResult::Skipped;
    }
    match crate::sbom_diff::fetch_and_diff_sboms(booted_ref, target_full_ref.to_string()).await {
        Some(d) => SbomResult::Ok(d),
        None => SbomResult::Unavailable,
    }
}

// ─── Stack rendering ────────────────────────────────────────────────────────

fn render_stack_items(
    group: &adw::PreferencesGroup,
    booted: Option<&ImageVersion>,
    target: &ImageVersion,
    host_kernel: Option<&str>,
) {
    let items = build_stack_items(booted, target, host_kernel);
    for item in items {
        let row = adw::ActionRow::builder().title(item.label).build();
        let diff = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        diff.set_valign(gtk::Align::Center);

        let from = caption_label(
            item.current.as_deref().unwrap_or("—"),
            &["dim-label", "monospace"],
        );
        let arrow = caption_label("→", &["dim-label"]);
        let to_classes: &[&str] = if item.bumped {
            &["monospace", "success"]
        } else {
            &["dim-label", "monospace"]
        };
        let to = caption_label(&item.target, to_classes);

        diff.append(&from);
        diff.append(&arrow);
        diff.append(&to);
        row.add_suffix(&diff);
        group.add(&row);
    }
}

// ─── Data helpers — duplicated from status_view.rs intentionally ────────────
//
// Keep parallel with `ui::status_view::{StackItem, build_stack_items,
// short_sha}` for now. A follow-up that retires the duplicate
// `rebuild_changelog_page` will collapse these into one home.

/// One row in the changelog "Stack" diff: a labelled component (Image,
/// Kernel, Revision, Built) plus its current and target value. `bumped`
/// flags rows whose value moved, driving the success-colour styling on
/// the target side.
#[derive(Debug, Clone)]
struct StackItem {
    label: &'static str,
    current: Option<String>,
    target: String,
    bumped: bool,
}

fn build_stack_items(
    booted: Option<&ImageVersion>,
    target: &ImageVersion,
    host_kernel: Option<&str>,
) -> Vec<StackItem> {
    let mut out = Vec::new();

    // Image (version annotation, e.g. "stable-daily-43.20260602")
    let image_bumped = booted.map(|b| b.version != target.version).unwrap_or(true);
    out.push(StackItem {
        label: "Image",
        current: booted.map(|b| b.version.clone()),
        target: target.version.clone(),
        bumped: image_bumped,
    });

    // Kernel — hidden when neither side has data (Dakota).
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
            (None, Some(_)) => true,
            _ => false,
        };
        out.push(StackItem {
            label: "Kernel",
            current: current_kernel,
            target: target_kernel.unwrap_or_default(),
            bumped: kernel_bumped,
        });
    }

    // Revision (short git sha)
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

    // Built
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

// ─── GTK helpers ────────────────────────────────────────────────────────────

/// Loading spinner row with a caption-style label.
fn loading_row(text: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(text).build();
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_size_request(16, 16);
    row.add_suffix(&spinner);
    row
}

/// Build a Label with a list of CSS classes — used for the from / arrow /
/// to row suffixes in the diff sections.
fn caption_label(text: &str, classes: &[&str]) -> gtk::Label {
    let lbl = gtk::Label::new(Some(text));
    lbl.add_css_class("caption");
    for cls in classes {
        lbl.add_css_class(cls);
    }
    lbl
}

/// Best-effort `uname -r` for the booted kernel. The standalone app does
/// the flatpak-spawn fallback; the panel runs in the cc process (no
/// sandbox) so a plain Command is enough.
fn host_kernel_string() -> Option<String> {
    let out = std::process::Command::new("uname").arg("-r").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

// Suppress dead_code on the Rc-typed weak-ref glue some callers may want
// later (currently unused outside this file).
#[allow(dead_code)]
fn _phantom_rc_use() {
    let _ = Rc::<RefCell<Option<String>>>::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone, Utc};

    fn fake_iv(version: &str, kernel: &str, revision: &str) -> ImageVersion {
        ImageVersion {
            date: NaiveDate::from_ymd_opt(2026, 5, 30).unwrap(),
            full_ref: format!("ghcr.io/example/image:{version}"),
            version: version.to_string(),
            kernel: kernel.to_string(),
            revision: revision.to_string(),
            created: Utc.with_ymd_and_hms(2026, 5, 30, 0, 0, 0).unwrap(),
        }
    }

    #[test]
    fn short_sha_truncates_at_seven() {
        assert_eq!(short_sha("abcdef0123456789"), "abcdef0");
    }

    #[test]
    fn short_sha_short_input_returns_as_is() {
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn stack_items_omits_kernel_when_both_empty() {
        let booted = fake_iv("a", "", "abc1234");
        let target = fake_iv("b", "", "def5678");
        let items = build_stack_items(Some(&booted), &target, None);
        assert!(!items.iter().any(|i| i.label == "Kernel"));
    }

    #[test]
    fn stack_items_includes_kernel_when_host_falls_back() {
        let booted = fake_iv("a", "", "abc1234");
        let target = fake_iv("b", "", "def5678");
        let items = build_stack_items(Some(&booted), &target, Some("7.0.7"));
        let k = items.iter().find(|i| i.label == "Kernel").unwrap();
        assert_eq!(k.current.as_deref(), Some("7.0.7"));
        assert_eq!(k.target, "");
    }

    #[test]
    fn stack_items_bumps_image_when_versions_differ() {
        let booted = fake_iv("20260527", "", "");
        let target = fake_iv("20260530", "", "");
        let items = build_stack_items(Some(&booted), &target, None);
        let img = items.iter().find(|i| i.label == "Image").unwrap();
        assert!(img.bumped);
    }
}
