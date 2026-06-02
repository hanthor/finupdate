//! Update check dialog — multi-source update checker inspired by uupd.
//!
//! Shows a modal dialog that sequentially checks each update source
//! (System image, Flatpak, Homebrew, Distrobox) and reports per-source
//! status with progress indication.
//!
//! ## Design
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │  Checking for updates…                      │
//! │  Powered by uupd                            │
//! │  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━        │
//! │                                             │
//! │  🖥  System image     ✓ Up to date          │
//! │  📦  Flatpak          ● 4 apps to update    │
//! │  🍺  Homebrew         ⟳ Checking…           │
//! │  📦  Distrobox        ○ Waiting             │
//! │                                             │
//! │  Querying 4 update sources…                 │
//! │                        [Close] [Install all]│
//! └─────────────────────────────────────────────┘
//! ```
//!
//! ## Integration
//!
//! The dialog runs the actual `uupd` subprocess (or simulated in dev mode)
//! and parses its structured log output to determine module transitions.
//! When checking completes, emits a summary back to the parent via a callback.

use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use crate::update_worker::{SimulationScenario, UpdateEvent, UpdateWorker, run_simulated};

/// Per-source check state.
#[derive(Debug, Clone, PartialEq)]
enum SourceState {
    Waiting,
    Checking,
    Found(String),
    UpToDate,
}

/// Result summary from the check dialog.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CheckResult {
    /// Whether the system image has an update available.
    pub system_update: bool,
    /// Number of total sources that found updates.
    pub sources_with_updates: u32,
    /// Total update count across all sources.
    pub total_updates: u32,
}

/// Source entry for the check UI.
///
/// Each entry renders as an `adw::ExpanderRow` whose status suffix shows the
/// current state (waiting / checking / found / uptodate) and whose expander
/// body holds the streaming log output for that module. The user can click
/// any row to see its logs inline instead of switching to a separate
/// fullscreen view.
#[allow(dead_code)]
struct SourceEntry {
    name: &'static str,
    subtitle: &'static str,
    icon_name: &'static str,
    row: adw::ExpanderRow,
    status_stack: gtk::Stack,
    spinner: gtk::Spinner,
    log_buffer: gtk::TextBuffer,
    state: SourceState,
}

/// Shared mutable state for the dialog's async check process.
#[allow(dead_code)]
struct CheckState {
    sources: Vec<SourceEntry>,
    progress_bar: gtk::ProgressBar,
    title_label: gtk::Label,
    summary_label: gtk::Label,
    install_btn: gtk::Button,
    close_btn: gtk::Button,
    done: bool,
}

/// Module keys from uupd logs, in order.
const SOURCE_DEFS: &[(&str, &str, &str, &str)] = &[
    (
        "system",
        "System image",
        "bootc · OS image updates",
        "drive-harddisk-symbolic",
    ),
    (
        "flatpak",
        "Flatpak",
        "Applications from Flathub",
        "system-software-install-symbolic",
    ),
    (
        "brew",
        "Homebrew",
        "User formulae and casks",
        "package-x-generic-symbolic",
    ),
    (
        "distrobox",
        "Distrobox",
        "Container environments",
        "utilities-terminal-symbolic",
    ),
];

