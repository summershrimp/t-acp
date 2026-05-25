mod adapters;
mod api;
mod daemon;
mod http;
mod pty;
mod util;
mod wrapper;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use std::env;
use std::ffi::OsString;
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

pub const DEFAULT_ADDR: &str = "127.0.0.1:48974";

fn main() -> ExitCode {
    init_tracing();

    match run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("t-acp: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<u8> {
    let cli = Cli::parse();
    let default_addr = env::var("T_ACP_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    match cli.command {
        None => {
            Cli::command().print_help()?;
            eprintln!();
            Ok(0)
        }
        Some(CommandKind::Daemon { addr }) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("failed to build Tokio runtime")?;
            runtime.block_on(daemon::run(addr.as_deref().unwrap_or(&default_addr)))?;
            Ok(0)
        }
        Some(CommandKind::Agent(args)) => wrapper::run(os_args_to_strings(args)?, &default_addr),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

fn os_args_to_strings(args: Vec<OsString>) -> Result<Vec<String>> {
    args.into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| anyhow::anyhow!("agent arguments must be valid UTF-8"))
        })
        .collect::<Result<Vec<_>>>()
        .and_then(|args| {
            if args.is_empty() {
                bail!("missing agent command")
            } else {
                Ok(args)
            }
        })
        .context("failed to parse agent command")
}

#[derive(Debug, Parser)]
#[command(name = "t-acp", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    Daemon {
        #[arg(long)]
        addr: Option<String>,
    },
    #[command(external_subcommand)]
    Agent(Vec<OsString>),
}
