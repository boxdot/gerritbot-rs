// need to raise recursion limit because many combinators
#![recursion_limit = "128"]
#![deny(bare_trait_objects)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;

use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use structopt::StructOpt;

use futures::{future, stream, sync::mpsc::channel, Future as _, Sink as _, Stream as _};
use log::{error, info, warn};

use gerritbot as bot;
use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

#[derive(StructOpt, Debug)]
#[structopt(name = "gerritbot-console", rename_all = "kebab-case")]
/// Run the gerritbot without actually connecting to Spark. Instead the
/// program's stdin can be used to simulate sending messages. Replies will be
/// sent to stdout. Log messages will only appear on stderr. If the --email
/// option (see below) is given each input line will be treated as message from
/// that user. Otherwise each line has to be prefixed with the email address of
/// the intended sender, a colon (:) and an optional space character.
struct Args {
    /// Gerrit bot username
    ///
    /// This is the username the bot uses to connect to the Gerrit server.
    #[structopt(short, long)]
    username: String,
    /// Gerrit hostname
    ///
    /// Address of the Gerrit server.
    hostname: String,
    /// Gerrit SSH port
    #[structopt(short, long, default_value = "29418")]
    port: u32,
    /// Path to SSH private key
    #[structopt(short, long, parse(from_os_str))]
    identity_file: PathBuf,
    /// User email address
    ///
    /// If given input messages will be treated as if coming from this user.
    /// Additionally, an "enable" message will be injected before the first
    /// input line.
    ///
    /// This option is mutually exclusive with the --json option.
    #[structopt(short, long, conflicts_with = "json")]
    email: Option<String>,
    /// JSON mode
    ///
    /// Switch input and output to JSON. This is option mutually exclusive with
    /// the --email option.
    #[structopt(long)]
    json: bool,
    /// Lua formatting script
    ///
    /// Can be used to change or test the formatting of messages. If not present
    /// the internal default format script will be used.
    #[structopt(long = "format-script")]
    format_script: Option<String>,
    #[structopt(short = "C", hidden = true)]
    working_directory: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct SimpleInputMessage {
    email: spark::Email,
    text: String,
}

impl From<SimpleInputMessage> for spark::Message {
    fn from(msg: SimpleInputMessage) -> Self {
        let SimpleInputMessage { email, text } = msg;
        Self {
            person_email: email,
            text,
            ..Default::default()
        }
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct SimpleOutputMessage {
    email: spark::Email,
    text: String,
}

#[derive(Clone)]
enum ConsoleSparkClient {
    Plain,
    Json,
}

impl bot::SparkClient for ConsoleSparkClient {
    type ReplyFuture = future::FutureResult<(), spark::Error>;
    fn send_message(&self, email: &spark::EmailRef, msg: &str) -> Self::ReplyFuture {
        // Write synchronously and crash if writing fails. There's no point in
        // error handling here.
        match self {
            ConsoleSparkClient::Plain => {
                writeln!(std::io::stdout(), "{}: {}", email, msg).expect("writing to stdout failed")
            }
            ConsoleSparkClient::Json => {
                let message = SimpleOutputMessage {
                    email: email.to_owned(),
                    text: msg.to_string(),
                };
                serde_json::to_writer(std::io::stdout(), &message)
                    .expect("writing JSON to stdout failed");
                std::io::stdout()
                    .write_all(b"\n")
                    .expect("writing to stdout failed");
            }
        }
        future::ok(())
    }
}

fn main() {
    env_logger::init_from_env(env_logger::Env::default().filter_or(
        "GERRITBOT_LOG",
        concat!(module_path!(), "=info,gerritbot=info,gerritbot_gerrit=info"),
    ));
    let args = Args::from_args();

    if let Some(working_directory) = args.working_directory.as_ref() {
        info!("Changing current directory to {:?}", working_directory);
        std::env::set_current_dir(working_directory).expect("failed to set working directory");
    }

    let connect_to_gerrit = || {
        info!(
            "Connecting to gerrit with username {} at {}:{}",
            args.username, args.hostname, args.port,
        );
        gerrit::Connection::connect(
            format!("{}:{}", args.hostname, args.port),
            args.username.clone(),
            args.identity_file.clone(),
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
                .with_format_script(&format_script)
                .unwrap_or_else(|err| {
                    error!("Failed to set format script: {:?}", err);
                    std::process::exit(1);
                })
        } else {
            bot_builder
        }
    };
    let (stdin_lines_sender, stdin_lines) = channel(1);
    std::thread::spawn(move || {
        stream::iter_ok::<_, ()>(
            BufReader::new(std::io::stdin())
                .lines()
                .filter_map(Result::ok),
        )
        .forward(stdin_lines_sender.sink_map_err(|e| error!("sink error: {}", e)))
        .wait()
    });

    // If we have an email, send an enable message.
    let maybe_enable_message = stream::iter_ok(args.email.as_ref().map(|_| "enable".to_string()));
    let stdin_lines = maybe_enable_message.chain(stdin_lines);

    let email = args.email.clone();
    let use_json = args.json;

    let message_from_line = move |line: String| {
        if use_json {
            serde_json::from_str::<SimpleInputMessage>(&line)
                .map_err(|e| error!("failed to parse message: {}", e))
                .map(Into::into)
                .ok()
        } else if let Some(email) = &email {
            // If we have an email, send each line from this email.
            Some(spark::Message {
                person_email: spark::Email::new(email.clone()),
                text: line,
                ..Default::default()
            })
        } else {
            // If no email was given, parse it from each line.
            lazy_static! {
                static ref LINE_REGEX: Regex =
                    Regex::new(r"^(?P<email>.*): ?(?P<message>.*)$").unwrap();
            };

            LINE_REGEX
                .captures(&line)
                .map(|captures| {
                    let email = captures.name("email").unwrap().as_str();
                    let message = captures.name("message").unwrap().as_str();
                    spark::Message {
                        person_email: spark::Email::new(email.to_string()),
                        text: message.to_string(),
                        ..Default::default()
                    }
                })
                .or_else(|| {
                    warn!(r#"input not understood: please send as "<email>: <message>""#);
                    None
                })
        }
    };

    let spark_messages = stdin_lines
        .filter(|line| !line.is_empty())
        .filter_map(message_from_line);
    let spark_client = if use_json {
        ConsoleSparkClient::Json
    } else {
        ConsoleSparkClient::Plain
    };

    let bot = bot_builder.build(gerrit_command_runner, spark_client);
    tokio::run(bot.run(gerrit_event_stream, spark_messages));
}
