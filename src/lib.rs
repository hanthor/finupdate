//! Finupdate library — shared modules for the GUI and CLI binaries.
//!
//! The service, registry, and settings modules are compiled into both binaries
//! (finupdate GUI and finupdate-cli headless). This lib.rs re-exports them
//! so tests can access the shared logic without depending on the full GUI/CLI stack.
//!
//! Also exposed via cdylib: see [`ffi`] for the C ABI consumed by the
//! gnome-control-center panel under `cc-panel/`.

pub mod action_journal;
pub mod app;
pub mod changelog_widget;
pub mod config;
pub mod dbus_progress;
pub mod ffi;
pub mod gpu;
pub mod orchestrator;
pub mod privileged;
pub mod rebase_widget;
pub mod registry_client;
pub mod runtime;
pub mod sbom_diff;
pub mod service;
pub mod settings;
#[cfg(test)]
pub mod test_support;
pub mod ui;
pub mod update_worker;
pub mod uupd_compat;
