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
    let endpoint_address: std::net::SocketAddr =
        spark_config.listen_address.parse().unwrap_or_else(|e| {
            error!("failed to parse endpoint url: {}", e);
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
                let spark::WebhookServer { messages, server } =
                    spark::start_webhook_server(&endpoint_address, client.clone());

                // consume messages
                let messages_future = messages.for_each(move |message| {
                    debug!("got a message: {:?}", message);
                    client
                        .send_message(
                            &message.person_email,
                            &format!("got post:\n```\n{:#?}\n```", message),
                        )
                        .map_err(|e| error!("failed to send message: {}", e))
                });

                // run server future and messages future
                server
                    .map_err(|e| error!("webhook server error: {}", e))
                    .select(messages_future)
                    // stop when the first future completes
                    .then(|_| Ok(()))
            })
    }));
}
