//! Top-level application component.
//!
//! ## relm4 Design Rationale
//!
//! This module demonstrates the **canonical relm4 component pattern** for Bluefin apps:
//!
//! 1. **Single top-level component** owns the `AdwApplicationWindow` and the state machine.
//!    It is the sole orchestrator — child components communicate UP via `Output` messages
//!    and receive commands DOWN via `emit()` on their controller handle.
//!
//! 2. **Message-driven state** — all state transitions happen through `AppMsg` variants
//!    processed in a single `update()` method. This makes state transitions explicit,
//!    traceable (via `tracing`), and impossible to miss. No widget callbacks mutate
//!    state directly.
//!
//! 3. **Forward pattern** — child component outputs are mapped to parent inputs via
//!    `.forward(sender, |output| match output { ... })`. This decouples children from
//!    the parent's message type.
//!
//! 4. **Action groups** — menu items and keyboard shortcuts use relm4's action system
//!    (`new_action_group!`, `new_stateless_action!`) rather than raw GAction. This keeps
//!    type safety and connects naturally to the message bus.
//!
//! 5. **Separate async thread** — long-running work (subprocess) runs on a tokio runtime
//!    in `std::thread::spawn`. Results flow back via `sender.emit()` which is thread-safe
//!    and queues messages on the GLib main loop.
//!
//! ## State machine
//!
//!   Idle → Updating → (Complete | Error) → Idle
//!
//! ## Component hierarchy
//!
//!   App (this)
//!   └── StatusView (content area, owns LogView)
//!       └── LogView (scrollable text output)
//!
//! ## Why SimpleComponent (not Component)?
//!
//! `SimpleComponent` is sufficient because:
//! - We don't need `CommandOutput` (we use manual thread + channel instead for streaming)
//! - We don't produce output messages (top-level component has no parent)
//! - The simpler trait reduces boilerplate
//!
//! Use full `Component` with `CommandOutput` when you need a single async result
//! (not streaming). Use `AsyncComponent` when the init itself is async.

use adw::prelude::*;
use relm4::actions::{AccelsPlus, RelmAction, RelmActionGroup};
use relm4::prelude::*;

use crate::config;
use crate::dbus_progress::ProgressDBus;
use crate::settings::Settings;
use crate::ui::preferences::show_preferences;
use crate::ui::rebase_dialog::show_rebase_dialog;
use crate::ui::status_view::{StatusView, StatusViewInput, StatusViewOutput};
use crate::ui::update_check_dialog::{CheckResult, show_update_check_dialog};
use crate::update_worker::{SimulationScenario, UpdateEvent, UpdateWorker, run_simulated};

/// Application-level state.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum AppState {
    /// No update in progress; ready to start one.
    #[default]
    Idle,
    /// Update is actively running.
    Updating,
    /// Update completed successfully.
    Complete,
    /// uupd exited with code 77 — system is already current, nothing to do.
    UpToDate,
    /// Update failed with an error message.
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreflightStatus {
    Checking,
    UpdateAvailable,
    UpToDate,
    Unknown,
}

/// Top-level model.
pub struct App {
    state: AppState,
    preflight_status: PreflightStatus,
    /// Selected developer-mode simulation scenario.
    sim_scenario: SimulationScenario,
    /// Accumulated output lines from the uupd process.
    log_lines: Vec<String>,
    /// Toast overlay reference for showing transient notifications.
    toast_overlay: adw::ToastOverlay,
    /// Child component: the main status/content view.
    status_view: Controller<StatusView>,
    /// Handle to cancel a running update (sends kill signal to subprocess).
    cancel_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Reference to header bar for dynamic subtitle updates.
    header_bar: adw::HeaderBar,
    /// Title widget in the header bar — title shows the current page name,
    /// subtitle shows the app name. Same idiom as gnome-control-center: the
    /// headerbar title swaps as the user navigates between panels.
    window_title: adw::WindowTitle,
    /// Back button in header bar
    back_btn: gtk::Button,
    /// Currently visible subpage ("main", "history", etc.)
    current_page: String,
    /// Banner shown when developer mode is active.
    dev_banner: adw::Banner,
    /// Persistent user preferences.
    settings: Settings,
    /// D-Bus progress publisher for the GNOME Shell panel extension.
    progress_dbus: ProgressDBus,
}

