//! Finupdate — system update frontend for uupd.
//!
//! Entry point pattern for Bluefin utility apps:
//! 1. Initialize tracing (structured logging)
//! 2. Create an `adw::Application` via relm4 with a proper app ID
//! 3. Hand control to the relm4 component tree
//!
//! This pattern ensures:
//! - D-Bus activation works (app ID matches .desktop file)
//! - Single-instance behavior is enforced by GApplication
//! - libadwaita styles are loaded before any widgets are created

mod app;
mod sbom_diff;
mod config;
pub mod dbus_progress;
mod gpu;
mod orchestrator;
mod registry_client;
mod service;
mod settings;
mod ui;
mod update_worker;
mod uupd_compat;

use app::App;

const USAGE: &str = "\
finupdate — system update frontend for bootc / uupd.

Usage: finupdate [OPTIONS]

Options:
  --dev-mode             Force developer mode for this run (simulated updates,
                         no destructive subprocesses). Overrides settings.json.
  --sim=<scenario>       Pre-select a developer-mode simulation outcome:
                         success | failure | up-to-date. Implies --dev-mode.
  --help, -h             Print this message and exit.

Without flags, finupdate reads developer-mode + simulator state from
settings.json. The hamburger menu no longer exposes these toggles per
HIG (no dev-only state visible to end users); use these flags instead.
";

fn main() {
    // Initialize structured logging — respects RUST_LOG env var.
    // Default to "info" for release, "debug" for dev builds.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // CLI args — parse before settings::Settings::load() so the overrides
    // apply to the very first read. We don't depend on `clap` to keep the
    // binary small and to avoid an extra crate dep for two switches.
    let args: Vec<String> = std::env::args().collect();
    let mut force_dev_mode = false;
    let mut sim_scenario: Option<&str> = None;
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--dev-mode" => force_dev_mode = true,
            "-h" | "--help" => {
                print!("{}", USAGE);
                std::process::exit(0);
            }
            a if a.starts_with("--sim=") => {
                let s = &a["--sim=".len()..];
                match s {
                    "success" | "failure" | "up-to-date" => {
                        sim_scenario = Some(match s {
                            "success" => "success",
                            "failure" => "failure",
                            _ => "up-to-date",
                        });
                        force_dev_mode = true;
                    }
                    _ => {
                        eprintln!("finupdate: invalid --sim scenario '{}'", s);
                        eprintln!("Valid: success | failure | up-to-date");
                        std::process::exit(2);
                    }
                }
            }
            _ => {
                eprintln!("finupdate: unknown argument '{}'", arg);
                eprint!("\n{}", USAGE);
                std::process::exit(2);
            }
        }
    }

    // Persist the CLI overrides into Settings so the rest of the app reads
    // them the same way it reads anything else. Only writes back to disk for
    // dev-mode (so a tester running `finupdate --dev-mode` once stays in dev
    // mode); --sim is per-run only and lives in an env var the simulator
    // reads on startup.
    if force_dev_mode {
        let mut s = settings::Settings::load();
        if !s.dev_mode {
            s.dev_mode = true;
            s.save();
            tracing::info!("CLI override: dev_mode enabled");
        }
    }
    if let Some(scenario) = sim_scenario {
        unsafe { std::env::set_var("FINUPDATE_SIM_SCENARIO", scenario); }
        tracing::info!("CLI override: simulator scenario = {}", scenario);
    }

    tracing::info!(
        "Starting Finupdate ({}) v{}",
        config::APP_ID,
        config::VERSION
    );

    // Install the process-wide UpdaterService before any UI builds — UI
    // components grab it via service::global() rather than threading an Arc
    // through every closure. Swap a mock here if integration-testing.
    service::init(service::BootcUpdaterService::new());

    // relm4::RelmApp handles:
    // - Creating the adw::Application (because we enabled the "libadwaita" feature)
    // - Calling adw::init() which loads Adwaita CSS and enables color scheme support
    // - Running the GLib main loop
    let app = relm4::RelmApp::new(config::APP_ID);
    app.run::<App>(());
}
