//! Update list component — shows per-module status during an update run.
//!
//! Displays four rows (System, Flatpak, Brew, Distrobox) as `adw::ExpanderRow`
//! widgets inside an `adw::PreferencesGroup`. Each row shows a status indicator
//! (spinner while running, icon when complete/failed).
//!
//! The **Nerd Mode** toggle lives in the group's header suffix slot
//! (`set_header_suffix`) — the HIG-correct placement for a group-level action.
//! When active, rows expand to reveal the raw log attributed to each module.
//!
//! ## HIG notes
//! - `adw::PreferencesGroup` + `adw::ExpanderRow` is the canonical pattern for
//!   expandable grouped rows; the group manages its own margins and separator.
//! - Status icons follow the system icon naming spec:
//!   `object-select-symbolic` (complete), `dialog-warning-symbolic` (failed).
//! - The log sub-row wraps its label in an `adw::ActionRow` so padding and
//!   typography match the rest of the row hierarchy automatically.

use adw::prelude::*;
use relm4::prelude::*;

/// The four uupd modules, in execution order.
/// Fields: (key, display name, short description)
const MODULES: &[(&str, &str, &str)] = &[
    ("system", "System", "OS image · bootc"),
    ("flatpak", "Flatpak", "Applications"),
    ("brew", "Brew", "Homebrew packages"),
    ("distrobox", "Distrobox", "Container environments"),
];

#[derive(Debug, Clone, PartialEq)]
enum ModuleStatus {
    Pending,
    Running,
    Complete,
    Failed,
}

struct ModuleEntry {
    #[allow(dead_code)]
    key: &'static str,
    description: &'static str,
    row: adw::ExpanderRow,
    status_stack: gtk::Stack,
    spinner: gtk::Spinner,
    log_label: gtk::Label,
    status: ModuleStatus,
    log_text: String,
}

/// Input messages for the UpdateList component.
#[derive(Debug)]
pub enum UpdateListInput {
    /// Reset all modules to Pending (call when a new update starts).
    Reset,
    /// Process one log line from uupd — detect module transitions and attribute output.
    ProcessLine(String),
    /// Toggle Nerd Mode: expand all rows to show raw log output.
    SetNerdMode(bool),
    /// Mark all Pending/Running modules as Complete (update finished OK).
    MarkAllComplete,
    /// Mark the currently Running module as Failed.
    MarkCurrentFailed,
}

/// Component model.
pub struct UpdateList {
    modules: Vec<ModuleEntry>,
    /// Index into `modules` for the module that is currently running.
    current_module: Option<usize>,
    nerd_mode: bool,
    nerd_btn: gtk::ToggleButton,
}

#[relm4::component(pub)]
impl SimpleComponent for UpdateList {
    type Init = ();
    type Input = UpdateListInput;
    type Output = ();

