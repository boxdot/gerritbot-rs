use std::fmt;
use std::io::{self, BufRead, BufReader, Read as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;

use backoff::Operation as _; // for retry_notify
use futures::sync::mpsc::{channel, Receiver, Sender};
use futures::sync::oneshot;
use futures::{Future, Sink as _, Stream};
use log::{debug, error, info};
use serde::Deserialize;
use serde_json;
use ssh2;
use ssh2::Channel;

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
    pub number: u32,
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
    pub number: u32,
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
    tcp: TcpStream,
    // Data needed for reconnection in case this connection was terminated.
    host: String,
    username: String,
    priv_key_path: PathBuf,
}

impl GerritConnection {
    fn connect_session(
        host: &str,
        username: &str,
        pub_key_path: &Path,
        priv_key_path: &Path,
    ) -> Result<(ssh2::Session, TcpStream), String> {
        let mut session = ssh2::Session::new().unwrap();

        debug!("Connecting to tcp: {}", &host);

        let tcp = TcpStream::connect(&host).or_else(|err| {
            Err(format!(
                "Could not connect to gerrit at {}: {:?}",
                host, err
            ))
        })?;

        session
            .handshake(&tcp)
            .or_else(|err| Err(format!("Could not connect to gerrit: {:?}", err)))?;

        // Try to authenticate
        session
            .userauth_pubkey_file(&username, Some(&pub_key_path), &priv_key_path, None)
            .or_else(|err| Err(format!("Could not authenticate: {:?}", err)))?;

        Ok((session, tcp))
    }

    pub fn connect(host: String, username: String, priv_key_path: PathBuf) -> Result<Self, String> {
        let pub_key_path = get_pub_key_path(&priv_key_path);
        debug!("Will use public key: {}", pub_key_path.to_str().unwrap());

        let (session, tcp) =
            Self::connect_session(&host, &username, &pub_key_path, &priv_key_path)?;

        Ok(Self {
            session,
            tcp,
            host,
            username,
            priv_key_path,
        })
    }

    /// Reconnect once.
    pub fn reconnect(&mut self) -> Result<(), String> {
        let pub_key_path = get_pub_key_path(&self.priv_key_path);
        let (session, tcp) = Self::connect_session(
            &self.host,
            &self.username,
            &pub_key_path,
            &self.priv_key_path,
        )?;

        self.session = session;
        self.tcp = tcp;

        Ok(())
    }

    /// Reconnect repeatedly with exponential backoff. This will try to
    /// reconnect indefinitely.
    pub fn reconnect_repeatedly(&mut self) -> Result<(), String> {
        let mut backoff = backoff::ExponentialBackoff::default();
        let mut reconnect = || self.reconnect().map_err(backoff::Error::Transient);

        // TODO: if reconnection fails permanently, this will prevent the
        // runtime from shutting down. Try to find a way to sleep that is
        // futures aware sleep and interruptible.
        reconnect
            .retry_notify(&mut backoff, |e, _| error!("reconnect failed: {}", e))
            .map_err(|e| match e {
                // neither of these should happen unless we reconfigure backoff
                // not to retry indefinitely
                backoff::Error::Transient(e) | backoff::Error::Permanent(e) => {
                    format!("reconnect failed: {}", e)
                }
            })
    }
}

struct CommandRequest {
    command: String,
    sender: oneshot::Sender<Result<String, String>>,
}

pub struct CommandRunner {
    sender: Sender<CommandRequest>,
}

impl CommandRunner {
    pub fn new(connection: GerritConnection) -> Result<Self, String> {
        let (sender, receiver) = channel(1);

        thread::Builder::new()
            .name("SSH command runner".to_string())
            .spawn(move || Self::run_commands(connection, receiver))
            .expect("failed to spawn thread");

        Ok(Self { sender })
    }

    fn run_commands(connection: GerritConnection, receiver: Receiver<CommandRequest>) {
        let mut connection = connection;
        let mut connection_healthy = true;

        for request in receiver.wait() {
            let CommandRequest { command, sender } = match request {
                Ok(request) => request,
                // other end was closed
                Err(_) => {
                    debug!("command runner thread shutting down");
                    return;
                }
            };

            let command_result = loop {
                if !connection_healthy {
                    info!("reconnecting");

                    if let Err(e) = connection.reconnect_repeatedly() {
                        error!("reconnect failed permanently: {}", e);
                        return;
                    }

                    connection_healthy = true;
                }

                let mut ssh_channel = match connection.session.channel_session() {
                    Ok(channel) => channel,
                    Err(e) => {
                        error!("failed to create ssh session channel: {}", e);
                        connection_healthy = false;
                        continue;
                    }
                };

                if let Err(e) = ssh_channel.exec(&command) {
                    error!("failed to request exec channel: {}", e);
                    break Err(format!("failed to request exec channel: {}", e));
                }

                let mut data = String::new();

                if let Err(e) = ssh_channel.read_to_string(&mut data) {
                    break Err(format!("failed to read from channel: {}", e));
                }

                match ssh_channel
                    .close()
                    .and_then(|()| ssh_channel.wait_close())
                    .and_then(|()| ssh_channel.exit_status())
                {
                    Ok(0) => break Ok(data),
                    Ok(i) => break Err(format!("command exited with status {}", i)),
                    Err(e) => break Err(format!("failed to close command channel: {}", e)),
                }
            };

            if let Err(_) = sender.send(command_result) {
                // receiver was closed, this is either an error or a signal to exit
                debug!("failed to send command result");
                return;
            }
        }
    }

    pub fn run_command(&mut self, command: String) -> impl Future<Item = String, Error = String> {
        // create a channel that the command thread can use to send the result of the command back
        let (sender, receiver) = oneshot::channel();
        self.sender
            .clone()
            .send(CommandRequest { command, sender })
            .map_err(|_| "command thread died before sending".to_string())
            .and_then(|_| receiver.map_err(|_| "command thread died after sending".to_string()))
            .and_then(|result| result)
    }
}

fn receiver_into_event_stream(
    rx: Receiver<Result<String, StreamError>>,
) -> impl Stream<Item = Event, Error = String> {
    rx
        // Receiver itself never fails, lower result value.
        .then(|event_data_result: Result<_, ()>| event_data_result.unwrap())
        .map_err(|err| format!("Stream error from Gerrit: {:?}", err))
        .filter_map(|event_data| {
            let event_result = serde_json::from_str(&event_data);
            debug!("Incoming Gerrit event: {:#?}", event_result);
            // Ignore JSON decoding errors.
            event_result.ok()
        })
}

pub fn event_stream(
    host: String,
    username: String,
    priv_key_path: PathBuf,
) -> impl Stream<Item = Event, Error = String> {
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

#[derive(Debug)]
pub struct ChangeDetails {
    pub user: String,
    pub message: String,
    pub change: Change,
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
        Box<dyn Stream<Item = ChangeDetails, Error = String>>,
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
            return Ok(ChangeDetails {
                user: user.clone(),
                message: message.clone(),
                change: change.clone(),
            });
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
                    Ok(ChangeDetails {
                        user,
                        message,
                        change,
                    })
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