/// Messages the App component can receive.
#[derive(Debug)]
pub enum AppMsg {
    /// User clicked "Update" — optionally bypass the metered-network confirmation.
    StartUpdate { skip_metered_check: bool },
    /// User clicked "Check" on the main view — open the check dialog.
    OpenCheckDialog,
    /// The check dialog completed with results.
    CheckComplete(CheckResult),
    /// User clicked "Install all" in the check dialog.
    InstallFromCheck,
    /// A line of output arrived from the subprocess.
    OutputLine(String),
    /// A module has started running.
    ModuleStarted(crate::orchestrator::Module),
    /// A module has finished.
    ModuleFinished(
        crate::orchestrator::Module,
        crate::orchestrator::ModuleStatus,
    ),
    /// The subprocess exited successfully.
    UpdateComplete,
    /// The subprocess reported that the system is already up to date (exit 77).
    UpdateUpToDate,
    /// The subprocess failed.
    UpdateFailed(String),
    /// User wants to cancel the running update.
    CancelUpdate,
    /// User wants to reboot the system.
    RequestReboot,
    /// User confirmed reboot in the dialog.
    ConfirmReboot,
    /// User requested the Rebase History dialog.
    ShowRebaseDialog,
    /// User requested the About dialog.
    ShowAbout,
    /// User requested the Preferences dialog.
    ShowPreferences,
    /// Settings were updated in the preferences dialog.
    SettingsChanged(Settings),
    /// Result of the startup preflight update check.
    PreflightResult(PreflightStatus),
    /// Developer mode toggle from the hamburger menu.
    ToggleDevMode(bool),
    /// Update the selected developer-mode simulation scenario.
    SetSimScenario(SimulationScenario),
    /// Quit the application.
    Quit,
    /// Window close was requested — check if we should allow it.
    CloseRequest,
    /// Navigate between pages
    PageChanged(String),
    /// Back button clicked
    GoBack,
    /// Show "What's new" / changelog for the latest available version.
    /// Wired to Ctrl+W so the action is reachable from the GUI test suite —
    /// the corresponding banner button is not exposed in the AT-SPI tree
    /// because libadwaita ActionRow doesn't enumerate suffix children.
    ShowWhatsNew,
    /// Dismiss the staged-reboot banner. Wired to Ctrl+Backspace for the
    /// same reason as ShowWhatsNew.
    DismissBanner,
    /// Open the powerwash confirmation dialog (menu / Advanced dialog).
    TriggerPowerwash,
    /// Open the factory-reset confirmation dialog (menu / Advanced dialog).
    TriggerFactoryReset,
    /// Navigate the StatusView stack to a specific subpage ("source",
    /// "history", "main"). Used by the Advanced dialog's action rows so a
    /// click on "Image Source" pushes the user to that subpage from
    /// wherever they were.
    ShowStatusPage(String),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Finupdate"),
            set_default_size: (750, 700),
            set_width_request: 400,
            set_height_request: 500,

            adw::ToolbarView {
                add_top_bar = &model.header_bar.clone() -> adw::HeaderBar {
                    set_title_widget: Some(&model.window_title.clone()),
                    pack_start = &model.back_btn.clone() -> gtk::Button {
                        connect_clicked[sender] => move |_| {
                            sender.input(AppMsg::GoBack);
                        }
                    },
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some("Main Menu"),
                        set_menu_model: Some(&main_menu),
                    },
                },

                add_top_bar = &model.dev_banner.clone() -> adw::Banner {
                    set_title: "Developer Mode — updates are simulated",
                    set_revealed: model.settings.dev_mode,
                },

