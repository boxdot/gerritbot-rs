#[macro_use]
extern crate clap;
extern crate futures;
extern crate hyper;
extern crate hyper_native_tls;
extern crate iron;
#[macro_use]
extern crate log;
extern crate lru_time_cache;
extern crate router;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;
extern crate ssh2;
extern crate stderrlog;
extern crate tokio_core;

use futures::Stream;
use futures::sync::mpsc::channel;
use iron::prelude::*;
use router::Router;

#[macro_use]
mod utils;
mod args;
mod bot;
mod gerrit;
mod spark;

fn main() {
    let args = args::parse_args();
    stderrlog::new()
        .module(module_path!())
        .quiet(args.quiet)
        .timestamp(stderrlog::Timestamp::Second)
        .verbosity(args.verbosity)
        .init()
        .unwrap();
    info!("Starting");

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
    if args.bot_msg_expiration.as_secs() != 0 && args.bot_msg_capacity != 0 {
        debug!(
            "Approval LRU cache: capacity - {}, expiration - {} sec",
            args.bot_msg_capacity,
            args.bot_msg_expiration.as_secs()
        );
        bot.init_msg_cache(args.bot_msg_capacity, args.bot_msg_expiration);
    };

    // event loop
    let mut core = tokio_core::reactor::Core::new().unwrap();

    // create spark message stream
    let spark_client = spark::SparkClient::new(&args).unwrap_or_else(|err| {
        error!("Could not create spark client: {}", err);
        std::process::exit(1);
    });
    if let Some(spark_webhook_url) = args.spark_webhook_url {
        if let Err(err) = spark_client.replace_webhook_url(&spark_webhook_url) {
            error!("{}", err);
            std::process::exit(1);
        };
        info!("Registered Spark's webhook url: {}", spark_webhook_url)
    };

    let remote = core.remote();
    let (tx, rx) = channel(1);
    let mut router = Router::new();
    router.post(
        "/",
        move |req: &mut Request| {
            debug!("Incoming webhook post request");
            spark::webhook_handler(req, &remote, tx.clone())
        },
        "post",
    );
    info!("Listening to Spark on {}", args.spark_endpoint);

    let spark_stream = rx.filter(|msg| msg.person_id != spark_client.bot_id)
        .map(|mut msg| {
            debug!("Loading text for message: {:?}", msg);
            if let Err(err) = msg.load_text(&spark_client) {
                error!("Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .filter_map(|msg| msg.map(spark::Message::into_action));

    // start listening for incoming messages on the webhook
    let spark_endpoint = args.spark_endpoint.clone();
    std::thread::spawn(move || {
        let mut iron = Iron::new(Chain::new(router));
        iron.threads = 2;
        iron.http(&spark_endpoint).unwrap()
    });

    // create gerrit event stream listener
    let gerrit_stream = gerrit::event_stream(
        &args.gerrit_hostname,
        args.gerrit_port,
        args.gerrit_username,
        args.gerrit_priv_key_path,
    );

    // join spark and gerrit action stream into one and fold over actions with accumulator `bot`
    let handle = core.handle();
    let actions = spark_stream
        .select(gerrit_stream)
        .filter(|action| match *action {
            bot::Action::NoOp => false,
            _ => true,
        })
        .filter_map(|action| {
            debug!("Handle action: {:?}", action);

            // fold over actions
            let old_bot = std::mem::replace(&mut bot, bot::Bot::new());
            let (new_bot, task) = bot::update(action, old_bot);
            std::mem::replace(&mut bot, new_bot);

            // Handle save task and return response.
            // Note: We have to do it here, since the value of `bot` is only
            // available in this function.
            if let Some(task) = task {
                debug!("New task {:?}", task);
                let response = match task {
                    bot::Task::Reply(response) => response,
                    bot::Task::ReplyAndSave(response) => {
                        let bot_clone = bot.clone();
                        handle.spawn_fn(move || {
                            if let Err(err) = bot_clone.save("state.json") {
                                error!("Could not save state: {:?}", err);
                            }
                            Ok(())
                        });
                        response
                    }
                };
                return Some(response);
            }
            None
        })
        .for_each(|response| {
            debug!("Replying with: {}", response.message);
            spark_client.reply(&response.person_id, &response.message);
            Ok(())
        });

    let _ = core.run(actions);
}
