use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;

use ssh2;
use ssh2::Channel;
use serde_json;

use futures::sync::mpsc::{channel, Receiver, Sender};
use futures::{Future, Sink, Stream};

use bot;

/// Gerrit username
pub type Username = String;

#[derive(Deserialize, Debug, Clone)]
pub struct User {
    pub name: Option<String>,
    pub username: Username,
    pub email: Option<String>,
}

impl fmt::Display for User {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.username)
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    #[serde(rename = "type")]
    pub approval_type: String,
    pub description: String,
    pub value: String,
    pub old_value: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
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
    pub comments: Option<Vec<InlineComment>>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InlineComment {
    pub file: String,
    pub line: u32,
    pub reviewer: User,
    pub message: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub project: String,
    pub branch: String,
    pub id: String,
    pub number: String,
    pub subject: String,
    pub topic: Option<String>,
    pub owner: User,
    pub url: String,
    pub commit_message: String,
    pub status: String,
    pub current_patch_set: Option<PatchSet>,
    pub comments: Option<Vec<Comment>>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Comment {
    pub timestamp: u64,
    pub reviewer: User,
    pub message: String,
}

#[derive(Deserialize, Debug, Eq, PartialEq, Hash, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChangeKey {
    pub id: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub enum EventType {
    #[serde(rename = "reviewer-added")]
    ReviewerAdded,
    #[serde(rename = "comment-added")]
    CommentAdded,
}

// Only specific events are accepted by this type by design!
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub author: Option<User>,
    pub uploader: Option<User>,
    pub approvals: Option<Vec<Approval>>,
    pub reviewer: Option<User>,
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
    pub event_type: EventType,
    #[serde(rename = "eventCreatedOn")]
    pub created_on: u32,
}

impl Event {
    pub fn into_action(self) -> bot::Action {
        if self.event_type == EventType::CommentAdded && self.approvals.is_some() {
            bot::Action::UpdateApprovals(Box::new(self))
        } else if self.event_type == EventType::ReviewerAdded {
            bot::Action::ReviewerAdded(Box::new(self))
        } else {
            bot::Action::NoOp
        }
    }
}

#[derive(Debug)]
pub enum StreamError {
    Io(io::Error),
    Parse(serde_json::Error),
    Terminated(String /* reason */),
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

fn send_terminate_msg<T>(
    tx: &Sender<Result<String, StreamError>>,
    reason: String,
) -> Result<T, ()> {
    let _ = tx.clone().send(Err(StreamError::Terminated(reason))).wait();
    Err(())
}

pub struct GerritConnection {
    pub session: ssh2::Session,
    /// tcp has to be kept alive with session together, even if it is never used directly
    tcp: Box<TcpStream>,
    // Data needed for reconnection in case this connection was terminated.
    host: String,
    username: String,
    priv_key_path: PathBuf,
}

impl GerritConnection {
    pub fn connect(host: String, username: String, priv_key_path: PathBuf) -> Result<Self, String> {
        let pub_key_path = get_pub_key_path(&priv_key_path);
        debug!("Will use public key: {}", pub_key_path.to_str().unwrap());

        let mut session = ssh2::Session::new().unwrap();

        debug!("Connecting to tcp: {}", &host);
        let tcp = Box::new(TcpStream::connect(&host).or_else(|err| {
            Err(format!(
                "Could not connect to gerrit at {}: {:?}",
                host, err
            ))
        })?);

        session
            .handshake(&*tcp)
            .or_else(|err| Err(format!("Could not connect to gerrit: {:?}", err)))?;

        // Try to authenticate
        session
            .userauth_pubkey_file(&username, Some(&pub_key_path), &priv_key_path, None)
            .or_else(|err| Err(format!("Could not authenticate: {:?}", err)))?;

        Ok(Self {
            session,
            tcp,
            host,
            username,
            priv_key_path,
        })
    }

