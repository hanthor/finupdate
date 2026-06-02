//! Finupdate library — shared modules for the GUI and CLI binaries.
//!
//! The service, registry, and settings modules are compiled into both binaries
//! (finupdate GUI and finupdate-cli headless). This lib.rs re-exports them
//! so tests can access the shared logic without depending on the full GUI/CLI stack.
//!
//! Also exposed via cdylib: see [`ffi`] for the C ABI consumed by the
//! gnome-control-center panel under `cc-panel/`.

pub mod config;
pub mod ffi;
pub mod orchestrator;
pub mod registry_client;
pub mod sbom_diff;
pub mod service;
pub mod settings;
pub mod update_worker;
pub mod uupd_compat;
