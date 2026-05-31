// Consumed by the GUI binary; finupdate-cli compiles it but doesn't use
// run() / run_simulated(), which trips dead-code warnings under that bin.
#![allow(dead_code)]

//! Event types and the thin public façade over the update orchestrator.
//!
//! The real work lives in [`crate::orchestrator`], which spawns the bundled
//! `finupdate-runner` shell script under a single `pkexec` elevation and
//! parses its marker-line protocol into structured [`UpdateEvent`]s.
//!
//! This module exists for two reasons:
//! 1. It owns the [`UpdateEvent`] enum that both real and simulated runs emit.
//! 2. It owns [`run_simulated`] — the dev-mode driver that walks through the
//!    same event sequence without touching the host, used to exercise the UI
//!    state machine without root or a real update.
//!
//! Sandbox awareness lives in [`is_flatpak`]; everywhere else that detects
//! "are we in a Flatpak?" calls this helper.

use tokio::sync::mpsc;

/// Events emitted by the update worker as the subprocess runs.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// A line of stdout/stderr output from the update process.
    Output(String),
    /// The process exited successfully (exit code 0).
    Complete,
    /// The process exited with code 77 — no update was needed.
    UpToDate,
    /// The process failed — includes a human-readable error description.
    Error(String),
    /// A module has started running.
    ModuleStarted(crate::orchestrator::Module),
    /// A module has finished with a status.
    ModuleFinished(
        crate::orchestrator::Module,
        crate::orchestrator::ModuleStatus,
    ),
}

/// What the simulated update run should end with.
#[derive(Debug, Clone, Copy)]
pub enum SimulationScenario {
    /// All four modules succeed, update completes normally.
    Success,
    /// System is already current — ends with UpToDate.
    AlreadyUpToDate,
    /// System module fails — ends with an Error.
    Failure,
}

/// Detect if we're running inside a Flatpak sandbox.
/// The canonical check is the existence of `/.flatpak-info`.
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Manages spawning and streaming output from the update process.
pub struct UpdateWorker;

impl UpdateWorker {
    pub fn new() -> Self {
        Self
    }

    /// Create a worker (custom command ignored — kept for API compatibility).
    #[allow(dead_code)]
    pub fn with_command(_command: impl Into<String>, _args: Vec<String>) -> Self {
        Self
    }

    /// Spawn the subprocess and return a receiver for streaming events.
    ///
    /// Delegates to `orchestrator::run()` which invokes `finupdate-runner`
    /// via a single pkexec elevation.
    pub async fn run(
        &mut self,
        cancel_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> mpsc::UnboundedReceiver<UpdateEvent> {
        crate::orchestrator::run(cancel_rx).await
    }
}

/// Run a scripted simulation of a uupd update run without touching the real system.
///
/// Emits realistic-looking log lines for all four modules with short delays,
/// then ends according to `scenario`. Respects `cancel_rx` — if cancelled,
/// emits an error event and returns.
///
/// This is the backend for Developer Mode: allows walking through the entire
/// UI flow (progress, per-module status, completion/failure screens) without
/// root privileges or a live uupd installation.
pub async fn run_simulated(
    scenario: SimulationScenario,
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<UpdateEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let tx2 = tx.clone();
        let do_sim = async move { simulate_update(&tx2, scenario).await };

        tokio::select! {
            _ = do_sim => {}
            _ = cancel_rx => {
                let _ = tx.send(UpdateEvent::Error("Update cancelled by user".to_string()));
            }
        }
    });

    rx
}

