use hyper;
use hyper_native_tls;
use iron::prelude::*;
use iron::status;
use regex;
use serde;
use serde_json;

use std::mem;
use std::sync::{Arc, Mutex};

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
fn get_json_with_token(url: String, token: &str) -> Result<hyper::client::Response, hyper::Error> {
    let client = new_client(&url);
    let auth = hyper::header::Authorization(hyper::header::Bearer { token: String::from(token) });
    client.get(&url)
        .header(hyper::header::ContentType::json())
        .header(hyper::header::Accept::json())
        .header(auth)
        .send()
}

/// Try to post json to the given url with basic token authorization.
pub fn post_with_token<T>(url: String,
                          token: &str,
                          data: &T)
                          -> Result<hyper::client::Response, hyper::Error>
    where T: serde::ser::Serialize
{
    let client = new_client(&url);
    let payload = serde_json::to_string(data).unwrap();
    let auth = hyper::header::Authorization(String::from("Bearer ") + token);
    client.post(&url)
        .header(hyper::header::ContentType::json())
        .header(auth)
        .body(&payload)
        .send()
}

pub struct SparkClient {
    url: String,
    bot_token: String,
    bot_id: String,
}

impl SparkClient {
    pub fn new(args: args::Args) -> SparkClient {
        SparkClient {
            url: args.spark_url,
            bot_token: args.spark_bot_token,
            bot_id: args.spark_bot_id,
        }
    }

    fn reply(&self, person_id: &str, msg: &str) {
        let json = json!({
            "toPersonId": person_id,
            "markdown": msg,
        });
        let res = post_with_token(self.url.clone() + "/messages", &self.bot_token, &json);
        match res {
            Err(err) => {
                println!("[E] Could not reply to gerrit: {:?}", err);
            }
            _ => (),
        };
    }

    fn get_message_text(&self, message_id: &str) -> Result<hyper::client::Response, hyper::Error> {
        get_json_with_token(self.url.clone() + "/messages/" + message_id,
                            &self.bot_token)
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
struct Message {
    created: String,
    id: String,
    person_email: String,
    person_id: String,
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
        let resp = client.get_message_text(&self.id)
            .map_err(|err| format!("Invalid response from spark: {}", err))?;
        let msg: Message = serde_json::from_reader(resp).map_err(
            |err| String::from(format!("Cannot parse json: {}", err)))?;
        self.text = msg.text;
        Ok(())
    }

    // Convert Spark message to bot action
    fn to_action(self) -> bot::Action {
        lazy_static! {
            static ref RE_CONFIGURE: regex::Regex = regex::Regex::new(r"^configure ([^ ]+)$")
                .unwrap();
            static ref RE_ALL: regex::RegexSet = regex::RegexSet::new(&[
                RE_CONFIGURE.as_str(),
                r"^enable$",
                r"^disable$",
                r"^help$",
            ]).unwrap();
        }

        let matches = RE_ALL.matches(&self.text);
        if !matches.matched_any() {
            return bot::Action::Unknown;
        }

        let pos = matches.iter().next().unwrap();
        match pos {
            0 => {
                let cap = RE_CONFIGURE.captures(&self.text).unwrap();
                return bot::Action::Configure(self.person_id, String::from(&cap[1]));
            }
            1 => bot::Action::Enable(self.person_id),
            2 => bot::Action::Disable(self.person_id),
            3 => bot::Action::Help,
            _ => bot::Action::Unknown,
        }
    }
}

/// Post hook from Spark
pub fn handle_post_webhook(req: &mut Request,
                           client: &SparkClient,
                           bot: Arc<Mutex<bot::Bot>>)
                           -> IronResult<Response> {
    let new_post: Post = match serde_json::from_reader(&mut req.body) {
        Ok(post) => post,
        Err(err) => {
            println!("[E] Could not parse post: {}", err);
            return Ok(Response::with(status::Ok));
        }
    };

    let mut msg = new_post.data;

    // filter own messages
    if msg.person_id == client.bot_id {
        return Ok(Response::with(status::Ok));
    }

    match msg.load_text(&client) {
        Err(err) => {
            println!("[E] Could not load post's text: {}", err);
            return Ok(Response::with(status::Ok));
        }
        _ => (),
    };
    println!("[I] Incoming: {:?}", msg);

    // handle message
    let person_id = msg.person_id.clone();
    let action = msg.to_action();

    let mut bot_guard = bot.lock().unwrap();
    let ref mut bot = *bot_guard;

    // fold over actions
    let old_bot = mem::replace(bot, bot::Bot::new());
    let (new_bot, response_msg) = bot::update(action, old_bot);
    mem::replace(bot, new_bot);

    println!("[D] New state: {:?}", bot);
    client.reply(&person_id, &response_msg);

    Ok(Response::with(status::Ok))
}
