use std::net::SocketAddr;
use std::{error, fmt, io};

use futures::future::{self, Future};
use futures::sync::mpsc::channel;
use futures::{IntoFuture as _, Sink, Stream};
use lazy_static::lazy_static;
use log::{debug, error, info, warn};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;

// mod sqs;

//
// Spark data model
//

/// Spark id of the user
pub type PersonId = String;

/// Email of the user
pub type Email = String;

/// Webhook's post request from Spark API
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    actor_id: String,
    app_id: String,
    created: String,
    created_by: String,
    pub data: Message,
    event: String,
    id: String,
    name: String,
    org_id: String,
    owned_by: String,
    resource: String,
    status: String,
    target_url: String,
}

#[derive(Deserialize, Debug, Clone, Default)]
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
    markdown: Option<String>,
    html: Option<String>,
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

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct WebhookRegistration {
    name: String,
    target_url: String,
    resource: String,
    event: String,
}

#[derive(Deserialize, Clone, Debug)]
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

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Webhooks {
    items: Vec<Webhook>,
}

//
// Client
//

#[derive(Debug, Clone)]
pub struct Client {
    client: reqwest::r#async::Client,
    url: String,
    bot_token: String,
    bot_id: String,
}

#[derive(Debug)]
pub enum Error {
    ReqwestError(reqwest::Error),
    HyperError(hyper::Error),
    // SqsError(sqs::Error),
    JsonError(serde_json::Error),
    RegisterWebhook(String),
    DeleteWebhook(String),
    IoError(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::ReqwestError(ref err) => fmt::Display::fmt(err, f),
            Error::HyperError(ref err) => fmt::Display::fmt(err, f),
            //Error::SqsError(ref err) => fmt::Display::fmt(err, f),
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
            Error::ReqwestError(ref err) => err.description(),
            Error::HyperError(ref err) => err.description(),
            // Error::SqsError(ref err) => err.description(),
            Error::JsonError(ref err) => err.description(),
            Error::RegisterWebhook(ref msg) | Error::DeleteWebhook(ref msg) => msg,
            Error::IoError(ref err) => err.description(),
        }
    }

    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            Error::ReqwestError(ref err) => err.source(),
            Error::HyperError(ref err) => err.source(),
            // Error::SqsError(ref err) => err.source(),
            Error::JsonError(ref err) => err.source(),
            Error::RegisterWebhook(_) | Error::DeleteWebhook(_) => None,
            Error::IoError(ref err) => err.source(),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Error::ReqwestError(err)
    }
}

impl From<hyper::Error> for Error {
    fn from(err: hyper::Error) -> Self {
        Error::HyperError(err)
    }
}

/*
impl From<sqs::Error> for Error {
    fn from(err: sqs::Error) -> Self {
        Error::SqsError(err)
    }
}
*/

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

impl Client {
    pub fn new(
        spark_api_url: String,
        bot_token: String,
    ) -> impl Future<Item = Self, Error = Error> {
        let bootstrap_client = Client {
            client: reqwest::r#async::Client::new(),
            url: spark_api_url,
            bot_token: bot_token,
            bot_id: String::new(),
        };

        bootstrap_client.get_bot_id().map(|bot_id| Client {
            bot_id: bot_id,
            ..bootstrap_client
        })
    }

    /// Try to get json from the given url with basic token authorization.
    fn api_get_json<T>(&self, resource: &str) -> impl Future<Item = T, Error = Error>
    where
        for<'a> T: Deserialize<'a>,
    {
        reqwest::r#async::Client::new()
            .get(&format!("{}/{}", self.url, resource))
            .bearer_auth(&self.bot_token)
            .header(http::header::ACCEPT, "application/json")
            .send()
            .from_err()
            .and_then(|response| decode_json_body(response.into_body()))
    }

    /// Try to post json to the given url with basic token authorization.
    fn api_post_json<T>(&self, resource: &str, data: &T) -> impl Future<Item = (), Error = Error>
    where
        T: Serialize,
    {
        self.client
            .post(&format!("{}/{}", self.url, resource))
            .bearer_auth(&self.bot_token)
            .header(http::header::ACCEPT, "application/json")
            .json(&data)
            .send()
            .from_err()
            .map(|_| ())
    }

    /// Try to post json to the given url with basic token authorization.
    fn api_delete(&self, resource: &str) -> impl Future<Item = (), Error = Error> {
        self.client
            .delete(&format!("{}/{}", self.url, resource))
            .bearer_auth(&self.bot_token)
            .header(http::header::ACCEPT, "application/json")
            .send()
            .from_err()
            .map(|_| ())
    }

