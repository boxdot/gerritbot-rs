// need to raise recursion limit because many combinators
#![recursion_limit = "128"]
#![deny(bare_trait_objects)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;

use structopt::StructOpt;

use futures::{future, stream, Future as _, Sink as _, Stream as _};
use log::{error, info};

use gerritbot as bot;
use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

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
    /// Gerrit user email address
    email: String,
    #[structopt(long = "format-script")]
    format_script: Option<String>,
}

#[derive(Clone)]
struct ConsoleSparkClient;

impl bot::SparkClient for ConsoleSparkClient {
    type ReplyFuture = future::FutureResult<(), spark::Error>;
    fn reply(&self, person_id: &spark::PersonId, msg: &str) -> Self::ReplyFuture {
        // Write synchronously and crash if writing fails. There's no point in
        // error handling here.
        write!(std::io::stdout(), "{}: {}\n", person_id, msg).expect("writing to stdout failed");
        future::ok(())
    }
}

fn main() {
    let args = Args::from_args();
    stderrlog::new()
        .module(module_path!())
        .module("gerritbot")
        .module("gerritbot_gerrit")
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(if args.verbose { 5 } else { 2 })
        .init()
        .unwrap();

    let connect_to_gerrit = || {
        info!(
            "Connecting to gerrit with username {} at {}:{}",
            args.username, args.hostname, args.port,
        );
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
    let gerrit_event_stream = gerrit::extended_event_stream(
        connect_to_gerrit(),
        connect_to_gerrit(),
        bot::request_extended_gerrit_info,
    );
    let gerrit_command_runner = gerrit::CommandRunner::new(connect_to_gerrit());
    let bot_builder = bot::Builder::new(bot::State::new());
    let bot_builder = {
        if let Some(format_script) = args.format_script {
            bot_builder
                .with_format_script(format_script)
                .unwrap_or_else(|err| {
                    error!("Failed to set format script: {:?}", err);
                    std::process::exit(1);
                })
        } else {
            bot_builder
        }
    };
    let email = args.email.clone();
    let (stdin_lines_sender, stdin_lines) = futures::sync::mpsc::channel::<String>(1);
    std::thread::spawn(move || {
        stream::iter_ok::<_, ()>(
            BufReader::new(std::io::stdin())
                .lines()
                .filter_map(Result::ok),
        )
        .forward(stdin_lines_sender.sink_map_err(|e| error!("sink error: {}", e)))
        .wait()
    });
    let console_spark_messages = stream::once(Ok("enable\n".to_string()))
        .chain(stdin_lines)
        .filter_map(|line| Some(line).filter(|line| !line.is_empty()))
        .map(move |line| {
            spark::Message::test_message(
                spark::Email::new(email.clone()),
                spark::PersonId::new(email.clone()),
                line,
            )
        });

    let bot = bot_builder.build(gerrit_command_runner, ConsoleSparkClient);
    tokio::run(bot.run(gerrit_event_stream, console_spark_messages));
}
