//! Shared helpers for the unit-test suite.
//!
//! # Why a process-wide environment lock exists
//!
//! Several test modules install mock executables into a temp dir and prepend it
//! to `PATH` so the code under test resolves those instead of the real
//! binaries. `PATH` is **process-global**, but `cargo test` runs tests
//! concurrently across threads, so two tests doing this at once corrupt each
//! other: one restores the original `PATH` while the other is still relying on
//! its mock, and the second test then resolves the *real* binary (or nothing).
//!
//! `uupd_compat` already had a module-local mutex, but `orchestrator`
//! (`orchestrator.rs:721`) mutates `PATH` without taking it. The two modules
//! therefore raced, making `test_is_uupd_installed` fail roughly one run in
//! three — a genuine flake, unrelated to the code being tested, and exactly the
//! kind of thing that erodes trust in a suite.
//!
//! A single lock shared by *every* test that touches process-wide state fixes
//! it. Use [`env_lock`] rather than adding another module-local mutex.

#![cfg(test)]

use tokio::sync::Mutex;

/// The one lock guarding process-global state (`PATH`, and any other env var a
/// test mutates).
///
/// A `tokio::sync::Mutex` rather than `std::sync::Mutex` because several of the
/// call sites are `#[tokio::test]` async fns that hold the guard across an
/// `.await` — which a std mutex guard cannot legally do.
///
/// ```ignore
/// let _lock = crate::test_support::env_lock().lock().await;
/// // ... mutate PATH, run the code under test, restore PATH ...
/// ```
pub fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());
    &ENV_LOCK
}
