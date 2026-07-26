//! The process's single shared tokio runtime.
//!
//! # Why this exists
//!
//! finupdate is a GTK app whose backend is async, so it needs a bridge between
//! the GLib main loop and tokio. That bridge was previously open-coded at every
//! call site — ten-plus of them (`app.rs:386`, `:428`, `:643`,
//! `rebase_dialog.rs:459`, `:1364`, `rebase_widget.rs:42`,
//! `changelog_widget.rs:56`, `status_view.rs`, `ffi.rs:72`, …), each building a
//! *fresh* `tokio::runtime` and blocking on it.
//!
//! Two failures followed from that:
//!
//! 1. **Thread exhaustion.** Each runtime brings its own worker and blocking
//!    pools. Some of these sites sit in per-row rendering code, so displaying an
//!    image with several hundred published tags tried to stand up several
//!    hundred runtimes at once. The process hit its thread limit and died with
//!    `OS can't spawn worker thread: Resource temporarily unavailable`,
//!    surfacing as a panic inside hyper's DNS resolver — far from the cause.
//!
//! 2. **Nested-runtime panics.** `Runtime::block_on` panics with "Cannot start a
//!    runtime from within a runtime" if the calling thread is already driving
//!    one. Whether a given helper was reached from the GTK thread or from a
//!    tokio worker depended on timing, so this crashed only when a background
//!    fetch happened to race UI construction.
//!
//! One runtime with bounded pools fixes both, and [`block_on`] picks the
//! correct strategy for the calling context instead of each site guessing.

use std::future::Future;
use std::sync::OnceLock;
use tokio::runtime::{Builder, Handle, Runtime};

/// Worker threads for the shared runtime.
///
/// The workload is a handful of concurrent HTTPS round-trips to a registry plus
/// the occasional subprocess — I/O-bound and low-volume. Four workers is ample
/// and keeps the footprint modest for a desktop utility that spends most of its
/// life idle.
const WORKER_THREADS: usize = 4;

/// Ceiling on the blocking pool. tokio's default is 512, which is what let the
/// old per-call-site runtimes multiply into thread exhaustion. DNS resolution
/// in hyper runs on this pool, so it needs real headroom — but bounded.
const MAX_BLOCKING_THREADS: usize = 32;

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .thread_name("finupdate-rt")
            .enable_all()
            .build()
            .expect("failed to build the shared tokio runtime")
    })
}

/// A handle to the shared runtime, for spawning detached background work.
pub fn handle() -> Handle {
    runtime().handle().clone()
}

/// Run `fut` to completion, blocking the caller.
///
/// Safe from **either** context, which is the whole point:
///
/// * **Not in a runtime** (the GTK main thread) — block on the shared runtime
///   directly.
/// * **Already inside the runtime** (a tokio worker) — `block_on` would panic,
///   so hand the current worker off with `block_in_place` and drive the future
///   on the same runtime. This requires a multi-threaded runtime, which is why
///   [`runtime`] is built with `new_multi_thread` even though the workload is
///   small.
///
/// Blocking the GTK thread still freezes the UI for the duration, so this is
/// for short calls (a cached `current_image()`, a status probe). Anything that
/// touches the network on a user-visible path should use [`spawn`] and deliver
/// its result through a channel instead.
pub fn block_on<F>(fut: F) -> F::Output
where
    F: Future,
{
    match Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => runtime().block_on(fut),
    }
}

/// Spawn detached work on the shared runtime.
pub fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_on_works_outside_a_runtime() {
        assert_eq!(block_on(async { 1 + 1 }), 2);
    }

    #[test]
    fn block_on_works_inside_the_runtime() {
        // The nested case that used to panic with "Cannot start a runtime from
        // within a runtime".
        let got = block_on(async { block_on(async { "nested" }) });
        assert_eq!(got, "nested");
    }

    #[test]
    fn repeated_calls_share_one_runtime() {
        // Regression guard for the thread-exhaustion bug: hammering the bridge
        // must not create a runtime per call. If it did, this would exhaust the
        // thread limit rather than complete.
        for i in 0..500 {
            assert_eq!(block_on(async move { i }), i);
        }
    }

    #[test]
    fn spawned_work_completes() {
        let h = spawn(async { 21 * 2 });
        assert_eq!(block_on(async { h.await.unwrap() }), 42);
    }
}
