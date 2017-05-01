extern crate futures;
extern crate hyper;
extern crate hyper_native_tls;
extern crate iron;
extern crate router;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate ssh2;

use std::sync::{Arc, Mutex};

use futures::{Future, Stream};
use iron::prelude::*;
use router::Router;

mod args;
mod bot;
mod gerrit;
mod spark;

const USAGE: &'static str = r#"
gerritbot <hostname> <port> <username> <priv_key_path> <bot_token>

Arguments:
    hostname        Gerrit hostname
    port            Gerrit port
    username        Username used to connect
    priv_key_path   Path to private key. Note: Due to the limitations of ssh2
                    only RSA and DSA are supported.
    bot_token       Token of the Spark bot for authentication.
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
    let args_clone = args.clone();
    router.post("/",
                move |r: &mut Request| spark::handle_post_webhook(r, args.clone(), bot.clone()),
                "post");
    // TODO: Do we really need a thread? How about a task in a event loop?
    std::thread::spawn(|| Iron::new(Chain::new(router)).http("localhost:8888").unwrap());

    // create gerrit event stream listener
    let stream = gerrit::event_stream(args_clone.hostname,
                                      args_clone.port,
                                      args_clone.username,
                                      args_clone.priv_key_path);
    stream.for_each(|event| Ok(println!("{:?}", event)))
        .wait()
        .ok();
}