                #[wrap(Some)]
                set_content = &model.toast_overlay.clone() -> adw::ToastOverlay {
                    set_child: Some(model.status_view.widget()),
                },
            }
        }
    }

    menu! {
        // Hamburger menu — kept minimal to match gnome-control-center which
        // only exposes app-level items here (Keyboard Shortcuts / About /
        // Quit). Panel-specific stuff (Image Source, Image History, Rebase,
        // Powerwash, Factory Reset, settings) lives in the Advanced dialog
        // reached from the "Advanced…" row on the main page.
        main_menu: {
            section! {
                "_Keyboard Shortcuts" => ShortcutsAction,
                "_About Finupdate" => AboutAction,
            },
            section! {
                "_Quit" => QuitAction,
            }
        }
    }

    /// Initialize the component tree.
    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        // Build child component: StatusView receives state updates and emits user actions.
        let status_view =
            StatusView::builder()
                .launch(AppState::Idle)
                .forward(sender.input_sender(), |output| match output {
                    StatusViewOutput::StartUpdate => AppMsg::StartUpdate {
                        skip_metered_check: false,
                    },
                    StatusViewOutput::CancelUpdate => AppMsg::CancelUpdate,
                    StatusViewOutput::Reboot => AppMsg::RequestReboot,
                    StatusViewOutput::ShowRebase => AppMsg::ShowRebaseDialog,
                    StatusViewOutput::OpenCheckDialog => AppMsg::OpenCheckDialog,
                    StatusViewOutput::PageChanged(page) => AppMsg::PageChanged(page),
                    StatusViewOutput::OpenAdvanced => AppMsg::ShowPreferences,
                });

        let toast_overlay = adw::ToastOverlay::new();
        let header_bar = adw::HeaderBar::new();
        // Headerbar title widget — title is the current page name, subtitle is
        // the app name. Same idiom as gnome-control-center About: header shows
        // "About" with no subtitle (because there's only the panel name worth
        // showing). Our root page sets title="Finupdate" with no subtitle for
        // the same effect; subpages set title=<page name> subtitle="Finupdate".
        let window_title = adw::WindowTitle::new("Finupdate", "");
        let dev_banner = adw::Banner::new("Developer Mode — updates are simulated");

        let settings = Settings::load();

        inject_app_css();

        let back_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .visible(false)
            .build();

        // Initial scenario from settings.json (set by behave smoke tests via
        // _write_settings(sim_scenario="...") or by the --sim CLI flag).
        // Default Success when unset or unparseable.
        let initial_sim_scenario = match settings.sim_scenario.as_deref() {
            Some("Failure") | Some("failure") => SimulationScenario::Failure,
            Some("AlreadyUpToDate") | Some("up-to-date") | Some("uptodate") => {
                SimulationScenario::AlreadyUpToDate
            }
            _ => SimulationScenario::Success,
        };
        let model = App {
            state: AppState::Idle,
            preflight_status: PreflightStatus::Checking,
            sim_scenario: initial_sim_scenario,
            log_lines: Vec::new(),
            toast_overlay: toast_overlay.clone(),
            status_view,
            cancel_tx: None,
            header_bar,
            window_title,
            back_btn,
            current_page: "main".to_string(),
            dev_banner,
            settings,
            progress_dbus: ProgressDBus::new(),
        };

        let widgets = view_output!();

        // ─── Actions ────────────────────────────────────────────────────
        let about_action: RelmAction<AboutAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowAbout);
            })
        };

        let preferences_action: RelmAction<PreferencesAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowPreferences);
            })
        };

        let quit_action: RelmAction<QuitAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::Quit);
            })
        };

        let shortcuts_action: RelmAction<ShortcutsAction> = {
            let root_clone = root.clone();
            RelmAction::new_stateless(move |_| {
                show_shortcuts_window(&root_clone);
            })
        };

        let rebase_action: RelmAction<RebaseAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::ShowRebaseDialog);
            })
        };

        // Source / History page navigation no longer needs dedicated
        // actions — clicks happen via the Advanced dialog's System group
        // rows, which dispatch AppMsg::ShowStatusPage directly through the
        // app input sender.

        let mut group = RelmActionGroup::<WindowActionGroup>::new();
        group.add_action(about_action);
        group.add_action(preferences_action);
        group.add_action(quit_action);
        group.add_action(shortcuts_action);
        group.add_action(rebase_action);

        let install_action: RelmAction<InstallAction> = {
            let sender = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| {
                sender.emit(AppMsg::InstallFromCheck);
            })
        };
        group.add_action(install_action);

        // Banner & destructive action accelerators — see action declarations
        // at the bottom of the file for context.
        let whats_new_action: RelmAction<WhatsNewAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(AppMsg::ShowWhatsNew))
        };
        group.add_action(whats_new_action);

        let restart_action: RelmAction<RestartAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(AppMsg::RequestReboot))
        };
        group.add_action(restart_action);

        let dismiss_action: RelmAction<DismissBannerAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(AppMsg::DismissBanner))
        };
        group.add_action(dismiss_action);

        let powerwash_action: RelmAction<PowerwashAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(AppMsg::TriggerPowerwash))
        };
        group.add_action(powerwash_action);

        let factory_reset_action: RelmAction<FactoryResetAction> = {
            let s = sender.input_sender().clone();
            RelmAction::new_stateless(move |_| s.emit(AppMsg::TriggerFactoryReset))
        };
        group.add_action(factory_reset_action);

        group.register_for_widget(&root);

        // ─── Keyboard Shortcuts ─────────────────────────────────────────
        let app = relm4::main_application();
        app.set_accelerators_for_action::<QuitAction>(&["<primary>q"]);
        app.set_accelerators_for_action::<ShortcutsAction>(&["<primary>question"]);
        app.set_accelerators_for_action::<PreferencesAction>(&["<primary>comma"]);
        app.set_accelerators_for_action::<InstallAction>(&["<primary>i"]);
        // Ctrl+Shift+R opens the Rebase / Version History dialog. Used by the
        // GUI test suite to drive rollback flows without menu navigation.
        app.set_accelerators_for_action::<RebaseAction>(&["<primary><shift>r"]);
        // Banner buttons — see action declarations for why these need
        // accelerators (libadwaita ActionRow suffix children aren't enumerable
        // via AT-SPI, so the GUI test suite can't click them directly).
        app.set_accelerators_for_action::<WhatsNewAction>(&["<primary>w"]);
        app.set_accelerators_for_action::<RestartAction>(&["<primary><shift>b"]);
        app.set_accelerators_for_action::<DismissBannerAction>(&["<primary>BackSpace"]);
        // Powerwash and Factory Reset deliberately have NO accelerator —
        // destructive actions shouldn't have keyboard shortcuts a user could
        // hit by mistake. They live in the hamburger menu and require an
        // intentional click; the in-dialog confirmation gates the actual
        // destructive call.

        // ─── Close Request Handler ──────────────────────────────────────
        // Intercept window close to warn if an update is in progress.
        let close_sender = sender.input_sender().clone();
        root.connect_close_request(move |_| {
            close_sender.emit(AppMsg::CloseRequest);
            // Inhibit default close — we handle it in update().
            gtk::glib::Propagation::Stop
        });

        // When mock_identity is set, skip the real preflight subprocess entirely
        // and pretend an update is available so the banner-buttons surface (Install,
        // What's new, Discard) are reachable in tests. The downstream banner state
        // machine already gates real actions behind dry_run / dev_mode.
        if model.settings.mock_identity.is_some() {
            let input_sender = sender.input_sender().clone();
            gtk::glib::idle_add_local_once(move || {
                input_sender.emit(AppMsg::PreflightResult(PreflightStatus::UpdateAvailable));
            });
        } else {
            // Defer preflight check until the GLib main loop is running to avoid
            // racing with component initialization (the thread could finish before
            // the relm4 message loop processes the first idle).
            let input_sender = sender.input_sender().clone();
            gtk::glib::idle_add_local_once(move || {
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");
                    rt.block_on(async move {
                        // Use `pkexec bootc upgrade --check` to detect pending updates.
                        // Exit 0 = update available, 77 = up to date, other = unknown.
                        let mut cmd = if std::path::Path::new("/.flatpak-info").exists() {
                            let mut c = tokio::process::Command::new("flatpak-spawn");
                            c.args(["--host", "pkexec", "bootc", "upgrade", "--check"]);
                            c
                        } else {
                            let mut c = tokio::process::Command::new("pkexec");
                            c.args(["bootc", "upgrade", "--check"]);
                            c
                        };
                        // Add a 15-second timeout to prevent hanging on slow/unavailable systems.
                        let timeout = std::time::Duration::from_secs(15);
                        let status = tokio::select! {
                            result = cmd.status() => {
                                match result {
                                    Ok(s) => Some(s),
                                    Err(_) => None,
                                }
                            }
                            _ = tokio::time::sleep(timeout) => None,
                        };

                        let result = match status {
                            Some(s) => match s.code() {
                                Some(0) => PreflightStatus::UpdateAvailable,
                                Some(77) => PreflightStatus::UpToDate,
                                _ => PreflightStatus::Unknown,
                            },
                            None => PreflightStatus::Unknown,
                        };
                        input_sender.emit(AppMsg::PreflightResult(result));
                    });
                });
            });
        } // end if/else for mock_identity preflight short-circuit

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::StartUpdate { skip_metered_check } => {
                // Prevent double-starts.
                if self.state == AppState::Updating {
                    return;
                }

                if !skip_metered_check
                    && self.settings.pause_on_metered
                    && gtk::gio::NetworkMonitor::default().is_network_metered()
                {
                    let dialog = adw::AlertDialog::new(
                        Some("Metered Connection Detected"),
                        Some(
                            "You're on a limited or cellular connection. Automatic updates are paused, but you can continue manually.",
                        ),
                    );
                    dialog.add_response("cancel", "_Cancel");
                    dialog.add_response("proceed", "_Update Anyway");
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    let update_sender = sender.input_sender().clone();
                    dialog.connect_response(None, move |_, response| {
                        if response == "proceed" {
                            update_sender.emit(AppMsg::StartUpdate {
                                skip_metered_check: true,
                            });
                        }
                    });

                    if let Some(root) = self.status_view.widget().root() {
                        if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                            dialog.present(Some(window));
                        }
                    }

                    return;
                }

                tracing::info!("Starting system update via uupd");
                self.state = AppState::Updating;
                self.log_lines.clear();
                self.progress_dbus
                    .update("updating", 0.0, "Starting update…");

                // Update header subtitle to indicate activity.
                self.update_subtitle();

                // Forward state to the child view.
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Updating));
                self.status_view.emit(StatusViewInput::ClearLog);

                // Create a cancellation channel for this update run.
                let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
                self.cancel_tx = Some(cancel_tx);

                // Spawn the async update worker on a background thread.
                // cancel_rx is passed INTO the worker so it can kill the real process.
                let input_sender = sender.input_sender().clone();
                // `dry_run` routes the orchestrator down the same simulated path
                // as `dev_mode`, so `pkexec finupdate-runner` is never spawned.
                let dev_mode = self.settings.dev_mode || self.settings.dry_run;
                let sim_scenario = self.sim_scenario;
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("Failed to create tokio runtime");

                    rt.block_on(async move {
                        let mut rx = if dev_mode {
                            tracing::info!(
                                ?sim_scenario,
                                "Developer mode active — running simulated update"
                            );
                            println!(
                                "[debug] app: developer mode update run start, scenario={:?}",
                                sim_scenario
                            );
                            run_simulated(sim_scenario, cancel_rx).await
                        } else {
                            println!("[debug] app: starting real update worker");
                            UpdateWorker::new().run(cancel_rx).await
                        };

                        while let Some(event) = rx.recv().await {
                            match event {
                                UpdateEvent::Output(line) => {
                                    input_sender.emit(AppMsg::OutputLine(line));
                                }
                                UpdateEvent::ModuleStarted(module) => {
                                    input_sender.emit(AppMsg::ModuleStarted(module));
                                }
                                UpdateEvent::ModuleFinished(module, status) => {
                                    input_sender.emit(AppMsg::ModuleFinished(module, status));
                                }
                                UpdateEvent::Complete => {
                                    input_sender.emit(AppMsg::UpdateComplete);
                                    break;
                                }
                                UpdateEvent::UpToDate => {
                                    input_sender.emit(AppMsg::UpdateUpToDate);
                                    break;
                                }
                                UpdateEvent::Error(err) => {
                                    input_sender.emit(AppMsg::UpdateFailed(err));
                                    break;
                                }
                            }
                        }
                    });
                });
            }

            AppMsg::OpenCheckDialog => {
                self.progress_dbus
                    .update("checking", 0.0, "Checking for updates…");
                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        let result_sender = sender.input_sender().clone();
                        let install_sender = sender.input_sender().clone();
                        show_update_check_dialog(
                            window,
                            self.settings.dev_mode || self.settings.dry_run,
                            self.sim_scenario,
                            move |result| {
                                result_sender.emit(AppMsg::CheckComplete(result));
                            },
                            move || {
                                install_sender.emit(AppMsg::InstallFromCheck);
                            },
                        );
                    }
                }
            }

            AppMsg::CheckComplete(result) => {
                tracing::info!(
                    system_update = result.system_update,
                    sources = result.sources_with_updates,
                    "Update check completed"
                );
                if result.sources_with_updates > 0 {
                    self.preflight_status = PreflightStatus::UpdateAvailable;
                    self.progress_dbus.update("idle", 0.0, "Updates available");
                } else {
                    self.preflight_status = PreflightStatus::UpToDate;
                    self.progress_dbus.reset();
                }
                self.status_view.emit(StatusViewInput::PreflightResult(
                    self.preflight_status.clone(),
                ));
            }

            AppMsg::InstallFromCheck => {
                // Trigger a full update (same as StartUpdate but skip metered check since
                // user explicitly chose to install from the dialog).
                sender.input(AppMsg::StartUpdate {
                    skip_metered_check: true,
                });
            }

            AppMsg::OutputLine(line) => {
                self.log_lines.push(line.clone());
                self.status_view.emit(StatusViewInput::AppendLog(line));
            }

            AppMsg::ModuleStarted(module) => {
                let key = module.key();
                tracing::debug!("Module started: {}", key);
                let module_count = match module {
                    crate::orchestrator::Module::System => 0,
                    crate::orchestrator::Module::Flatpak => 1,
                    crate::orchestrator::Module::Brew => 2,
                    crate::orchestrator::Module::Distrobox => 3,
                };
                let progress = (module_count as f64 / 4.0).min(0.95);
                self.progress_dbus.set_progress(progress);
                self.progress_dbus
                    .set_message(&format!("Updating {}…", key));
                self.status_view
                    .emit(StatusViewInput::ModuleStarted(module));
            }

            AppMsg::ModuleFinished(module, status) => {
                tracing::debug!("Module finished: {} {:?}", module.key(), status);
                self.status_view
                    .emit(StatusViewInput::ModuleFinished(module, status));
            }

            AppMsg::UpdateComplete => {
                tracing::info!("System update completed successfully");
                self.state = AppState::Complete;
                self.cancel_tx = None;
                self.progress_dbus
                    .update("complete", 1.0, "Update complete");
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Complete));

                // HIG: Use AdwToast for transient success notifications.
                let toast = adw::Toast::new("System update complete — restart to apply");
                toast.set_timeout(0); // Persistent until dismissed
                toast.set_button_label(Some("Dismiss"));
                self.toast_overlay.add_toast(toast);

                // Send a desktop notification if window is not focused.
                send_notification(
                    "update-complete",
                    "System Update Complete",
                    "Your system has been updated. Restart to apply changes.",
                );
            }

            AppMsg::UpdateUpToDate => {
                tracing::info!("System is already up to date (uupd exit 77)");
                self.state = AppState::UpToDate;
                self.cancel_tx = None;
                self.progress_dbus.update("complete", 1.0, "Up to date");
                self.update_subtitle();
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::UpToDate));
            }

            AppMsg::UpdateFailed(err) => {
                tracing::error!("System update failed: {}", err);
                self.state = AppState::Error(err.clone());
                self.cancel_tx = None;
                self.progress_dbus.update("error", 0.0, &err);
                self.update_subtitle();

                // Notify the user if window is backgrounded.
                send_notification("update-failed", "System Update Failed", &err);
                self.status_view
                    .emit(StatusViewInput::StateChanged(AppState::Error(err)));
            }

            AppMsg::CancelUpdate => {
                if let Some(tx) = self.cancel_tx.take() {
                    tracing::info!("User requested update cancellation");
                    let _ = tx.send(());
                }
            }

            AppMsg::RequestReboot => {
                // HIG: Destructive actions require confirmation.
                let dialog = adw::AlertDialog::builder()
                    .heading("Restart System?")
                    .body("The system will restart to apply updates. Save any open work before continuing.")
                    .build();

                dialog.add_response("cancel", "_Cancel");
                dialog.add_response("restart", "_Restart");
                dialog.set_response_appearance("restart", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let reboot_sender = sender.input_sender().clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "restart" {
                        reboot_sender.emit(AppMsg::ConfirmReboot);
                    }
                });

                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        dialog.present(Some(window));
                    }
                }
            }

            AppMsg::ConfirmReboot => {
                if self.settings.dev_mode || self.settings.dry_run {
                    let reason = if self.settings.dry_run && !self.settings.dev_mode {
                        "dry-run"
                    } else {
                        "developer mode"
                    };
                    tracing::warn!(
                        "Reboot suppressed — {} is active. \
                         Would have called `systemctl reboot`.",
                        reason
                    );
                    let toast = adw::Toast::new(&format!("Reboot suppressed ({})", reason));
                    toast.set_timeout(3);
                    self.toast_overlay.add_toast(toast);
                } else {
                    tracing::info!("User confirmed system reboot");
                    std::thread::spawn(|| {
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("Failed to create tokio runtime");

                        rt.block_on(async {
                            let result = if crate::update_worker::is_flatpak() {
                                tokio::process::Command::new("flatpak-spawn")
                                    .args(["--host", "systemctl", "reboot"])
                                    .status()
                                    .await
                            } else {
                                tokio::process::Command::new("systemctl")
                                    .arg("reboot")
                                    .status()
                                    .await
                            };

                            if let Err(e) = result {
                                tracing::error!("Failed to initiate reboot: {}", e);
                            }
                        });
                    });
                }
            }

            AppMsg::ShowRebaseDialog => {
                let window_opt = self
                    .status_view
                    .widget()
                    .root()
                    .and_then(|r| r.downcast::<adw::ApplicationWindow>().ok())
                    .or_else(|| {
                        // Fallback: ask the GtkApplication for its active window.
                        // status_view.widget().root() returns None when the
                        // accelerator fires before the StatusView's gtk::Stack
                        // has been parented into the toplevel.
                        relm4::main_application()
                            .active_window()
                            .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
                    });
                match window_opt {
                    Some(window) => {
                        let suppress_real = self.settings.dev_mode || self.settings.dry_run;
                        show_rebase_dialog(&window, suppress_real);
                    }
                    None => {
                        tracing::warn!(
                            "ShowRebaseDialog: no ApplicationWindow found via either status_view root or main_application active_window"
                        );
                    }
                }
            }

            AppMsg::ShowAbout => {
                let about = adw::AboutDialog::builder()
                    .application_name("Finupdate")
                    .application_icon(config::APP_ID)
                    .developer_name("Project Bluefin")
                    .version(config::VERSION)
                    .website("https://projectbluefin.io")
                    .issue_url("https://github.com/castrojo/finupdate/issues")
                    .license_type(gtk::License::MitX11)
                    .developers(vec!["Project Bluefin Contributors"])
                    .comments("A friendly system update frontend for Bluefin")
                    .build();

                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        about.present(Some(window));
                    }
                }
            }

            AppMsg::ShowPreferences => {
                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        let s1 = sender.input_sender().clone();
                        let s2 = sender.input_sender().clone();
                        show_preferences(window, self.settings.clone(), s1, move |updated| {
                            s2.emit(AppMsg::SettingsChanged(updated));
                        });
                    }
                }
            }

            AppMsg::SettingsChanged(new_settings) => {
                tracing::debug!("Settings updated: dev_mode={}", new_settings.dev_mode);
                self.settings = new_settings.clone();
                self.dev_banner.set_revealed(self.settings.dev_mode);
                // Forward to StatusView so the front-page Auto Updates
                // SwitchRow stays in sync with the Advanced dialog (and any
                // other future settings-aware widgets on the main page).
                self.status_view
                    .emit(StatusViewInput::SettingsChanged(new_settings));
            }

            AppMsg::PreflightResult(status) => {
                self.preflight_status = status.clone();
                self.status_view
                    .emit(StatusViewInput::PreflightResult(status));
            }

            AppMsg::PageChanged(page) => {
                self.current_page = page.clone();
                let page_label = match page.as_str() {
                    "main" => "Finupdate",
                    "history" => "Version History",
                    "source" => "Image Source",
                    "changelog" => "What’s New",
                    _ => "Finupdate",
                };
                // Drive the headerbar's WindowTitle: page name on top, app
                // name as subtitle on subpages. Same shape as control-center
                // panels — title swaps as you navigate.
                self.window_title.set_title(page_label);
                self.window_title
                    .set_subtitle(if page == "main" { "" } else { "Finupdate" });
                // Window title (used by GNOME Shell's task list) keeps the
                // expanded form so the user can identify the open page.
                if let Some(root) = self.status_view.widget().root() {
                    if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                        let win_title = if page == "main" {
                            "Finupdate".to_string()
                        } else {
                            format!("Finupdate — {}", page_label)
                        };
                        window.set_title(Some(&win_title));
                    }
                }
                self.back_btn.set_visible(page != "main");
            }

            AppMsg::GoBack => {
                self.status_view
                    .emit(StatusViewInput::ShowPage("main".to_string()));
            }

            AppMsg::ShowWhatsNew => {
                // Forward to status_view as a SelectChangelogVersion with the
                // currently-displayed selected tag. status_view re-renders the
                // changelog page and switches the stack to it.
                self.status_view
                    .emit(StatusViewInput::SelectChangelogVersion(String::new()));
            }

            AppMsg::DismissBanner => {
                self.status_view.emit(StatusViewInput::DismissBanner);
            }

            AppMsg::TriggerPowerwash => {
                self.status_view
                    .emit(StatusViewInput::TogglePin("powerwash".to_string()));
            }

            AppMsg::TriggerFactoryReset => {
                self.status_view
                    .emit(StatusViewInput::TogglePin("factory".to_string()));
            }

            AppMsg::ShowStatusPage(page) => {
                self.status_view.emit(StatusViewInput::ShowPage(page));
            }

            AppMsg::ToggleDevMode(enabled) => {
                tracing::info!("Developer mode toggled via menu: {}", enabled);
                self.settings.dev_mode = enabled;
                self.settings.save();
                self.dev_banner.set_revealed(enabled);
            }

            AppMsg::SetSimScenario(scenario) => {
                tracing::info!(?scenario, "Selected developer simulation scenario");
                self.sim_scenario = scenario;
            }

            AppMsg::Quit => {
                // If update in progress, treat like close request (ask first).
                if self.state == AppState::Updating {
                    sender.input(AppMsg::CloseRequest);
                } else {
                    relm4::main_application().quit();
                }
            }

            AppMsg::CloseRequest => {
                if self.state == AppState::Updating {
                    // Warn user that an update is running.
                    let dialog = adw::AlertDialog::builder()
                        .heading("Update in Progress")
                        .body("An update is currently running. Closing now may leave your system in an inconsistent state.")
                        .build();

                    dialog.add_response("cancel", "_Keep Waiting");
                    dialog.add_response("close", "_Close Anyway");
                    dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    dialog.connect_response(None, move |_, response| {
                        if response == "close" {
                            relm4::main_application().quit();
                        }
                    });

                    if let Some(root) = self.status_view.widget().root() {
                        if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                            dialog.present(Some(window));
                        }
                    }
                } else {
                    // Not updating — close immediately.
                    relm4::main_application().quit();
                }
            }
        }
    }
}

