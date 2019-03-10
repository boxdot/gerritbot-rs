use std::path::PathBuf;

use futures::Future as _;
use log::error;
use structopt::StructOpt;
use tokio;

use gerritbot_gerrit as gerrit;

#[derive(StructOpt, Debug)]
struct Args {
    /// Gerrit username
    #[structopt(short = "u")]
    username: String,
    /// Gerrit hostname
    hostname: String,
    /// Gerrit SSH port
    #[structopt(short = "p", default_value = "29418")]
    port: u32,
    /// Path to SSH private key
    #[structopt(short = "i", parse(from_os_str))]
    private_key_path: PathBuf,
    /// Enable verbose output
    #[structopt(short = "v")]
    verbose: bool,
    query: String,
}

fn main() {
    let args = Args::from_args();
    stderrlog::new()
        .module(module_path!())
        .module("gerritbot_gerrit")
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(if args.verbose { 5 } else { 2 })
        .init()
        .unwrap();

    let connection = gerrit::GerritConnection::connect(
        format!("{}:{}", args.hostname, args.port),
        args.username,
        args.private_key_path,
    )
    .unwrap_or_else(|e| {
        error!("connection to gerrit failed: {}", e);
        std::process::exit(1);
    });

    let mut command_runner = gerrit::CommandRunner::new(connection).unwrap_or_else(|e| {
        error!("failed to create command runner: {}", e);
        std::process::exit(1);
    });

    tokio::run(
        command_runner
            .run_command(format!("gerrit query {}", args.query))
            .map_err(|e| error!("error running query: {}", e))
            .map(|output| println!("{}", output)),
    );
}
