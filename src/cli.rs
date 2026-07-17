use std::{path::PathBuf, time::Duration};

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "flatpak-smoke")]
#[command(about = "CI-friendly smoke verifier for Flatpak application artifacts")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    #[arg(long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Verify a .flatpak bundle produced by flatpak build-bundle.
    VerifyBundle(VerifyBundleArgs),
    /// Verify an app ref from a local Flatpak OSTree repository.
    VerifyRepo(VerifyRepoArgs),
    /// Check required host/container tooling.
    Doctor,
}

#[derive(Debug, Args)]
pub struct VerifyBundleArgs {
    pub bundle: PathBuf,

    #[command(flatten)]
    pub common: VerifyCommonArgs,
}

#[derive(Debug, Args)]
pub struct VerifyRepoArgs {
    pub repo: PathBuf,
    pub app_ref: String,

    #[command(flatten)]
    pub common: VerifyCommonArgs,
}

#[derive(Debug, Clone, Args)]
pub struct VerifyCommonArgs {
    #[arg(long)]
    pub output: PathBuf,

    #[arg(long)]
    pub force: bool,

    #[arg(long, value_parser = parse_duration, default_value = "30s")]
    pub window_timeout: Duration,

    #[arg(long, value_parser = parse_duration, default_value = "10s")]
    pub display_timeout: Duration,

    #[arg(long, value_parser = parse_duration, default_value = "10s")]
    pub screenshot_timeout: Duration,

    #[arg(long, value_parser = parse_duration, default_value = "60s")]
    pub overall_timeout: Duration,

    #[arg(long)]
    pub allow_network_remotes: bool,

    #[arg(long, default_value = "000-window-visible.png")]
    pub screenshot_name: String,
}

pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("duration cannot be empty".to_string());
    }

    if let Some(ms) = input.strip_suffix("ms") {
        return parse_positive_u64(ms, input).map(Duration::from_millis);
    }

    if let Some(seconds) = input.strip_suffix('s') {
        return parse_positive_u64(seconds, input).map(Duration::from_secs);
    }

    if let Some(minutes) = input.strip_suffix('m') {
        return parse_positive_u64(minutes, input).map(|value| Duration::from_secs(value * 60));
    }

    parse_positive_u64(input, input).map(Duration::from_secs)
}

fn parse_positive_u64(value: &str, original: &str) -> Result<u64, String> {
    let parsed = value.parse::<u64>().map_err(|_| {
        format!(
            "invalid duration '{original}'; use a positive integer with optional ms, s, or m suffix"
        )
    })?;

    if parsed == 0 {
        Err("duration must be greater than zero".to_string())
    } else {
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_duration_suffixes() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("15s").unwrap(), Duration::from_secs(15));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("7").unwrap(), Duration::from_secs(7));
    }

    #[test]
    fn rejects_zero_duration() {
        assert!(parse_duration("0s").is_err());
    }

    #[test]
    fn parses_verify_bundle_command() {
        let cli = Cli::parse_from([
            "flatpak-smoke",
            "verify-bundle",
            "app.flatpak",
            "--output",
            "out",
            "--force",
            "--window-timeout",
            "5s",
        ]);

        match cli.command {
            Commands::VerifyBundle(args) => {
                assert_eq!(args.bundle, PathBuf::from("app.flatpak"));
                assert_eq!(args.common.output, PathBuf::from("out"));
                assert!(args.common.force);
                assert_eq!(args.common.window_timeout, Duration::from_secs(5));
            }
            _ => panic!("expected verify-bundle"),
        }
    }
}