impl App {
    /// Update the header bar subtitle to reflect current state.
    fn update_subtitle(&self) {
        let subtitle = match &self.state {
            AppState::Idle => None,
            AppState::Updating => Some("Updating…"),
            AppState::Complete => Some("Update complete"),
            AppState::UpToDate => Some("Already up to date"),
            AppState::Error(_) => Some("Update failed"),
        };
        // AdwHeaderBar doesn't have subtitle — set window title instead.
        if let Some(root) = self.status_view.widget().root() {
            if let Some(window) = root.downcast_ref::<adw::ApplicationWindow>() {
                let title = match subtitle {
                    Some(s) => format!("System Update — {}", s),
                    None => "System Update".to_string(),
                };
                window.set_title(Some(&title));
            }
        }
    }
}

/// Show the keyboard shortcuts window.
fn show_shortcuts_window(window: &adw::ApplicationWindow) {
    // Must list every accelerator wired in init() above — this dialog is the
    // only place users can discover the bindings. The GUI test suite relies
    // on `Ctrl+Shift+R` (rollback scenarios) and `Ctrl+Alt+P` /
    // `Ctrl+Alt+F` (destructive flows); they're easy to miss without this
    // overview. Group titles match GNOME HIG: "Application", "Updates",
    // "System".
    let shortcuts = gtk::ShortcutsWindow::builder()
        .transient_for(window)
        .modal(true)
        .build();

    let section = gtk::ShortcutsSection::builder()
        .section_name("shortcuts")
        .visible(true)
        .build();

    let app_group = gtk::ShortcutsGroup::builder().title("Application").build();
    for (title, accel) in [
        ("Preferences", "<Primary>comma"),
        ("Keyboard Shortcuts", "<Primary>question"),
        ("Quit", "<Primary>q"),
    ] {
        app_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&app_group);

    let updates_group = gtk::ShortcutsGroup::builder().title("Updates").build();
    for (title, accel) in [
        ("Install staged update", "<Primary>i"),
        ("What's new in this update", "<Primary>w"),
        ("Restart to apply", "<Primary><Shift>b"),
        ("Dismiss update banner", "<Primary>BackSpace"),
    ] {
        updates_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&updates_group);

    let system_group = gtk::ShortcutsGroup::builder().title("System").build();
    for (title, accel) in [("Open Rebase / Version History", "<Primary><Shift>r")] {
        system_group.add_shortcut(
            &gtk::ShortcutsShortcut::builder()
                .title(title)
                .accelerator(accel)
                .build(),
        );
    }
    section.add_group(&system_group);
    // Note: Powerwash and Factory Reset are intentionally absent here —
    // destructive actions don't get keyboard shortcuts. Hamburger menu only.

    shortcuts.add_section(&section);
    shortcuts.set_visible(true);
}

