//! The single chokepoint for every privileged host command finupdate runs.
//!
//! # The bug class this closes
//!
//! Before this module, each destructive action open-coded the same three
//! concerns at its own call site:
//!
//! ```ignore
//! if settings.dry_run || settings.dev_mode {
//!     tracing::warn!("reboot suppressed (dry_run={}, dev_mode={})", ...);
//!     toast("Restart scheduled (dry-run)");
//! } else if is_flatpak() {
//!     Command::new("flatpak-spawn").args(["--host", "pkexec", "shutdown", ...])
//! } else {
//!     Command::new("pkexec").args(["shutdown", ...])
//! }
//! ```
//!
//! Three problems followed from that duplication:
//!
//! 1. **The guard is opt-in.** A newly added destructive action executes for
//!    real in dry-run until someone remembers to wrap it. That is exactly the
//!    failure mode you cannot afford in a GUI test suite that clicks buttons.
//! 2. **The sandbox prefix is re-derived** at ~8 sites, so a fix to the
//!    `flatpak-spawn --host` handling has to land in all of them.
//! 3. **Suppression was only `tracing::warn!`** — human-readable, not
//!    machine-assertable. Nothing could prove the *right* command was chosen.
//!
//! Routing every privileged command through [`privileged`] makes suppression
//! structural rather than remembered: you cannot obtain a runnable `Command`
//! without passing a [`Suppressed`], and the journal entry is written on the
//! way through.
//!
//! # Usage
//!
//! ```ignore
//! match privileged("reboot", json!({}), &["systemctl", "reboot"], suppressed) {
//!     Exec::Suppressed => { /* show the dry-run toast */ }
//!     Exec::Run(mut cmd) => { let _ = cmd.output(); }
//! }
//! ```
//!
//! The caller still decides what to *say* when suppressed — the UI copy for a
//! suppressed powerwash differs from a suppressed reboot — but it can no longer
//! decide whether to *fire*.

use crate::action_journal::{self, Suppressed};

/// Result of asking for a privileged command.
pub enum Exec {
    /// Execute this. The sandbox prefix is already applied.
    Run(std::process::Command),
    /// Suppressed by dry-run or dev-mode. Already journalled; the caller should
    /// render whatever success/staged affordance it would have shown, without
    /// touching the host.
    Suppressed,
}

/// Async twin of [`Exec`] for the call sites already inside a tokio context.
pub enum ExecAsync {
    Run(tokio::process::Command),
    Suppressed,
}

/// How much privilege a command needs.
///
/// Not every host command finupdate runs is a root command, and forcing
/// `pkexec` onto all of them would be wrong in both directions: it would
/// prompt the user for a password to read a world-readable file, and it would
/// mis-record `would_run` in the journal so tests assert the wrong argv.
///
/// Concretely, all three of these exist in the codebase today:
///   * `pkexec bootc switch …` — genuinely needs root.
///   * `bootc status --json` — tried unprivileged first, with a pkexec
///     fallback only if that fails (`registry_client.rs:403` vs `:413`).
///   * `flatpak-spawn --host cat /etc/uupd/config.json` — needs to escape the
///     sandbox, never needs root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    /// Elevate with `pkexec`.
    Pkexec,
    /// Run on the host as the current user — no elevation. Still routed through
    /// here so the journal sees it and dry-run can withhold it.
    Host,
}

/// Build the full argv, applying the `flatpak-spawn --host` prefix when
/// sandboxed and `pkexec` when the command needs elevation.
///
/// Kept separate from command construction so both the sync and async variants
/// — and the journal — all describe the *same* argv. The journal records this
/// resolved form, not the caller's fragment, so a test asserting `would_run`
/// sees precisely what would have been executed.
fn resolve_argv(argv: &[&str], privilege: Privilege) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len() + 3);
    if crate::update_worker::is_flatpak() {
        out.push("flatpak-spawn".into());
        out.push("--host".into());
    }
    if privilege == Privilege::Pkexec {
        out.push("pkexec".into());
    }
    out.extend(argv.iter().map(|s| (*s).to_string()));
    out
}

