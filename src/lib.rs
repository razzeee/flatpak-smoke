mod cli;
mod command;
mod installer;
mod output;
mod process;
mod result;
mod screenshot;
mod session;
mod tools;
mod verify;

use anyhow::Context;
use clap::Parser;
use cli::{Cli, Commands};
use tracing_subscriber::{EnvFilter, fmt};

pub use cli::{Cli as FlatpakSmokeCli, parse_duration};
pub use result::{Artifact, Failure, FailureReason, RunResult, RunStatus, Timings};

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    process::install_signal_handlers().context("installing cancellation handlers")?;

    match cli.command {
        Commands::Doctor(args) => {
            if args.desktop.is_some() {
                return screenshot::doctor();
            }
            tools::doctor().context("doctor check failed")?;
            Ok(())
        }
        Commands::VerifyBundle(args) => verify::verify_bundle(args),
        Commands::VerifyRepo(args) => verify::verify_repo(args),
        Commands::ScreenshotBundle(args) => screenshot::bundle(args),
        Commands::ScreenshotRepo(args) => screenshot::repo(args),
    }
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose { "debug" } else { "warn" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = fmt().with_env_filter(filter).try_init();
}
