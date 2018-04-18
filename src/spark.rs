use std::{error, fmt, io, thread};
use std::rc::Rc;

use futures::future::Future;
use futures::{Sink, Stream};
use futures::sync::mpsc::{channel, Sender};
use hyper;
use hyper_native_tls;
use iron::prelude::*;
use iron::status;
use regex::Regex;
use router::Router;
use serde;
use serde_json;
use tokio_core;
use rusoto_core;

use bot;
use sqs;

//
// Helper functions
//

/// Create a new hyper client for the given url.
fn new_client(url: &str) -> hyper::Client {
    if url.starts_with("https:") {
        let ssl = hyper_native_tls::NativeTlsClient::new().unwrap();
        let connector = hyper::net::HttpsConnector::new(ssl);
        return hyper::Client::with_connector(connector);
    }

    hyper::Client::new()
}

/// Try to get json from the given url with basic token authorization.
fn get_json_with_token(url: &str, token: &str) -> Result<hyper::client::Response, hyper::Error> {
    let client = new_client(url);
    let auth = hyper::header::Authorization(hyper::header::Bearer {
        token: String::from(token),
    });
    client
        .get(url)
        .header(hyper::header::ContentType::json())
        .header(hyper::header::Accept::json())
        .header(auth)
        .send()
}

/// Try to post json to the given url with basic token authorization.
fn post_with_token<T>(
    url: &str,
    token: &str,
    data: &T,
) -> Result<hyper::client::Response, hyper::Error>
where
    T: serde::ser::Serialize,
{
    let client = new_client(url);
    let payload = serde_json::to_string(data).unwrap();
    let auth = hyper::header::Authorization(String::from("Bearer ") + token);
    client
        .post(url)
        .header(hyper::header::ContentType::json())
        .header(auth)
        .body(&payload)
        .send()
}

/// Try to post json to the given url with basic token authorization.
fn delete_with_token(url: &str, token: &str) -> Result<hyper::client::Response, hyper::Error> {
    let client = new_client(url);
    let auth = hyper::header::Authorization(String::from("Bearer ") + token);
    client
        .delete(url)
        .header(hyper::header::ContentType::json())
        .header(auth)
        .send()
}

//
// Spark data model
//

/// Spark id of the user
pub type PersonId = String;

/// Email of the user
pub type Email = String;

