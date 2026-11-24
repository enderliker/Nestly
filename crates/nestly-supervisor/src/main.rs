#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use nestly_core::{Config, Redactor};
use nestly_supervisor::{RunMode, Supervisor};

#[derive(Debug, Parser)]
#[command(
    name = "nestly-supervisor",
    version,
    about = "Supervisor for nestly services"
)]
struct Args {
    /// Path to the nestly configuration file.
    #[arg(long, default_value = "nestly.toml")]
    config: PathBuf,
    /// Start every service, wait until all are ready, print a report, and exit.
    #[arg(long)]
    boot_probe: bool,
    /// Validate the configuration and exit without starting anything.
    #[arg(long)]
    check: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // rustls reaches us through several dependencies; pick ring explicitly so
    // provider auto-detection never aborts.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();

    if let Err(source) = nestly_sys::harden_self() {
        eprintln!("warning: cannot fully harden the supervisor process: {source}");
    }

    let config = match Config::from_path(&args.config) {
        Ok(config) => config,
        Err(source) => {
            eprintln!("configuration error in {}: {source}", args.config.display());
            return ExitCode::FAILURE;
        }
    };

    if args.check {
        println!(
            "configuration ok: {} module(s), edge {}, boot budget {} ms",
            config.modules.len(),
            if config.supervisor.edge.enabled {
                "enabled"
            } else {
                "disabled"
            },
            config.supervisor.boot_deadline_ms
        );
        return ExitCode::SUCCESS;
    }

    let redactor = Arc::new(Redactor::new(config.redaction_secrets()));
    nestly_supervisor::logging::init(&config.supervisor.log_level, redactor.clone());
    let _ = rustls::crypto::ring::default_provider().install_default();

    let supervisor = match Supervisor::new(config, redactor) {
        Ok(supervisor) => supervisor,
        Err(source) => {
            tracing::error!(error = %source, "cannot initialize supervisor");
            return ExitCode::FAILURE;
        }
    };

    let mode = if args.boot_probe {
        RunMode::BootProbe
    } else {
        RunMode::Normal
    };

    match supervisor.run(mode).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            tracing::error!(error = %source, "supervisor stopped with an error");
            ExitCode::FAILURE
        }
    }
}