/// Show the update check dialog.
///
/// `on_result` is called when the check completes with the summary.
/// `on_install` is called if the user clicks "Install all".
pub fn show_update_check_dialog(
    parent: &adw::ApplicationWindow,
    dev_mode: bool,
    sim_scenario: SimulationScenario,
    on_result: impl Fn(CheckResult) + 'static,
    on_install: impl Fn() + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title("Check for Updates")
        .content_width(420)
        .content_height(380)
        .build();

    // ── Build content ────────────────────────────────────────────────────
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_start(24);
    content.set_margin_end(24);
    content.set_margin_top(20);
    content.set_margin_bottom(20);

    // Header. The dialog runs the actual update via UpdateWorker — the
    // "check" framing was misleading because clicking the button kicked off
    // a full install. Title is "Updating…" until the orchestrator returns.
    let title_label = gtk::Label::new(Some("Updating…"));
    title_label.add_css_class("title-3");
    title_label.set_halign(gtk::Align::Start);
    title_label.set_margin_bottom(4);

    let subtitle = gtk::Label::new(Some("Powered by uupd · system, flatpak, brew, distrobox"));
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    subtitle.set_halign(gtk::Align::Start);
    subtitle.set_margin_bottom(16);

    // No top-level progress bar — per-row spinners + check icons already
    // signal progress, and the user's directive was "one progress bar across
    // the app" (which lives on the Updating page during install). The
    // progress_bar widget is still allocated below as an orphan so the
    // existing update_progress() / s.progress_bar.set_fraction() call sites
    // don't have to change; it just never reaches a parent container.
    let progress_bar = gtk::ProgressBar::new();

    content.append(&title_label);
    content.append(&subtitle);

    // Source rows in a ListBox
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_bottom(20);

    let mut sources = Vec::with_capacity(SOURCE_DEFS.len());

    for &(_key, name, sub, icon) in SOURCE_DEFS {
        let row = adw::ExpanderRow::builder().title(name).subtitle(sub).build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));

        // Status stack: waiting / checking (spinner) / found / uptodate
        let status_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .valign(gtk::Align::Center)
            .build();

        let waiting_label = gtk::Label::new(Some("Waiting"));
        waiting_label.add_css_class("dim-label");
        waiting_label.add_css_class("caption");

        let spinner = gtk::Spinner::new();
        spinner.set_size_request(16, 16);
        let checking_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        checking_box.append(&spinner);
        let checking_label = gtk::Label::new(Some("Checking…"));
        checking_label.add_css_class("caption");
        checking_box.append(&checking_label);

        let found_label = gtk::Label::new(Some("Update found"));
        found_label.add_css_class("caption");
        found_label.add_css_class("accent");

        let uptodate_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let check_icon = gtk::Image::from_icon_name("object-select-symbolic");
        check_icon.add_css_class("success");
        check_icon.set_pixel_size(14);
        uptodate_box.append(&check_icon);
        let uptodate_label = gtk::Label::new(Some("Up to date"));
        uptodate_label.add_css_class("caption");
        uptodate_label.add_css_class("success");
        uptodate_box.append(&uptodate_label);

        status_stack.add_named(&waiting_label, Some("waiting"));
        status_stack.add_named(&checking_box, Some("checking"));
        status_stack.add_named(&found_label, Some("found"));
        status_stack.add_named(&uptodate_box, Some("uptodate"));
        status_stack.set_visible_child_name("waiting");

        row.add_suffix(&status_stack);

        // Inline log panel — read-only TextView in a scrolled window. Lives
        // inside the ExpanderRow body so clicking the row chevron reveals
        // streaming logs for THIS module. Replaces the previous fullscreen
        // "updating" Stack page; everything lives in the modal now.
        let log_buffer = gtk::TextBuffer::new(None);
        let log_view = gtk::TextView::with_buffer(&log_buffer);
        log_view.set_editable(false);
        log_view.set_cursor_visible(false);
        log_view.set_monospace(true);
        log_view.set_wrap_mode(gtk::WrapMode::WordChar);
        log_view.add_css_class("dim-label");
        log_view.add_css_class("caption");
        let log_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(120)
            .max_content_height(220)
            .vexpand(false)
            .build();
        log_scroll.set_child(Some(&log_view));
        log_scroll.set_margin_start(12);
        log_scroll.set_margin_end(12);
        log_scroll.set_margin_top(4);
        log_scroll.set_margin_bottom(8);
        row.add_row(&log_scroll);

        list_box.append(&row);

        sources.push(SourceEntry {
            name,
            subtitle: sub,
            icon_name: icon,
            row,
            status_stack,
            spinner,
            log_buffer,
            state: SourceState::Waiting,
        });
    }

    content.append(&list_box);

    // Footer: summary + buttons
    let summary_label = gtk::Label::new(Some("Running update across 4 sources…"));
    summary_label.add_css_class("dim-label");
    summary_label.set_halign(gtk::Align::Start);
    summary_label.set_margin_bottom(12);
    content.append(&summary_label);

    let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    button_box.set_halign(gtk::Align::End);
    button_box.set_hexpand(true);

    // Dialog action buttons (bottom-right, in a horizontal box) — per GNOME
    // HIG / control-center idiom these are rectangular with rounded corners
    // (.suggested-action alone), not pill-shaped. .pill is reserved for
    // centered standalone hero CTAs.
    // Labelled "Cancel" while the update is in flight, swapped to "Close"
    // once Done/Error fires. Matches the GNOME HIG "destructive abort during,
    // dismiss after" idiom for long-running modal operations.
    let close_btn = gtk::Button::with_label("Cancel");

    // The install_btn is allocated but never shown — the dialog already runs
    // the full update on open via UpdateWorker, so a separate "Install all"
    // would re-run the same work. Allocated as an orphan so call sites that
    // still touch `s.install_btn` don't have to change. Drop this once those
    // touches are removed.
    let install_btn = gtk::Button::with_label("Install all");
    install_btn.set_visible(false);

    button_box.append(&close_btn);
    content.append(&button_box);

    // Wrap in a toolbar view with header bar
    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    dialog.set_child(Some(&toolbar_view));

    // ── Shared state ─────────────────────────────────────────────────────
    let state = Rc::new(RefCell::new(CheckState {
        sources,
        progress_bar: progress_bar.clone(),
        title_label: title_label.clone(),
        summary_label: summary_label.clone(),
        install_btn: install_btn.clone(),
        close_btn: close_btn.clone(),
        done: false,
    }));

    // ── Wire close button ────────────────────────────────────────────────
    let dialog_close = dialog.clone();
    close_btn.connect_clicked(move |_| {
        dialog_close.close();
    });

    // The install_btn is now orphaned (see allocation above). The dialog
    // runs the update itself, so there's nothing for an explicit "Install"
    // click to do that wasn't already happening. Keep the `on_install`
    // parameter wired so callers don't change yet.
    let _ = on_install;

    // ── Run the check process ────────────────────────────────────────────
    let state_clone = state.clone();
    let on_result = Rc::new(on_result);

    // Spawn the update check on a background thread.
    // Use a polling mechanism to receive results on the main thread.
    let (line_tx, line_rx) = mpsc::channel::<CheckEvent>();

    glib::timeout_add_local(std::time::Duration::from_millis(50), {
        let state = state_clone.clone();
        let on_result = on_result.clone();
        move || {
            for event in line_rx.try_iter() {
                let mut s = state.borrow_mut();
                match event {
                    CheckEvent::ModuleStart(key) => {
                        // Mark the matching source as Checking
                        for source in s.sources.iter_mut() {
                            if source_matches_key(source.name, &key) {
                                source.state = SourceState::Checking;
                                source.status_stack.set_visible_child_name("checking");
                                source.spinner.set_spinning(true);
                            }
                        }
                        update_progress(&s);
                    }
                    CheckEvent::ModuleComplete(key) => {
                        for source in s.sources.iter_mut() {
                            if source_matches_key(source.name, &key) {
                                source.state = SourceState::UpToDate;
                                source.status_stack.set_visible_child_name("uptodate");
                                source.spinner.set_spinning(false);
                            }
                        }
                        update_progress(&s);
                    }
                    CheckEvent::ModuleFound(key, msg) => {
                        for source in s.sources.iter_mut() {
                            if source_matches_key(source.name, &key) {
                                if let Some(found_label) =
                                    source.status_stack.child_by_name("found")
                                {
                                    if let Ok(label) = found_label.downcast::<gtk::Label>() {
                                        label.set_label(&msg);
                                    }
                                }
                                source.state = SourceState::Found(msg.clone());
                                source.status_stack.set_visible_child_name("found");
                                source.spinner.set_spinning(false);
                            }
                        }
                        update_progress(&s);
                    }
                    CheckEvent::LogLine { module, line } => {
                        // Append to the matching module's buffer. Drop the
                        // line if the line happens to start with ANSI
                        // escapes — common in flatpak / brew output — so
                        // the panel stays legible.
                        let stripped = strip_ansi_escapes(&line);
                        for source in s.sources.iter_mut() {
                            if source_matches_key(source.name, &module) {
                                let buf = &source.log_buffer;
                                let mut end = buf.end_iter();
                                if buf.char_count() > 0 {
                                    buf.insert(&mut end, "\n");
                                }
                                buf.insert(&mut end, &stripped);
                            }
                        }
                    }
                    CheckEvent::Done => {
                        s.done = true;
                        s.progress_bar.set_fraction(1.0);

                        let updates_found: u32 = s
                            .sources
                            .iter()
                            .filter(|src| matches!(src.state, SourceState::Found(_)))
                            .count() as u32;
                        let system_update = s
                            .sources
                            .first()
                            .map(|src| matches!(src.state, SourceState::Found(_)))
                            .unwrap_or(false);

                        // Mark any still-waiting sources as up-to-date
                        for source in s.sources.iter_mut() {
                            if source.state == SourceState::Waiting
                                || source.state == SourceState::Checking
                            {
                                source.state = SourceState::UpToDate;
                                source.status_stack.set_visible_child_name("uptodate");
                                source.spinner.set_spinning(false);
                            }
                        }

                        if updates_found > 0 {
                            s.title_label.set_label(&format!(
                                "Updates installed · {} source{}",
                                updates_found,
                                if updates_found == 1 { "" } else { "s" }
                            ));
                            s.summary_label
                                .set_label("Reboot to apply system changes.");
                        } else {
                            s.title_label.set_label("Everything is up to date");
                            s.summary_label
                                .set_label("You're running the latest images and apps.");
                        }
                        s.close_btn.set_label("Close");

                        let result = CheckResult {
                            system_update,
                            sources_with_updates: updates_found,
                            total_updates: updates_found,
                        };
                        (on_result)(result);
                    }
                    CheckEvent::Error(msg) => {
                        s.done = true;
                        s.title_label.set_label("Check failed");
                        s.summary_label.set_label(&msg);
                        // Mark running sources as up-to-date (fallback)
                        for source in s.sources.iter_mut() {
                            if source.state == SourceState::Checking {
                                source.state = SourceState::UpToDate;
                                source.status_stack.set_visible_child_name("uptodate");
                                source.spinner.set_spinning(false);
                            }
                        }
                    }
                }
            }

            if state.borrow().done {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        }
    });

    // Spawn the background check thread
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        rt.block_on(async move {
            let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();

            let mut rx = if dev_mode {
                run_simulated(sim_scenario, cancel_rx).await
            } else {
                UpdateWorker::new().run(cancel_rx).await
            };

            let mut current_module: Option<String> = None;
            let mut system_found = false;

            while let Some(event) = rx.recv().await {
                match event {
                    UpdateEvent::Output(line) => {
                        // Parse module transitions from uupd structured logs
                        if let Some(key) = extract_module_key_from_line(&line) {
                            // Complete previous module if switching
                            if let Some(ref prev) = current_module {
                                if prev != key {
                                    let _ = line_tx.send(CheckEvent::ModuleComplete(prev.clone()));
                                }
                            }
                            current_module = Some(key.to_string());
                            let _ = line_tx.send(CheckEvent::ModuleStart(key.to_string()));
                        }

                        // Detect update-found signals
                        if line.contains("Updates Completed Successfully") {
                            // Final success — mark system as found if bootc did work
                            if current_module.as_deref() == Some("system") {
                                system_found = true;
                            }
                        }

                        // Route the raw line into the active module's log
                        // buffer so the user can expand the row and see
                        // what's happening live. Skip lines we can't route.
                        if let Some(ref m) = current_module {
                            let _ = line_tx.send(CheckEvent::LogLine {
                                module: m.clone(),
                                line: line.clone(),
                            });
                        }
                    }
                    UpdateEvent::Complete => {
                        // Mark final module as complete
                        if let Some(ref last) = current_module {
                            let _ = line_tx.send(CheckEvent::ModuleComplete(last.clone()));
                        }
                        // `Complete` = orchestrator exit 0 = the runner did
                        // real work. Treat the system module as "found"
                        // unconditionally — the earlier `system_found`
                        // string match against the runner's log output was
                        // brittle and missed every successful update on
                        // systems where the runner uses a different success
                        // string. The exit code is the ground truth.
                        if !system_found {
                            tracing::debug!(
                                "check: orchestrator exited 0 but log parse missed the system success marker — treating Complete as updates installed"
                            );
                        }
                        let _ = line_tx.send(CheckEvent::ModuleFound(
                            "system".to_string(),
                            "Update installed".to_string(),
                        ));
                        let _ = line_tx.send(CheckEvent::Done);
                        break;
                    }
                    UpdateEvent::UpToDate => {
                        if let Some(ref last) = current_module {
                            let _ = line_tx.send(CheckEvent::ModuleComplete(last.clone()));
                        }
                        let _ = line_tx.send(CheckEvent::Done);
                        break;
                    }
                    UpdateEvent::Error(err) => {
                        let _ = line_tx.send(CheckEvent::Error(err));
                        break;
                    }
                    UpdateEvent::ModuleStarted(module) => {
                        let key = module.key().to_string();
                        // Complete previous module if switching.
                        if let Some(ref prev) = current_module {
                            if prev != &key {
                                let _ = line_tx.send(CheckEvent::ModuleComplete(prev.clone()));
                            }
                        }
                        current_module = Some(key.clone());
                        let _ = line_tx.send(CheckEvent::ModuleStart(key));
                    }
                    UpdateEvent::ModuleFinished(module, status) => {
                        use crate::orchestrator::ModuleStatus;
                        let key = module.key().to_string();
                        match status {
                            ModuleStatus::Success => {
                                // System module success means a staged update.
                                if module == crate::orchestrator::Module::System {
                                    system_found = true;
                                }
                                let _ = line_tx.send(CheckEvent::ModuleComplete(key));
                            }
                            ModuleStatus::UpToDate | ModuleStatus::Skipped => {
                                let _ = line_tx.send(CheckEvent::ModuleComplete(key));
                            }
                            ModuleStatus::Failed(code) => {
                                let _ = line_tx.send(CheckEvent::Error(format!(
                                    "{key} module failed (exit {code})"
                                )));
                            }
                        }
                    }
                }
            }
        });
    });

    // Present the dialog
    dialog.present(Some(parent));
}