/// Internal simulation: emits structured module events for all four modules.
async fn simulate_update(tx: &mpsc::UnboundedSender<UpdateEvent>, scenario: SimulationScenario) {
    use crate::orchestrator::{Module, ModuleStatus};
    use tokio::time::{Duration, sleep};

    macro_rules! line {
        ($msg:expr) => {{
            if tx.send(UpdateEvent::Output($msg.to_string())).is_err() {
                return;
            }
            sleep(Duration::from_millis(300)).await;
        }};
        ($msg:expr, $delay:expr) => {{
            if tx.send(UpdateEvent::Output($msg.to_string())).is_err() {
                return;
            }
            sleep(Duration::from_millis($delay)).await;
        }};
    }
    macro_rules! module_start {
        ($m:expr) => {{
            if tx.send(UpdateEvent::ModuleStarted($m)).is_err() {
                return;
            }
        }};
    }
    macro_rules! module_done {
        ($m:expr, $s:expr) => {{
            if tx.send(UpdateEvent::ModuleFinished($m, $s)).is_err() {
                return;
            }
        }};
    }

    line!("[DEV MODE] Starting finupdate simulation (DRY RUN)");
    line!("[DEV MODE] No actual commands will be executed - this is a preview");
    sleep(Duration::from_millis(400)).await;

    // ── System module ────────────────────────────────────────────────────
    module_start!(Module::System);
    line!("$ pkexec bootc upgrade --check", 300);
    line!("Checking for OS image updates...", 600);

    if matches!(scenario, SimulationScenario::AlreadyUpToDate) {
        line!("Image is already up to date", 400);
        module_done!(Module::System, ModuleStatus::UpToDate);
        let _ = tx.send(UpdateEvent::UpToDate);
        return;
    }

    if matches!(scenario, SimulationScenario::Failure) {
        line!("[DRY RUN] Would execute: $ pkexec bootc upgrade", 300);
        line!("bootc upgrade failed (simulated)", 400);
        module_done!(Module::System, ModuleStatus::Failed(1));
        let _ = tx.send(UpdateEvent::Error(
            "[DEV MODE] Simulated system module failure".to_string(),
        ));
        return;
    }

    line!("[DRY RUN] Would execute: $ pkexec bootc upgrade", 300);
    line!("bootc: Fetching image manifest...", 500);
    line!("bootc: Pulling 3 new layers (42.1 MB)", 800);
    line!("bootc: Staging complete — reboot to apply", 400);
    module_done!(Module::System, ModuleStatus::Success);

    // ── Flatpak module ───────────────────────────────────────────────────
    module_start!(Module::Flatpak);
    line!("$ flatpak remote-info --updates flathub", 300);
    line!("Checking for Flatpak updates...", 500);
    line!("[DRY RUN] Would execute: $ flatpak update -y", 300);
    line!("Updated: org.mozilla.firefox (130.0.1 → 131.0)", 400);
    line!("Updated: com.spotify.Client (1.2.45 → 1.2.46)", 400);
    line!("Flatpak: 2 apps updated", 300);
    module_done!(Module::Flatpak, ModuleStatus::Success);

    // ── Brew module ──────────────────────────────────────────────────────
    module_start!(Module::Brew);
    line!("$ brew upgrade --dry-run", 300);
    line!("Upgrading Homebrew packages...", 500);
    line!("[DRY RUN] Would execute: $ brew upgrade", 300);
    line!("Already up-to-date: neovim, fzf, ripgrep", 300);
    module_done!(Module::Brew, ModuleStatus::Success);

    // ── Distrobox module ─────────────────────────────────────────────────
    module_start!(Module::Distrobox);
    line!("$ distrobox list --json", 300);
    line!("Upgrading Distrobox containers...", 500);
    line!("[DRY RUN] Would execute: $ distrobox upgrade all", 300);
    line!("ubuntu-22: updated 5 packages", 400);
    module_done!(Module::Distrobox, ModuleStatus::Success);

    sleep(Duration::from_millis(300)).await;
    let _ = tx.send(UpdateEvent::Complete);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{Module, ModuleStatus};

    /// Drain a receiver into a Vec until the channel closes.
    async fn drain(mut rx: mpsc::UnboundedReceiver<UpdateEvent>) -> Vec<UpdateEvent> {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        events
    }

    fn module_events(events: &[UpdateEvent]) -> Vec<(&'static str, &str)> {
        events
            .iter()
            .filter_map(|e| match e {
                UpdateEvent::ModuleStarted(m) => Some(("start", m.key())),
                UpdateEvent::ModuleFinished(m, _) => Some(("end", m.key())),
                _ => None,
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn success_scenario_emits_all_four_modules_then_complete() {
        let (_tx_cancel, rx_cancel) = tokio::sync::oneshot::channel();
        let rx = run_simulated(SimulationScenario::Success, rx_cancel).await;
        let events = drain(rx).await;

        // Last event must be Complete.
        assert!(
            matches!(events.last(), Some(UpdateEvent::Complete)),
            "expected Complete at end, got {:?}",
            events.last()
        );

        // Every module starts and finishes in order.
        let module_seq = module_events(&events);
        assert_eq!(
            module_seq,
            vec![
                ("start", "system"),
                ("end", "system"),
                ("start", "flatpak"),
                ("end", "flatpak"),
                ("start", "brew"),
                ("end", "brew"),
                ("start", "distrobox"),
                ("end", "distrobox"),
            ]
        );

        // Every ModuleFinished should be Success in this scenario.
        for ev in &events {
            if let UpdateEvent::ModuleFinished(_, status) = ev {
                assert_eq!(status, &ModuleStatus::Success);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn already_up_to_date_short_circuits_after_system() {
        let (_tx, rx_cancel) = tokio::sync::oneshot::channel();
        let rx = run_simulated(SimulationScenario::AlreadyUpToDate, rx_cancel).await;
        let events = drain(rx).await;

        // Last event is UpToDate.
        assert!(matches!(events.last(), Some(UpdateEvent::UpToDate)));

        // Only the system module ran.
        let module_seq = module_events(&events);
        assert_eq!(module_seq, vec![("start", "system"), ("end", "system")]);

        // System finished as UpToDate, not Success.
        let system_finish = events
            .iter()
            .find_map(|e| match e {
                UpdateEvent::ModuleFinished(Module::System, s) => Some(s),
                _ => None,
            })
            .expect("expected ModuleFinished for system");
        assert_eq!(system_finish, &ModuleStatus::UpToDate);
    }

    #[tokio::test(start_paused = true)]
    async fn failure_scenario_emits_error_after_system() {
        let (_tx, rx_cancel) = tokio::sync::oneshot::channel();
        let rx = run_simulated(SimulationScenario::Failure, rx_cancel).await;
        let events = drain(rx).await;

        // Last event is Error with the dev-mode marker text.
        match events.last() {
            Some(UpdateEvent::Error(msg)) => {
                assert!(msg.contains("[DEV MODE]"), "got: {msg}");
            }
            other => panic!("expected Error, got {other:?}"),
        }

        // System started and finished as Failed(1).
        let system_finish = events
            .iter()
            .find_map(|e| match e {
                UpdateEvent::ModuleFinished(Module::System, s) => Some(s),
                _ => None,
            })
            .expect("expected ModuleFinished for system");
        assert_eq!(system_finish, &ModuleStatus::Failed(1));

        // No other modules ran.
        let other_modules = module_events(&events)
            .into_iter()
            .filter(|(_, key)| *key != "system")
            .count();
        assert_eq!(other_modules, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_emits_error_and_stops() {
        let (tx_cancel, rx_cancel) = tokio::sync::oneshot::channel();
        let rx = run_simulated(SimulationScenario::Success, rx_cancel).await;

        // Cancel almost immediately.
        let _ = tx_cancel.send(());
        let events = drain(rx).await;

        // The cancel message should land as an Error.
        let last_is_cancel = matches!(
            events.last(),
            Some(UpdateEvent::Error(m)) if m.contains("cancelled")
        );
        assert!(last_is_cancel, "tail: {:?}", events.last());

        // We should NOT see Complete after a cancel.
        assert!(!events.iter().any(|e| matches!(e, UpdateEvent::Complete)));
    }
}
