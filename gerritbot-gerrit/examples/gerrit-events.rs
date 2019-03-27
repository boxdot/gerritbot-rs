use std::path::PathBuf;

use futures::Stream as _;
use log::error;
use structopt::StructOpt;

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

    let connection = gerrit::Connection::connect(
        format!("{}:{}", args.hostname, args.port),
        args.username,
        args.private_key_path,
    )
    .unwrap_or_else(|e| {
        error!("failed to connect to gerrit: {}", e);
        std::process::exit(1);
    });

    let gerrit_stream = gerrit::event_stream(connection);

    tokio::run(
        gerrit_stream
            .for_each(|event| {
                println!("{:#?}", event);
                Ok(())
            }),
    );
}
