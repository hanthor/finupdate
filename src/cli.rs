//! Headless CLI for the finupdate backend.
//!
//! Exists as a separate `[[bin]]` to prove the UpdaterService abstraction
//! actually decouples the backend from the GTK frontend — this binary doesn't
//! link app/ui/dbus_progress/gpu, only the service trait + its dependencies.
//!
//! Subcommands:
//!   - `status`   — print the currently-booted image ref
//!   - `family`   — print the detected family + its switchable features
//!   - `versions` — list recent dated versions of the booted image
//!   - `tags`     — list tags published for the booted image's stream
//!
//! Honours the same precedence chain as the GUI for image detection:
//! `Settings::mock_identity` → `FINUPDATE_IMAGE` env → `bootc status` →
//! `/etc/os-release`. So `FINUPDATE_IMAGE=ghcr.io/ublue-os/aurora:stable
//! finupdate-cli versions` works without touching the host's bootc state.

mod config;
mod orchestrator;
mod registry_client;
mod sbom_diff;
mod service;
mod settings;
mod update_worker;
mod uupd_compat;

use std::process::ExitCode;

const USAGE: &str = "\
finupdate-cli — headless image queries via the UpdaterService trait.

Usage: finupdate-cli <command>

Commands:
  status    Show the currently-booted image
  family    Show the detected family and available feature toggles
  versions  List recent dated versions for the booted image
  tags      List published tags for the booted image's stream
  help      Print this help

Environment:
  FINUPDATE_IMAGE   Override detected image (registry/org/image:tag)
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    service::init(service::BootcUpdaterService::new());

    let cmd = std::env::args().nth(1).unwrap_or_else(|| "help".to_string());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            print!("{}", USAGE);
            ExitCode::SUCCESS
        }
        "status" => rt.block_on(cmd_status()),
        "family" => rt.block_on(cmd_family()),
        "versions" => rt.block_on(cmd_versions()),
        "tags" => rt.block_on(cmd_tags()),
        other => {
            eprintln!("finupdate-cli: unknown command '{}'\n", other);
            eprint!("{}", USAGE);
            ExitCode::from(2)
        }
    }
}

async fn cmd_status() -> ExitCode {
    match service::global().current_image().await {
        Ok(img) => {
            println!("{}", img);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: no booted image detected: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_family() -> ExitCode {
    match service::global().current_family().await {
        Ok(Some(fam)) => {
            println!("family: {}", fam.name);
            println!("base:   {}", fam.base_image);
            if fam.features.is_empty() {
                println!("features: (none)");
            } else {
                println!("features:");
                for f in &fam.features {
                    println!("  - {} ({})", f.id, f.display_name);
                }
            }
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("finupdate-cli: booted image is not in KNOWN_FAMILIES");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_versions() -> ExitCode {
    let svc = service::global();
    let image = match svc.current_image().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match svc.list_versions(&image, 8).await {
        Ok(versions) if versions.is_empty() => {
            eprintln!("finupdate-cli: no versions found for {}", image);
            ExitCode::FAILURE
        }
        Ok(versions) => {
            for v in versions {
                println!(
                    "{}  {}  kernel={}",
                    v.date.format("%Y-%m-%d"),
                    v.version,
                    if v.kernel.is_empty() { "?" } else { &v.kernel }
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}

async fn cmd_tags() -> ExitCode {
    let svc = service::global();
    let image = match svc.current_image().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            return ExitCode::FAILURE;
        }
    };
    match svc.list_available_tags(&image).await {
        Ok(tags) => {
            for t in tags {
                println!("{}", t);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("finupdate-cli: {}", e);
            ExitCode::FAILURE
        }
    }
}
