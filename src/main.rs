extern crate chrono;
extern crate futures;
extern crate hyper;
extern crate hyper_native_tls;
extern crate iron;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate router;
extern crate rustc_serialize;
extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate serde_json;
extern crate sha2;
extern crate ssh2;
extern crate tokio_core;

use std::cell::Cell;

use futures::Stream;
use futures::sync::mpsc::channel;
use iron::prelude::*;
use router::Router;

mod args;
mod bot;
mod gerrit;
mod spark;
mod utils;

const USAGE: &'static str = r#"
gerritbot <hostname> <port> <username> <priv_key_path> <bot_token>

Arguments:
    hostname        Gerrit hostname
    port            Gerrit port
    username        Username used to connect
    priv_key_path   Path to private key. Note: Due to the limitations of ssh2
                    only RSA and DSA are supported.
    bot_token       Token of the Spark bot for authentication.
    bot_id          Identity of the Spark bot for filtering own messages.
"#;

fn main() {
    let args = match args::parse_args(std::env::args()) {
        Ok(args) => args,
        Err(msg) => {
            println!("Error: {}\nUsage: {}", msg, USAGE);
            std::process::exit(1);
        }
    };

    // event loop
    let mut core = tokio_core::reactor::Core::new().unwrap();

    // create new thread-shareable bot
    let bot = Cell::new(bot::Bot::new());

    // create spark message stream
    let spark_client = spark::SparkClient::new(&args);
    let remote = core.remote();
    let (tx, rx) = channel(1);
    let mut router = Router::new();
    router.post("/",
                move |req: &mut Request| {
                    println!("[D] new webhook post request");
                    spark::webhook_handler(req, &remote, tx.clone())
                },
                "post");

    let spark_stream = rx.filter(|msg| msg.person_id != spark_client.bot_id)
        .map(|mut msg| {
            println!("[D] loading message text");
            if let Err(err) = msg.load_text(&spark_client) {
                println!("[E] Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .filter_map(|msg| msg.map(spark::Message::into_action));

    // start listening to the webhook
    std::thread::spawn(|| Iron::new(Chain::new(router)).http("localhost:8888").unwrap());

    // create gerrit event stream listener
    let gerrit_stream =
        gerrit::event_stream(&args.hostname, args.port, args.username, args.priv_key_path)
            .map(gerrit::Event::into_action)
            .map_err(|err| {
                println!("[E] {:?}", err);
            });

    // join spark and gerrit action stream into one and fold over actions with accumulator `bot`
    let actions = spark_stream.select(gerrit_stream)
        .filter(|action| match *action {
            bot::Action::NoOp => false,
            _ => true,
        })
        .map(|action| {
            println!("[D] handle: {:?}", action);

            // fold over actions
            let old_bot = bot.replace(bot::Bot::new());
            let (new_bot, response) = bot::update(action, old_bot);
            bot.replace(new_bot);

            response
        })
        .for_each(|response| {
            if let Some(response) = response {
                println!("[D] {:?}", response);
                spark_client.reply(&response.person_id, &response.message);
            }

            Ok(())
        });

    core.run(actions).unwrap();
}
