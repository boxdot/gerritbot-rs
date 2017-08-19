use std::{error, fmt};

use futures::future::Future;
use futures::Sink;
use futures::sync::mpsc::Sender;
use hyper;
use hyper_native_tls;
use iron::prelude::*;
use iron::status;
use serde;
use serde_json;
use tokio_core;

use args;
use bot;

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
    let auth = hyper::header::Authorization(hyper::header::Bearer { token: String::from(token) });
    client
        .get(url)
        .header(hyper::header::ContentType::json())
        .header(hyper::header::Accept::json())
        .header(auth)
        .send()
}

/// Try to post json to the given url with basic token authorization.
pub fn post_with_token<T>(
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
pub fn delete_with_token(url: &str, token: &str) -> Result<hyper::client::Response, hyper::Error> {
    let client = new_client(url);
    let auth = hyper::header::Authorization(String::from("Bearer ") + token);
    client
        .delete(url)
        .header(hyper::header::ContentType::json())
        .header(auth)
        .send()
}

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

#[derive(Deserialize, Debug)]
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

pub struct SparkClient {
    url: String,
    bot_token: String,
    pub bot_id: String,
}

#[derive(Debug)]
pub enum Error {
    HyperError(hyper::Error),
    JsonError(serde_json::Error),
    RegisterWebhook(String),
    DeleteWebhook(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::HyperError(ref err) => fmt::Display::fmt(err, f),
            Error::JsonError(ref err) => fmt::Display::fmt(err, f),
            Error::RegisterWebhook(ref msg) => fmt::Display::fmt(msg, f),
            Error::DeleteWebhook(ref msg) => fmt::Display::fmt(msg, f),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::HyperError(ref err) => err.description(),
            Error::JsonError(ref err) => err.description(),
            Error::RegisterWebhook(ref msg) => msg,
            Error::DeleteWebhook(ref msg) => msg,
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::HyperError(ref err) => err.cause(),
            Error::JsonError(ref err) => err.cause(),
            Error::RegisterWebhook(_) => None,
            Error::DeleteWebhook(_) => None,
        }
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

impl SparkClient {
    pub fn new(args: &args::Args) -> Result<SparkClient, Error> {
        let mut client = SparkClient {
            url: args.spark_url.clone(),
            bot_token: args.spark_bot_token.clone(),
            bot_id: String::new(),
        };
        client.bot_id = client.get_bot_id()?;
        debug!("Bot id: {}", client.bot_id);
        Ok(client)
    }

    pub fn reply(&self, person_id: &str, msg: &str) {
        let json = json!({
            "toPersonId": person_id,
            "markdown": msg,
        });
        let res = post_with_token(&(self.url.clone() + "/messages"), &self.bot_token, &json);
        if let Err(err) = res {
            error!("Could not reply to gerrit: {:?}", err);
        }
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
            .and_then(|resp| if resp.status != hyper::status::StatusCode::Ok {
                Err(Error::RegisterWebhook(
                    format!("Could not register Spark's webhook: {}", resp.status),
                ))
            } else {
                Ok(())
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
            .and_then(|resp| if resp.status !=
                hyper::status::StatusCode::NoContent
            {
                Err(Error::DeleteWebhook(
                    format!("Could not delete webhook: {}", resp.status),
                ))
            } else {
                Ok(())
            })
    }

    pub fn replace_webhook_url(&self, url: &str) -> Result<(), Error> {
        // remove all other webhooks
        let webhooks = self.list_webhooks()?;
        let to_remove = webhooks.items.into_iter().filter_map(
            |webhook| if webhook.resource == "messages" &&
                webhook.event == "created"
            {
                Some(webhook)
            } else {
                None
            },
        );
        for webhook in to_remove {
            self.delete_webhook(&webhook.id)?;
            debug!("Removed webhook from Spark: {}", webhook.target_url);
        }

        // register new webhook
        self.register_webhook(url)
    }

    fn get_message_text(&self, message_id: &str) -> Result<hyper::client::Response, hyper::Error> {
        get_json_with_token(
            &(self.url.clone() + "/messages/" + message_id),
            &self.bot_token,
        )
    }
}

impl Message {
    /// Load text from Spark for a received message
    /// Note: Spark does not send the text with the message to the registered post hook.
    pub fn load_text(&mut self, client: &SparkClient) -> Result<(), String> {
        let resp = client.get_message_text(&self.id).map_err(
            |err| format!("Invalid response from spark: {}", err),
        )?;
        let msg: Message = serde_json::from_reader(resp).map_err(|err| {
            String::from(format!("Could not parse json: {}", err))
        })?;
        self.text = msg.text;
        Ok(())
    }

    /// Convert Spark message to bot action
    pub fn into_action(self) -> bot::Action {
        match &self.text.trim().to_lowercase()[..] {
            "enable" => bot::Action::Enable(self.person_id, self.person_email),
            "disable" => bot::Action::Disable(self.person_id, self.person_email),
            "status" => bot::Action::Status(self.person_id),
            "help" => bot::Action::Help(self.person_id),
            _ => bot::Action::Unknown(self.person_id),
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
