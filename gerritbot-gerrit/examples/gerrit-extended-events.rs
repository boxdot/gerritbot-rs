use std::borrow::Cow;
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

    let connect = || {
        gerrit::Connection::connect(
            format!("{}:{}", args.hostname, args.port),
            args.username.clone(),
            args.private_key_path.clone(),
        )
        .unwrap_or_else(|e| {
            error!("failed to connect to gerrit: {}", e);
            std::process::exit(1);
        })
    };

    let gerrit_stream = gerrit::extended_event_stream(connect(), connect(), |_| {
        Cow::Borrowed(&[
            gerrit::ExtendedInfo::SubmitRecords,
            gerrit::ExtendedInfo::InlineComments,
        ])
    });

    tokio::run(gerrit_stream.for_each(|event| {
        println!("{:#?}", event);
        Ok(())
    }));
}
