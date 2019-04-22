#![deny(bare_trait_objects)]

use std::convert::identity;
use std::net::SocketAddr;
use std::{error, fmt, io};

use futures::future::{self, Future};
use futures::sync::mpsc::channel;
use futures::{IntoFuture as _, Sink, Stream};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};

mod sqs;

//
// Spark data model
//

/// Define a newtype String.
macro_rules! newtype_string {
    ($type_name:ident, $type_ref_name:ident) => {
        #[derive(
            Deserialize, Serialize, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
        )]
        #[serde(transparent)]
        pub struct $type_name(String);

        impl $type_name {
            pub fn new(s: String) -> Self {
                Self(s)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $type_name {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        #[derive(Serialize, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $type_ref_name(str);

        impl $type_ref_name {
            pub fn new(s: &str) -> &Self {
                unsafe { &*(s as *const str as *const Self) }
            }
        }

        impl std::fmt::Display for $type_ref_name {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl std::borrow::Borrow<$type_ref_name> for $type_name {
            fn borrow(&self) -> &$type_ref_name {
                &*self
            }
        }

        impl std::borrow::ToOwned for $type_ref_name {
            type Owned = $type_name;
            fn to_owned(&self) -> Self::Owned {
                Self::Owned::new(self.0.to_string())
            }
        }

        impl std::ops::Deref for $type_name {
            type Target = $type_ref_name;

            fn deref(&self) -> &$type_ref_name {
                Self::Target::new(&self.0)
            }
        }

        impl<'a> std::cmp::PartialEq<&'a $type_ref_name> for $type_name {
            fn eq(&self, other: &&$type_ref_name) -> bool {
                self.0 == other.0
            }
        }

        impl<'a, 'b> std::cmp::PartialEq<&'a $type_ref_name> for &'b $type_name {
            fn eq(&self, other: &&$type_ref_name) -> bool {
                self.0 == other.0
            }
        }

        impl<'a> std::cmp::PartialEq<$type_name> for &'a $type_ref_name {
            fn eq(&self, other: &$type_name) -> bool {
                self.0 == other.0
            }
        }
    };
}

/// Spark id of the user
newtype_string!(PersonId, PersonIdRef);
newtype_string!(ResourceId, ResourceIdRef);
newtype_string!(Email, EmailRef);
newtype_string!(WebhookId, WebhookIdRef);
newtype_string!(MessageId, MessageIdRef);
newtype_string!(RoomId, RoomIdRef);

#[derive(Deserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RoomType {
    Direct,
    Group,
}

#[derive(Deserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    Memberships,
    Messages,
    Rooms,
}

#[derive(Deserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Created,
    Updated,
    Deleted,
}

fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<chrono::DateTime<chrono::Utc>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    chrono::DateTime::parse_from_rfc3339(&s)
        .map_err(serde::de::Error::custom)
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn serialize_timestamp<S>(
    timestamp: &chrono::DateTime<chrono::Utc>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&timestamp.to_rfc3339())
}

#[derive(Deserialize, Serialize, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct Timestamp(
    #[serde(deserialize_with = "deserialize_timestamp")]
    #[serde(serialize_with = "serialize_timestamp")]
    chrono::DateTime<chrono::Utc>,
);

/// Webhook's post request from Spark API
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WebhookMessage {
    id: WebhookId,
    actor_id: PersonId,
    app_id: String,
    created: Timestamp,
    created_by: PersonId,
    pub data: Message,
    event: EventType,
    name: String,
    org_id: String,
    owned_by: String,
    resource: ResourceId,
    status: String,
    target_url: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    created: Option<Timestamp>,
    id: MessageId,
    pub person_email: Email,
    pub person_id: PersonId,
    room_id: RoomId,
    room_type: RoomType,

    // a message contained in a post does not have text loaded
    #[serde(default)]
    pub text: String,
    markdown: Option<String>,
    html: Option<String>,
    files: Option<Vec<String>>,
}

