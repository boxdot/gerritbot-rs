// need to raise recursion limit because many combinators
#![recursion_limit = "128"]

use futures::future::lazy;
use futures::{Future as _, Stream as _};
use log::{debug, error, info};
use serde::Deserialize;
use std::path::PathBuf;
use structopt::StructOpt;

use gerritbot_spark as spark;

#[derive(Debug, Deserialize, Clone)]
struct SparkConfig {
    bot_token: String,
    api_uri: String,
    webhook_url: String,
    sqs_url: String,
    sqs_region: String,
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
    let sqs_region: rusoto_core::Region = spark_config.sqs_region.parse().unwrap_or_else(|e| {
            error!("invalid sqs_region: {}", e);
            std::process::exit(1);
    });

    tokio::run(lazy(move || {
        let webhook_url = spark_config.webhook_url.clone();

        spark::Client::new(spark_config.api_uri.clone(), spark_config.bot_token.clone())
            .map_err(|e| error!("failed to create spark client: {}", e))
            .and_then(move |client| {
                info!("created spark client: {}", client.id());

                let next_client = client.clone();

                client
                    .register_webhook(&webhook_url)
                    .map_err(|e| error!("failed to register webhook: {}", e))
                    .map(move |()| next_client)
            })
            .and_then(move |client| {
                spark::sqs_event_stream(
                    spark_config.sqs_url.clone(),
                    sqs_region,
                    client.clone(),
                )
                .for_each(move |message| {
                    debug!("got a message: {:?}", message);
                    client
                        .send_message(
                            &message.person_email,
                            &format!("got post:\n```\n{:#?}\n```", message),
                        )
                        .map_err(|e| error!("failed to send message: {}", e))
                })
            })
    }));
}