    fn get_bot_id(&self) -> impl Future<Item = String, Error = Error> {
        self.api_get_json("people/me")
            .map(|details: PersonDetails| details.id)
    }

    fn add_webhook(&self, url: &str) -> impl Future<Item = (), Error = Error> {
        let webhook = WebhookRegistration {
            name: "gerritbot".to_string(),
            target_url: url.to_string(),
            resource: "messages".to_string(),
            event: "created".to_string(),
        };

        debug!("adding webhook: {:?}", webhook);

        self.api_post_json("webhooks", &webhook)
            .map(|()| debug!("added webhook"))
    }

    fn list_webhooks(&self) -> impl Future<Item = Webhooks, Error = Error> {
        self.api_get_json("webhooks")
    }

    fn delete_webhook(&self, id: &str) -> impl Future<Item = (), Error = Error> {
        self.api_delete(&format!("webhooks/{}", id))
            .or_else(|e| match e {
                Error::ReqwestError(ref e)
                    if e.status() == Some(http::StatusCode::NO_CONTENT)
                        || e.status() == Some(http::StatusCode::NOT_FOUND) =>
                {
                    Ok(())
                }
                _ => Err(Error::DeleteWebhook(format!(
                    "Could not delete webhook: {}",
                    e
                ))),
            })
            .map(|()| debug!("deleted webhook"))
    }

    pub fn register_webhook<'a>(self, url: &str) -> impl Future<Item = (), Error = Error> {
        let url = url.to_string();
        let delete_client = self.clone();
        let add_client = self.clone();
        self.list_webhooks()
            .map(|webhooks| futures::stream::iter_ok(webhooks.items))
            .flatten_stream()
            .filter(|webhook| webhook.resource == "messages" && webhook.event == "created")
            .inspect(|webhook| debug!("Removing webhook from Spark: {}", webhook.target_url))
            .for_each(move |webhook| delete_client.delete_webhook(&webhook.id))
            .and_then(move |()| add_client.add_webhook(&url))
    }

    pub fn id(&self) -> &str {
        &self.bot_id
    }

    pub fn reply(&self, person_id: &str, msg: &str) -> impl Future<Item = (), Error = Error> {
        let json = json!({
            "toPersonId": person_id,
            "markdown": msg,
        });
        debug!("send message to {}", person_id);
        self.api_post_json("messages", &json)
    }

    pub fn get_message(&self, message_id: &str) -> impl Future<Item = Message, Error = Error> {
        self.api_get_json(&format!("messages/{}", message_id))
    }
}

#[derive(Debug, Clone)]
pub struct CommandMessage {
    pub sender_email: String,
    pub sender_id: String,
    pub command: Command,
}

#[derive(Debug, Clone)]
pub enum Command {
    Enable,
    Disable,
    ShowStatus,
    ShowHelp,
    ShowFilter,
    EnableFilter,
    DisableFilter,
    SetFilter(String),
    Unknown,
}

impl Message {
    /// Load text from Spark for a received message
    /// Note: Spark does not send the text with the message to the registered post hook.
    // pub fn load_text<C: SparkClient + ?Sized>(&mut self, client: &C) -> Result<(), Error> {
    //     let msg = client.get_message(&self.id)?;
    //     self.text = msg.text;
    //     Ok(())
    // }

    /// Convert Spark message to command
    pub fn into_command(self) -> CommandMessage {
        lazy_static! {
            static ref FILTER_REGEX: Regex = Regex::new(r"(?i)^filter (.*)$").unwrap();
        };

        let sender_email = self.person_email;
        let sender_id = self.person_id;
        let command = match &self.text.trim().to_lowercase()[..] {
            "enable" => Command::Enable,
            "disable" => Command::Disable,
            "status" => Command::ShowStatus,
            "help" => Command::ShowHelp,
            "filter" => Command::ShowFilter,
            "filter enable" => Command::EnableFilter,
            "filter disable" => Command::DisableFilter,
            _ => FILTER_REGEX
                .captures(&self.text.trim()[..])
                .and_then(|cap| cap.get(1))
                .map(|m| Command::SetFilter(m.as_str().to_string()))
                .unwrap_or(Command::Unknown),
        };

        CommandMessage {
            sender_email,
            sender_id,
            command,
        }
    }
}