    view! {
        #[root]
        gtk::Box {
            set_orientation: gtk::Orientation::Vertical,
            set_spacing: 0,
        }

    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // ── Nerd Mode button — lives in the group header suffix slot ──────
        // This is the HIG-correct placement for a group-level toggle action.
        let nerd_btn = gtk::ToggleButton::builder()
            .label("Nerd Mode")
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Show detailed log output per component")
            .build();
        nerd_btn.add_css_class("flat");

        let nerd_sender = sender.input_sender().clone();
        nerd_btn.connect_toggled(move |btn| {
            nerd_sender.emit(UpdateListInput::SetNerdMode(btn.is_active()));
        });

        // ── PreferencesGroup: handles title, margins, and boxed-list ─────
        // adw::PreferencesGroup is the canonical HIG container for grouped
        // rows; it owns the separator style and the correct top-level margins.
        let group = adw::PreferencesGroup::builder()
            .title("Component Updates")
            .build();
        group.set_header_suffix(Some(&nerd_btn));

        let mut modules = Vec::with_capacity(MODULES.len());

        for &(key, name, description) in MODULES {
            // adw::ExpanderRow implements AdwPreferencesRow so it can be
            // added directly to the group — no intermediate gtk::ListBox needed.
            let row = adw::ExpanderRow::builder()
                .title(name)
                .subtitle(description)
                // Arrow hidden until Nerd Mode is enabled — keeps the list
                // compact and avoids affordance confusion while not interactive.
                .enable_expansion(false)
                .build();

            // ── Status indicator (suffix) ─────────────────────────────────
            // gtk::Stack with crossfade transitions between four visual states.
            let status_stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(150)
                .valign(gtk::Align::Center)
                .build();

            // Pending: subtle loading indicator — neutral, non-intrusive
            let pending_icon = gtk::Image::from_icon_name("content-loading-symbolic");
            pending_icon.add_css_class("dim-label");

            // Running: animated spinner
            let spinner = gtk::Spinner::new();
            spinner.set_size_request(16, 16);

            // Complete: system standard "selected/done" checkmark
            let complete_icon = gtk::Image::from_icon_name("object-select-symbolic");
            complete_icon.add_css_class("success");

            // Failed: warning triangle (less alarming than error, still clear)
            let failed_icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            failed_icon.add_css_class("warning");

            status_stack.add_named(&pending_icon, Some("pending"));
            status_stack.add_named(&spinner, Some("running"));
            status_stack.add_named(&complete_icon, Some("complete"));
            status_stack.add_named(&failed_icon, Some("failed"));
            status_stack.set_visible_child_name("pending");

            row.add_suffix(&status_stack);

            // ── Log sub-row (Nerd Mode) ───────────────────────────────────
            // Wrapping in adw::ActionRow gives the correct indentation, row
            // height, and separator handling for a child of ExpanderRow.
            let log_row = adw::ActionRow::new();
            log_row.set_activatable(false);

            let log_label = gtk::Label::builder()
                .use_markup(false)
                .selectable(true)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .valign(gtk::Align::Start)
                .margin_top(6)
                .margin_bottom(6)
                .build();
            log_label.add_css_class("monospace");
            log_label.add_css_class("caption");
            log_label.add_css_class("dim-label");
            log_label.set_text("No output yet.");

            log_row.set_child(Some(&log_label));
            row.add_row(&log_row);

            group.add(&row);

            modules.push(ModuleEntry {
                key,
                description,
                row,
                status_stack,
                spinner,
                log_label,
                status: ModuleStatus::Pending,
                log_text: String::new(),
            });
        }

        root.append(&group);

        let model = UpdateList {
            modules,
            current_module: None,
            nerd_mode: false,
            nerd_btn,
        };

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            UpdateListInput::Reset => {
                self.current_module = None;
                self.nerd_mode = false;
                self.nerd_btn.set_active(false);
                for i in 0..self.modules.len() {
                    self.modules[i].log_text.clear();
                    self.modules[i].log_label.set_text("No output yet.");
                    self.modules[i].row.set_enable_expansion(false);
                    self.modules[i].row.set_expanded(false);
                    self.apply_status(i, ModuleStatus::Pending);
                }
            }

            UpdateListInput::ProcessLine(line) => {
                // Check if this line announces a module starting.
                if let Some(idx) = detect_module_start(&line) {
                    // Mark the previously running module complete before switching.
                    if let Some(prev) = self.current_module {
                        if self.modules[prev].status == ModuleStatus::Running {
                            self.apply_status(prev, ModuleStatus::Complete);
                        }
                    }
                    self.current_module = Some(idx);
                    self.apply_status(idx, ModuleStatus::Running);
                }

                // Attribute the line to whatever module is currently running.
                if let Some(idx) = self.current_module {
                    if !self.modules[idx].log_text.is_empty() {
                        self.modules[idx].log_text.push('\n');
                    }
                    self.modules[idx].log_text.push_str(&line);
                    let text = self.modules[idx].log_text.clone();
                    self.modules[idx].log_label.set_text(&text);
                }

                // Detect failure signals in the log.
                if line.contains("level=ERROR") || line.contains("module_fail") {
                    if let Some(idx) = self.current_module {
                        self.apply_status(idx, ModuleStatus::Failed);
                    }
                }
            }

            UpdateListInput::SetNerdMode(enabled) => {
                self.nerd_mode = enabled;
                for entry in &self.modules {
                    entry.row.set_enable_expansion(enabled);
                    entry.row.set_expanded(enabled);
                }
            }

            UpdateListInput::MarkAllComplete => {
                for i in 0..self.modules.len() {
                    if matches!(
                        self.modules[i].status,
                        ModuleStatus::Running | ModuleStatus::Pending
                    ) {
                        self.apply_status(i, ModuleStatus::Complete);
                    }
                }
                self.current_module = None;
            }

            UpdateListInput::MarkCurrentFailed => {
                if let Some(idx) = self.current_module {
                    if self.modules[idx].status == ModuleStatus::Running {
                        self.apply_status(idx, ModuleStatus::Failed);
                    }
                }
            }
        }
    }
}

