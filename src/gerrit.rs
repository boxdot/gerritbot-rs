use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use ssh2;
use serde_json;

use futures::stream::BoxStream;
use futures::sync::mpsc::channel;
use futures::{Future, Sink, Stream};

/// Gerrit username
pub type Username = String;

#[derive(Deserialize, Debug)]
pub struct User {
    name: Option<String>,
    username: Username,
    email: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    #[serde(rename="type")]
    approval_type: String,
    description: String,
    value: String,
    old_value: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PatchSet {
    number: String,
    revision: String,
    parents: Vec<String>,
    #[serde(rename="ref")]
    reference: String,
    uploader: User,
    created_on: u32,
    author: User,
    is_draft: bool,
    kind: String,
    size_insertions: u32,
    size_deletions: u32,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    project: String,
    branch: String,
    id: String,
    number: String,
    subject: String,
    owner: User,
    url: String,
    commit_message: String,
    status: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ChangeKey {
    id: String,
}

// Only specific events are accepted by this type by design!
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub author: Option<User>,
    pub uploader: Option<User>,
    pub approvals: Option<Vec<Approval>>,
    pub comment: Option<String>,
    #[serde(rename="patchSet")]
    pub patchset: PatchSet,
    pub change: Change,
    pub project: String,
    #[serde(rename="refName")]
    pub ref_name: String,
    #[serde(rename="changeKey")]
    pub changekey: ChangeKey,
    #[serde(rename="type")]
    pub event_type: String,
    #[serde(rename="eventCreatedOn")]
    pub created_on: u32,
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

pub fn event_stream(host: &str,
                    port: u16,
                    username: String,
                    priv_key_path: PathBuf)
                    -> BoxStream<Event, StreamError> {
    let hostport = format!("{}:{}", host, port);
    let mut session = ssh2::Session::new().unwrap();

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

    // TODO: Right now, we are interested only in +1/-1/+2/-2 events. When we have implemented all
    // event types mappings, we can provide here full parsing by removing the filtering.
    rx.then(|event| {
            // event from our channel cannot fail
            let json: String = event.unwrap()?;
            let res = serde_json::from_str(&json);
            if res.is_err() {
                println!("[D] {:?} for json: {}", res, json);
            }
            Ok(res.ok())
        })
        .filter(|event| event.is_some())
        .map(|event| event.unwrap()) // we cannot fail here, since we filtered all None's
        .boxed()
}

// "some change in gerrit: +1 (Code-Review), -1 (QA) from Some One"
pub fn approvals_to_message(event: Event) -> Option<String> {
    if let Some(approvals) = event.approvals {
        let approval_msgs = approvals.iter()
            .filter(|approval| approval.old_value != approval.value)
            .map(|approval| {
                let value: i32 = approval.value.parse().unwrap();
                format!("{}{} ({})",
                        if value > 0 { "+" } else { "" },
                        value,
                        approval.description)
            })
            .fold(String::new(), |acc, msg| if !acc.is_empty() {
                acc + ", " + &msg
            } else {
                msg
            });

        let author = event.author.unwrap();
        let name = match author.name {
            Some(name) => name,
            None => author.username,
        };

        let message = format!("{}: {} from {}", event.change.subject, approval_msgs, name);
        println!("[D] {:?}", message);
        Some(message)
    } else {
        None
    }
}
