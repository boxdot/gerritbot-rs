use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use ssh2;
use serde_json;

use futures::stream::BoxStream;
use futures::sync::mpsc::{channel, Sender};
use futures::{Future, Sink, Stream};

use bot;

/// Gerrit username
pub type Username = String;

#[derive(Deserialize, Debug)]
pub struct User {
    pub name: Option<String>,
    pub username: Username,
    pub email: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    #[serde(rename = "type")]
    pub approval_type: String,
    pub description: String,
    pub value: String,
    pub old_value: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PatchSet {
    pub number: String,
    pub revision: String,
    pub parents: Vec<String>,
    #[serde(rename = "ref")]
    pub reference: String,
    pub uploader: User,
    pub created_on: u32,
    pub author: User,
    pub is_draft: bool,
    pub kind: String,
    pub size_insertions: i32,
    pub size_deletions: i32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub project: String,
    pub branch: String,
    pub id: String,
    pub number: String,
    pub subject: String,
    pub owner: User,
    pub url: String,
    pub commit_message: String,
    pub status: String,
}

#[derive(Deserialize, Debug, Eq, PartialEq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ChangeKey {
    pub id: String,
}

// Only specific events are accepted by this type by design!
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub author: Option<User>,
    pub uploader: Option<User>,
    pub approvals: Option<Vec<Approval>>,
    pub comment: Option<String>,
    #[serde(rename = "patchSet")]
    pub patchset: PatchSet,
    pub change: Change,
    pub project: String,
    #[serde(rename = "refName")]
    pub ref_name: String,
    #[serde(rename = "changeKey")]
    pub changekey: ChangeKey,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(rename = "eventCreatedOn")]
    pub created_on: u32,
}

impl Event {
    pub fn into_action(self) -> bot::Action {
        if self.approvals.is_some() {
            return bot::Action::UpdateApprovals(self);
        }
        bot::Action::NoOp
    }
}

#[derive(Debug)]
pub enum StreamError {
    Io(io::Error),
    Parse(serde_json::Error),
    Terminated,
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

fn get_pub_key_path(priv_key_path: &PathBuf) -> PathBuf {
    let mut pub_key_path = PathBuf::from(priv_key_path.to_str().unwrap());
    pub_key_path.set_extension("pub");
    pub_key_path
}

fn send_terminate_msg(tx: Sender<Result<String, ()>>) {
    // we terminate our stream by sending an empty err message
    let _ = tx.send(Err(())).wait();
}

pub fn event_stream(
    host: &str,
    port: u16,
    username: String,
    priv_key_path: PathBuf,
) -> BoxStream<Event, StreamError> {
    let hostport = format!("{}:{}", host, port);
    let pub_key_path = get_pub_key_path(&priv_key_path);
    println!(
        "[D] Will use public key: {}",
        pub_key_path.to_str().unwrap()
    );

    let (main_tx, rx) = channel(1);
    thread::spawn(move || {
        loop {
            println!("[I] (Re)connecting to Gerrit over ssh: {}", hostport);

            // Connect to the local SSH server
            let mut session = match ssh2::Session::new() {
                Some(session) => session,
                None => {
                    println!("[E] Could not create a new ssh session for connecting to Gerrit.");
                    send_terminate_msg(main_tx);
                    return;
                }
            };

            let tcp = match TcpStream::connect(&hostport) {
                Ok(tcp) => tcp,
                Err(err) => {
                    println!("[E] Could not connect to gerrit at {}: {:?}", hostport, err);
                    send_terminate_msg(main_tx);
                    return;
                }
            };

            if let Err(err) = session.handshake(&tcp) {
                println!("[E] Could not connect to gerrit: {:?}", err);
                send_terminate_msg(main_tx);
                return;
            };

            // Try to authenticate
            if let Err(err) = session.userauth_pubkey_file(
                &username,
                Some(&pub_key_path),
                &priv_key_path,
                None,
            )
            {
                println!("[E] Could not authenticate: {:?}", err);
                send_terminate_msg(main_tx);
                return;
            };

            let mut ssh_channel = match session.channel_session() {
                Ok(ssh_channel) => ssh_channel,
                Err(err) => {
                    println!("[E] Could not create ssh channel: {:?}", err);
                    send_terminate_msg(main_tx);
                    return;
                }
            };

            if let Err(err) = ssh_channel.exec("gerrit stream-events -s comment-added") {
                println!(
                    "[E] Could not execture gerrit stream-event command over ssh: {:?}",
                    err
                );
                send_terminate_msg(main_tx);
                return;
            };

            let buf_channel = BufReader::new(ssh_channel);
            let mut tx = main_tx.clone();
            for line in buf_channel.lines() {
                if let Ok(line) = line {
                    match tx.clone().send(Ok(line)).wait() {
                        Ok(s) => tx = s,
                        Err(err) => {
                            println!("[E] Cannot send message through channel {:?}", err);
                            break;
                        }
                    }
                } else {
                    println!("[E] Could not read line from buffer. Will drop connection.");
                    break;
                }
            }
        }
    });

    rx.then(|event| match event.unwrap() {
        Ok(event) => {
            let json: String = event;
            let res = serde_json::from_str(&json);
            println!("[D] {:?} for json: {}", res, json);
            Ok(res.ok())
        }
        Err(_) => Err(StreamError::Terminated),
    }).filter_map(|event| event)
        .boxed()
}

#[cfg(test)]
mod test {
    use super::{get_pub_key_path, PathBuf};

    #[test]
    fn test_get_pub_key_path() {
        let result = get_pub_key_path(&PathBuf::from("some_priv_key"));
        assert!(result == PathBuf::from("some_priv_key.pub"));
    }
}
