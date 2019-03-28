use std::borrow::Cow;
use std::io::{BufRead, BufReader, Read as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;

use backoff::Operation as _; // for retry_notify
use futures::sync::mpsc::{channel, Receiver, Sender};
use futures::sync::oneshot;
use futures::{future, Future, Sink, Stream};
use log::{debug, error, info};
use serde::Deserialize;

/// Gerrit username
pub type Username = String;

#[derive(Deserialize, Debug, Clone)]
pub struct User {
    pub name: Option<String>,
    pub username: Option<Username>,
    pub email: String,
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
pub struct Patchset {
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
pub enum ChangeStatus {
    NEW,
    DRAFT,
    MERGED,
    ABANDONED,
}

#[allow(non_camel_case_types)]
#[derive(Deserialize, Debug, Clone)]
pub enum SubmitStatus {
    OK,
    NOT_READY,
    RULE_ERROR,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRecord {
    status: SubmitStatus,
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
    pub status: ChangeStatus,
    pub current_patch_set: Option<Patchset>,
    pub patch_sets: Option<Vec<Patchset>>,
    pub comments: Option<Vec<Comment>>,
    pub submit_records: Option<Vec<SubmitRecord>>,
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

#[derive(Deserialize, Debug, Clone)]
pub struct CommentAddedEvent {
    pub change: Change,
    #[serde(rename = "patchSet")]
    pub patchset: Patchset,
    pub author: User,
    pub approvals: Vec<Approval>,
    pub comment: String,
    #[serde(rename = "eventCreatedOn")]
    pub created_on: u32,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ReviewerAddedEvent {
    pub change: Change,
    #[serde(rename = "patchSet")]
    pub patchset: Patchset,
    pub reviewer: User,
    #[serde(rename = "eventCreatedOn")]
    pub created_on: u32,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "comment-added")]
    CommentAdded(CommentAddedEvent),
    #[serde(rename = "reviewer-added")]
    ReviewerAdded(ReviewerAddedEvent),
}

fn get_pub_key_path(priv_key_path: &PathBuf) -> PathBuf {
    let mut pub_key_path = PathBuf::from(priv_key_path.to_str().unwrap());
    pub_key_path.set_extension("pub");
    pub_key_path
}

pub struct Connection {
    pub session: ssh2::Session,
    /// tcp has to be kept alive with session together, even if it is never used directly
    tcp: TcpStream,
    // Data needed for reconnection in case this connection was terminated.
    host: String,
    username: String,
    priv_key_path: PathBuf,
}

impl Connection {
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
    pub fn new(connection: Connection) -> Self {
        let (sender, receiver) = channel(1);

        thread::Builder::new()
            .name("SSH command runner".to_string())
            .spawn(move || Self::run_commands(connection, receiver))
            .expect("failed to spawn thread");

        Self { sender }
    }

    fn run_commands(connection: Connection, receiver: Receiver<CommandRequest>) {
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

            if sender.send(command_result).is_err() {
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

fn receiver_into_event_stream(rx: Receiver<String>) -> impl Stream<Item = Event, Error = ()> {
    rx.filter_map(|event_data| {
        let event_result = serde_json::from_str(&event_data);
        debug!("Incoming Gerrit event: {:#?}", event_result);
        // Ignore JSON decoding errors.
        event_result.ok()
    })
}

pub fn event_stream(connection: Connection) -> impl Stream<Item = Event, Error = ()> {
    let (main_tx, rx) = channel(1);

    fn process_events(connection: &mut Connection, tx: &Sender<String>) -> Result<(), ()> {
        let mut ssh_channel = connection
            .session
            .channel_session()
            .map_err(|err| error!("Could not open SSH channel: {:?}", err))?;
        ssh_channel
            .exec("gerrit stream-events -s comment-added -s reviewer-added")
            .map_err(|err| {
                error!(
                    "Could not execute gerrit stream-event command over ssh: {:?}",
                    err
                )
            })?;
        info!("Connected to Gerrit.");

        let buf_channel = BufReader::new(ssh_channel);
        for line in buf_channel.lines() {
            let line =
                line.map_err(|_| error!("Could not read line from buffer. Will drop connection."))?;
            tx.clone()
                .send(line)
                .wait()
                .map_err(|err| error!("Cannot send message through channel {:?}", err))?;
        }
        Ok(())
    }

    thread::spawn(move || {
        let mut connection = connection;
        while !main_tx.is_closed() {
            if process_events(&mut connection, &main_tx).is_err() {
                info!("reconnecting");

                if let Err(e) = connection.reconnect_repeatedly() {
                    error!("reconnect failed permanently: {}", e);
                    return;
                }
            }
        }
    });

    receiver_into_event_stream(rx)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExtendedInfo {
    SubmitRecords,
    InlineComments,
}

/// Fetch extended event info. On error the original event and an error message
/// is returned.
fn fetch_extended_info(
    command_runner: &mut CommandRunner,
    event: Event,
    extended_info: &[ExtendedInfo],
) -> impl Future<Item = Event, Error = (Event, String)> {
    if extended_info.is_empty() {
        return future::Either::A(future::ok(event));
    }

    let mut query = "gerrit query --format=JSON".to_string();

    if extended_info.contains(&ExtendedInfo::SubmitRecords) {
        query += " --submit-records";
    }

    if extended_info.contains(&ExtendedInfo::InlineComments) {
        query += " --patch-sets --comments";
    }

    let change_id = match &event {
        Event::CommentAdded(event) => &event.change.id,
        Event::ReviewerAdded(event) => &event.change.id,
    };

    query += &format!(" change:{}", change_id);

    future::Either::B(command_runner.run_command(query).then(
        move |result| -> Result<Event, (Event, String)> {
            let result = match result {
                Ok(result) => result,
                Err(e) => return Err((event, e)),
            };
            let line = result.lines().next().unwrap_or("");

            let mut event = event;
            let (change, patchset): (&mut Change, &mut Patchset) = match &mut event {
                Event::CommentAdded(event) => (&mut event.change, &mut event.patchset),
                Event::ReviewerAdded(event) => (&mut event.change, &mut event.patchset),
            };

            let mut new_change: Change = match serde_json::from_str(line) {
                Ok(change) => change,
                Err(e) => return Err((event, format!("failed to decode result: {}", e))),
            };

            // copy patchset from change for the comments
            if let Some(patchsets) = new_change.patch_sets.take() {
                if let Some(new_patchset) = patchsets
                    .iter()
                    .find(|patchset| patchset.number == patchset.number)
                {
                    *patchset = new_patchset.clone();
                }
            }

            // copy over submit records
            change.submit_records = new_change.submit_records.take();

            Ok(event)
        },
    ))
}

pub fn extended_event_stream<F>(
    stream_connection: Connection,
    command_connection: Connection,
    select_extended_info: F,
) -> impl Stream<Item = Event, Error = ()>
where
    F: FnMut(&Event) -> Cow<'static, [ExtendedInfo]>,
{
    let mut command_runner = CommandRunner::new(command_connection);
    let mut select_extended_info = select_extended_info;

    event_stream(stream_connection).and_then(move |event| {
        let extended_info = select_extended_info(&event);
        fetch_extended_info(&mut command_runner, event, extended_info.as_ref()).or_else(
            |(event, err)| {
                error!("failed to fetch extended event info: {}", err);
                Ok(event)
            },
        )
    })
}

#[cfg(test)]
mod test {
    use super::*;

    use spectral::prelude::*;

    #[test]
    fn test_get_pub_key_path() {
        let result = get_pub_key_path(&PathBuf::from("some_priv_key"));
        assert!(result == PathBuf::from("some_priv_key.pub"));
    }

    const COMMENT_ADDED_JSON: &str = r#"
{"author":{"name":"Administrator","email":"admin@example.com","username":"admin"},"approvals":[{"type":"Code-Review","description":"Code-Review","value":"2","oldValue":"0"}],"comment":"Patch Set 1: Code-Review+2","patchSet":{"number":1,"revision":"c4f7d43450e366f9c8e4dcb94fbd91573cd40766","parents":["20332c6ee056bdf3f814c8cff9905154d443d2f0"],"ref":"refs/changes/01/1/1","uploader":{"name":"Administrator","email":"admin@example.com","username":"admin"},"createdOn":1553631812,"author":{"name":"Frank Benkstein","email":"frank@benkstein.net","username":""},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":-18},"change":{"project":"gerritbot-rs","branch":"master","id":"I5e53df227fd2739ddd65c3034b2f9f789200bd89","number":1,"subject":"get rid of non-macro extern crate","owner":{"name":"Administrator","email":"admin@example.com","username":"admin"},"assignee":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"url":"http://localhost:8080/1","commitMessage":"get rid of non-macro extern crate\n\nChange-Id: I5e53df227fd2739ddd65c3034b2f9f789200bd89\n","createdOn":1553631812,"status":"NEW"},"project":"gerritbot-rs","refName":"refs/heads/master","changeKey":{"id":"I5e53df227fd2739ddd65c3034b2f9f789200bd89"},"type":"comment-added","eventCreatedOn":1553632440}
"#;

    const REVIEWER_ADDED_JSON: &str = r#"
{"reviewer":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"patchSet":{"number":1,"revision":"c4f7d43450e366f9c8e4dcb94fbd91573cd40766","parents":["20332c6ee056bdf3f814c8cff9905154d443d2f0"],"ref":"refs/changes/01/1/1","uploader":{"name":"Administrator","email":"admin@example.com","username":"admin"},"createdOn":1553631812,"author":{"name":"Frank Benkstein","email":"frank@benkstein.net","username":""},"isDraft":false,"kind":"REWORK","sizeInsertions":0,"sizeDeletions":-18},"change":{"project":"gerritbot-rs","branch":"master","id":"I5e53df227fd2739ddd65c3034b2f9f789200bd89","number":1,"subject":"get rid of non-macro extern crate","owner":{"name":"Administrator","email":"admin@example.com","username":"admin"},"assignee":{"name":"jdoe","email":"john.doe@localhost","username":"jdoe"},"url":"http://localhost:8080/1","commitMessage":"get rid of non-macro extern crate\n\nChange-Id: I5e53df227fd2739ddd65c3034b2f9f789200bd89\n","createdOn":1553631812,"status":"NEW"},"project":"gerritbot-rs","refName":"refs/heads/master","changeKey":{"id":"I5e53df227fd2739ddd65c3034b2f9f789200bd89"},"type":"reviewer-added","eventCreatedOn":1553632329}
"#;

    #[test]
    fn test_deserialize_comment_added() {
        let event: Event =
            serde_json::from_str(COMMENT_ADDED_JSON).expect("failed to deserialize event");
        match event {
            Event::CommentAdded(event) => {
                assert_that!(event.author.name)
                    .is_some()
                    .is_equal_to("Administrator".to_string());
                assert_that!(event.comment).is_equal_to("Patch Set 1: Code-Review+2".to_string());
            }
            _ => panic!("unexpected_event: {:?}", event),
        }
    }

    #[test]
    fn test_reviewer_added() {
        let event: Event =
            serde_json::from_str(REVIEWER_ADDED_JSON).expect("failed to deserialize event");
        match event {
            Event::ReviewerAdded(event) => {
                assert_that!(event.reviewer.name)
                    .is_some()
                    .is_equal_to("jdoe".to_string());
            }
            _ => panic!("unexpected_event: {:?}", event),
        }
    }
}