/// Send a desktop notification via GApplication.
/// Notifications appear in the system notification area if the app is backgrounded.
fn send_notification(id: &str, title: &str, body: &str) {
    let app = relm4::main_application();
    let notification = gtk::gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gtk::gio::ThemedIcon::new(
        "software-update-available-symbolic",
    ));
    app.send_notification(Some(id), &notification);
}

// Action group and actions for the window-level menu.
relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(PreferencesAction, WindowActionGroup, "preferences");
relm4::new_stateless_action!(QuitAction, WindowActionGroup, "quit");
relm4::new_stateless_action!(ShortcutsAction, WindowActionGroup, "show-shortcuts");
relm4::new_stateless_action!(RebaseAction, WindowActionGroup, "rebase-history");
relm4::new_stateless_action!(InstallAction, WindowActionGroup, "install");
// Banner button & dangerous-action accelerators — see AppMsg variant docs for
// why these exist (libadwaita ActionRow doesn't expose suffix buttons in the
// AT-SPI tree, so the GUI test suite can't click them directly).
relm4::new_stateless_action!(WhatsNewAction, WindowActionGroup, "whats-new");
relm4::new_stateless_action!(RestartAction, WindowActionGroup, "restart");
relm4::new_stateless_action!(DismissBannerAction, WindowActionGroup, "dismiss-banner");
relm4::new_stateless_action!(PowerwashAction, WindowActionGroup, "powerwash");
relm4::new_stateless_action!(FactoryResetAction, WindowActionGroup, "factory-reset");

