use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use ssh2;
use serde_json;

use futures::stream::BoxStream;
use futures::sync::mpsc::channel;
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

pub fn event_stream(
    host: &str,
    port: u16,
    username: String,
    priv_key_path: PathBuf,
) -> BoxStream<Event, StreamError> {
    let hostport = format!("{}:{}", host, port);
    let mut pub_key_path = PathBuf::from(priv_key_path.to_str().unwrap());
    pub_key_path.set_extension("pub");
    println!("[D] Deduced public key: {}", pub_key_path.to_str().unwrap());

    let (main_tx, rx) = channel(1);
    thread::spawn(move || {
        loop {
            println!("[I] (Re)connecting to Gerrit over ssh: {}", hostport);

            // TODO: Remove all unwraps here, since otherwise we may stuck in
            // an endless loop.

            // Connect to the local SSH server
            let mut session = ssh2::Session::new().unwrap();
            let tcp = TcpStream::connect(&hostport).unwrap();
            session.handshake(&tcp).unwrap();

            // Try to authenticate
            session
                .userauth_pubkey_file(&username, Some(&pub_key_path), &priv_key_path, None)
                .unwrap();

            let mut ssh_channel = session.channel_session().unwrap();
            ssh_channel.exec("gerrit stream-events").unwrap();

            let buf_channel = BufReader::new(ssh_channel);
            let mut tx = main_tx.clone();
            for line in buf_channel.lines() {
                if let Ok(line) = line {
                    match tx.clone().send(line).wait() {
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

    // TODO: Right now, we are interested only in +1/-1/+2/-2 events, and new
    // draft events. When we have implemented all event types mappings, we can
    // provide here full parsing by removing the filtering.
    rx.then(|event| {
        // event from our channel cannot fail
        let json: String = event.unwrap();
        let res = serde_json::from_str(&json);
        println!("[D] {:?} for json: {}", res, json);
        Ok(res.ok())
    }).filter_map(|event| event)
        .boxed()
}
