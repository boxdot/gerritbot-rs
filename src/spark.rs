use futures::future::Future;
use futures::Sink;
use futures::sync::mpsc::Sender;
use hyper;
use hyper_native_tls;
use iron::prelude::*;
use iron::status;
use regex;
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

pub struct SparkClient {
    url: String,
    bot_token: String,
    pub bot_id: String,
}

impl SparkClient {
    pub fn new(args: &args::Args) -> SparkClient {
        SparkClient {
            url: args.spark_url.clone(),
            bot_token: args.spark_bot_token.clone(),
            bot_id: args.spark_bot_id.clone(),
        }
    }

    pub fn reply(&self, person_id: &str, msg: &str) {
        let json = json!({
            "toPersonId": person_id,
            "markdown": msg,
        });
        let res = post_with_token(&(self.url.clone() + "/messages"), &self.bot_token, &json);
        if let Err(err) = res {
            println!("[E] Could not reply to gerrit: {:?}", err);
        }
    }

    fn get_message_text(&self, message_id: &str) -> Result<hyper::client::Response, hyper::Error> {
        get_json_with_token(
            &(self.url.clone() + "/messages/" + message_id),
            &self.bot_token,
        )
    }
}

/// Spark id of the user
pub type PersonId = String;

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

impl Message {
    /// Load text from Spark for a received message
    /// Note: Spark does not send the text with the message to the registered post hook.
    pub fn load_text(&mut self, client: &SparkClient) -> Result<(), String> {
        let resp = client.get_message_text(&self.id).map_err(
            |err| format!("Invalid response from spark: {}", err),
        )?;
        let msg: Message = serde_json::from_reader(resp).map_err(|err| {
            String::from(format!("Cannot parse json: {}", err))
        })?;
        self.text = msg.text;
        Ok(())
    }

    /// Convert Spark message to bot action
    pub fn into_action(self) -> bot::Action {
        lazy_static! {
            static ref RE_CONFIGURE: regex::Regex = regex::Regex::new(r"^configure ([^ ]+)$")
                .unwrap();
        }

        match &self.text.trim().to_lowercase()[..] {
            "enable" => bot::Action::Enable(self.person_id),
            "disable" => bot::Action::Disable(self.person_id),
            "help" => bot::Action::Help(self.person_id),
            _ => {
                let cap = RE_CONFIGURE.captures(&self.text);
                match cap {
                    Some(cap) => bot::Action::Configure(self.person_id, String::from(&cap[1])),
                    None => bot::Action::Unknown(self.person_id),
                }
            }
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
            println!("[E] Could not parse post: {}", err);
            return Ok(Response::with(status::Ok));
        }
    };

    let msg = new_post.data;
    remote.spawn(move |_| tx.send(msg).map_err(|_| ()).map(|_| ()));

    Ok(Response::with(status::Ok))
}