/// Internal events sent from the check thread to the main thread.
#[derive(Debug, Clone)]
enum CheckEvent {
    ModuleStart(String),
    ModuleComplete(String),
    ModuleFound(String, String),
    /// Raw log line for the currently-active module — routed into that
    /// row's log buffer so the user can expand it and see what happened.
    /// `module` is the key (system/flatpak/brew/distrobox); `line` is the
    /// log content with no trailing newline.
    LogLine { module: String, line: String },
    Done,
    Error(String),
}

/// Match a source display name to a module key from logs.
fn source_matches_key(source_name: &str, key: &str) -> bool {
    match key {
        "system" => source_name == "System image",
        "flatpak" => source_name == "Flatpak",
        "brew" => source_name == "Homebrew",
        "distrobox" => source_name == "Distrobox",
        _ => false,
    }
}

/// Extract module key from uupd log line (same logic as status_view).
fn extract_module_key_from_line(line: &str) -> Option<&'static str> {
    if line.contains("module_name=System") {
        Some("system")
    } else if line.contains("module_name=Flatpak") {
        Some("flatpak")
    } else if line.contains("module_name=Brew") {
        Some("brew")
    } else if line.contains("module_name=Distrobox") {
        Some("distrobox")
    } else {
        None
    }
}

/// Update the progress bar fraction based on completed sources.
/// Strip ANSI escape sequences from a log line. Flatpak and brew dump
/// colour codes that render as `^[[32m` in a TextView — drop them so the
/// expanded log panel stays readable. Conservative regex-free strip:
/// drops `ESC [ ... letter` runs.
fn strip_ansi_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&n) = chars.peek() {
                chars.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn update_progress(state: &CheckState) {
    let total = state.sources.len() as f64;
    let completed = state
        .sources
        .iter()
        .filter(|s| matches!(s.state, SourceState::Found(_) | SourceState::UpToDate))
        .count() as f64;
    let checking = state
        .sources
        .iter()
        .filter(|s| s.state == SourceState::Checking)
        .count() as f64;
    // Count checking as half-progress
    let fraction = (completed + checking * 0.5) / total;
    state.progress_bar.set_fraction(fraction);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── source_matches_key ──────────────────────────────────────────────
    // Maps display labels shown in the check dialog ("System image",
    // "Homebrew", etc.) to the module key uupd uses internally. Lets the
    // dialog highlight whichever source corresponds to the running module.

    #[test]
    fn source_matches_system_image_for_system_key() {
        assert!(source_matches_key("System image", "system"));
    }

    #[test]
    fn source_matches_flatpak() {
        assert!(source_matches_key("Flatpak", "flatpak"));
    }

    #[test]
    fn source_matches_homebrew_for_brew_key() {
        // uupd internally calls it "brew" but the user-facing label is the
        // full "Homebrew" — pin both sides of the mapping.
        assert!(source_matches_key("Homebrew", "brew"));
    }

    #[test]
    fn source_matches_distrobox() {
        assert!(source_matches_key("Distrobox", "distrobox"));
    }

    #[test]
    fn source_rejects_wrong_label_for_key() {
        assert!(!source_matches_key("Flatpak", "system"));
        assert!(!source_matches_key("System image", "brew"));
    }

    #[test]
    fn source_rejects_unknown_key() {
        assert!(!source_matches_key("System image", "mystery"));
    }

    // ── extract_module_key_from_line ────────────────────────────────────

    #[test]
    fn extract_recognises_system() {
        assert_eq!(
            extract_module_key_from_line("time=2026-05-30 module_name=System level=INFO"),
            Some("system")
        );
    }

    #[test]
    fn extract_recognises_flatpak() {
        assert_eq!(
            extract_module_key_from_line("module_name=Flatpak"),
            Some("flatpak")
        );
    }

    #[test]
    fn extract_recognises_brew() {
        assert_eq!(
            extract_module_key_from_line("module_name=Brew"),
            Some("brew")
        );
    }

    #[test]
    fn extract_recognises_distrobox() {
        assert_eq!(
            extract_module_key_from_line("module_name=Distrobox"),
            Some("distrobox")
        );
    }

    #[test]
    fn extract_returns_none_for_non_module_lines() {
        assert!(extract_module_key_from_line("time=2026-05-30 level=INFO message=ready").is_none());
        assert!(extract_module_key_from_line("").is_none());
    }
}
