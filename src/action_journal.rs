//! Structured record of every privileged/destructive action finupdate *would*
//! take, so tests can assert intent without granting privilege.
//!
//! # Why this exists
//!
//! `Settings::dry_run` already promised that "every destructive subprocess
//! (reboot, `bootc switch`, uupd timer toggle, uupd config write) is logged and
//! short-circuited to synthetic success instead of executing". In practice that
//! was implemented as a scattered `if settings.dry_run || settings.dev_mode {
//! toast(); return; }` guard at each call site, with the "logged" half going to
//! `tracing::warn!` — human-readable, but not machine-assertable.
//!
//! That made the interesting half of GUI testing untestable. A screenshot can
//! prove the *rebase dialog looks right*; only a journal can prove that clicking
//! **Switch** would have run `bootc switch ghcr.io/ublue-os/bluefin:stable` and
//! not, say, the currently-booted ref.
//!
//! # Model
//!
//! One JSONL line per intended action, appended to the path in
//! `$FINUPDATE_ACTION_JOURNAL`. When that variable is unset the journal is a
//! no-op, so production builds pay nothing but an atomic load.
//!
//! ```json
//! {"seq":3,"action":"switch_image","args":{"target":"ghcr.io/ublue-os/bluefin:stable"},
//!  "would_run":["pkexec","bootc","switch","ghcr.io/ublue-os/bluefin:stable"],
//!  "suppressed_by":"dry_run","ts":"2026-07-26T18:03:11Z"}
//! ```
//!
//! `would_run` is the argv that a real run would have executed. Keeping it
//! verbatim — rather than re-deriving it in the test — is the whole point: the
//! assertion checks the command finupdate actually built.
//!
//! # Ordering
//!
//! `seq` is a process-wide monotonic counter assigned under the same lock that
//! serialises the append, so lines are totally ordered even when several async
//! tasks journal concurrently. Tests that care about "did the switch happen
//! before the reboot" can rely on it; wall-clock `ts` is for humans only.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Environment variable naming the JSONL sink. Unset ⇒ journalling disabled.
pub const JOURNAL_ENV: &str = "FINUPDATE_ACTION_JOURNAL";

/// Why the action was recorded instead of executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppressed {
    /// `Settings::dry_run` — block destructive work, keep everything else real.
    DryRun,
    /// `Settings::dev_mode` — simulated updates.
    DevMode,
    /// Not suppressed: the action really ran. Recorded so a journal captured
    /// against a real system still shows the full action sequence.
    No,
}

impl Suppressed {
    fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry_run",
            Self::DevMode => "dev_mode",
            Self::No => "none",
        }
    }

    /// Map the two independent settings flags onto a single reason, preferring
    /// `dev_mode` when both are set because it is the broader simulation.
    pub fn from_flags(dev_mode: bool, dry_run: bool) -> Self {
        match (dev_mode, dry_run) {
            (true, _) => Self::DevMode,
            (false, true) => Self::DryRun,
            (false, false) => Self::No,
        }
    }

    /// True when the caller must *not* execute the underlying command.
    pub fn blocks_execution(self) -> bool {
        !matches!(self, Self::No)
    }
}

/// Process-wide append lock. Also guards `seq` assignment so the counter and
/// the file order can't disagree.
fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var_os(JOURNAL_ENV)?;
        if path.is_empty() {
            return None;
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => Some(Mutex::new(f)),
            Err(e) => {
                // Don't take the app down over a test-only facility.
                tracing::error!(
                    "action journal disabled: cannot open {:?}: {e}",
                    path
                );
                None
            }
        }
    })
    .as_ref()
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Record one intended action.
///
/// `action` is a stable identifier (`switch_image`, `reboot`, …) that tests
/// match on. `args` is the semantic payload. `would_run` is the exact argv a
/// real execution would use — pass the same vector you hand to `Command`.
///
/// Never panics and never propagates I/O errors: a broken journal must not
/// change application behaviour.
pub fn record(
    action: &str,
    args: serde_json::Value,
    would_run: &[String],
    suppressed: Suppressed,
) {
    let Some(sink) = sink() else { return };

    let mut guard = match sink.lock() {
        Ok(g) => g,
        // A poisoned lock means a previous writer panicked mid-append. The
        // file is still usable; recover rather than cascading the panic.
        Err(poisoned) => poisoned.into_inner(),
    };

    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let line = serde_json::json!({
        "seq": seq,
        "action": action,
        "args": args,
        "would_run": would_run,
        "suppressed_by": suppressed.as_str(),
        "ts": chrono::Utc::now().to_rfc3339(),
    });

    // One write call per line keeps appends atomic for reasonable line sizes,
    // which matters because the GUI and a spawned finupdate-cli may share a
    // journal path.
    let mut buf = line.to_string();
    buf.push('\n');
    if let Err(e) = guard.write_all(buf.as_bytes()).and_then(|_| guard.flush()) {
        tracing::error!("action journal write failed: {e}");
    }
}

/// Convenience for the common case: an action that is being suppressed, whose
/// argv is a borrowed `&[&str]`.
pub fn record_str(
    action: &str,
    args: serde_json::Value,
    would_run: &[&str],
    suppressed: Suppressed,
) {
    let owned: Vec<String> = would_run.iter().map(|s| (*s).to_string()).collect();
    record(action, args, &owned, suppressed);
}

/// True when journalling is active. Useful to skip building an expensive
/// `args` payload in hot paths.
pub fn is_enabled() -> bool {
    sink().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The sink is a process-wide OnceLock keyed off an env var read at first
    // use, so a test can't meaningfully re-point it. These tests therefore
    // cover the pure logic; end-to-end journal contents are asserted by the
    // integration tests in tests/action_journal.rs, which spawn a real
    // finupdate-cli process with FINUPDATE_ACTION_JOURNAL set.

    #[test]
    fn dev_mode_wins_over_dry_run() {
        assert_eq!(Suppressed::from_flags(true, true), Suppressed::DevMode);
        assert_eq!(Suppressed::from_flags(true, false), Suppressed::DevMode);
    }

    #[test]
    fn dry_run_alone_is_dry_run() {
        assert_eq!(Suppressed::from_flags(false, true), Suppressed::DryRun);
    }

    #[test]
    fn neither_flag_means_execution_proceeds() {
        let s = Suppressed::from_flags(false, false);
        assert_eq!(s, Suppressed::No);
        assert!(!s.blocks_execution());
    }

    #[test]
    fn both_suppression_reasons_block_execution() {
        assert!(Suppressed::DryRun.blocks_execution());
        assert!(Suppressed::DevMode.blocks_execution());
    }

    #[test]
    fn reason_strings_are_stable_test_contract() {
        // Tests match on these; changing them breaks the assertion surface.
        assert_eq!(Suppressed::DryRun.as_str(), "dry_run");
        assert_eq!(Suppressed::DevMode.as_str(), "dev_mode");
        assert_eq!(Suppressed::No.as_str(), "none");
    }
}
