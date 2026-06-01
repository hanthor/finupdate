//! Finupdate library — shared modules for the GUI and CLI binaries.
//!
//! The service, registry, and settings modules are compiled into both binaries
//! (finupdate GUI and finupdate-cli headless). This lib.rs re-exports them
//! so tests can access the shared logic without depending on the full GUI/CLI stack.

pub mod registry_client;
pub mod sbom_diff;
pub mod service;
pub mod settings;
pub mod uupd_compat;
