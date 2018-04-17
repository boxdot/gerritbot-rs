extern crate docopt;
extern crate futures;
extern crate hyper;
extern crate hyper_native_tls;
extern crate iron;
extern crate itertools;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate lru_time_cache;
extern crate regex;
extern crate rlua;
extern crate router;
extern crate rusoto_core;
extern crate rusoto_sqs;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;
extern crate serde_yaml;
extern crate ssh2;
extern crate stderrlog;
extern crate tokio_core;

use futures::{Future, Sink, Stream};
use std::time::Duration;
use std::rc::Rc;

#[macro_use]
mod utils;
mod args;
mod bot;
mod gerrit;
mod spark;
mod sqs;

use spark::SparkClient;

fn spark_client_from_config(spark_config: args::SparkConfig) -> Rc<SparkClient> {
    if !spark_config.bot_token.is_empty() {
        Rc::new(
            spark::WebClient::new(
                spark_config.api_uri,
                spark_config.bot_token,
                spark_config.webhook_url,
            ).unwrap_or_else(|err| {
                error!("Could not create spark client: {}", err);
                std::process::exit(1);
            }),
        )
    } else {
        warn!("Using console as Spark client due to empty bot_token.");
        Rc::new(spark::ConsoleClient::new())
    }
}

fn main() {
    let args = args::parse_args();
    stderrlog::new()
        .module(module_path!())
        .quiet(args.flag_quiet)
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(if args.flag_verbose { 5 } else { 2 })
        .init()
        .unwrap();
    let config = args::parse_config(args.flag_config);

    // load or create a new bot
    let mut bot = match bot::Bot::load("state.json") {
        Ok(bot) => {
            info!(
                "Loaded bot from 'state.json' with {} user(s).",
                bot.num_users()
            );
            bot
        }
        Err(err) => {
            warn!("Could not load bot from 'state.json': {:?}", err);
            bot::Bot::new()
        }
    };
    if config.bot.msg_expiration != 0 && config.bot.msg_capacity != 0 {
        debug!(
            "Approval LRU cache: capacity - {}, expiration - {} sec",
            config.bot.msg_capacity, config.bot.msg_expiration
        );
        bot.init_msg_cache(
            config.bot.msg_capacity,
            Duration::from_secs(config.bot.msg_expiration),
        );
    };

    // event loop
    let mut core = tokio_core::reactor::Core::new().unwrap();

    // create spark client and event stream listener
    let spark_client = spark_client_from_config(config.spark.clone());
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
    let gerrit_config = config.gerrit.clone();
    info!(
        "(Re)connecting to Gerrit over ssh: {}",
        gerrit_config.host
    );

    let (gerrit_change_id_sink, gerrit_change_response_stream) = gerrit::change_sink(
        gerrit_config.host.clone(),
        gerrit_config.username.clone(),
        gerrit_config.priv_key_path.clone(),
    ).unwrap_or_else(|err| {
        error!(
            "Could not connect to Gerrit via SSH for sending commands: {}",
            err
        );
        std::process::exit(1);
    });

    let gerrit_stream = gerrit::event_stream(
        gerrit_config.host,
        gerrit_config.username,
        gerrit_config.priv_key_path,
    );

    // join spark and gerrit action streams into one and fold over actions with accumulator `bot`
    let spark_client = spark_client_from_config(config.spark.clone());
    let handle = core.handle();
    let actions = spark_stream
        .select(gerrit_stream)
        .select(gerrit_change_response_stream)
        .filter(|action| match *action {
            bot::Action::NoOp => false,
            _ => true,
        })
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
}