/// Journal the intent, then hand back a runnable command — or `Suppressed`.
///
/// `action` is the stable identifier tests match on (`reboot`,
/// `schedule_reboot`, `switch_image`, `powerwash`, …). `args` carries the
/// semantic payload so an assertion can check *what* was targeted without
/// string-matching argv. `argv` is the command **without** any `pkexec` /
/// `flatpak-spawn` prefix — this function adds those.
pub fn privileged(
    action: &str,
    args: serde_json::Value,
    argv: &[&str],
    privilege: Privilege,
    suppressed: Suppressed,
) -> Exec {
    let full = resolve_argv(argv, privilege);
    action_journal::record(action, args, &full, suppressed);

    if suppressed.blocks_execution() {
        tracing::warn!(
            "{action} suppressed ({}) — would have run: {}",
            match suppressed {
                Suppressed::DryRun => "dry_run",
                Suppressed::DevMode => "dev_mode",
                Suppressed::No => unreachable!("blocks_execution() implies suppression"),
            },
            full.join(" "),
        );
        return Exec::Suppressed;
    }

    let mut cmd = std::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    Exec::Run(cmd)
}

/// Async variant of [`privileged`], for call sites already on tokio.
pub fn privileged_async(
    action: &str,
    args: serde_json::Value,
    argv: &[&str],
    privilege: Privilege,
    suppressed: Suppressed,
) -> ExecAsync {
    let full = resolve_argv(argv, privilege);
    action_journal::record(action, args, &full, suppressed);

    if suppressed.blocks_execution() {
        tracing::warn!("{action} suppressed — would have run: {}", full.join(" "));
        return ExecAsync::Suppressed;
    }

    let mut cmd = tokio::process::Command::new(&full[0]);
    cmd.args(&full[1..]);
    ExecAsync::Run(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `is_flatpak()` keys off FLATPAK_ID / /.flatpak-info, neither of which is
    // present in the test environment, so resolve_argv takes the unsandboxed
    // branch here. The sandboxed branch is covered by the shape assertion
    // below plus the Flatpak smoke test in the GUI suite.

    #[test]
    fn argv_gets_pkexec_prefix() {
        let got = resolve_argv(&["systemctl", "reboot"], Privilege::Pkexec);
        assert_eq!(got, vec!["pkexec", "systemctl", "reboot"]);
    }

    #[test]
    fn argv_preserves_argument_order_and_arity() {
        let got = resolve_argv(
            &["bootc", "switch", "ghcr.io/ublue-os/bluefin:stable"],
            Privilege::Pkexec,
        );
        assert_eq!(
            got,
            vec![
                "pkexec",
                "bootc",
                "switch",
                "ghcr.io/ublue-os/bluefin:stable"
            ]
        );
    }

    #[test]
    fn host_privilege_does_not_elevate() {
        // The uupd config *read* and the unprivileged `bootc status` attempt
        // must not gain a pkexec prefix — otherwise the journal would claim a
        // password prompt that never happens.
        let got = resolve_argv(&["cat", "/etc/uupd/config.json"], Privilege::Host);
        assert_eq!(got, vec!["cat", "/etc/uupd/config.json"]);
    }

    #[test]
    fn empty_argv_still_yields_pkexec() {
        // Degenerate but must not panic — privileged() indexes full[0].
        assert_eq!(resolve_argv(&[], Privilege::Pkexec), vec!["pkexec"]);
    }

    #[test]
    fn suppressed_returns_no_command() {
        let e = privileged(
            "reboot",
            serde_json::json!({}),
            &["systemctl", "reboot"],
            Privilege::Pkexec,
            Suppressed::DryRun,
        );
        assert!(matches!(e, Exec::Suppressed));
    }

    #[test]
    fn dev_mode_also_suppresses() {
        let e = privileged(
            "powerwash",
            serde_json::json!({}),
            &["bootc", "install"],
            Privilege::Pkexec,
            Suppressed::DevMode,
        );
        assert!(matches!(e, Exec::Suppressed));
    }

    #[test]
    fn unsuppressed_yields_a_command_with_the_resolved_program() {
        let e = privileged(
            "reboot",
            serde_json::json!({}),
            &["systemctl", "reboot"],
            Privilege::Pkexec,
            Suppressed::No,
        );
        match e {
            Exec::Run(cmd) => {
                assert_eq!(cmd.get_program(), "pkexec");
                let args: Vec<_> = cmd.get_args().collect();
                assert_eq!(args, vec!["systemctl", "reboot"]);
            }
            Exec::Suppressed => panic!("Suppressed::No must produce a runnable command"),
        }
    }

    #[test]
    fn async_variant_suppresses_identically() {
        let e = privileged_async(
            "switch_image",
            serde_json::json!({"target": "ghcr.io/x/y:z"}),
            &["bootc", "switch", "ghcr.io/x/y:z"],
            Privilege::Pkexec,
            Suppressed::DryRun,
        );
        assert!(matches!(e, ExecAsync::Suppressed));
    }
}
