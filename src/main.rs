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

use std::mem;
use std::sync::{Arc, Mutex};

use futures::{Future, Stream};
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

    // create new thread-shareable bot
    let bot = Arc::new(Mutex::new(bot::Bot::new()));

    // create spark post webhook handler
    let mut router = Router::new();
    let bot_for_spark_handler = bot.clone();
    let spark_client = spark::SparkClient::new(&args);
    router.post("/",
                move |r: &mut Request| {
                    spark::handle_post_webhook(r, &spark_client, bot_for_spark_handler.clone())
                },
                "post");
    // TODO: Do we really need a thread? How about a task in a event loop?
    std::thread::spawn(|| Iron::new(Chain::new(router)).http("localhost:8888").unwrap());

    // create gerrit event stream listener
    // TODO: I have to create the client again, since it was moved above. Why was it moved? It
    // should have been captured by reference. Is it because it was moved in a different thread?
    let spark_client = spark::SparkClient::new(&args);
    let stream = gerrit::event_stream(&args.hostname, args.port, args.username, args.priv_key_path);
    stream.map(gerrit::Event::into_action)
        .filter(|action| match *action {
            bot::Action::Unknown(_) => false,
            _ => true,
        })
        .for_each(|action| {
            let mut bot_guard = bot.lock().unwrap();
            let bot = &mut (*bot_guard);

            // fold over actions
            let old_bot = mem::replace(bot, bot::Bot::new());
            let (new_bot, response) = bot::update(action, old_bot);
            mem::replace(bot, new_bot);

            println!("[D] New state: {:?}", bot);
            if let Some(response) = response {
                println!("[D] {:?}", response);
                spark_client.reply(&response.person_id, &response.message);
            }

            Ok(())
        })
        .wait()
        .ok();
}
