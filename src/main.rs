#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate ssh2;
extern crate futures;

use std::path::PathBuf;

use futures::{Future, Stream};

mod gerrit;

struct Args {
    hostname: String,
    port: u16,
    username: String,
    priv_key_path: PathBuf,
}

fn parse_args<Iter>(mut args: Iter) -> Result<Args, &'static str>
    where Iter: Iterator<Item = String>
{
    args.next();
    let hostname = args.next().ok_or("argument 'hostname' missing")?;
    let port = args.next().ok_or("argument 'port' is missing")?;
    let port: u16 = port.parse().map_err(|_| "cannot parse port")?;
    let username = args.next().ok_or("argument 'username' missing")?;
    let priv_key_path = args.next().ok_or("path to private key is missing")?;

    Ok(Args {
        hostname: hostname,
        port: port,
        username: username,
        priv_key_path: PathBuf::from(priv_key_path),
    })
}

const USAGE: &'static str = r#"
gerritbot <hostname> <port> <username> <priv_key_path>

Arguments:
    hostname        Gerrit hostname
    port            Gerrit port
    username        Username used to connect
    priv_key_path   Path to private key. Note: Due to the limitations of ssh2
                    only rsa and dsa are supported.
"#;

fn main() {
    let args = match parse_args(std::env::args()) {
        Ok(args) => args,
        Err(msg) => {
            println!("Error: {}\nUsage: {}", msg, USAGE);
            std::process::exit(1);
        }
    };

    let stream = gerrit::event_stream(args.hostname, args.port, args.username, args.priv_key_path);
    stream.for_each(|event| Ok(println!("{:?}", event)))
        .wait()
        .ok();
}
