//! C ABI surface for the gnome-control-center panel.
//!
//! The downstream `gnome-control-center` build in
//! [Path 1 of the integration plan](../../docs/control-center-integration.md)
//! links against `libfinupdate.so` (built from this crate's `cdylib`
//! target) and calls into the functions defined here. The C-side panel
//! is in `cc-panel/`.
//!
//! ## Lifetime + threading
//!
//! All entry points take a `*mut Handle` obtained from
//! [`finupdate_new`] and freed by [`finupdate_free`]. The handle owns
//! a `tokio` runtime and the cached service state — caller is
//! responsible for keeping it alive for the lifetime of the panel.
//! Concurrent calls on the same handle are safe (`tokio` + the
//! service's internal `Mutex`); calls from multiple threads are not
//! recommended only because the eventual GTK callbacks need to land on
//! the GLib main loop, which is the C panel's job.
//!
//! ## Async results
//!
//! Functions that perform I/O take a `*const c_void user_data` plus a
//! C callback `extern "C" fn`. The runtime invokes the callback from a
//! worker thread when the operation completes — the C panel must
//! marshal the result back to the GLib main loop (via `g_idle_add` or
//! `g_main_context_invoke`) before touching widgets.
//!
//! ## Strings
//!
//! Every `*mut c_char` returned from this module is heap-allocated by
//! Rust (`CString::into_raw`) and **must** be freed with
//! [`finupdate_string_free`]. `*const c_char` parameters going INTO
//! Rust are borrowed for the duration of the call — Rust will not
//! retain them.
//!
//! ## Stability
//!
//! This surface is the contract between the Rust backend and the
//! downstream C panel. Treat it as a public API: additions are fine,
//! removals or signature changes break the panel's ABI and require a
//! coordinated rebuild.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;

use crate::service::{self, UpdaterService};

/// Opaque handle the C panel passes back into every entry point. Holds a
/// tokio runtime + the service trait object so each FFI call doesn't have
/// to spin up its own runtime (which would deadlock under
/// `Runtime::block_on` if called from inside another runtime).
pub struct Handle {
    rt: tokio::runtime::Runtime,
    service: Arc<dyn UpdaterService>,
}

/// Construct a new handle. Returns `NULL` if the tokio runtime can't be
/// built (e.g. file descriptor exhaustion). The caller owns the result and
/// must release it via [`finupdate_free`].
///
/// # Safety
///
/// Safe to call from C; the returned pointer is to a Rust-allocated
/// `Box<Handle>` and must not be freed with `free()`.
#[unsafe(no_mangle)]
pub extern "C" fn finupdate_new() -> *mut Handle {
    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("finupdate-ffi")
        .build()
    else {
        return ptr::null_mut();
    };

    // Lazily initialise the process-wide service the same way the GUI
    // does. The C panel will be the only caller in its process, so this
    // OnceLock write is uncontested.
    let svc = service::ensure_initialised();

    Box::into_raw(Box::new(Handle { rt, service: svc }))
}

/// Release a handle previously returned by [`finupdate_new`].
///
/// # Safety
///
/// `handle` must be a pointer returned by [`finupdate_new`] that has not
/// yet been freed. Passing NULL is permitted and is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_free(handle: *mut Handle) {
    if handle.is_null() {
        return;
    }
    // SAFETY: caller has promised the pointer originated from `Box::into_raw`.
    drop(unsafe { Box::from_raw(handle) });
}

/// Release a string previously returned by any `finupdate_*` getter.
///
/// # Safety
///
/// `s` must be either NULL or a pointer returned by a `finupdate_*`
/// function. Double-free is undefined behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // SAFETY: caller has promised the pointer originated from CString::into_raw.
    drop(unsafe { CString::from_raw(s) });
}

/// Pretty title for the currently booted image, e.g. "Bluefin Dakota".
/// Returns a heap-allocated NUL-terminated UTF-8 string the caller owns
/// (free with [`finupdate_string_free`]). Never returns NULL — falls back
/// to a generic "System Image" string when detection fails.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_current_image_title(handle: *mut Handle) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return string_to_c("System Image");
    };
    let title = handle
        .rt
        .block_on(async { handle.service.current_image().await.ok() })
        .map(|img| format!("{}/{}", img.org, img.image))
        .unwrap_or_else(|| "System Image".to_string());
    string_to_c(&title)
}