impl Message {
    /// Create a simple message for use in tests.
    pub fn test_message(person_email: Email, person_id: PersonId, text: String) -> Self {
        Self {
            person_email,
            person_id,
            text,
            created: None,
            room_id: Default::default(),
            room_type: RoomType::Direct,
            html: None,
            files: None,
            id: Default::default(),
            markdown: None,
        }
    }
}

#[derive(Serialize, Debug, Clone)]
pub enum CreateMessageTarget<'a> {
    #[serde(rename = "roomId")]
    RoomId(&'a RoomIdRef),
    #[serde(rename = "toPersonId")]
    PersonId(&'a PersonIdRef),
    #[serde(rename = "toPersonEmail")]
    PersonEmail(&'a EmailRef),
}

impl<'a> From<&'a RoomId> for CreateMessageTarget<'a> {
    fn from(room_id: &'a RoomId) -> CreateMessageTarget<'a> {
        CreateMessageTarget::RoomId(room_id)
    }
}

impl<'a> From<&'a RoomIdRef> for CreateMessageTarget<'a> {
    fn from(room_id: &'a RoomIdRef) -> CreateMessageTarget<'a> {
        CreateMessageTarget::RoomId(room_id)
    }
}

impl<'a> From<&'a PersonId> for CreateMessageTarget<'a> {
    fn from(person_id: &'a PersonId) -> CreateMessageTarget<'a> {
        CreateMessageTarget::PersonId(person_id)
    }
}

impl<'a> From<&'a PersonIdRef> for CreateMessageTarget<'a> {
    fn from(person_id: &'a PersonIdRef) -> CreateMessageTarget<'a> {
        CreateMessageTarget::PersonId(person_id)
    }
}

impl<'a> From<&'a Email> for CreateMessageTarget<'a> {
    fn from(email: &'a Email) -> CreateMessageTarget<'a> {
        CreateMessageTarget::PersonEmail(email)
    }
}

impl<'a> From<&'a EmailRef> for CreateMessageTarget<'a> {
    fn from(email: &'a EmailRef) -> CreateMessageTarget<'a> {
        CreateMessageTarget::PersonEmail(email)
    }
}

#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CreateMessageParameters<'a> {
    #[serde(flatten)]
    target: CreateMessageTarget<'a>,
    text: Option<&'a str>,
    markdown: Option<&'a str>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PersonDetails {
    id: PersonId,
    emails: Vec<Email>,
    display_name: String,
    nick_name: Option<String>,
    org_id: String,
    created: Timestamp,
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
    resource: ResourceType,
    event: EventType,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Webhook {
    id: WebhookId,
    name: String,
    target_url: String,
    resource: ResourceType,
    event: EventType,
    org_id: String,
    created_by: String,
    app_id: String,
    owned_by: String,
    status: String,
    created: Timestamp,
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
    bot_id: PersonId,
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
            bot_token,
            bot_id: PersonId(String::new()),
        };

        bootstrap_client.get_bot_id().map(|bot_id| Client {
            bot_id,
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
            .json(data)
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

    fn get_bot_id(&self) -> impl Future<Item = PersonId, Error = Error> {
        self.api_get_json("people/me")
            .map(|details: PersonDetails| details.id)
    }

    fn add_webhook(&self, url: &str) -> impl Future<Item = (), Error = Error> {
        let webhook = WebhookRegistration {
            name: "gerritbot".to_string(),
            target_url: url.to_string(),
            resource: ResourceType::Messages,
            event: EventType::Created,
        };

        debug!("adding webhook: {:?}", webhook);

        self.api_post_json("webhooks", &webhook)
            .map(|()| debug!("added webhook"))
    }

    fn list_webhooks(&self) -> impl Future<Item = Webhooks, Error = Error> {
        self.api_get_json("webhooks")
    }

    fn delete_webhook(&self, id: &WebhookId) -> impl Future<Item = (), Error = Error> {
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

    pub fn register_webhook(self, url: &str) -> impl Future<Item = (), Error = Error> {
        let url = url.to_string();
        let delete_client = self.clone();
        let add_client = self.clone();
        self.list_webhooks()
            .map(|webhooks| futures::stream::iter_ok(webhooks.items))
            .flatten_stream()
            .filter(|webhook| {
                webhook.resource == ResourceType::Messages && webhook.event == EventType::Created
            })
            .inspect(|webhook| debug!("Removing webhook from Spark: {}", webhook.target_url))
            .for_each(move |webhook| delete_client.delete_webhook(&webhook.id))
            .and_then(move |()| add_client.add_webhook(&url))
    }

    pub fn id(&self) -> &PersonId {
        &self.bot_id
    }

    pub fn send_message<'a, T: ?Sized>(
        &self,
        target: &'a T,
        markdown: &'a str,
    ) -> impl Future<Item = (), Error = Error>
    where
        &'a T: Into<CreateMessageTarget<'a>>,
    {
        self.create_message(CreateMessageParameters {
            target: target.into(),
            markdown: Some(markdown),
            text: None,
        })
    }

    pub fn create_message<'a>(
        &self,
        parameters: CreateMessageParameters<'a>,
    ) -> impl Future<Item = (), Error = Error> {
        debug!("send message to {:?}", parameters.target);
        let json = match serde_json::to_value(&parameters) {
            Ok(json) => json,
            Err(e) => return future::Either::A(future::err(e).from_err()),
        };

        future::Either::B(self.api_post_json("messages", &json))
    }

    pub fn get_message(
        &self,
        message_id: &MessageId,
    ) -> impl Future<Item = Message, Error = Error> {
        self.api_get_json(&format!("messages/{}", message_id))
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
    M: Stream<Item = WebhookMessage, Error = ()>,
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
    impl Stream<Item = WebhookMessage, Error = ()>,
    impl Future<Item = (), Error = hyper::Error>,
> {
    use hyper::{Body, Response};
    let (message_sink, messages) = channel(1);

    info!("listening to Spark on {}", listen_address);

    // very simple webhook listener
    let server = hyper::Server::bind(&listen_address).serve(move || {
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
                    .and_then(|post: WebhookMessage| {
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

/// Fetch messages from webhook message stream using client. Skip messages from
/// own id, log and then ignore errors.
fn fetch_messages<M>(client: Client, raw_messages: M) -> impl Stream<Item = Message, Error = ()>
where
    M: Stream<Item = WebhookMessage, Error = ()>,
{
    let own_id = client.id().clone();
    raw_messages
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
        .filter_map(std::convert::identity)
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

    let messages = fetch_messages(client, raw_messages);

    WebhookServer { messages, server }
}

pub fn raw_sqs_event_stream(
    sqs_url: String,
    sqs_region: rusoto_core::Region,
) -> impl Stream<Item = WebhookMessage, Error = ()> {
    sqs::sqs_receiver(sqs_url, sqs_region)
        // skip messages with an empty body
        .filter_map(|sqs_message| sqs_message.body)
        // decode body
        .and_then(|data| {
            future::ok(
                serde_json::from_str(&data)
                    // log and ignore errors
                    .map_err(|e| error!("failed to parse sqs message body: {}", e))
                    .ok(),
            )
        })
        .filter_map(identity)
}

pub fn sqs_event_stream(
    sqs_url: String,
    sqs_region: rusoto_core::Region,
    client: Client,
) -> impl Stream<Item = Message, Error = ()> {
    let raw_messages = raw_sqs_event_stream(sqs_url, sqs_region);
    fetch_messages(client, raw_messages)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn person_id_ref() {
        let p = PersonId("person-id".to_string());
        let ref_p: &PersonIdRef = &p;
        assert_eq!(p, ref_p);
    }
}
