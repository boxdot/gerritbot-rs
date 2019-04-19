// need to raise recursion limit because many combinators
#![recursion_limit = "128"]
#![deny(bare_trait_objects)]

use std::time::Duration;

use futures::{future, future::lazy, Future, Stream};
use log::{debug, error, info, warn};

use gerritbot as bot;
use gerritbot::args;
use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

/// Create spark message stream. Returns a future representing a webhook server
/// and a stream of messages.
fn create_spark_message_stream(
    spark_config: args::SparkConfig,
    spark_client: spark::Client,
) -> (
    impl Future<Item = (), Error = ()>,
    Box<dyn Stream<Item = spark::Message, Error = ()> + Send>,
) {
    match spark_config.mode {
        args::ModeConfig::Direct {
            endpoint: listen_address,
        } => {
            let spark::WebhookServer { server, messages } =
                spark::start_webhook_server(&listen_address, spark_client);
            (
                future::Either::A(server.map_err(|e| error!("webhook server error: {}", e))),
                Box::new(messages),
            )
        }
        args::ModeConfig::Sqs { uri, region } => (
            future::Either::B(future::empty()),
            Box::new(spark::sqs_event_stream(uri, region, spark_client)),
        ),
    }
}

fn main() {
    let args = args::parse_args();

    if args.dump_format_script {
        print!("{}", bot::DEFAULT_FORMAT_SCRIPT);
        return;
    }

    stderrlog::new()
        .module(module_path!())
        .module("gerritbot_gerrit")
        .module("gerritbot_spark")
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(match (args.quiet, args.verbose) {
            (true, _) => 0,      // ERROR
            (false, false) => 2, // INFO
            (_, true) => 4,      // TRACE
        })
        .init()
        .unwrap();
    let args::Config {
        gerrit: gerrit_config,
        bot: bot_config,
        spark: spark_config,
    } = args::parse_config(args.config);

    // load or create a new bot
    let bot_state = bot::State::load("state.json")
        .map(|state| {
            info!(
                "Loaded bot from 'state.json' with {} user(s).",
                state.num_users()
            );
            state
        })
        .unwrap_or_else(|err| {
            warn!("Could not load bot from 'state.json': {:?}", err);
            bot::State::new()
        });

    let bot_builder = bot::Builder::new(bot_state);
    let bot_builder = {
        if bot_config.msg_expiration != 0 && bot_config.msg_capacity != 0 {
            debug!(
                "Approval LRU cache: capacity - {}, expiration - {} sec",
                bot_config.msg_capacity, bot_config.msg_expiration
            );
            bot_builder.with_msg_cache(
                bot_config.msg_capacity,
                Duration::from_secs(bot_config.msg_expiration),
            )
        } else {
            bot_builder
        }
    };
    let bot_builder = {
        if let Some(format_script) = bot_config.format_script {
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
    let connect_to_gerrit = || {
        info!(
            "Connecting to gerrit with username {} at {}",
            gerrit_config.username, gerrit_config.host
        );
        gerrit::Connection::connect(
            gerrit_config.host.clone(),
            gerrit_config.username.clone(),
            gerrit_config.priv_key_path.clone(),
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

    // run rest of the logic while the tokio runtime is running
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
            .and_then(move |spark_client| {
                let (spark_webhook_server, spark_messages) =
                    create_spark_message_stream(spark_config.clone(), spark_client.clone());

                let bot = bot_builder.build(gerrit_command_runner, spark_client);

                fn ignore<T>(_: T) {}

                // run webhook server or bot to completion - they should never
                // exit unless there's an error, in which case they should print
                // that
                spark_webhook_server
                    .select(bot.run(gerrit_event_stream, spark_messages))
                    .map(ignore)
                    .map_err(ignore)
            })
    }))
}