/// Full registry reference for the currently booted image, e.g.
/// `ghcr.io/projectbluefin/dakota:latest`. Returns NULL when the booted
/// image can't be detected (caller should hide the row).
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_current_image_ref(handle: *mut Handle) -> *mut c_char {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return ptr::null_mut();
    };
    let Some(img) = handle
        .rt
        .block_on(async { handle.service.current_image().await.ok() })
    else {
        return ptr::null_mut();
    };
    string_to_c(&format!(
        "{}/{}/{}:{}",
        img.registry, img.org, img.image, img.tag
    ))
}

/// Probe the registry for a newer build of the booted image. Asynchronous —
/// invokes `callback(update_available, user_data)` from a worker thread
/// when the check completes. `update_available` is 1 when a newer build
/// exists, 0 when up-to-date, -1 on error.
///
/// The C panel is expected to marshal the callback back onto the GLib
/// main loop via `g_idle_add_full` before touching widgets.
///
/// # Safety
///
/// `handle` must be a valid non-NULL pointer returned by [`finupdate_new`].
/// `callback` must outlive the call (typically a static function pointer).
/// `user_data` is opaque to Rust; ensure it remains valid until the
/// callback fires (or until [`finupdate_free`] is called, which won't
/// cancel the in-flight check — TODO: add a cancellation token).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn finupdate_check_for_updates(
    handle: *mut Handle,
    callback: extern "C" fn(update_available: c_int, user_data: *mut c_void),
    user_data: *mut c_void,
) {
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        callback(-1, user_data);
        return;
    };
    let svc = handle.service.clone();
    // Wrap user_data so we can pass it into the async task. It's a raw
    // pointer the caller has promised will stay valid until the callback
    // fires — we just relay it.
    let user_data_addr = user_data as usize;
    handle.rt.spawn(async move {
        let result: c_int = match svc.current_image().await {
            Ok(image) => match svc.list_versions(&image, 4).await {
                Ok(versions) => {
                    // "Update available" heuristic: registry has a newer
                    // build than the booted one. The GUI does a richer
                    // check (digest comparison, SBOM diff); this is the
                    // minimal signal for the cc-panel status row.
                    if versions.iter().any(|v| v.version != image.tag) {
                        1
                    } else {
                        0
                    }
                }
                Err(_) => -1,
            },
            Err(_) => -1,
        };
        callback(result, user_data_addr as *mut c_void);
    });
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn string_to_c(s: &str) -> *mut c_char {
    // Strip interior NULs — UTF-8 strings from Rust shouldn't contain
    // them, but the panel will treat the result as a C string regardless.
    let cleaned: String = s.chars().filter(|&c| c != '\0').collect();
    match CString::new(cleaned) {
        Ok(c) => c.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

#[allow(dead_code)]
fn cstr_to_str<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }
    // SAFETY: caller guarantees the pointer is a NUL-terminated C string.
    unsafe { CStr::from_ptr(s) }.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_c_roundtrip() {
        let p = string_to_c("Bluefin Dakota");
        // SAFETY: just allocated by string_to_c.
        let back = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        assert_eq!(back, "Bluefin Dakota");
        unsafe { finupdate_string_free(p) };
    }

    #[test]
    fn string_to_c_strips_interior_nul() {
        // Defensive: shouldn't happen from Rust but guards against future
        // changes that leak a NUL into a label.
        let p = string_to_c("foo\0bar");
        let back = unsafe { CStr::from_ptr(p) }.to_str().unwrap().to_string();
        assert_eq!(back, "foobar");
        unsafe { finupdate_string_free(p) };
    }

    #[test]
    fn free_null_string_is_noop() {
        unsafe { finupdate_string_free(ptr::null_mut()) };
    }

    #[test]
    fn free_null_handle_is_noop() {
        unsafe { finupdate_free(ptr::null_mut()) };
    }
}
