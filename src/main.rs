extern crate chrono;
#[macro_use]
extern crate clap;
extern crate futures;
extern crate hyper;
extern crate hyper_native_tls;
extern crate iron;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate router;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;
extern crate sha2;
extern crate ssh2;
extern crate tokio_core;

use futures::Stream;
use futures::sync::mpsc::channel;
use iron::prelude::*;
use router::Router;

mod args;
mod bot;
mod gerrit;
mod spark;
mod utils;

fn main() {
    let args = args::parse_args();

    // event loop
    let mut core = tokio_core::reactor::Core::new().unwrap();

    // load or create a new bot
    let mut bot = match bot::Bot::load("state.json") {
        Ok(bot) => {
            println!(
                "[I] Loaded bot from 'state.json' with {} user(s).",
                bot.num_users()
            );
            bot
        }
        Err(err) => {
            println!("[W] Could not load bot from 'state.json': {:?}", err);
            bot::Bot::new(args.username.clone())
        }
    };

    // create spark message stream
    let spark_client = spark::SparkClient::new(&args);
    let remote = core.remote();
    let (tx, rx) = channel(1);
    let mut router = Router::new();
    router.post(
        "/",
        move |req: &mut Request| {
            println!("[D] new webhook post request");
            spark::webhook_handler(req, &remote, tx.clone())
        },
        "post",
    );

    let spark_stream = rx.filter(|msg| msg.person_id != spark_client.bot_id)
        .map(|mut msg| {
            println!("[D] loading text for message:\n{:?}", msg);
            if let Err(err) = msg.load_text(&spark_client) {
                println!("[E] Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .filter_map(|msg| msg.map(spark::Message::into_action));

    // start listening to the webhook
    // TODO: Add listening host port argument for webhook
    std::thread::spawn(|| {
        let mut iron = Iron::new(Chain::new(router));
        iron.threads = 2;
        iron.http("localhost:8888").unwrap()
    });

    // create gerrit event stream listener
    let gerrit_stream =
        gerrit::event_stream(&args.hostname, args.port, args.username, args.priv_key_path)
            .map(gerrit::Event::into_action)
            .map_err(|err| {
                println!("[E] Gerrit stream error {:?}", err);
            });

    // join spark and gerrit action stream into one and fold over actions with accumulator `bot`
    let handle = core.handle();
    let actions = spark_stream
        .select(gerrit_stream)
        .filter(|action| match *action {
            bot::Action::NoOp => false,
            _ => true,
        })
        .map(|action| {
            println!("[D] handle: {:?}", action);

            // fold over actions
            let old_bot = std::mem::replace(&mut bot, bot::Bot::default());
            let (new_bot, task) = bot::update(action, old_bot);
            std::mem::replace(&mut bot, new_bot);

            // Handle save task and return response.
            // Note: We have to do it here, since the value of `bot` is only available in this
            // function.
            if let Some(task) = task {
                println!("[D] new task {:?}", task);
                let response = match task {
                    bot::Task::Reply(response) => response,
                    bot::Task::ReplyAndSave(response) => {
                        let bot_clone = bot.clone();
                        handle.spawn_fn(move || {
                            if let Err(err) = bot_clone.save("state.json") {
                                println!("[E] Coult not save sate: {:?}", err);
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
            if let Some(response) = response {
                println!("[D] Replying with:\n{}", response.message);
                spark_client.reply(&response.person_id, &response.message);
            }
            Ok(())
        });

    let result = core.run(actions);
    if let Err(err) = result {
        print!("Shutting down due to error: {:?}", err);
    };
}
