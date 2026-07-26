//! Destructive host operations, isolated from the 4900-line `status_view`.
//!
//! Everything here changes the machine: powerwash, factory reset, unpin, and
//! the scheduled reboot. Keeping them in one small module rather than buried
//! among UI construction makes the blast radius reviewable — you can read every
//! privileged thing this app does in one screenful of function signatures.
//!
//! All of them route through [`crate::privileged::privileged`], so dry-run
//! withholds the command and the action journal records the exact argv that
//! would have run. Each also re-checks the suppression state immediately before
//! dispatch: the user confirms in a dialog, and settings can change between the
//! dialog opening and the response arriving.

use adw::prelude::*;
use gtk::prelude::*;
use relm4::gtk;
use std::process::Command;

use crate::settings::Settings;

/// Run `bootc install reset --experimental --apply` on the host (the canonical
/// factory-reset command, per https://bootc.dev/bootc/experimental-install-reset.html)
/// and surface success / failure back through the toast overlay.
///
/// `label` is used in toast / log messages so the caller (Powerwash vs.
/// Factory reset) can distinguish them.
///
/// This is destructive. It should only be reached after the caller has
/// confirmed `!settings.dry_run && !settings.dev_mode` AND user confirmation.
pub(super) fn run_bootc_install_reset(toast_overlay: &adw::ToastOverlay, label: &'static str) {
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

    let settings = Settings::load();
    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);

    // The most destructive command in the app — it redeploys the factory image
    // and erases user data. Nothing about it may run under dry-run, and the
    // journal must record the exact argv so a test can prove the guard held.
    let mut cmd = match crate::privileged::privileged(
        "factory_reset",
        serde_json::json!({ "label": label }),
        &["bootc", "install", "reset", "--experimental", "--apply"],
        crate::privileged::Privilege::Pkexec,
        suppressed,
    ) {
        crate::privileged::Exec::Suppressed => {
            let t = adw::Toast::new(&format!("{label} staged (dry-run, no commands run)"));
            t.set_timeout(4);
            toast_overlay.add_toast(t);
            return;
        }
        crate::privileged::Exec::Run(cmd) => cmd,
    };

    std::thread::spawn(move || {
        let cmd_result = cmd.output();

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
pub(super) fn run_unpin_to_stream(
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

    let settings = Settings::load();
    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);

    let mut cmd = match crate::privileged::privileged(
        "unpin",
        serde_json::json!({ "target": target_ref, "stream": stream_tag }),
        &["bootc", "switch", &target_for_thread],
        crate::privileged::Privilege::Pkexec,
        suppressed,
    ) {
        crate::privileged::Exec::Suppressed => {
            let t = adw::Toast::new("Unpin staged (dry-run, no commands run)");
            t.set_timeout(4);
            toast_overlay.add_toast(t);
            return;
        }
        crate::privileged::Exec::Run(cmd) => cmd,
    };

    std::thread::spawn(move || {
        let cmd_result = cmd.output();

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
pub(super) fn schedule_reboot_tonight(toast_overlay: &adw::ToastOverlay) {
    // Ask the chokepoint for the command. It journals the intent either way,
    // so a GUI test can assert `schedule_reboot` was requested with
    // `shutdown -r 02:00` without the host ever being scheduled to reboot.
    let settings = Settings::load();
    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);

    let mut cmd = match crate::privileged::privileged(
        "schedule_reboot",
        serde_json::json!({ "when": "02:00" }),
        &["shutdown", "-r", "02:00"],
        crate::privileged::Privilege::Pkexec,
        suppressed,
    ) {
        crate::privileged::Exec::Suppressed => {
            let t = adw::Toast::new("Restart scheduled for 02:00 (dry-run)");
            t.set_timeout(4);
            toast_overlay.add_toast(t);
            return;
        }
        crate::privileged::Exec::Run(cmd) => cmd,
    };

    // adw::ToastOverlay is GObject-but-not-Send, so we run shutdown(8) on a
    // std::thread and pipe the summary back via mpsc that's drained on the
    // GLib main loop (where the overlay is touchable). Same shape as
    // run_bootc_install_reset above.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let toast_overlay = toast_overlay.clone();

    std::thread::spawn(move || {
        let result = cmd.output();

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
pub(super) fn run_powerwash(toast_overlay: &adw::ToastOverlay) {
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
/// Run an unprivileged host command as one step of a multi-step destructive
/// operation (powerwash's flatpak/distrobox teardown).
///
/// Routed through the [`privileged`](crate::privileged) chokepoint with
/// `Privilege::Host` — these don't need root, but they *are* destructive, so
/// dry-run must withhold them and the journal must record them. A suppressed
/// step reports success so the caller's summary reflects what would have
/// happened.
pub(super) fn run_host_command(label: &'static str, args: &[&str]) -> (&'static str, bool, String) {
    let settings = Settings::load();
    let suppressed =
        crate::action_journal::Suppressed::from_flags(settings.dev_mode, settings.dry_run);

    let mut cmd = match crate::privileged::privileged(
        label,
        serde_json::json!({ "step": label }),
        args,
        crate::privileged::Privilege::Host,
        suppressed,
    ) {
        crate::privileged::Exec::Suppressed => return (label, true, String::new()),
        crate::privileged::Exec::Run(cmd) => cmd,
    };

    let output = cmd.output();
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