impl UpdateList {
    /// Apply a status transition to one module entry, updating all visual indicators.
    fn apply_status(&mut self, idx: usize, status: ModuleStatus) {
        let entry = &mut self.modules[idx];

        let stack_page = match &status {
            ModuleStatus::Pending => "pending",
            ModuleStatus::Running => "running",
            ModuleStatus::Complete => "complete",
            ModuleStatus::Failed => "failed",
        };
        let subtitle = match &status {
            ModuleStatus::Pending => entry.description,
            ModuleStatus::Running => "Updating…",
            ModuleStatus::Complete => "Complete",
            ModuleStatus::Failed => "Failed",
        };

        entry.status_stack.set_visible_child_name(stack_page);
        entry.row.set_subtitle(subtitle);

        if status == ModuleStatus::Running {
            entry.spinner.start();
        } else {
            entry.spinner.stop();
        }

        entry.status = status;
    }
}

/// Parse a log line from uupd and return the index into MODULES if it announces
/// a module starting. uupd emits `module_name=System` etc. when a module begins.
fn detect_module_start(line: &str) -> Option<usize> {
    // Match against the capitalized display names uupd uses in its structured log.
    const PATTERNS: &[(&str, &str)] = &[
        ("System", "system"),
        ("Flatpak", "flatpak"),
        ("Brew", "brew"),
        ("Distrobox", "distrobox"),
    ];

    for &(display, key) in PATTERNS {
        let needle = format!("module_name={}", display);
        if line.contains(needle.as_str()) {
            return MODULES.iter().position(|&(k, _, _)| k == key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_module_start ─────────────────────────────────────────────
    // Parses uupd's structured log lines to figure out which module just
    // started — drives the per-module status indicators in the update list.
    // If uupd renames a module these tests fail loudly.

    #[test]
    fn detect_module_start_recognises_system() {
        let idx = detect_module_start("time=2026-05-30 module_name=System message=starting")
            .expect("System matched");
        assert_eq!(MODULES[idx].0, "system");
    }

    #[test]
    fn detect_module_start_recognises_flatpak() {
        let idx = detect_module_start("module_name=Flatpak module_state=running").expect("matched");
        assert_eq!(MODULES[idx].0, "flatpak");
    }

    #[test]
    fn detect_module_start_recognises_brew() {
        let idx = detect_module_start("module_name=Brew").expect("matched");
        assert_eq!(MODULES[idx].0, "brew");
    }

    #[test]
    fn detect_module_start_recognises_distrobox() {
        let idx = detect_module_start("module_name=Distrobox").expect("matched");
        assert_eq!(MODULES[idx].0, "distrobox");
    }

    #[test]
    fn detect_module_start_ignores_non_module_lines() {
        assert!(detect_module_start("time=2026-05-30 level=INFO message=ready").is_none());
    }

    #[test]
    fn detect_module_start_rejects_lowercase_module_name() {
        // uupd emits the display-name capitalised; if it ever switches to
        // lowercase the matcher needs to be updated — this test pins the
        // current contract.
        assert!(detect_module_start("module_name=system").is_none());
    }

    #[test]
    fn detect_module_start_rejects_unknown_module() {
        assert!(detect_module_start("module_name=Mystery").is_none());
    }
}