fn inject_app_css() {
    // Minimal CSS layer. Status colours, banner icons, and hero icons now use
    // built-in Adwaita classes (.accent, .success, .warning, .error, .caption)
    // instead of bespoke pills / gradient boxes — matches gnome-control-center.
    //
    // What's left:
    //   - .destructive-title  — adw::ActionRow doesn't expose a "danger" style
    //                           for the title label; this just tints it red on
    //                           the Factory Reset row.
    //   - .deploy-indicator-* — small coloured dots in the deployment list.
    //                           Equivalent to control-center's connection-
    //                           strength dots; no built-in class for "current
    //                           deployment marker" specifically.
    let css = gtk::CssProvider::new();
    css.load_from_string(
        r#"
        .destructive-title label {
            color: @error_color;
        }
        .deploy-indicator-current {
            border: 2px solid @accent_color;
            background-color: @accent_color;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        .deploy-indicator-staged {
            border: 2px solid @accent_color;
            background-color: transparent;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        .deploy-indicator-archive {
            border: 2px solid @window_fg_color;
            opacity: 0.5;
            background-color: transparent;
            min-width: 14px;
            min-height: 14px;
            border-radius: 8px;
        }
        "#,
    );
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service;
    use std::sync::Once;

    static INIT: Once = Once::new();

    fn init_gtk() {
        INIT.call_once(|| {
            unsafe {
                std::env::set_var("GDK_BACKEND", "broadway");
                std::env::set_var(
                    "FINUPDATE_IMAGE",
                    "ghcr.io/projectbluefin/dakota:latest-20260527",
                );
            }
            gtk::init().expect("Failed to initialize headless GTK");
            adw::init().expect("Failed to initialize headless Adwaita");

            // Ensure process-wide global service is initialized.
            service::init(service::BootcUpdaterService::new());
        });
    }

    #[tokio::test]
    async fn test_app_ui_flow() {
        init_gtk();

        // Create a Relm4 app controller
        let controller = App::builder().launch(()).detach();

        // Assert initial state
        assert_eq!(controller.model().state, AppState::Idle);
        assert_eq!(controller.model().current_page, "main");

        // Send messages to toggle dev mode and sim scenario
        controller
            .sender()
            .send(AppMsg::ToggleDevMode(true))
            .unwrap();
        controller
            .sender()
            .send(AppMsg::SetSimScenario(SimulationScenario::Success))
            .unwrap();

        // Wait briefly for the Relm4 message loop to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        assert!(controller.model().settings.dev_mode);
        assert_eq!(controller.model().sim_scenario, SimulationScenario::Success);

        // Test Page navigation
        controller
            .sender()
            .send(AppMsg::PageChanged("preferences".to_string()))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(controller.model().current_page, "preferences");

        controller.sender().send(AppMsg::GoBack).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(controller.model().current_page, "main");
    }
}
