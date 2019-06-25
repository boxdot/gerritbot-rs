use std::path::PathBuf;

use futures::Future as _;
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
    query: String,
}

fn main() {
    env_logger::init_from_env(
        env_logger::Env::default()
            .filter_or(
                "GERRITBOT_LOG",
                concat!(module_path!(), "=info,gerritbot_gerrit=info"),
            )
    );
    let args = Args::from_args();

    let connection = gerrit::Connection::connect(
        format!("{}:{}", args.hostname, args.port),
        args.username,
        args.private_key_path,
    )
    .unwrap_or_else(|e| {
        error!("connection to gerrit failed: {}", e);
        std::process::exit(1);
    });

    let mut command_runner = gerrit::CommandRunner::new(connection);

    tokio::run(
        command_runner
            .run_command(format!("gerrit query {}", args.query))
            .map_err(|e| error!("error running query: {}", e))
            .map(|output| println!("{}", output)),
    );
}