fn reject_webhook_request(
    request: &hyper::Request<hyper::Body>,
) -> Option<hyper::Response<hyper::Body>> {
    use hyper::{Body, Response};

    if request.uri() != "/" {
        // only accept requests at "/"
        Some(
            Response::builder()
                .status(http::StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap(),
        )
    } else if request.method() != http::Method::POST {
        // only accept POST
        Some(
            Response::builder()
                .status(http::StatusCode::METHOD_NOT_ALLOWED)
                .body(Body::empty())
                .unwrap(),
        )
    } else if !request
        .headers()
        .get(http::header::CONTENT_TYPE)
        .map(|v| v.as_bytes().starts_with(&b"application/json"[..]))
        .unwrap_or(false)
    {
        // require "content-type: application/json"
        Some(
            Response::builder()
                .status(http::StatusCode::UNSUPPORTED_MEDIA_TYPE)
                .body(Body::empty())
                .unwrap(),
        )
    } else {
        None
    }
}

/// Decode json body of HTTP request or response.
fn decode_json_body<T, B, C, E>(body: B) -> impl Future<Item = T, Error = Error>
where
    for<'a> T: Deserialize<'a>,
    B: Stream<Item = C, Error = E>,
    C: AsRef<[u8]>,
    Error: From<E>,
{
    // TODO: find a way to avoid copying here
    body.fold(Vec::new(), |mut v, chunk| {
        v.extend_from_slice(chunk.as_ref());
        future::ok(v)
    })
    .from_err()
    .and_then(|v| serde_json::from_slice::<T>(&v).into_future().from_err())
}

pub struct RawWebhookServer<M, S>
where
    M: Stream<Item = Post, Error = ()>,
    S: Future<Item = (), Error = hyper::Error>,
{
    /// Stream of webhook posts.
    pub messages: M,
    /// Future of webhook server. Must be run in order for messages to produce
    /// anything.
    pub server: S,
}

pub fn start_raw_webhook_server(
    listen_address: &SocketAddr,
) -> RawWebhookServer<
    impl Stream<Item = Post, Error = ()>,
    impl Future<Item = (), Error = hyper::Error>,
> {
    use hyper::{Body, Response};
    let (message_sink, messages) = channel(1);
    let listen_address = listen_address.clone();

    // very simple webhook listener
    let server = hyper::Server::bind(&listen_address).serve(move || {
        info!("listening to Spark on {}", listen_address);
        let message_sink = message_sink.clone();

        hyper::service::service_fn_ok(move |request: hyper::Request<Body>| {
            debug!("webhook request: {:?}", request);

            if let Some(error_response) = reject_webhook_request(&request) {
                // reject requests we don't understand
                warn!("rejecting webhook request: {:?}", error_response);
                error_response
            } else {
                let message_sink = message_sink.clone();
                // now try to decode the body
                let f = decode_json_body(request.into_body())
                    .map_err(|e| error!("failed to decode post body: {}", e))
                    .and_then(|post: Post| {
                        message_sink
                            .send(post.clone())
                            .map_err(|e| error!("failed to send post body: {}", e))
                            .map(|_| ())
                    });

                // spawn a future so all of the above actually happens
                // XXX: maybe send future over the stream instead?
                tokio::spawn(f);

                Response::new(Body::empty())
            }
        })
    });

    RawWebhookServer { messages, server }
}

pub struct WebhookServer<M, S>
where
    M: Stream<Item = Message, Error = ()>,
    S: Future<Item = (), Error = hyper::Error>,
{
    /// Stream of webhook posts.
    pub messages: M,
    /// Future of webhook server. Must be run in order for messages to produce
    /// anything.
    pub server: S,
}

pub fn start_webhook_server(
    listen_address: &SocketAddr,
    client: Client,
) -> WebhookServer<
    impl Stream<Item = Message, Error = ()>,
    impl Future<Item = (), Error = hyper::Error>,
> {
    let RawWebhookServer {
        messages: raw_messages,
        server,
    } = start_raw_webhook_server(listen_address);

    let own_id = client.id().to_string();

    let messages = raw_messages
        // ignore own messages
        .filter(move |post| post.data.person_id != own_id)
        .and_then(move |post| {
            client.get_message(&post.data.id).then(|message_result| {
                future::ok(
                    message_result
                        .map_err(|e| error!("failed to fetch message: {}", e))
                        .map(Some)
                        .unwrap_or(None),
                )
            })
        })
        .filter_map(std::convert::identity);

    WebhookServer { messages, server }
}

/*
pub fn sqs_event_stream<C: SparkClient + 'static + ?Sized>(
    client: Rc<C>,
    sqs_url: String,
    sqs_region: rusoto_core::Region,
) -> Result<Box<dyn Stream<Item = CommandMessage, Error = String>>, Error> {
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
        .filter_map(move |mut msg| {
            debug!("Loading text for message: {:#?}", msg);
            if let Err(err) = msg.load_text(&*client) {
                error!("Could not load post's text: {}", err);
                return None;
            }
            Some(msg)
        })
        .map(|msg| msg.into_command())
        .map_err(|err| format!("Error from Spark: {:?}", err));
    Ok(Box::new(sqs_stream))
}
*/
