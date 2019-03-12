use std::path::PathBuf;
use std::rc::Rc;

use futures::Stream as _;
use log::{error, info};
use serde::Deserialize;
use structopt::StructOpt;
use tokio_core;
use toml;

use gerritbot_spark as spark;

use spark::SparkClient as _;

#[derive(Debug, Deserialize, Clone)]
struct SparkConfig {
    bot_token: String,
    api_uri: String,
    webhook_url: String,
    endpoint_url: String,
}

#[derive(StructOpt, Debug)]
struct Args {
    /// Config file
    #[structopt(short = "f")]
    config_file: PathBuf,
    /// Enable verbose output
    #[structopt(short = "v")]
    verbose: bool,
}

fn main() {
    let args = Args::from_args();
    stderrlog::new()
        .module(module_path!())
        .module("gerritbot_spark")
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(if args.verbose { 5 } else { 2 })
        .init()
        .unwrap();
    let spark_config: SparkConfig = std::fs::read(args.config_file)
        .map_err(|e| e.to_string())
        .and_then(|data| toml::from_slice(&data).map_err(|e| e.to_string()))
        .unwrap_or_else(|e| {
            error!("failed to read config file: {}", e);
            std::process::exit(1);
        });

    // event loop
    let mut core = tokio_core::reactor::Core::new().unwrap();

    let client = spark::WebClient::new(
        spark_config.api_uri,
        spark_config.bot_token,
        Some(spark_config.webhook_url),
    )
    .unwrap_or_else(|e| {
        error!("Could not create spark client: {}", e);
        std::process::exit(1);
    });
    let client = Rc::new(client);
    let stream =
        spark::webhook_event_stream(client.clone(), &spark_config.endpoint_url, core.remote())
            .unwrap_or_else(|e| {
                error!("Could not create spark stream: {}", e);
                std::process::exit(1)
            });

    core.run(stream.for_each(move |command_message| {
        info!("got a command: {:?}", command_message);
        client.reply(
            &command_message.sender_id,
            &format!("got command: {:?}", command_message.command),
        );
        Ok(())
    }))
    .unwrap_or_else(|e| error!("main loop exited: {}", e));
}
