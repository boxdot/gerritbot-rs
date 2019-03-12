use futures::future::lazy;
use futures::Stream as _;
use log::{error, info};
use serde::Deserialize;
use std::path::PathBuf;
use structopt::StructOpt;
use tokio;
use toml;

use gerritbot_spark as spark;

use spark::SparkClient as _;

#[derive(Debug, Deserialize, Clone)]
struct SparkConfig {
    bot_token: String,
    api_uri: String,
    webhook_url: String,
    listen_address: String,
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

    let client = spark::WebClient::new(
        spark_config.api_uri,
        spark_config.bot_token,
        Some(spark_config.webhook_url),
    )
    .unwrap_or_else(|e| {
        error!("Could not create spark client: {}", e);
        std::process::exit(1);
    });
    let endpoint_address = spark_config.listen_address.parse().unwrap_or_else(|e| {
        error!("failed to parse endpoint url: {}", e);
        std::process::exit(1);
    });

    tokio::run(lazy(move || {
        let stream = spark::webhook_event_stream(&endpoint_address);

        stream.for_each(move |post| {
            info!("got a post: {:?}", post);
            client.reply(&post.data.person_id, &format!("got post:\n```\n{:#?}\n```", post));
            Ok(())
        })
    }));
}