    pub fn reconnect(&mut self) -> Result<(), String> {
        let mut session = ssh2::Session::new().unwrap();
        let tcp = Box::new(TcpStream::connect(&self.host).or_else(|err| {
            Err(format!(
                "Could not connect to gerrit at {}: {:?}",
                self.host, err
            ))
        })?);

        session
            .handshake(&*tcp)
            .or_else(|err| Err(format!("Could not connect to gerrit: {:?}", err)))?;

        // Try to authenticate
        let pub_key_path = get_pub_key_path(&self.priv_key_path);
        debug!("Will use public key: {}", pub_key_path.to_str().unwrap());
        session
            .userauth_pubkey_file(
                &self.username,
                Some(&pub_key_path),
                &self.priv_key_path,
                None,
            )
            .or_else(|err| Err(format!("Could not authenticate: {:?}", err)))?;

        self.session = session;
        self.tcp = tcp;

        Ok(())
    }
}

fn receiver_into_event_stream(
    rx: Receiver<Result<String, StreamError>>,
) -> Box<Stream<Item = bot::Action, Error = String>> {
    let stream = rx.then(|event| {
        // parse each json message as event (if we did not get an error)
        event.unwrap().map(|event| {
            let json: String = event;
            let res = serde_json::from_str(&json);
            debug!("Incoming Gerrit event: {:#?}", res);
            res.ok()
        })
    }).filter_map(|event| event.map(Event::into_action))
        .map_err(|err| format!("Stream error from Gerrit: {:?}", err));
    Box::new(stream)
}

pub fn event_stream(
    host: String,
    username: String,
    priv_key_path: PathBuf,
) -> Box<Stream<Item = bot::Action, Error = String>> {
    let (main_tx, rx) = channel(1);
    thread::spawn(move || -> Result<(), ()> {
        loop {
            info!("(Re)connecting to Gerrit over SSH: {}", &host);
            let conn =
                GerritConnection::connect(host.clone(), username.clone(), priv_key_path.clone())
                    .or_else(|err| {
                        send_terminate_msg(
                            &main_tx.clone(),
                            format!("Could not connect to Gerrit: {}", err),
                        )
                    })?;

            let mut ssh_channel = conn.session.channel_session().or_else(|err| {
                send_terminate_msg(
                    &main_tx.clone(),
                    format!("Could not open SSH channel: {:?}", err),
                )
            })?;
            ssh_channel
                .exec("gerrit stream-events -s comment-added -s reviewer-added")
                .or_else(|err| {
                    send_terminate_msg(
                        &main_tx.clone(),
                        format!(
                            "Could not execute gerrit stream-event command over ssh: {:?}",
                            err
                        ),
                    )
                })?;
            info!("Connected to Gerrit.");

            let buf_channel = BufReader::new(ssh_channel);
            let mut tx = main_tx.clone();
            for line in buf_channel.lines() {
                if let Ok(line) = line {
                    match tx.clone().send(Ok(line)).wait() {
                        Ok(s) => tx = s,
                        Err(err) => {
                            error!("Cannot send message through channel {:?}", err);
                            break;
                        }
                    }
                } else {
                    error!("Could not read line from buffer. Will drop connection.");
                    break;
                }
            }
        }
    });

    receiver_into_event_stream(rx)
}

/// Create a channel accepting change ids and a stream of reponses with `Change` corresponding to
/// incoming change ids.
///
/// Note: If connection to Gerrit is lost, the stream will try to establish a new one for every
/// incoming change id.
pub fn change_sink(
    host: String,
    username: String,
    priv_key_path: PathBuf,
) -> Result<
    (
        Sender<(String, Change, String)>,
        Box<Stream<Item = bot::Action, Error = String>>,
    ),
    String,
> {
    let mut conn = GerritConnection::connect(host, username, priv_key_path)?;
    let (tx, rx) = channel::<(String, Change, String)>(1);
    let response_stream = rx.then(move |data| {
        let (user, mut change, message) = data.expect("receiver should never fail");

        let change_id = change.id.clone();
        let res = conn.session.channel_session().map(|ssh_channel| {
            let comments = match fetch_patch_set(ssh_channel, change_id.clone()) {
                Ok(c) => c,
                Err(e) => {
                    error!("Could not fetch additional comments: {:?}", e);
                    None
                }
            };
            debug!("Got comments: {:#?}", comments);
            change.current_patch_set = comments;
        });

        if let Ok(_) = res {
            return Ok(bot::Action::ChangeFetched(
                user.clone(),
                message.clone(),
                Box::new(change),
            ));
        } else {
            info!(
                "Reconnecting to Gerrit over SSH for sending commands: {}",
                &conn.host
            );
            if let Err(e) = conn.reconnect() {
                return Err(format!("Failed to reconnect to Gerrit over SSH: {}", e));
            };

            conn.session
                .channel_session()
                .map_err(|e| {
                    format!(
                        "Failed to reconnect to Gerrit over SSH for sending commands: {}",
                        e
                    )
                })
                .and_then(|ssh_channel| {
                    let comments = match fetch_patch_set(ssh_channel, change_id) {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Could not fetch additional comments: {:?}", e);
                            None
                        }
                    };
                    debug!("Got comments: {:#?}", comments);
                    change.current_patch_set = comments;
                    Ok(bot::Action::ChangeFetched(user, message, Box::new(change)))
                })
        }
    });

    Ok((tx, Box::new(response_stream)))
}

pub fn fetch_patch_set(
    mut ssh_channel: Channel,
    change_id: String,
) -> Result<Option<PatchSet>, serde_json::Error> {
    let query = format!(
        "gerrit query --format JSON --current-patch-set --comments {}",
        change_id
    );
    ssh_channel.exec(&query).unwrap();

    let buf_channel = BufReader::new(ssh_channel);
    let line = buf_channel.lines().next();

    // event from our channel cannot fail
    let json: String = line.unwrap().ok().unwrap();
    debug!("{}", json);
    let complete_change: Change = serde_json::from_str::<Change>(&json)?;
    debug!("{:#?}", complete_change);
    //debug!("{:#?}", complete_change);
    //let mut new_change = change.clone();
    //change.comments = complete_change.comments;
    Ok(complete_change.current_patch_set)
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
