// need to raise recursion limit because many combinators
#![recursion_limit = "128"]
#![deny(bare_trait_objects)]
#![allow(unused_imports)]

use std::convert::identity;
use std::rc::Rc;
use std::time::Duration;

use futures::{future, future::lazy, Future, Sink, Stream};
use log::{debug, error, info, warn};

use gerritbot_gerrit as gerrit;
use gerritbot_spark as spark;

mod args;
mod bot;

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
    stderrlog::new()
        .module(module_path!())
        .module("gerritbot_gerrit")
        .module("gerritbot_spark")
        .quiet(args.flag_quiet)
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(if args.flag_verbose { 5 } else { 2 })
        .init()
        .unwrap();
    let args::Config {
        gerrit: gerrit_config,
        bot: bot_config,
        spark: spark_config,
    } = args::parse_config(args.flag_config);

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
                .with_format_script(format_script)
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
        bot::Bot::request_extended_gerrit_info,
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

                let mut bot = bot_builder.build(gerrit_command_runner, spark_client);

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

    /*

    // create spark client and event stream listener
    // let spark_client_factory = get_spark_client_factory(spark_config.clone());
    // let (spark_webhook
    let spark_stream = match config.spark.mode.clone() {
        args::ModeConfig::Direct { endpoint } => {
            spark::webhook_event_stream(spark_client, &endpoint, core.remote())
        }
        args::ModeConfig::Sqs { uri, region } => spark::sqs_event_stream(spark_client, uri, region),
    };

    let spark_stream = spark_stream.unwrap_or_else(|err| {
        error!("Could not start listening to spark: {}", err);
        std::process::exit(1);
    });

    // create gerrit event stream listener
    let gerrit_config = &config.gerrit.clone();

    let (gerrit_change_id_sink, gerrit_change_response_stream) =
        gerrit::change_sink(connect_to_gerrit());

    let gerrit_stream = gerrit::event_stream(connect_to_gerrit());

    // join spark and gerrit action streams into one and fold over actions with accumulator `bot`
    let spark_client = spark_client_from_config(config.spark.clone());
    let handle = core.handle();
    let actions = spark_stream
        .map(|command| bot::Bot::handle_command(command))
        .select(gerrit_stream.map(|event| bot::Bot::handle_gerrit_event(event)))
        .filter_map(identity)
        .select(gerrit_change_response_stream.map(
            |gerrit::ChangeDetails {
                 user,
                 message,
                 change,
             }| bot::Action::ChangeFetched(user, message, change),
        ))
        .filter_map(move |action| {
            debug!("Handle action: {:#?}", action);

            // fold over actions
            let old_bot = std::mem::replace(&mut bot, bot::Bot::new());
            let (new_bot, task) = bot::update(action, old_bot);
            std::mem::replace(&mut bot, new_bot);

            // Handle save task and return response.
            // Note: We have to do it here, since the value of `bot` is only
            // available in this function.
            if let Some(task) = task {
                debug!("New task {:#?}", task);
                let response = match task {
                    bot::Task::Reply(response) => Some(response),
                    bot::Task::ReplyAndSave(response) => {
                        let bot_clone = bot.clone();
                        handle.spawn_fn(move || {
                            if let Err(err) = bot_clone.save("state.json") {
                                error!("Could not save state: {:?}", err);
                            }
                            Ok(())
                        });
                        Some(response)
                    }
                    bot::Task::FetchComments(user, change, message) => {
                        handle.spawn(
                            gerrit_change_id_sink
                                .clone()
                                .send((user, change, message))
                                .map_err(|e| {
                                    error!("Could not fetch comments: {}", e);
                                })
                                .then(|_| Ok(())),
                        );
                        None
                    }
                };
                return response;
            }
            None
        })
        .or_else(|err| {
            error!("Exit due to error: {:?}", err);
            Err(())
        })
        .for_each(|response| {
            debug!("Replying with: {}", response.message);
            spark_client.reply(&response.person_id, &response.message);
            Ok(())
        });

    let _ = core.run(actions);
    */
}