/// Webhook's post request from Spark API
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Post {
    actor_id: String,
    app_id: String,
    created: String,
    created_by: String,
    data: Message,
    event: String,
    id: String,
    name: String,
    org_id: String,
    owned_by: String,
    resource: String,
    status: String,
    target_url: String,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    created: Option<String>,
    id: String,
    pub person_email: String,
    pub person_id: String,
    room_id: String,
    room_type: String,

    // a message contained in a post does not have text loaded
    #[serde(default)]
    text: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PersonDetails {
    id: String,
    emails: Vec<String>,
    display_name: String,
    nick_name: Option<String>,
    org_id: String,
    created: String,
    last_activity: Option<String>,
    status: Option<String>,
    #[serde(rename = "type")]
    person_type: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Webhook {
    id: String,
    name: String,
    target_url: String,
    resource: String,
    event: String,
    org_id: String,
    created_by: String,
    app_id: String,
    owned_by: String,
    status: String,
    created: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Webhooks {
    items: Vec<Webhook>,
}

//
// Client
//

pub trait SparkClient {
    fn id(&self) -> &str;
    fn reply(&self, person_id: &str, msg: &str);
    fn get_message(&self, message_id: &str) -> Result<Message, Error>;
}

#[derive(Debug, Clone)]
pub struct WebClient {
    url: String,
    bot_token: String,
    pub bot_id: String,
}

#[derive(Debug)]
pub enum Error {
    HyperError(hyper::Error),
    SqsError(sqs::Error),
    JsonError(serde_json::Error),
    RegisterWebhook(String),
    DeleteWebhook(String),
    IoError(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::HyperError(ref err) => fmt::Display::fmt(err, f),
            Error::SqsError(ref err) => fmt::Display::fmt(err, f),
            Error::JsonError(ref err) => fmt::Display::fmt(err, f),
            Error::RegisterWebhook(ref msg) | Error::DeleteWebhook(ref msg) => {
                fmt::Display::fmt(msg, f)
            }
            Error::IoError(ref err) => fmt::Display::fmt(err, f),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::HyperError(ref err) => err.description(),
            Error::SqsError(ref err) => err.description(),
            Error::JsonError(ref err) => err.description(),
            Error::RegisterWebhook(ref msg) | Error::DeleteWebhook(ref msg) => msg,
            Error::IoError(ref err) => err.description(),
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::HyperError(ref err) => err.cause(),
            Error::SqsError(ref err) => err.cause(),
            Error::JsonError(ref err) => err.cause(),
            Error::RegisterWebhook(_) | Error::DeleteWebhook(_) => None,
            Error::IoError(ref err) => err.cause(),
        }
    }
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        Error::HyperError(err)
    }
}

impl From<sqs::Error> for Error {
    fn from(err: sqs::Error) -> Self {
        Error::SqsError(err)
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::JsonError(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::IoError(err)
    }
}

impl WebClient {
    pub fn new(
        spark_api_url: String,
        bot_token: String,
        webhook_url: Option<String>,
    ) -> Result<Self, Error> {
        let mut client = Self {
            url: spark_api_url,
            bot_token: bot_token,
            bot_id: String::new(),
        };

        client.bot_id = client.get_bot_id()?;
        debug!("Bot id: {}", client.bot_id);

        if let Some(webhook_url) = webhook_url {
            client.replace_webhook_url(&webhook_url)?;
            info!("Registered Spark's webhook url: {}", webhook_url);
        }

        Ok(client)
    }

    fn get_bot_id(&self) -> Result<String, Error> {
        let resp = get_json_with_token(&(self.url.clone() + "/people/me"), &self.bot_token)?;
        let details: PersonDetails = serde_json::from_reader(resp)?;
        Ok(details.id)
    }

    fn register_webhook(&self, url: &str) -> Result<(), Error> {
        let json = json!({
            "name": "gerritbot",
            "targetUrl": String::from(url),
            "resource": "messages",
            "event": "created"
        });
        post_with_token(&(self.url.clone() + "/webhooks"), &self.bot_token, &json)
            .map_err(Error::from)
            .and_then(|resp| {
                if resp.status != hyper::status::StatusCode::Ok {
                    Err(Error::RegisterWebhook(format!(
                        "Could not register Spark's webhook: {}",
                        resp.status
                    )))
                } else {
                    Ok(())
                }
            })
    }

    fn list_webhooks(&self) -> Result<Webhooks, Error> {
        let resp = get_json_with_token(&(self.url.clone() + "/webhooks"), &self.bot_token)?;
        let webhooks: Webhooks = serde_json::from_reader(resp)?;
        Ok(webhooks)
    }

    fn delete_webhook(&self, id: &str) -> Result<(), Error> {
        delete_with_token(&(self.url.clone() + "/webhooks/" + id), &self.bot_token)
            .map_err(Error::from)
            .and_then(|resp| {
                if resp.status != hyper::status::StatusCode::NoContent {
                    Err(Error::DeleteWebhook(format!(
                        "Could not delete webhook: {}",
                        resp.status
                    )))
                } else {
                    Ok(())
                }
            })
    }

    fn replace_webhook_url(&self, url: &str) -> Result<(), Error> {
        // remove all other webhooks
        let webhooks = self.list_webhooks()?;
        let to_remove = webhooks.items.into_iter().filter_map(|webhook| {
            if webhook.resource == "messages" && webhook.event == "created" {
                Some(webhook)
            } else {
                None
            }
        });
        for webhook in to_remove {
            self.delete_webhook(&webhook.id)?;
            debug!("Removed webhook from Spark: {}", webhook.target_url);
        }

        // register new webhook
        self.register_webhook(url)
    }
}

impl SparkClient for WebClient {
    fn id(&self) -> &str {
        &self.bot_id
    }

    fn reply(&self, person_id: &str, msg: &str) {
        let json = json!({
            "toPersonId": person_id,
            "markdown": msg,
        });
        let res = post_with_token(&(self.url.clone() + "/messages"), &self.bot_token, &json);
        if let Err(err) = res {
            error!("Could not reply to gerrit: {:?}", err);
        }
    }

    fn get_message(&self, message_id: &str) -> Result<Message, Error> {
        let resp = get_json_with_token(
            &(self.url.clone() + "/messages/" + message_id),
            &self.bot_token,
        )?;
        serde_json::from_reader(resp).map_err(Error::from)
    }
}

#[derive(Debug, Clone)]
pub struct ConsoleClient {
    stdin_enabled: bool,
}

impl ConsoleClient {
    /// Create a console client which resolves the message text always with a placeholder text.
    pub fn new() -> Self {
        Self {
            stdin_enabled: false,
        }
    }

    // Create a console client which resolves the message text from stdin.
    pub fn _with_stdin() -> Self {
        Self {
            stdin_enabled: true,
        }
    }
}

impl SparkClient for ConsoleClient {
    fn id(&self) -> &str {
        "console-client"
    }

    fn reply(&self, person_id: &str, msg: &str) {
        print!("Would reply to {}: {}", person_id, msg);
    }

    fn get_message(&self, message_id: &str) -> Result<Message, Error> {
        if self.stdin_enabled {
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            serde_json::from_str(&line).map_err(Error::from)
        } else {
            let mut message = Message::default();
            message.id = message_id.into();
            message.text = "Placeholder text".into();
            Ok(message)
        }
    }
}

impl Message {
    /// Load text from Spark for a received message
    /// Note: Spark does not send the text with the message to the registered post hook.
    pub fn load_text<C: SparkClient + ?Sized>(&mut self, client: &C) -> Result<(), Error> {
        let msg = client.get_message(&self.id)?;
        self.text = msg.text;
        Ok(())
    }

    /// Convert Spark message to bot action
    pub fn into_action(self) -> bot::Action {
        lazy_static! {
            static ref FILTER_REGEX: Regex = Regex::new(r"(?i)^filter (.*)$").unwrap();
        };

        match &self.text.trim().to_lowercase()[..] {
            "enable" => bot::Action::Enable(self.person_id, self.person_email),
            "disable" => bot::Action::Disable(self.person_id, self.person_email),
            "status" => bot::Action::Status(self.person_id),
            "help" => bot::Action::Help(self.person_id),
            "filter" => bot::Action::FilterStatus(self.person_id),
            "filter enable" => bot::Action::FilterEnable(self.person_id),
            "filter disable" => bot::Action::FilterDisable(self.person_id),
            _ => match FILTER_REGEX
                .captures(&self.text.trim()[..])
                .and_then(|cap| cap.get(1))
            {
                Some(m) => bot::Action::FilterAdd(self.person_id, String::from(m.as_str())),
                None => bot::Action::Unknown(self.person_id),
            },
        }
    }
}

/// Post hook from Spark
pub fn webhook_handler(
    req: &mut Request,
    remote: &tokio_core::reactor::Remote,
    tx: Sender<Message>,
) -> IronResult<Response> {
    let new_post: Post = match serde_json::from_reader(&mut req.body) {
        Ok(post) => post,
        Err(err) => {
            error!("Could not parse post: {}", err);
            return Ok(Response::with(status::Ok));
        }
    };

    let msg = new_post.data;
    remote.spawn(move |_| tx.send(msg).map_err(|_| ()).map(|_| ()));

    Ok(Response::with(status::Ok))
}

pub fn webhook_event_stream<C: 'static + SparkClient + ?Sized>(
    client: Rc<C>,
    listen_url: &str,
    remote: tokio_core::reactor::Remote,
) -> Result<Box<Stream<Item = bot::Action, Error = String>>, Error> {
    let (tx, rx) = channel(1);
    let mut router = Router::new();
    router.post(
        "/",
        move |req: &mut Request| {
            debug!("Incoming webhook post request");
            webhook_handler(req, &remote, tx.clone())
        },
        "post",
    );

    let bot_id = String::from(client.id());
    let stream = rx.filter(move |msg| msg.person_id != bot_id)
        .map(move |mut msg| {
            debug!("Loading text for message: {:#?}", msg);
            if let Err(err) = msg.load_text(&*client) {
                error!("Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .filter_map(|msg| msg.map(Message::into_action))
        .map_err(|err| format!("Error from Spark: {:?}", err));

    // start listening
    let listen_url_string = listen_url.to_string();
    thread::spawn(move || {
        let mut iron = Iron::new(Chain::new(router));
        iron.threads = 2;
        iron.http(&listen_url_string).unwrap()
    });
    info!("Listening to Spark on {}", listen_url);

    Ok(Box::new(stream))
}

pub fn sqs_event_stream<C: SparkClient + 'static + ?Sized>(
    client: Rc<C>,
    sqs_url: String,
    sqs_region: rusoto_core::Region,
) -> Result<Box<Stream<Item = bot::Action, Error = String>>, Error> {
    let bot_id = String::from(client.id());
    let sqs_stream = sqs::sqs_receiver(sqs_url, sqs_region)?;
    let sqs_stream = sqs_stream
        .filter_map(|sqs_message| {
            if let Some(body) = sqs_message.body {
                let new_post: Post = match serde_json::from_str(&body) {
                    Ok(post) => post,
                    Err(err) => {
                        error!("Could not parse post: {}", err);
                        return None;
                    }
                };
                Some(new_post.data)
            } else {
                None
            }
        })
        .filter(move |msg| msg.person_id != bot_id)
        .map(move |mut msg| {
            debug!("Loading text for message: {:#?}", msg);
            if let Err(err) = msg.load_text(&*client) {
                error!("Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .filter_map(|msg| msg.map(Message::into_action))
        .map_err(|err| format!("Error from Spark: {:?}", err));
    Ok(Box::new(sqs_stream))
}
