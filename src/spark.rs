use hyper;
use hyper_native_tls;
use iron::prelude::*;
use iron::status;
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
pub fn get_json_with_token(url: &str,
                           token: &str)
                           -> Result<hyper::client::Response, hyper::Error> {
    let client = new_client(url);
    let auth = hyper::header::Authorization(hyper::header::Bearer { token: String::from(token) });
    client.get(url)
        .header(hyper::header::ContentType::json())
        .header(hyper::header::Accept::json())
        .header(auth)
        .send()
}

/// Try to post json to the given url with basic token authorization.
pub fn post_with_token<T>(url: &str,
                          token: &str,
                          data: &T)
                          -> Result<hyper::client::Response, hyper::Error>
    where T: serde::ser::Serialize
{
    let client = new_client(url);
    let payload = serde_json::to_string(data).unwrap();
    let auth = hyper::header::Authorization(String::from("Bearer ") + token);
    client.post(url)
        .header(hyper::header::ContentType::json())
        .header(auth)
        .body(&payload)
        .send()
}

fn reply(url: &str, token: &str, person_id: &str, msg: &str) {
    let json = json!({
        "toPersonId": person_id,
        "markdown": msg,
    });
    let res = post_with_token(&(String::from(url) + "/messages"), token, &json);
    match res {
        Err(err) => {
            println!("[E] Could not reply to gerrit: {:?}", err);
        }
        _ => (),
    };
}

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
    pub fn load_text(&mut self, spark_url: &str, token: &str) -> Result<(), String> {
        let url = String::from(spark_url) + "/messages/" + &self.id;
        let resp = get_json_with_token(&url, token).map_err(
            |err| format!("Invalid response from spark: {}", err))?;
        let msg: Message = serde_json::from_reader(resp).map_err(
            |err| String::from(format!("Cannot parse json: {}", err)))?;
        self.text = msg.text;
        Ok(())
    }

    // Convert Spark message to bot action
    fn to_action(self) -> bot::Action {
        match self.text.trim().to_lowercase().as_ref() {
            "help" => bot::Action::Help,
            "enable" => bot::Action::Enable(self.person_id),
            "disable" => bot::Action::Disable(self.person_id),
            _ => bot::Action::Unknown,
        }
    }
}

const GREETINGS_MSG: &'static str = r#"Hi. I am GerritBot. I can watch Gerrit reviews for you,
and notify you about new +1/-1's.

For more information, type in **help**.

By the way, my icon is made by
[ Madebyoliver ](http://www.flaticon.com/authors/madebyoliver)
from
[ www.flaticon.com ](http://www.flaticon.com)
and is licensed by
[ CC 3.0 BY](http://creativecommons.org/licenses/by/3.0/).
"#;

const HELP_MSG: &'static str = r#"Commands:

`enable` I will start notifying you.

`disable` I will stop notifying you.

`status` I will tell if notification are enabled or disabled for you.

`help` This message
"#;

/// Post hook from Spark
pub fn handle_post_webhook(req: &mut Request,
                           args: args::Args,
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
    if msg.person_id == args.spark_bot_id {
        return Ok(Response::with(status::Ok));
    }

    match msg.load_text(&args.spark_url, &args.spark_bot_token) {
        Err(err) => {
            println!("[E] Could not load post's text: {}", err);
            return Ok(Response::with(status::Ok));
        }
        _ => (),
    };
    println!("[I] Incoming: {:?}", msg);

    // fold over actions
    let person_id = msg.person_id.clone();
    let action = msg.to_action();
    match action {
        bot::Action::Help => {
            println!("[D] Got help action.");
            reply(&args.spark_url, &args.spark_bot_token, &person_id, HELP_MSG);
        }
        bot::Action::Unknown => {
            println!("[D] Got unknown action.");
            reply(&args.spark_url,
                  &args.spark_bot_token,
                  &person_id,
                  GREETINGS_MSG);
        }
        _ => {
            let mut bot_guard = bot.lock().unwrap();
            let ref mut bot = *bot_guard;

            let old_bot = mem::replace(bot, bot::Bot::new());
            let new_bot = bot::update(action, old_bot);
            mem::replace(bot, new_bot);

            println!("[D] New state: {:?}", bot);

            reply(&args.spark_url,
                  &args.spark_bot_token,
                  &person_id,
                  "Got it!");
        }
    };

    Ok(Response::with(status::Ok))
}
