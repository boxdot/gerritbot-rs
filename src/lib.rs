#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate ssh2;
extern crate futures;

use ssh2::Session;

use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use futures::stream::BoxStream;
use futures::sync::mpsc::channel;
use futures::{Future, Sink, Stream};

pub mod event;

use event::Event;

#[derive(Debug)]
pub enum StreamError {
    Io(io::Error),
    Parse(serde_json::Error),
}

impl From<io::Error> for StreamError {
    fn from(err: io::Error) -> StreamError {
        StreamError::Io(err)
    }
}

impl From<serde_json::Error> for StreamError {
    fn from(err: serde_json::Error) -> StreamError {
        StreamError::Parse(err)
    }
}

pub fn gerrit(host: String,
              port: u16,
              username: String,
              priv_key_path: PathBuf)
              -> BoxStream<Event, StreamError> {
    let hostport = format!("{}:{}", host, port);
    let mut session = Session::new().unwrap();

    let (mut tx, rx) = channel(1);
    thread::spawn(move || {
        // Connect to the local SSH server
        let tcp = TcpStream::connect(hostport).unwrap();
        session.handshake(&tcp).unwrap();

        // Try to authenticate
        session.userauth_pubkey_file(&username, None, &priv_key_path, None)
            .unwrap();

        let mut ssh_channel = session.channel_session().unwrap();
        ssh_channel.exec("gerrit stream-events").unwrap();

        let buf_channel = BufReader::new(ssh_channel);
        for line in buf_channel.lines() {
            match tx.send(line).wait() {
                Ok(s) => tx = s,
                Err(_) => break,
            }
        }
    });
    rx.then(|event| {
            // event from our channel cannot fail
            let json: String = event.unwrap()?;
            Ok(serde_json::from_str(&json)?)
        })
        .boxed()
}
