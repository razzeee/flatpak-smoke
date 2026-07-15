mod cli;
mod command;
mod installer;
mod output;
mod result;
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

    match cli.command {
        Commands::Doctor => {
            tools::doctor().context("doctor check failed")?;
            Ok(())
        }
        Commands::VerifyBundle(args) => verify::verify_bundle(args),
        Commands::VerifyRepo(args) => verify::verify_repo(args),
    }
}

fn init_tracing(verbose: bool) {
    let default_level = if verbose { "debug" } else { "warn" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = fmt().with_env_filter(filter).try_init();
}
